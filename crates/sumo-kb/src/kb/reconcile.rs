//! File-level reconciliation: diff a freshly-read KIF file against
//! the axioms the KB already has under that file tag, apply the
//! delta, then revalidate the narrow neighbourhood of symbols the
//! delta touched.
//!
//! Used by the CLI so that commands like `sumo ask -f Merge.kif` see
//! the on-disk state of `Merge.kif` regardless of whether the LMDB
//! has a stale copy of those axioms from an earlier `sumo load`.
//! The mutation is **in memory only** — persistence is the exclusive
//! job of `sumo load`.
//!
//! ## Algorithm
//!
//! 1. Parse the new text into an AST + per-root structural fingerprint.
//! 2. Compare against the KB's stored `file_roots[file]` +
//!    `file_hashes[file]` via [`compute_file_diff`]: produces
//!    `retained` (sid + new span), `removed` (old sid gone from new),
//!    and `added` (new AST with no matching old sentence).
//! 3. Update spans on every retained sentence (a pure position shift
//!    — symbol content unchanged — doesn't trigger revalidation).
//! 4. **Un-index** each removed sentence: pull it from the store,
//!    the SInE index, the CNF fingerprint/clauses side-cars, and the
//!    TFF axiom cache.  Symbols mentioned in the removed sentence
//!    land in the "altered" set.
//! 5. **Add** the new sentences by re-emitting them as KIF text and
//!    running them through the normal `ingest()` → `make_session_axiomatic()`
//!    pipeline — this reuses every post-parse phase (clausify,
//!    fingerprint-dedup, symbol register, SInE add, taxonomy extend).
//!    Symbols mentioned in the newly-appended sids land in the
//!    altered set.
//! 6. **Taxonomy:** if any removed *or* added sentence has a
//!    taxonomy-relation head (`subclass`/`instance`/`subrelation`/
//!    `subAttribute`) do a full `rebuild_taxonomy`; otherwise the
//!    incremental extend that `ingest()` did in step 5 is sufficient.
//! 7. **Axiom cache:** dropped unconditionally on any delta —
//!    rebuilt lazily on the next `ask_embedded` call.
//! 8. **Smart revalidation:** feed the altered-symbol set into SInE
//!    as a seed and revalidate only the selected neighbourhood.
//!    Running the full KB validator after every edit would be
//!    O(N) per reconcile — for a 100k-axiom SUMO load that's seconds
//!    of wasted work.  Scoping to SInE-reachable axioms revalidates
//!    exactly the formulas that could plausibly have been broken by
//!    the symbol-level changes.

// Previously `#![cfg(feature = "ask")]` because the smart-revalidate
// phase used the SInE index.  SInE is now a plain axiom-relevance
// index (no `ask`-specific state), so reconcile is available in
// every feature combination.

use std::collections::HashSet;

use crate::error::{KbError, SemanticError};
use crate::kb::{compute_file_diff, KnowledgeBase};
use crate::parse::fingerprint::sentence_fingerprint;
use crate::parse::parse_document;
use crate::sine::SineParams;
use crate::types::{SentenceId, SymbolId};

/// Per-file summary of what changed during a reconcile pass.  Used
/// by the CLI's `sumo load` to emit an info-level line and commit
/// deltas to the LMDB, and by tests to assert the expected deltas.
#[derive(Debug, Default)]
pub struct ReconcileReport {
    /// File tag the reconcile ran against.  Same value that appears
    /// in `Sentence.file`.
    pub file:         String,
    /// Number of sentences whose content was unchanged (span may
    /// have shifted).  These are kept verbatim — no reclausification,
    /// no revalidation, no SInE churn.
    pub retained:     usize,
    /// Sentence ids that existed in the KB under this `file` tag
    /// but are absent from the freshly-read text.  Each was removed
    /// from the store, SInE index, and CNF side-cars in memory;
    /// the caller (typically `sumo load`) is responsible for
    /// deleting these from the persistent DB.
    pub removed_sids: Vec<SentenceId>,
    /// Sentence ids newly ingested from `new_text`.  The in-memory
    /// KB already has these as axioms; the caller commits them.
    pub added_sids:   Vec<SentenceId>,
    /// Parse errors encountered while reading `new_text`.  Reported
    /// verbatim — the reconcile aborts without mutating the KB when
    /// any parse error is present.
    pub parse_errors: Vec<KbError>,
    /// Semantic errors surfaced by the smart-revalidate pass.
    ///
    /// Only *hard* errors land here — true hard errors and warnings
    /// promoted via `-W <code>` / `-Wall` (both return `Err` from
    /// `validate_sentence`).  Plain warnings are already logged by
    /// `SemanticError::handle` and never reach this vec.  A
    /// `sumo load` invocation should abort the commit when this is
    /// non-empty to mirror `--flush`'s "validation errors abort"
    /// semantics.  Read-only commands (`ask`, `validate`, …) log
    /// each entry but proceed.
    pub semantic_errors: Vec<SemanticError>,
    /// Number of axioms the smart-revalidator visited.  Useful for
    /// tests and for `-v` diagnostics; compared to `kb.axiom_count()`
    /// it shows how much work was saved by scoping.
    pub revalidated:     usize,
}

