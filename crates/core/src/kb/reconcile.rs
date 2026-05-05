// crates/core/src/kb/reconcile.rs
//
// KB file reconciliation

//! Tag level reconciliation: diff a freshly-read KIF file against
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

use std::collections::{HashMap, HashSet};

use crate::{AstNode, Span};
use crate::semantics::errors::SemanticError;
use crate::kb::{KnowledgeBase};
use crate::parse::fingerprint::sentence_fingerprint;
use crate::parse::parse_document;
use crate::sine::SineParams;
use crate::types::{SentenceId, SymbolId};

use super::KbError;
use super::ingest::TellResult;

// KB implementation (public API)
impl KnowledgeBase {
    /// Reconcile the KB's in-memory state for `file` against
    /// `new_text`.  See module docs for algorithm.
    ///
    /// Idempotent when `new_text` hasn't changed since the KB was
    /// built: produces a report with only `retained` populated.
    /// Parse errors abort without mutating the KB — the existing
    /// sentences under `file` stay intact.
    pub fn reconcile_file(&mut self, file: &str, new_text: &str) -> ReconcileReport {
        with_guard!(self);
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
            self.layer.semantic.syntactic.update_sentence_span(*sid, new_span.clone());
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
            self.layer.semantic.rebuild_taxonomy();
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
    ) -> FileDiff {
        let old_sids = self.layer.semantic.syntactic.file_roots
            .get(file).cloned().unwrap_or_default();
        let old_hashes = match self.layer.semantic.syntactic.file_hashes.get(file) {
            Some(h) if h.len() == old_sids.len() => h.clone(),
            _ => old_sids.iter()
                .map(|&sid| crate::parse::fingerprint::sentence_fingerprint_from_store(
                    sid, &self.layer.semantic.syntactic,
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
            for sym in self.layer.semantic.syntactic.sentence_symbols(sid) {
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
            self.layer.semantic.syntactic.remove_sentence(sid);
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
        let touches_tax = self.reconcile_apply_additions_deferred(
            file, added, altered_syms, report,
        );
        // Single-file callers promote here.  The batched
        // `reconcile_files` skips this and promotes once after the
        // whole batch so `make_session_axiomatic` can do one bulk
        // SInE rebuild instead of N quadratic per-axiom updates.
        self.make_session_axiomatic(crate::session_tags::SESSION_RECONCILE_ADD);
        touches_tax
    }

    /// Deferred-promotion variant of [`reconcile_apply_additions`].
    ///
    /// Performs every step of the addition phase — snapshot the
    /// pre-ingest root set, stream the new ASTs through `ingest()`
    /// into the shared `SESSION_RECONCILE_ADD`, diff the post-ingest
    /// root set to identify freshly-minted sids, collect their
    /// symbols into `altered_syms` — except the final
    /// `make_session_axiomatic` call.
    ///
    /// Callers batching across multiple files (the bulk
    /// [`reconcile_files`] path) invoke this once per file, then
    /// call `make_session_axiomatic(SESSION_RECONCILE_ADD)` exactly
    /// once after the last file.  That way `SineIndex::add_axioms`
    /// sees the whole batch in one call and routes it through the
    /// bulk-rebuild path instead of paying per-file incremental
    /// cost that scales with KB size.
    fn reconcile_apply_additions_deferred(
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
        let pre_roots: HashSet<SentenceId> = self.layer.semantic.syntactic.file_roots
            .get(file)
            .map(|v| v.iter().copied().collect())
            .unwrap_or_default();

        let _ingest_result = self.ingest(&added_text, file, ADD_SESSION, /*validate=*/ false);

        let new_sids: Vec<SentenceId> = self.layer.semantic.syntactic.file_roots
            .get(file).cloned().unwrap_or_default()
            .into_iter()
            .filter(|sid| !pre_roots.contains(sid))
            .collect();

        for &sid in &new_sids {
            for sym in self.layer.semantic.syntactic.sentence_symbols(sid) {
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
            if let Err(e) = self.layer.semantic.validate_sentence(sid) {
                report.semantic_errors.push(e);
            }
        }
    }

    /// Reconcile multiple `(file_tag, new_text)` pairs in a single
    /// batched pass.
    ///
    /// Structurally equivalent to calling [`Self::reconcile_file`] in a
    /// loop, but with every "batchable" phase folded into one
    /// whole-batch pass at the end:
    ///
    /// | Phase                           | Per-file / batched |
    /// |---------------------------------|--------------------|
    /// | Parse                           | per-file           |
    /// | Compute diff                    | per-file           |
    /// | Update retained spans           | per-file           |
    /// | Apply removals                  | per-file           |
    /// | Ingest additions (shared session) | per-file (deferred) |
    /// | `make_session_axiomatic`        | **batched (once)** |
    /// | `rebuild_taxonomy`              | **batched (if needed)** |
    /// | Drop axiom cache                | batched (once)     |
    /// | Smart revalidation              | **batched (union of altered symbols)** |
    ///
    /// The batched shape is a strict speed win on any multi-file
    /// input where the per-file batches would otherwise fall into
    /// `SineIndex::add_axioms`' incremental path: boot-time bulk
    /// loads, `sumo/setActiveFiles` wholesale KB swaps, and the
    /// `load` subcommand's initial ingest.  For a single-file call
    /// the semantics are identical to [`Self::reconcile_file`] modulo
    /// the SInE call site.
    ///
    /// Per-file error reporting is preserved: parse errors surface
    /// on the offending file's `ReconcileReport` and don't abort
    /// the rest of the batch; semantic errors from the union
    /// revalidation fan back to whichever file each errored sid
    /// lives in via its `Sentence.file` tag.
    ///
    /// Returns one `ReconcileReport` per input file, in input
    /// order.  Files with parse errors produce a report with
    /// `parse_errors` populated and `retained` / `added_sids` /
    /// `removed_sids` empty.
    pub fn reconcile_files<'a, I, S>(&mut self, files: I) -> Vec<ReconcileReport>
    where
        I: IntoIterator<Item = (&'a str, S)>,
        S: AsRef<str>,
    {
        with_guard!(self);
        let mut reports:       Vec<ReconcileReport>     = Vec::new();
        let mut altered_syms:  HashSet<SymbolId>        = HashSet::new();
        let mut needs_tax_rebuild = false;
        let mut any_adds_or_removes = false;

        // -- Phase 1: per-file work that can't batch. -----------------------
        for (tag, text) in files {
            let mut report = ReconcileReport {
                file: tag.to_owned(),
                ..Default::default()
            };

            // Parse.
            let doc = match self.reconcile_parse(tag, text.as_ref(), &mut report) {
                Some(d) => d,
                None    => { reports.push(report); continue; }
            };

            // Diff.
            let diff = self.reconcile_compute_diff(tag, &doc);
            report.retained     = diff.retained.len();
            report.removed_sids = diff.removed.clone();

            // Retained: span-only updates.
            for (sid, new_span) in &diff.retained {
                self.layer.semantic.syntactic.update_sentence_span(*sid, new_span.clone());
            }

            // Noop fast-path — same signal reconcile_file uses.
            if diff.removed.is_empty() && diff.added.is_empty() {
                reports.push(report);
                continue;
            }
            any_adds_or_removes = true;

            // Removals: must apply per-file (they mutate per-file
            // state), but altered_syms accumulates across files.
            if !diff.removed.is_empty() {
                needs_tax_rebuild |= self.any_touches_taxonomy(&diff.removed);
                self.reconcile_apply_removals(&diff.removed, &mut altered_syms);
            }

            // Additions: ingest into the shared session, DO NOT promote.
            if !diff.added.is_empty() {
                let added_tax_touch = self.reconcile_apply_additions_deferred(
                    tag, &diff.added, &mut altered_syms, &mut report,
                );
                needs_tax_rebuild |= added_tax_touch;
            }

            reports.push(report);
        }

        // Nothing mutated — every file was a no-op or parse-errored.
        // Skip the expensive phase 2–4 passes entirely.
        if !any_adds_or_removes {
            return reports;
        }

        // -- Phase 2: one promotion over the accumulated shared session. ----
        // Even if no file contributed adds (pure-removal batch), the
        // session is empty and `make_session_axiomatic` is a no-op —
        // cheap to call unconditionally.
        self.make_session_axiomatic(crate::session_tags::SESSION_RECONCILE_ADD);

        // -- Phase 3: one taxonomy rebuild if any file needed it. -----------
        if needs_tax_rebuild {
            self.layer.semantic.rebuild_taxonomy();
        }
        #[cfg(feature = "ask")]
        { self.axiom_cache = None; }

        // -- Phase 4: one SInE-scoped revalidation over the union. ----------
        if !altered_syms.is_empty() {
            let selected: HashSet<SentenceId> = {
                let idx = self.sine_index.read().expect("sine_index poisoned");
                idx.select(&altered_syms, SineParams::default().depth_limit)
            };
            // Per-file revalidated counts + semantic-error fan-out.
            // Each selected sid maps to exactly one file via its
            // `Sentence.file` tag, so the attribution is deterministic.
            // Lookups into `reports` are linear per-error, but the
            // typical batch is 10s of files and errors are rare —
            // the constant-factor cost is far below the cost of doing
            // N SInE selects in a per-file loop.
            for sid in &selected {
                let file_opt = self.sentence(*sid).map(|s| s.file.clone());
                if let Some(ref file) = file_opt {
                    if let Some(r) = reports.iter_mut().find(|r| r.file == *file) {
                        r.revalidated += 1;
                    }
                }
                if let Err(e) = self.layer.semantic.validate_sentence(*sid) {
                    if let Some(file) = file_opt {
                        if let Some(r) = reports.iter_mut().find(|r| r.file == file) {
                            r.semantic_errors.push(e);
                        }
                    }
                }
            }
        }

        reports
    }

    /// Thin delegator to [`SyntacticLayer::any_touches_taxonomy`].  Kept
    /// as a `KnowledgeBase` method so the reconcile algorithm reads
    /// naturally (`self.any_touches_taxonomy(&diff.removed)`) without
    /// callers reaching into `self.layer.semantic.syntactic`.
    #[inline]
    fn any_touches_taxonomy(&self, sids: &[SentenceId]) -> bool {
        self.store_for_testing().any_touches_taxonomy(sids)
    }

    /// Apply an incremental reload diff to the knowledge base.
    ///
    /// General-purpose primitive for any consumer that wants to
    /// re-sync an in-memory KB with a changed source file without
    /// paying the full [`KnowledgeBase::remove_file`] + 
    /// [`KnowledgeBase::load_kif`] cost. See [`reconcile::FileDiff`] 
    ///
    /// Orphan pruning + cache invalidation run once at the end: the
    /// union of symbol sets from removed + added sentences is
    /// collected and handed to
    /// [`SemanticLayer::invalidate_symbols`]
    /// for targeted eviction.  Retained sentences trigger no cache
    /// churn.
    ///
    /// # What this does **not** do
    ///
    /// Compared to [`KnowledgeBase::reconcile_file`] (which runs the
    /// full ingest pipeline on added sentences), this method is a
    /// lower-level primitive and deliberately skips several
    /// derived-state updates:
    ///
    /// - **No CNF dedup / fingerprint registration.**  Added
    ///   sentences go straight into the store without consulting
    ///   `self.fingerprints`.  An added sentence that happens to be
    ///   a clause-level duplicate of an existing axiom is accepted
    ///   silently — both copies remain.  The LSP use case doesn't
    ///   need prover-level dedup; CLI reconcile does, which is why
    ///   CLI callers should use `reconcile_file` instead.
    /// - **No SInE maintenance.**  Removed sids stay in
    ///   `SineIndex::sym_axioms`; added sids aren't inserted.
    ///   Correct only under the LSP invariant that proofs aren't run
    ///   between diffs.
    /// - **No taxonomy rebuild or extend.**  The `SemanticLayer`'s
    ///   taxonomy keeps any stale edges from the removed sentences.
    ///   Again, acceptable for editor tooling, not for proof paths.
    /// - **No axiom cache invalidation.**  The TFF IR cache survives
    ///   across the diff.
    ///
    /// For any caller that runs proofs, persists to the DB, or
    /// otherwise needs the full derived-state consistency, use
    /// [`KnowledgeBase::reconcile_file`] — it layers
    /// `compute_file_diff` + the full ingest pipeline on top of this
    /// primitive's shape.
    pub fn apply_file_diff(&mut self, diff: FileDiff) -> TellResult {
        let mut result = TellResult { ok: true, errors: Vec::new(), warnings: Vec::new() };
        let mut affected_syms: HashSet<SymbolId> = HashSet::new();

        // 1. Retained: update spans only.
        for (sid, new_span) in &diff.retained {
            let ok = self.layer.semantic.syntactic.update_sentence_span(*sid, new_span.clone());
            if !ok {
                self.warn(format!("apply_file_diff: retained sid={} missing in store", sid));
            }
        }

        // 2. Removed: collect symbols first (they'll need invalidation),
        //    then drop each sentence.
        for &sid in &diff.removed {
            for sym in self.layer.semantic.syntactic.sentence_symbols(sid) {
                affected_syms.insert(sym);
            }
            self.layer.semantic.syntactic.remove_sentence(sid);
        }

        // 3. Added: append as root sentences.
        let mut parse_errs: Vec<(Span, KbError)> = Vec::new();
        for node in &diff.added {
            if let Some(sid) = self.layer.semantic.syntactic.append_root_sentence(node, &diff.file, &mut parse_errs) {
                for sym in self.layer.semantic.syntactic.sentence_symbols(sid) {
                    affected_syms.insert(sym);
                }
            }
        }
        for (_, e) in parse_errs {
            result.ok = false;
            result.errors.push(e);
        }

        // 4. Prune orphaned symbols + invalidate affected cache entries.
        if !diff.removed.is_empty() {
            self.layer.semantic.syntactic.prune_orphaned_symbols_now();
        }
        self.layer.semantic.invalidate_symbols(&affected_syms);
        // `SortAnnotations` depends on domain/range edges -- easier to
        // rebuild wholesale than track per-sentence.  Only flush when
        // the diff actually mutated the KB.
        if !diff.removed.is_empty() || !diff.added.is_empty() {
            self.layer.invalidate_sort_annotations();
        }

        self.debug(format!("apply_file_diff file='{}': {} retained, {} removed, {} added, {} affected syms", diff.file, diff.retained.len(), diff.removed.len(), diff.added.len(), affected_syms.len()));

        result
    }
}

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

/// Incremental-reload input for a single file.
///
/// Describes the delta between the KB's current view of `file` and
/// the new source text: which existing sentences survive (with their
/// updated spans), which were removed, and which fresh AST nodes need
/// to be built into new root sentences.
///
/// Produced by [`compute_file_diff`] (or directly by any consumer that
/// already tracks per-file fingerprints).  Consumed by
/// [`KnowledgeBase::apply_file_diff`].
///
/// Entirely general-purpose: used by LSP didChange handling, file
/// watcher CLIs, and hot-reload test harnesses with no type
/// differences.
#[derive(Debug, Clone, Default)]
pub struct FileDiff {
    /// The `Sentence.file` tag this diff applies to.
    pub file:     String,
    /// Sentence ids whose body is unchanged; only the span moves.
    pub retained: Vec<(SentenceId, Span)>,
    /// Sentence ids that no longer exist in the new source.
    pub removed:  Vec<SentenceId>,
    /// Fresh AST nodes to intern as new root sentences.  Positionally
    /// aligned with `added_hashes` / `added_spans` when produced by
    /// `compute_file_diff`; the `apply_file_diff` path doesn't require
    /// the auxiliary vectors.
    pub added:    Vec<AstNode>,
}

/// Compute a sentence-level diff for `file` given its new
/// per-root-sentence fingerprint list + AST nodes + spans.
///
/// Uses a positional-greedy match: walks `new_hashes` in source order
/// and, for each hash, pops a matching old sid off a per-hash bucket
/// if one exists.  Duplicate sentences (same hash) preserve their
/// ids in source-order pairing; the first new duplicate pairs with
/// the first old duplicate, second with second, etc.  Anything left
/// over on the old side becomes `removed`; anything left over on the
/// new side becomes `added`.
///
/// Callers that don't need AST preservation (e.g. consumers that
/// plan to rebuild from scratch anyway) can pass `new_ast = &[]`
/// and ignore the `added` field.
pub fn compute_file_diff(
    file:        &str,
    old_sids:    &[SentenceId],
    old_hashes:  &[u64],
    new_hashes:  &[u64],
    new_ast:     &[AstNode],
    new_spans:   &[Span],
) -> FileDiff {
    debug_assert_eq!(old_sids.len(),    old_hashes.len(),
                     "old_sids and old_hashes must be positionally aligned");
    debug_assert_eq!(new_hashes.len(),  new_spans.len(),
                     "new_hashes and new_spans must be positionally aligned");
    debug_assert!(new_ast.is_empty() || new_ast.len() == new_hashes.len(),
                  "new_ast, when provided, must be positionally aligned with new_hashes");

    // Bucket old sids by hash, preserving source order for duplicates.
    let mut buckets: HashMap<u64, std::collections::VecDeque<SentenceId>> = HashMap::new();
    for (sid, &h) in old_sids.iter().zip(old_hashes) {
        buckets.entry(h).or_default().push_back(*sid);
    }

    let mut retained = Vec::with_capacity(new_hashes.len().min(old_sids.len()));
    let mut added: Vec<AstNode> = Vec::new();

    for (i, &h) in new_hashes.iter().enumerate() {
        match buckets.get_mut(&h).and_then(|b| b.pop_front()) {
            Some(sid) => {
                retained.push((sid, new_spans[i].clone()));
            }
            None => {
                if !new_ast.is_empty() {
                    added.push(new_ast[i].clone());
                }
            }
        }
    }

    // Anything still in the buckets is gone.
    let mut removed: Vec<SentenceId> = buckets.into_values().flatten().collect();
    removed.sort_unstable();  // deterministic for testing

    FileDiff { file: file.to_owned(), retained, removed, added }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
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