impl ReconcileReport {
    /// Convenience — number of sentences added to the in-memory KB.
    #[inline] pub fn added(&self) -> usize { self.added_sids.len() }
    /// Convenience — number of sentences removed from the in-memory KB.
    #[inline] pub fn removed(&self) -> usize { self.removed_sids.len() }
}

impl ReconcileReport {
    /// `true` when nothing changed structurally — retained only,
    /// no adds or removes.  Callers use this to skip log output.
    pub fn is_noop(&self) -> bool {
        self.added_sids.is_empty()
            && self.removed_sids.is_empty()
            && self.parse_errors.is_empty()
    }
}

impl KnowledgeBase {
    /// Reconcile the KB's in-memory state for `file` against
    /// `new_text`.  See module docs for algorithm.
    ///
    /// Idempotent when `new_text` hasn't changed since the KB was
    /// built: produces a report with only `retained` populated.
    /// Parse errors abort without mutating the KB — the existing
    /// sentences under `file` stay intact.
    pub fn reconcile_file(&mut self, file: &str, new_text: &str) -> ReconcileReport {
        let mut report = ReconcileReport {
            file: file.to_owned(),
            ..Default::default()
        };

        // -- 1. Parse.  Parse errors abort without mutation. ----------------
        let doc = match self.reconcile_parse(file, new_text, &mut report) {
            Some(d) => d,
            None    => return report,
        };

        // -- 2. Diff vs. stored state. --------------------------------------
        let diff = self.reconcile_compute_diff(file, &doc);
        report.retained     = diff.retained.len();
        report.removed_sids = diff.removed.clone();

        // -- 3. Retained: update spans only.  No revalidation — a
        //    pure position shift doesn't change symbol content. -------------
        for (sid, new_span) in &diff.retained {
            self.layer.store.update_sentence_span(*sid, new_span.clone());
        }

        // Noop fast-path — common case for an unedited `-f` file.
        if diff.removed.is_empty() && diff.added.is_empty() {
            return report;
        }

        // -- 4–5. Track altered symbols and un-index removed sentences. -----
        let mut altered_syms: HashSet<SymbolId> = HashSet::new();
        let removed_touches_tax = self.any_touches_taxonomy(&diff.removed);
        self.reconcile_apply_removals(&diff.removed, &mut altered_syms);

        // -- 6. Ingest added sentences through the normal pipeline. ---------
        let added_touches_tax =
            self.reconcile_apply_additions(file, &diff.added, &mut altered_syms, &mut report);

        // -- 7–8. Taxonomy rebuild (when needed) + axiom-cache drop. --------
        if removed_touches_tax || added_touches_tax {
            self.layer.rebuild_taxonomy();
        }
        // The TFF axiom cache lives behind `feature = "ask"` (only
        // the embedded prover consumes it).  Drop it here so a
        // follow-up `ask_embedded` rebuilds lazily — no-op in builds
        // without the field.
        #[cfg(feature = "ask")]
        { self.axiom_cache = None; }

        // -- 9. Smart revalidation over the SInE neighbourhood. -------------
        self.reconcile_smart_revalidate(&altered_syms, &mut report);

        report
    }

    /// Phase 1 of reconcile — parse `new_text`, surfacing diagnostics
    /// as parse errors on `report`.  Returns `None` (and leaves
    /// `report.parse_errors` populated) on any parse failure so the
    /// caller aborts before mutating the KB.
    fn reconcile_parse(
        &self,
        file:     &str,
        new_text: &str,
        report:   &mut ReconcileReport,
    ) -> Option<crate::parse::ParsedDocument> {
        let doc = parse_document(file, new_text);
        if doc.has_errors() {
            for d in &doc.diagnostics {
                report.parse_errors.push(KbError::Other(format!(
                    "{}: {}", d.range.file, d.message
                )));
            }
            return None;
        }
        Some(doc)
    }

    /// Phase 2 — compute the diff between the stored `file_roots[file]`
    /// (+ `file_hashes[file]` when available) and the fresh parse.
    ///
    /// When the KB was built via `load_kif` / `tell`, `file_hashes` is
    /// populated positionally alongside `file_roots`.  LMDB rehydrate
    /// currently only restores `file_roots` (see `persist/load.rs`),
    /// so the fallback recomputes hashes on the fly via
    /// [`sentence_fingerprint_from_store`] — the plain
    /// (non-canonical) hash that matches
    /// [`sentence_fingerprint`](crate::parse::fingerprint::sentence_fingerprint)
    /// on the equivalent AST.
    fn reconcile_compute_diff(
        &self,
        file: &str,
        doc:  &crate::parse::ParsedDocument,
    ) -> crate::kb::FileDiff {
        let old_sids = self.layer.store.file_roots
            .get(file).cloned().unwrap_or_default();
        let old_hashes = match self.layer.store.file_hashes.get(file) {
            Some(h) if h.len() == old_sids.len() => h.clone(),
            _ => old_sids.iter()
                .map(|&sid| crate::parse::fingerprint::sentence_fingerprint_from_store(
                    sid, &self.layer.store,
                ))
                .collect(),
        };
        let new_hashes: Vec<u64> =
            doc.ast.iter().map(sentence_fingerprint).collect();
        compute_file_diff(
            file,
            &old_sids,
            &old_hashes,
            &new_hashes,
            &doc.ast,
            &doc.root_spans,
        )
    }

    /// Phase 5 — un-index every sid in `removed` from the store, SInE,
    /// and CNF side-cars.  Their symbol sets are union'd into
    /// `altered_syms` so the smart-revalidate pass later sees every
    /// symbol whose axiom neighbourhood just shrank.
    fn reconcile_apply_removals(
        &mut self,
        removed:      &[SentenceId],
        altered_syms: &mut HashSet<SymbolId>,
    ) {
        for &sid in removed {
            for sym in self.layer.store.sentence_symbols(sid) {
                altered_syms.insert(sym);
            }
            // SInE: decrement generality, recompute affected triggers.
            {
                let mut idx = self.sine_index.write().expect("sine_index poisoned");
                idx.remove_axiom(sid);
            }
            // CNF side-cars.
            #[cfg(feature = "cnf")]
            {
                self.fingerprints.retain(|_, (s, _)| *s != sid);
                self.clauses.remove(&sid);
            }
            // Store: drops the sentence, head-index entry, and file
            // root/hash row.  `remove_sentence` is a no-op when the
            // sid is already missing.
            self.layer.store.remove_sentence(sid);
        }
    }

    /// Phase 6 — re-emit every added AST as KIF text and run it
    /// through the normal `ingest()` + `make_session_axiomatic()`
    /// pipeline.  Reuses every post-parse phase (clausify,
    /// fingerprint-dedup, SInE add, taxonomy extend, symbol register)
    /// so reconcile and fresh load produce the same end-state
    /// modulo the retained-sid preservation.
    ///
    /// `validate=false` because validation happens later in the
    /// SInE-scoped revalidate pass — re-running on every freshly-
    /// ingested sentence would double the work.
    ///
    /// Returns `true` iff any newly-added sentence has a
    /// taxonomy-relation head (which means phase 7 needs a full
    /// rebuild rather than the cheaper incremental extend that
    /// `ingest` already ran).
    fn reconcile_apply_additions(
        &mut self,
        file:         &str,
        added:        &[crate::parse::ast::AstNode],
        altered_syms: &mut HashSet<SymbolId>,
        report:       &mut ReconcileReport,
    ) -> bool {
        if added.is_empty() { return false; }

        let added_text = added.iter()
            .map(|n| n.format_plain(0))
            .collect::<Vec<_>>()
            .join("\n");

        const ADD_SESSION: &str = crate::session_tags::SESSION_RECONCILE_ADD;

        // Snapshot `file_roots[file]` pre-ingest so we can identify
        // freshly-minted sids afterwards (ingest doesn't return them
        // directly).
        let pre_roots: HashSet<SentenceId> = self.layer.store.file_roots
            .get(file)
            .map(|v| v.iter().copied().collect())
            .unwrap_or_default();

        let _ingest_result = self.ingest(&added_text, file, ADD_SESSION, /*validate=*/ false);
        self.make_session_axiomatic(ADD_SESSION);

        let new_sids: Vec<SentenceId> = self.layer.store.file_roots
            .get(file).cloned().unwrap_or_default()
            .into_iter()
            .filter(|sid| !pre_roots.contains(sid))
            .collect();

        for &sid in &new_sids {
            for sym in self.layer.store.sentence_symbols(sid) {
                altered_syms.insert(sym);
            }
        }
        let touches_tax = self.any_touches_taxonomy(&new_sids);
        report.added_sids = new_sids;
        touches_tax
    }

    /// Phase 9 — SInE-seed from altered symbols, run semantic
    /// validation on the neighbourhood, and collect any hard errors
    /// into `report.semantic_errors`.
    ///
    /// Depth `None` lets BFS run to fixed point — on focused edits
    /// the neighbourhood is small, on structural edits (e.g. altering
    /// a base class) it's larger by design.  Validation is cheap
    /// per-sentence so overselecting is safe; underselecting would
    /// silently miss cross-file regressions.
    fn reconcile_smart_revalidate(
        &self,
        altered_syms: &HashSet<SymbolId>,
        report:       &mut ReconcileReport,
    ) {
        if altered_syms.is_empty() { return; }
        let selected: HashSet<SentenceId> = {
            let idx = self.sine_index.read().expect("sine_index poisoned");
            idx.select(altered_syms, SineParams::default().depth_limit)
        };
        report.revalidated = selected.len();
        for sid in selected {
            if let Err(e) = self.layer.validate_sentence(sid) {
                report.semantic_errors.push(e);
            }
        }
    }

    /// Reconcile multiple `(file_tag, new_text)` pairs in one pass.
    /// Each file is reconciled independently — removals in one
    /// don't cascade into another, which matches the user's stated
    /// semantics: `-f` is a per-file operation.
    pub fn reconcile_files<'a, I, S>(&mut self, files: I) -> Vec<ReconcileReport>
    where
        I: IntoIterator<Item = (&'a str, S)>,
        S: AsRef<str>,
    {
        files
            .into_iter()
            .map(|(tag, text)| self.reconcile_file(tag, text.as_ref()))
            .collect()
    }

    /// Thin delegator to [`KifStore::any_touches_taxonomy`].  Kept
    /// as a `KnowledgeBase` method so the reconcile algorithm reads
    /// naturally (`self.any_touches_taxonomy(&diff.removed)`) without
    /// callers reaching into `self.layer.store`.
    #[inline]
    fn any_touches_taxonomy(&self, sids: &[SentenceId]) -> bool {
        self.store_for_testing().any_touches_taxonomy(sids)
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KnowledgeBase;

    fn load_file(kb: &mut KnowledgeBase, file: &str, text: &str) {
        let r = kb.load_kif(text, file, Some(file));
        assert!(r.ok, "initial load failed: {:?}", r.errors);
        kb.make_session_axiomatic(file);
    }

    #[test]
    fn noop_reconcile_when_text_unchanged() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)\n(subclass Cat Mammal)");
        let r = kb.reconcile_file("t.kif", "(subclass Dog Mammal)\n(subclass Cat Mammal)");
        assert_eq!(r.retained, 2);
        assert_eq!(r.added(), 0);
        assert_eq!(r.removed(), 0);
        assert!(r.is_noop());
    }

    #[test]
    fn reconcile_detects_pure_addition() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)");
        let r = kb.reconcile_file(
            "t.kif",
            "(subclass Dog Mammal)\n(subclass Cat Mammal)",
        );
        assert_eq!(r.retained, 1);
        assert_eq!(r.added(), 1);
        assert_eq!(r.removed(), 0);
    }

    #[test]
    fn reconcile_detects_pure_removal() {
        let mut kb = KnowledgeBase::new();
        load_file(
            &mut kb,
            "t.kif",
            "(subclass Dog Mammal)\n(subclass Cat Mammal)",
        );
        let r = kb.reconcile_file("t.kif", "(subclass Dog Mammal)");
        assert_eq!(r.retained, 1);
        assert_eq!(r.added(), 0);
        assert_eq!(r.removed(), 1);
    }

    #[test]
    fn reconcile_detects_mixed_edit() {
        let mut kb = KnowledgeBase::new();
        load_file(
            &mut kb,
            "t.kif",
            "(subclass Dog Mammal)\n(subclass Cat Mammal)",
        );
        let r = kb.reconcile_file(
            "t.kif",
            "(subclass Dog Mammal)\n(subclass Whale Mammal)",
        );
        assert_eq!(r.retained, 1);
        assert_eq!(r.added(), 1);
        assert_eq!(r.removed(), 1);
    }

    #[test]
    fn removed_axioms_drop_from_sine_index() {
        let mut kb = KnowledgeBase::new();
        load_file(
            &mut kb,
            "t.kif",
            "(subclass Dog Mammal)\n(subclass Cat Mammal)",
        );
        // Both axioms should be indexed.
        let before = kb.sine_index.read().unwrap().axiom_count();
        assert_eq!(before, 2);

        let _r = kb.reconcile_file("t.kif", "(subclass Dog Mammal)");
        let after = kb.sine_index.read().unwrap().axiom_count();
        assert_eq!(after, 1);
    }

    #[test]
    fn added_axioms_are_indexed_in_sine() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)");
        let _r = kb.reconcile_file(
            "t.kif",
            "(subclass Dog Mammal)\n(subclass Whale Mammal)",
        );
        assert_eq!(kb.sine_index.read().unwrap().axiom_count(), 2);
        // Whale should now be a known symbol in SInE.
        let whale = kb.store_for_testing().sym_id("Whale");
        assert!(whale.is_some(), "Whale should have been interned");
    }

    #[test]
    fn retained_sentences_keep_their_sids() {
        // The contract that makes reconcile cheaper than wipe-and-reload:
        // retained sentences keep the exact same SentenceId, so SInE
        // triggers, fingerprint keys, and downstream caches all stay
        // valid without rehashing.
        let mut kb = KnowledgeBase::new();
        load_file(
            &mut kb,
            "t.kif",
            "(subclass Dog Mammal)\n(subclass Cat Mammal)",
        );
        let old_dog_sid = kb.store_for_testing().file_roots.get("t.kif").unwrap()[0];

        let _r = kb.reconcile_file(
            "t.kif",
            "(subclass Dog Mammal)\n(subclass Whale Mammal)",
        );
        let new_dog_sid = kb.store_for_testing().file_roots.get("t.kif").unwrap()[0];
        assert_eq!(
            old_dog_sid, new_dog_sid,
            "retained sentence must keep its SentenceId"
        );
    }

    #[test]
    fn parse_error_aborts_without_mutation() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)");
        let axioms_before = kb.sine_index.read().unwrap().axiom_count();
        // Unclosed paren → parse error.  Reconcile must leave the
        // KB untouched.
        let r = kb.reconcile_file("t.kif", "(subclass Dog Mammal");
        assert!(!r.parse_errors.is_empty());
        assert_eq!(r.retained, 0);
        assert_eq!(r.added(), 0);
        assert_eq!(r.removed(), 0);
        assert_eq!(kb.sine_index.read().unwrap().axiom_count(), axioms_before);
    }

    #[test]
    fn alpha_equivalent_edit_is_treated_as_remove_and_add() {
        // `compute_file_diff` uses structural (non-alpha-equivalent)
        // fingerprints — renaming `?X` to `?Y` makes the sentence
        // look different at the file-diff level even though it's
        // logically the same.  Document this: the resulting KB is
        // still correct (the CNF-fingerprint dedup inside `ingest`
        // will recognise the alpha-equivalent form and silently
        // drop the "added" as a duplicate of the already-promoted
        // version), but file-diff classifies it as retain-0 /
        // add-1 / remove-1 because the surface text differs.
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(=> (P ?X) (Q ?X))");
        let r = kb.reconcile_file("t.kif", "(=> (P ?Y) (Q ?Y))");
        assert_eq!(r.retained, 0);
        assert_eq!(r.added(),    1);
        assert_eq!(r.removed(),  1);
    }
}
