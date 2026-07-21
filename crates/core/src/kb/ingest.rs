//! Formula ingestion API.

use thiserror::Error;
use std::collections::HashSet;

use std::path::PathBuf;
use std::sync::Arc;

use crate::{SentenceId, ToDiagnostic};
use crate::cache::events::Event;
use crate::cache::router::RouteOutcome;
use crate::types::SourceFile;
use crate::{Diagnostic};

use super::KnowledgeBase;

impl<L: crate::layer::TopLayer + crate::layer::Layer> KnowledgeBase<L> {
    /// Stage the ingestion of a [`SourceFile`] into a named session.
    ///
    /// The source's `parser` selects the format (KIF / TPTP / …) and its
    /// `origin` the semantics. Findings come back in
    /// [`IngestResult::diagnostics`].
    ///
    /// This only STAGES modifications; call [`KnowledgeBase::commit`] to commit
    /// them before later operations such as `make_session_axiomatic`.
    pub fn stage(&mut self, source: SourceFile, session: &str) -> IngestResult {
        with_guard!(self);
        let outcome = self.ingest_source(source, session, true);
        // Staged removals are part of the reported diff: the file no longer
        // contains them even though they stay live until `commit`.
        let recycled: Vec<u64> = outcome
            .emitted
            .iter()
            .filter_map(|e| match e {
                Event::FormulasRecycled { nodes } => Some(nodes.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        let mut result = IngestResult::from_outcome(outcome, session);
        for fp in recycled {
            for sid in self.layer.semantic().syntactic.roots_of_fingerprint(fp) {
                if !result.removed_sids.contains(&sid) {
                    result.removed_sids.push(sid);
                }
            }
        }
        result
    }

    /// Ingest a [`SourceFile`] into a named session and commit immediately.
    ///
    /// Like [`KnowledgeBase::stage`] but the change is committed at once.
    pub fn load(&mut self, source: SourceFile, session: &str) -> IngestResult {
        with_guard!(self);
        let outcome = self.ingest_source(source, session, false);
        IngestResult::from_outcome(outcome, session)
    }

    /// Assert an inline KIF string into a named session.
    ///
    /// Shorthand for `stage(SourceFile::inline_kif("", kif), session)`.
    /// Successive calls into one session accumulate; [`Self::flush_session`]
    /// discards them.
    pub fn tell(&mut self, kif: &str, session: &str) -> IngestResult {
        self.stage(SourceFile::inline_kif("", kif.to_string()), session)
    }

    /// Test-only shorthand staging `SourceFile::kif(file, text)` into `session`.
    #[cfg(test)]
    pub(crate) fn reload_kif(&mut self, text: &str, file: &PathBuf, session: &str) -> IngestResult {
        self.stage(SourceFile::kif(file.clone(), text.to_string()), session)
    }

    /// Test-only batch over [`Self::reload_kif`].
    #[cfg(test)]
    pub(crate) fn reload_kifs<'a, I>(&mut self, files: I, session: &str) -> Vec<IngestResult>
    where I: IntoIterator<Item = (&'a str, &'a str)> {
        files.into_iter()
            .map(|(path, text)| self.stage(SourceFile::kif(PathBuf::from(path), text.to_string()), session))
            .collect()
    }

    // -- Source control operations -------------------------------------------

    /// Commit a staged file update's deferred removals.
    ///
    /// Each recycled axiom is demoted (this source's promotion dropped;
    /// un-promoted from Base if it was the sole promoter) and removed when no
    /// reference remains (cascading `RootRemoved`). Clears the session's
    /// tombstones. `path` doubles as the review session. No-op if nothing
    /// pending.
    pub fn commit(&mut self, path: &str) {
        with_guard!(self);
        let recycled = self.layer.semantic().syntactic.recycled_fingerprints_of(path);
        let mut events = Vec::new();
        for fp in recycled {
            let sids = self.layer.semantic().syntactic.roots_of_fingerprint(fp);
            for sid in sids {
                self.layer.semantic().syntactic.sessions.demote(path, sid);
            }
            if self.layer.semantic().syntactic.drop_source_ref(path, fp) {
                self.layer.semantic().syntactic.source.entries().evict_keys(&[fp]);
                events.push(Event::FormulaRemoved { node: fp });
            }
        }
        self.layer.semantic().syntactic.clear_source_recycle(path);
        let _ = self.layer.semantic().syntactic.sessions
            .take_tombstones(crate::syntactic::caches::session::session_id(path));
        if !events.is_empty() {
            let _ = self.layer.cascade(events);
        }
    }

    /// Veto a staged file update's deferred removals.
    ///
    /// The recycled axioms are kept (the update is not allowed to delete them),
    /// and the session's tombstones are cleared so its scoped view shows them
    /// again. Additions made by the staged reload remain; this governs only the
    /// removals.
    pub fn rollback(&mut self, path: &str) {
        with_guard!(self);
        self.layer.semantic().syntactic.clear_source_recycle(path);
        let _ = self.layer.semantic().syntactic.sessions
            .take_tombstones(crate::syntactic::caches::session::session_id(path));
    }

    /// The axiom sentence ids a staged update of `path` is holding in the
    /// recycle bin (would-be removals awaiting accept/reject); empty if none.
    pub fn pending_axiom_removals(&self, path: &str) -> Vec<SentenceId> {
        let syn = &self.layer.semantic().syntactic;
        syn.recycled_fingerprints_of(path)
            .into_iter()
            .flat_map(|fp| syn.roots_of_fingerprint(fp))
            .collect()
    }

    /// Drive one ingest cascade for `source` under `session` from the top layer.
    ///
    /// Returns the raw [`RouteOutcome`]; callers harvest added / removed roots
    /// with [`roots_from_outcome`] and surface `outcome.errors` as diagnostics.
    /// The source cache diffs the file against its previous contents, so a
    /// re-ingest of an existing source is the reconcile (emitting
    /// `FormulaAdded` / `FormulaRemoved` for the delta). `staged` defers
    /// removals.
    pub(crate) fn ingest_source(&self, mut source: SourceFile, session: &str, staged: bool) -> RouteOutcome {
        profile_span!(self, "ingest.source_cascade");
        if matches!(source.origin, crate::types::FileOrigin::Inline) {
            if source.name.is_empty() {
                source.name = self.layer.semantic().syntactic.next_inline_source_key();
            }
        }
        self.layer.cascade(vec![Event::SourceAdded {
            session: Arc::new(session.to_owned()),
            file:    source,
            staged,
        }])
    }

    // -- Session management ---------------------------------------------------

    /// Collect all SentenceIds that are currently promoted axioms.
    ///
    /// "Promoted" means committed KB content — not an open-session assertion and
    /// not an in-flight ephemeral sentence (e.g. a conjecture parsed under a
    /// `__query__` / `__sine_query__` tag).
    pub(super) fn axiom_ids_set(&self) -> HashSet<SentenceId> {
        self.layer.semantic().syntactic.axiom_ids_set()
    }

    /// Discard a session's transient assertions.
    ///
    /// A sentence is removed only if it is not already an axiom and not still
    /// referenced by another session. The session key is then dropped from the
    /// caches. A source that contributed a promoted axiom is left intact so
    /// committed knowledge survives the flush.
    pub fn flush_session(&mut self, session: &str) {
        with_guard!(self);
        let sids = self.session_sids(session);
        self.sessions.remove(session);
        if sids.is_empty() {
            self.layer.semantic().syntactic.forget_source_session(session);
            return;
        }

        // Reconcile the session's sources to empty, except one that produced a
        // promoted axiom.
        let sources = self.layer.semantic().syntactic.sources_of_session(session);
        for src in sources {
            if self.layer.semantic().syntactic.source_produces_axiom(&src) { continue; }
            let _ = self.ingest_source(SourceFile {
                parser:   crate::Parser::Kif,
                name:     src,
                path:     PathBuf::new(),
                origin:   crate::types::FileOrigin::Inline,
                contents: String::new(),
                prebuilt: None,
            }, session, false);
        }
        self.layer.semantic().syntactic.forget_source_session(session);

        // Fingerprint cleanup for the transient (non-axiom) sids only; a
        // promoted sid remains an axiom and must survive.
        {
            let removable: std::collections::HashSet<SentenceId> = sids.iter().copied()
                .filter(|sid| !self.layer.semantic().syntactic.sessions.is_axiom(*sid))
                .collect();
            self.syntax_fingerprints.retain(|_, sid| !removable.contains(sid));
        }

        // Cascade from the top layer so `SessionRetracted` reaches the semantic
        // `tax_edges` (a syntactic-only cascade stops at the session cache).
        let _ = self.layer.cascade(vec![Event::SessionRetracted {
            session: session.to_string(),
        }]);

        self.info(format!("flush_session: flushed session '{}'", session));
    }

    /// Return the SentenceIds for a session (empty if it doesn't exist).
    pub fn session_sids(&self, session: &str) -> Vec<SentenceId> {
        self.layer.semantic().syntactic.sessions.session_sentences(session)
    }

    /// Drop every root sentence tagged with `file` from the in-memory KB by
    /// re-ingesting the file as empty: the source cache diffs the now-empty
    /// contents against its prior formulas and retracts them through the
    /// cascade (refcounting keeps any still referenced by another file/session).
    /// A persistent store (if attached) is untouched; call
    /// `KnowledgeBase::persist` to flush.
    pub fn remove_file(&mut self, file: &str) {
        let outcome = self.load(SourceFile::truncate(PathBuf::from(file)), file);
        let removed = outcome.removed_sids;
        let removed_set: HashSet<SentenceId> = removed.into_iter().collect();

        // Prune the session mirror of any sentences that were removed.
        for sids in self.sessions.values_mut() {
            sids.retain(|s| !removed_set.contains(s));
        }
    }

    /// Promote `session`'s assertions to axioms.
    pub fn make_session_axiomatic(
        &mut self,
        session: &str,
    ) -> Result<PromoteReport, PromoteError> {
        with_guard!(self);

        self.info(format!("promote: session='{}'", session));

        // Inline (`tell`) assertions are transient super-hypotheses that can
        // never be lifted; a session holding any is rejected wholesale.
        if self.layer.semantic().syntactic.session_has_inline_assertions(session) {
            return Err(PromoteError::ContainsInline { session: session.to_owned() });
        }

        let sids: Vec<SentenceId>  = self.session_sids(session);

        let mut report = PromoteReport::default();

        if sids.is_empty() {
            self.info(format!("promote: session '{}' empty", session));
            return Ok(report);
        }

        // Cascade from the top layer so `SessionAxiomatized` reaches both the
        // syntactic (`sine` / `axiom_index`) and semantic (`tax_edges`)
        // reactors; a syntactic-only cascade misses `tax_edges`.
        {
            profile_span!(self, "promote: update trigger indices (SInE + taxonomy)");
            let _ = self.layer.cascade(vec![Event::SessionAxiomatized {
                session: session.to_string(),
            }]);
        }

        self.info(format!("promote: {} sentence(s) from session '{}' promoted to axioms", sids.len(), session));
        report.promoted = sids.to_vec();

        self.sessions.remove(session);

        Ok(report)
    }
}

/// Split a [`RouteOutcome`]'s emitted events into `(added, removed)` root ids.
pub(super) fn roots_from_outcome(outcome: &RouteOutcome) -> (Vec<SentenceId>, Vec<SentenceId>) {
    let mut added   = Vec::new();
    let mut removed = Vec::new();
    for e in &outcome.emitted {
        match e {
            Event::RootAdded   { sid }     => added.push(*sid),
            Event::RootRemoved { sid, .. } => removed.push(*sid),
            _ => {}
        }
    }
    (added, removed)
}

impl IngestResult {
    fn from_outcome(outcome: RouteOutcome, session: &str) -> Self {
        let (added, removed) = roots_from_outcome(&outcome);

        // `ok` gates only on a parse failure (`kind == "parse"`), not on
        // semantic findings — those are advisory and may be escalated to
        // `Error` by `-Wall` without meaning the file failed to load.
        let ok = !outcome.errors.iter().any(|d| d.kind == "parse");

        // Retained: formulas a file reconcile carried over unchanged, batched as
        // one `FormulasUnchanged` per source (a fresh load has none).
        let retained = outcome.emitted.iter()
            .filter_map(|e| match e {
                Event::FormulasUnchanged { nodes } => Some(nodes.len()),
                _ => None,
            })
            .sum();

        Self {
            session: session.to_string(),
            ok,
            diagnostics: outcome.errors,
            sids: added,
            retained,
            removed_sids: removed,
        }
    }
}

// -- Ingest result types -------------------------------------------------------

/// Result of an ingest call ([`KnowledgeBase::tell`], [`KnowledgeBase::load`],
/// [`KnowledgeBase::stage`]).
///
/// Findings are a single severity-tagged [`Diagnostic`] list (`diagnostics`);
/// use [`IngestResult::errors`] / [`IngestResult::warnings`] to split by
/// severity. Duplicate formulas are de-duplicated silently by content
/// addressing, so they produce no diagnostic.
#[derive(Debug)]
pub struct IngestResult {
    /// The name of the session/file being ingested to.
    pub session:      String,
    /// True if the source parsed and ingested — i.e. no parse-level error.
    /// Semantic findings are advisory and do NOT clear this flag.
    pub ok:           bool,
    /// All diagnostics raised by this call — parse errors and semantic
    /// findings alike, each carrying its own [`Severity`].
    pub diagnostics:  Vec<Diagnostic>,
    /// Sentence ids newly added to the KB by this call.
    pub sids:         Vec<SentenceId>,
    /// Number of sentences carried over unchanged from the previous
    /// version of the file.
    pub retained:     usize,
    /// Sentence ids removed from the KB by this call, including a staged
    /// reconcile's deferred removals (tombstoned live until `commit`, but no
    /// longer part of the file's content, so they count in the diff).
    pub removed_sids: Vec<SentenceId>,
}

impl IngestResult {
    /// Number of sentences added.
    #[inline] pub fn added(&self) -> usize { self.sids.len() }
    /// Number of sentences removed.
    #[inline] pub fn removed(&self) -> usize { self.removed_sids.len() }
    /// Diagnostics of error severity.
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter().filter(|d| d.severity == crate::Severity::Error)
    }
    /// Diagnostics of warning severity.
    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter().filter(|d| d.severity == crate::Severity::Warning)
    }
    /// True if any diagnostic is an error.
    pub fn has_errors(&self) -> bool { self.errors().next().is_some() }
    /// True when nothing changed — no adds, removes, or diagnostics.
    #[inline] pub fn is_noop(&self) -> bool {
        self.sids.is_empty() && self.removed_sids.is_empty() && self.diagnostics.is_empty()
    }
}

impl Default for IngestResult {
    fn default() -> Self {
        Self {
            session:      String::new(),
            ok:           true,
            diagnostics:  Vec::new(),
            sids:         Vec::new(),
            retained:     0,
            removed_sids: Vec::new(),
        }
    }
}

// -- Promotion result types ----------------------------------------------------

/// Successful result from a promotion: the SentenceIds lifted to axioms.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct PromoteReport {
    /// SentenceIds successfully promoted to axioms.
    pub promoted: Vec<SentenceId>,
}

/// Error returned by the consistency-checked promotion path.
#[derive(Debug, Error)]
pub enum PromoteError {
    /// The prover showed the session assertions make the KB inconsistent.
    #[error("promotion rejected: session '{session}' makes the KB inconsistent")]
    Inconsistent {
        session: String,
        /// Raw prover output explaining the inconsistency.
        explanation: String,
        /// Assertion SentenceIds implicated (best-effort extraction).
        conflicting: Vec<SentenceId>,
    },

    /// The prover could not determine consistency (timeout or unknown result).
    /// Promotion is conservatively rejected.
    #[error("promotion rejected: prover could not determine consistency ({reason})")]
    ProverUncertain { reason: String },

    /// The session holds inline (`tell`) assertions, which are transient
    /// "super-hypotheses" and can never be lifted to axioms.  To commit them,
    /// re-ingest the content as a source file.
    #[error("promotion rejected: session '{session}' contains inline (tell) assertions, which cannot be lifted; re-ingest them as a source file")]
    ContainsInline { session: String },
}

impl ToDiagnostic for PromoteError {
    fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic { 
            kind: "promotion",
            range: crate::Span::synthetic(),
            severity: crate::Severity::Error,
            code: match &self {
                PromoteError::Inconsistent { .. } => "inconsistent",
                PromoteError::ProverUncertain { .. } => "uncertain",
                PromoteError::ContainsInline { .. } => "inline",
            },
            message: self.to_string(),
            related: vec![],
            sids: match self {
                PromoteError::Inconsistent { conflicting, .. } => conflicting.clone(),
                _ => vec![]
            },
            highlight_arg: i32::MAX,
            highlight_var: None
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::KnowledgeBase;
    use crate::SourceFile;
    use super::PromoteError;
    use std::{collections::HashSet, path::PathBuf};

    const TEST_PATH: PathBuf = PathBuf::new();

    // Load `text` as a real FILE source (path == `file`), then promote it.  Files
    // are the reconcile/liftable path — `tell` is inline scratch — so a base-KB
    // fixture must come in as a file, not a tell.
    fn load_file(kb: &mut KnowledgeBase, file: &str, text: &str) {
        let r = kb.reload_kif(text, &PathBuf::from(file), file);
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        let r = kb.make_session_axiomatic(
            file,
        );
        assert!(matches!(r, Ok(_)), "promotion failed: {:?}", r.err());
    }

    // ── tell ───────────────────────────────────────────────────────────────
    #[test]
    fn infer_class_end_to_end_through_kb() {
        use std::path::PathBuf;
        use crate::semantics::types::{ClassInference, Scope};
        let mut kb = KnowledgeBase::new();
        let f = PathBuf::from("t.kif");

        // Ingest, then promote as an explicit bulk step (reload_kif does NOT
        // auto-promote).  This is what populates the axiom index that the
        // pattern-based inference depends on.
        kb.reload_kif(
            "(subclass Human Entity)(subclass Dog Entity)(instance A Human)(instance B Dog)(equal A B)",
            &f, "t.kif");
        kb.make_session_axiomatic("t.kif").expect("promote");

        let b     = kb.symbol_id("B").unwrap();
        let human = kb.symbol_id("Human").unwrap();
        let dog   = kb.symbol_id("Dog").unwrap();

        // Equality `(equal A B)` folds A's Human into B (which is itself a Dog).
        match kb.layer.semantic.infer_class_scoped(b, Scope::Base) {
            ClassInference::Multiple(v) =>
                assert!(v.contains(&human) && v.contains(&dog), "B should be {{Dog, Human}}, got {v:?}"),
            other => panic!("expected Multiple([Dog, Human]), got {other:?}"),
        }

        // Remove the equality, re-promote: B drops Human, back to Single(Dog).
        kb.reload_kif(
            "(subclass Human Entity)(subclass Dog Entity)(instance A Human)(instance B Dog)",
            &f, "t.kif");
        kb.commit("t.kif");
        kb.make_session_axiomatic("t.kif").ok();
        assert!(matches!(kb.layer.semantic.infer_class_scoped(b, Scope::Base), ClassInference::Single(d) if d == dog),
            "after removing the equality, B should be Single(Dog), got {:?}",
            kb.layer.semantic.infer_class_scoped(b, Scope::Base));
    }







    #[test]
    fn transient_taxonomy_does_not_leak_across_sessions() {
        // Session A asserts `(subclass Human Object)` transiently (never
        // promoted).  That edge
        // must be visible ONLY through session A's scope — never in `Base` and
        // never in a concurrent session B.
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;

        let mut kb = KnowledgeBase::new();
        kb.tell("(subclass Human Object)", "session_a");

        let human  = kb.symbol_id("Human").unwrap();
        let object = kb.symbol_id("Object").unwrap();
        let sa = Scope::Session(session_id("session_a"));
        let sb = Scope::Session(session_id("session_b"));

        let sem = &kb.layer.semantic;
        // Visible in the asserting session's overlay …
        assert!(sem.has_ancestor_scoped(human, object, sa),
            "session A asserted (subclass Human Object) — must see it in its own scope");
        // … but NOT in Base (it was never promoted) …
        assert!(!sem.has_ancestor(human, object),
            "transient edge must not appear in the Base taxonomy");
        // … and NOT in a concurrent session that never asserted it.
        assert!(!sem.has_ancestor_scoped(human, object, sb),
            "session B never asserted this edge — must not leak across sessions");
    }

    #[test]
    fn promotion_graduates_transient_edge_into_base() {
        // After promotion the same edge becomes a `Base` axiom, visible in every
        // scope (the graduation flips the overlay edge to Base).
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;

        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "session_a", "(subclass Human Object)");

        let human  = kb.symbol_id("Human").unwrap();
        let object = kb.symbol_id("Object").unwrap();

        let sem = &kb.layer.semantic;
        assert!(sem.has_ancestor(human, object),
            "after promotion the edge is a Base axiom");
        assert!(sem.has_ancestor_scoped(human, object, Scope::Session(session_id("session_b"))),
            "a Base axiom is visible from every session scope");
    }

    #[test]
    fn tell_into_one_session_keeps_other_sessions_intact() {
        // A promoted base axiom must survive a later, unrelated session tell
        // (symbol `Relation` stays interned).
        use crate::types::Symbol;
        let rel_id = Symbol::hash_name("Relation");

        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "base", "(subclass BinaryRelation Relation)");
        assert!(kb.symbol_id("Relation").is_some(), "Relation interned after promote");

        kb.tell("(instance likes BinaryRelation)(likes Foo Bar)", "session_a");
        assert!(kb.symbol_id("Relation").is_some(),
            "a tell into session_a must not retract the promoted base axiom (Relation must stay interned)");
        let syn = &kb.layer.semantic.syntactic;
        assert!(syn.sentences.referenced_symbols().contains(&rel_id),
            "the base (subclass BinaryRelation Relation) root must still be live");
    }

    #[test]
    fn flushing_a_session_drops_its_overlay_edges() {
        // Session-flush wiring: a session's transient taxonomy edge is gone after
        // the session is flushed — the source-empty emits RelationRemoved, which
        // removes the tax_edges overlay entry and unbumps the per-session overlay
        // refcount, so the session is no longer overlay-active.
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;

        let mut kb = KnowledgeBase::new();
        kb.tell("(subclass Foo Bar)", "sess_a");

        let foo = kb.symbol_id("Foo").unwrap();
        let bar = kb.symbol_id("Bar").unwrap();
        let a = Scope::Session(session_id("sess_a"));

        assert!(kb.layer.semantic.has_ancestor_scoped(foo, bar, a));
        assert!(kb.layer.semantic.tax_session_active(a),
            "sess_a is overlay-active before flush");

        kb.flush_session("sess_a");

        assert!(!kb.layer.semantic.tax_session_active(a),
            "sess_a's overlay refcount is cleared after flush");
        assert!(!kb.layer.semantic.has_ancestor_scoped(foo, bar, a),
            "the flushed session's transient edge is gone (falls through to Base)");
    }

    #[test]
    fn re_asserting_in_a_second_session_propagates_scope_and_shares() {
        // A second session re-asserting an existing transient edge dedups to the
        // same root (no RootAdded), but `SessionReferenced` now propagates the
        // scope to tax_edges — so the second session sees the edge it asserted,
        // and the edge is genuinely shared.  Flushing one session then strips
        // only its scope (the SessionRetracted slow path), leaving the other.
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;

        let mut kb = KnowledgeBase::new();
        kb.tell("(subclass Foo Bar)", "sess_a");
        kb.tell("(subclass Foo Bar)", "sess_b"); // dedup re-assert

        let foo = kb.symbol_id("Foo").unwrap();
        let bar = kb.symbol_id("Bar").unwrap();
        let a = Scope::Session(session_id("sess_a"));
        let b = Scope::Session(session_id("sess_b"));

        assert!(kb.layer.semantic.has_ancestor_scoped(foo, bar, a));
        assert!(kb.layer.semantic.has_ancestor_scoped(foo, bar, b),
            "the second session sees the edge it re-asserted (scope propagated)");
        assert!(kb.layer.semantic.tax_session_active(a));
        assert!(kb.layer.semantic.tax_session_active(b));

        kb.flush_session("sess_a");

        assert!(!kb.layer.semantic.tax_session_active(a),
            "sess_a's scope is dropped after flush");
        assert!(kb.layer.semantic.has_ancestor_scoped(foo, bar, b),
            "sess_b still owns the shared edge after sess_a is flushed");
        assert!(!kb.layer.semantic.has_ancestor_scoped(foo, bar, a),
            "flushed session falls through to Base (edge never promoted)");
    }

    #[test]
    fn tells_append_within_a_session_and_flush_removes_them() {
        // Each tell is its own inline source, so successive tells ACCUMULATE
        // (append, not reconcile); flushing the session removes them all.
        let mut kb = KnowledgeBase::new();
        kb.tell("(married brian amy)", "abc");
        kb.tell("(mother amy steve)", "abc");
        assert!(!kb.layer.semantic.syntactic.by_head("married").is_empty(),
            "first tell survives the second (append, not reconcile)");
        assert!(!kb.layer.semantic.syntactic.by_head("mother").is_empty());
        assert_eq!(kb.session_sids("abc").len(), 2, "both tells accumulate");

        kb.flush_session("abc");
        assert!(kb.layer.semantic.syntactic.by_head("married").is_empty());
        assert!(kb.layer.semantic.syntactic.by_head("mother").is_empty());
    }

    #[test]
    fn promotion_drains_session_membership_and_records_provenance() {
        // After promotion the content graduates to a Base axiom owned by NO
        // session: it leaves the session's membership, retracting the session
        // does nothing to it, and its origin is kept only as provenance.
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "base.kif", "(subclass Dog Animal)");

        assert!(kb.session_sids("base.kif").is_empty(),
            "promoted content is drained out of the session membership");

        let dog = kb.symbol_id("Dog").unwrap();
        let animal = kb.symbol_id("Animal").unwrap();
        assert!(kb.layer.semantic.has_ancestor(dog, animal), "promoted to the Base taxonomy");

        let sid = *kb.layer.semantic.syntactic.by_head("subclass").iter().next().unwrap();
        assert!(kb.layer.semantic.syntactic.sessions.is_axiom(sid), "is an axiom");
        assert_eq!(kb.layer.semantic.syntactic.sessions.provenance_of(sid),
            vec!["base.kif".to_string()], "provenance points back to the origin file");

        // Retracting the (now-drained) session is a no-op for the axiom.
        kb.flush_session("base.kif");
        assert!(kb.layer.semantic.has_ancestor(dog, animal),
            "the promoted axiom survives session retraction");
    }

    #[test]
    fn staged_file_update_defers_axiom_removal_until_accept() {
        // A staged file update that would delete a promoted axiom DEFERS the
        // removal (recycle bin) — the axiom stays live until accept commits it.
        let path = PathBuf::from("model.kif");
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "model.kif", "(subclass Dog Animal)");
        let dog = kb.symbol_id("Dog").unwrap();
        let animal = kb.symbol_id("Animal").unwrap();
        assert!(kb.layer.semantic.has_ancestor(dog, animal));

        // Staged: drop Dog⊂Animal, add Cat⊂Animal.
        kb.reload_kif("(subclass Cat Animal)", &path, "model.kif");
        assert!(kb.layer.semantic.has_ancestor(dog, animal),
            "removal of the promoted axiom is deferred, not applied");
        assert!(!kb.pending_axiom_removals("model.kif").is_empty(),
            "the dropped axiom waits in the recycle bin");
        assert!(kb.symbol_id("Cat").is_some(), "the addition applied immediately");

        kb.commit("model.kif");
        assert!(!kb.layer.semantic.has_ancestor(dog, animal),
            "after accept, the removal is committed");
        assert!(kb.pending_axiom_removals("model.kif").is_empty());
    }

    #[test]
    fn re_adding_a_recycled_axiom_clears_it_from_the_bin() {
        // Stage a deletion of a promoted axiom (→ recycle bin), then reload with
        // the axiom back: the retained formula must leave the recycle bin, so a
        // later accept does NOT delete a formula the latest version keeps.
        let path = PathBuf::from("model.kif");
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "model.kif", "(subclass Dog Animal)");
        let dog = kb.symbol_id("Dog").unwrap();
        let animal = kb.symbol_id("Animal").unwrap();

        // 1. Staged update drops Dog⊂Animal → deferred into the recycle bin.
        kb.reload_kif("(subclass Cat Animal)", &path, "model.kif");
        assert!(!kb.pending_axiom_removals("model.kif").is_empty(),
            "the dropped axiom is pending");

        // 2. The file is edited again to put Dog⊂Animal back.
        kb.reload_kif("(subclass Dog Animal)", &path, "model.kif");
        assert!(kb.pending_axiom_removals("model.kif").is_empty(),
            "re-adding the formula clears it from the recycle bin");

        // 3. Accept: must NOT delete Dog⊂Animal — the latest version keeps it.
        kb.commit("model.kif");
        assert!(kb.layer.semantic.has_ancestor(dog, animal),
            "a re-added axiom survives a later accept");
    }

    #[test]
    fn staged_removal_is_session_local() {
        // A staged file deletion of an axiom is a per-session tombstone: the
        // reviewing session sees it gone, Base and every other session see it.
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;
        let path = PathBuf::from("P");
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "P", "(subclass Dog Animal)");
        let dog = kb.symbol_id("Dog").unwrap();
        let animal = kb.symbol_id("Animal").unwrap();
        let p = Scope::Session(session_id("P"));
        let q = Scope::Session(session_id("Q"));

        kb.reload_kif("", &path, "P");   // staged delete of the file's only axiom

        assert!(!kb.layer.semantic.has_ancestor_scoped(dog, animal, p),
            "the reviewing session sees the axiom as gone");
        assert!(kb.layer.semantic.has_ancestor(dog, animal),
            "Base is unaffected during staging");
        assert!(kb.layer.semantic.has_ancestor_scoped(dog, animal, q),
            "another session sees it normally");
    }

    #[test]
    fn rejecting_a_staged_update_restores_the_session_view() {
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;
        let path = PathBuf::from("P");
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "P", "(subclass Dog Animal)");
        let dog = kb.symbol_id("Dog").unwrap();
        let animal = kb.symbol_id("Animal").unwrap();
        let p = Scope::Session(session_id("P"));

        kb.reload_kif("", &path, "P");
        assert!(!kb.layer.semantic.has_ancestor_scoped(dog, animal, p));
        kb.rollback("P");
        assert!(kb.layer.semantic.has_ancestor_scoped(dog, animal, p),
            "reject restores the axiom in the session's view");
        assert!(kb.layer.semantic.has_ancestor(dog, animal));
    }

    #[test]
    fn accepting_a_staged_removal_commits_globally() {
        let path = PathBuf::from("P");
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "P", "(subclass Dog Animal)");
        let dog = kb.symbol_id("Dog").unwrap();
        let animal = kb.symbol_id("Animal").unwrap();

        kb.reload_kif("", &path, "P");
        kb.commit("P");
        assert!(!kb.layer.semantic.has_ancestor(dog, animal),
            "accept removes the axiom from Base");
        assert!(kb.pending_axiom_removals("P").is_empty());
    }

    #[test]
    fn tombstone_is_latent_when_another_session_promoted_it() {
        // The same axiom promoted by two files; a staged removal from one does NOT
        // hide it (committing wouldn't remove it — the other file keeps it).
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "P", "(subclass Dog Animal)");
        load_file(&mut kb, "Q", "(subclass Dog Animal)");   // same content, 2nd promoter
        let dog = kb.symbol_id("Dog").unwrap();
        let animal = kb.symbol_id("Animal").unwrap();
        let p = Scope::Session(session_id("P"));

        kb.reload_kif("", &PathBuf::from("P"), "P");
        assert!(kb.layer.semantic.has_ancestor_scoped(dog, animal, p),
            "removal is latent (Q still promotes it) → not hidden even in P's view");
    }

    #[test]
    fn rejecting_a_staged_update_keeps_the_axiom() {
        let path = PathBuf::from("model.kif");
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "model.kif", "(subclass Dog Animal)");
        let dog = kb.symbol_id("Dog").unwrap();
        let animal = kb.symbol_id("Animal").unwrap();

        kb.reload_kif("(subclass Cat Animal)", &path, "model.kif");
        kb.rollback("model.kif");
        assert!(kb.layer.semantic.has_ancestor(dog, animal),
            "after reject, the axiom the update would have deleted is kept");
        assert!(kb.pending_axiom_removals("model.kif").is_empty());
    }

    #[test]
    fn make_session_axiomatic_rejects_inline_assertions() {
        // Tells are transient "super-hypotheses" — they can never be lifted.
        let mut kb = KnowledgeBase::new();
        kb.tell("(married brian amy)", "abc");
        let r = kb.make_session_axiomatic("abc");
        assert!(matches!(r, Err(PromoteError::ContainsInline { .. })),
            "a session with inline (tell) assertions must not promote; got {r:?}");
        // The rejection touches nothing — the assertion is still there, unpromoted.
        assert!(!kb.layer.semantic.syntactic.by_head("married").is_empty());
    }

    #[test]
    fn flush_keeps_a_promoted_file_axiom_a_tell_restated() {
        // A file axiom that a session re-states (dedup → shared) survives flush;
        // only the session's own transient tell is dropped.
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "base.kif", "(married brian amy)");   // file → promoted axiom
        kb.tell("(married brian amy)", "abc");            // re-states the axiom
        kb.tell("(sister susan steve)", "abc");           // transient

        kb.flush_session("abc");
        assert!(!kb.layer.semantic.syntactic.by_head("married").is_empty(),
            "the promoted file axiom survives flush");
        assert!(kb.layer.semantic.syntactic.by_head("sister").is_empty(),
            "the transient tell is flushed");
    }

    #[test]
    fn re_asserting_a_base_axiom_does_not_activate_the_session() {
        // Re-asserting a now-global axiom in a session must NOT add a redundant
        // overlay: the session sees it through Base, and stays overlay-inactive
        // so its transitive queries keep falling through to Base.
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;

        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "base", "(subclass Foo Bar)");

        let foo = kb.symbol_id("Foo").unwrap();
        let bar = kb.symbol_id("Bar").unwrap();
        let b = Scope::Session(session_id("sess_b"));

        kb.tell("(subclass Foo Bar)", "sess_b"); // redundant re-assert of a Base axiom

        assert!(kb.layer.semantic.has_ancestor_scoped(foo, bar, b),
            "the session sees the global axiom through Base");
        assert!(!kb.layer.semantic.tax_session_active(b),
            "re-asserting a Base axiom must not mark the session overlay-active");
    }

    #[test]
    fn fall_through_to_base_avoids_redundant_session_entries() {
        // A session-scoped query for a symbol the session does not touch is
        // keyed under Base, so no redundant per-session cache entry is created
        // and the shared Base memo is reused.
        use crate::semantics::types::{Scope, Scoped};
        use crate::syntactic::caches::session::session_id;

        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "base", "(subclass Dog Animal)(subclass Animal Entity)");
        // session_a declares its OWN taxonomy (Cat) — overlay-active — but never
        // touches Dog.
        kb.tell("(subclass Cat Animal)", "session_a");

        let dog    = kb.symbol_id("Dog").unwrap();
        let entity = kb.symbol_id("Entity").unwrap();
        let sa = Scope::Session(session_id("session_a"));
        let sb = Scope::Session(session_id("session_b")); // never asserted → inactive
        let sem = &kb.layer.semantic;

        // Direct cache, per-symbol fall-through: Dog has no overlay in the
        // (otherwise active) session_a → is_class(Dog) keys under Base.
        assert!(sem.is_class_scoped(dog, sa));
        assert!(sem.is_class.peek(&Scoped { scope: Scope::Base, key: dog }).is_some(),
            "is_class(Dog) memoised under Base");
        assert!(sem.is_class.peek(&Scoped { scope: sa, key: dog }).is_none(),
            "no redundant session-keyed is_class(Dog) entry");

        // Transitive cache, per-session fall-through: session_b declares no
        // taxonomy at all → has_ancestor keys under Base.
        assert!(sem.has_ancestor_scoped(dog, entity, sb));
        assert!(sem.has_ancestor.peek(&Scoped { scope: Scope::Base, key: (dog, entity) }).is_some(),
            "has_ancestor(Dog, Entity) memoised under Base");
        assert!(sem.has_ancestor.peek(&Scoped { scope: sb, key: (dog, entity) }).is_none(),
            "no redundant session-keyed has_ancestor entry for an inactive session");

        // Sanity: an active session DOES get its own entry for a symbol it touches.
        let cat = kb.symbol_id("Cat").unwrap();
        assert!(sem.is_class_scoped(cat, sa));
        assert!(sem.is_class.peek(&Scoped { scope: sa, key: cat }).is_some(),
            "Cat carries a session_a overlay edge → keyed under the session");
    }

    #[test]
    fn session_scoped_validation_differs_from_global() {
        // Validating a session reasons in that session's scope, so a
        // relation declared only transiently in the session is recognised — while
        // global validation (the default) still flags its use as HeadNotRelation.
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;
        use crate::layer::Layer;

        let mut kb = KnowledgeBase::new();
        // A single session transiently declares the relation taxonomy, declares
        // `likes` a relation, and uses it.  Nothing is promoted, so this evidence
        // lives only in the session overlay.
        kb.tell(
            "(subclass BinaryRelation Relation)(instance likes BinaryRelation)(likes Foo Bar)",
            "session_a");

        let likes_sid = *kb.layer.semantic.syntactic.by_head("likes").iter().next()
            .expect("a (likes ...) root");
        let sa = Scope::Session(session_id("session_a"));

        let scoped = kb.layer.semantic.validator_scoped(sa).validate_sentence_collect(likes_sid);
        let global = kb.layer.semantic.validator_scoped(Scope::Base).validate_sentence_collect(likes_sid);

        // E002 = HeadNotRelation.
        assert!(!scoped.iter().any(|e| e.code() == "E002"),
            "session scope sees (instance likes BinaryRelation) → likes is a relation; got {:?}",
            scoped.iter().map(|e| e.code()).collect::<Vec<_>>());
        assert!(global.iter().any(|e| e.code() == "E002"),
            "Base never saw the declaration → HeadNotRelation; got {:?}",
            global.iter().map(|e| e.code()).collect::<Vec<_>>());

        // The ValidateSession event cascades through the reactive graph cleanly.
        let _ = kb.layer.cascade(vec![crate::cache::events::Event::ValidateSession {
            session: "session_a".to_string(),
        }]);
    }

    #[test]
    fn scoped_infer_class_sees_session_transient_evidence() {
        // infer_class reasons over a session's transient (un-promoted)
        // assertions when asked in that scope, and only in that scope.  This
        // exercises BOTH scoped paths: the overlay taxonomy edge (`instance A
        // Human` via parents_of_scoped) and the session-scan pattern match
        // (`equal A B` via scoped_contain_roots' session branch).
        use crate::semantics::types::{ClassInference, Scope};
        use crate::syntactic::caches::session::session_id;

        let mut kb = KnowledgeBase::new();
        kb.tell("(subclass Human Entity)(instance A Human)(equal A B)", "session_a");

        let b     = kb.symbol_id("B").unwrap();
        let human = kb.symbol_id("Human").unwrap();
        let sa = Scope::Session(session_id("session_a"));
        let sb = Scope::Session(session_id("session_b"));
        let sem = &kb.layer.semantic;

        // In session A, equality folds A's Human onto B.
        assert!(matches!(sem.infer_class_scoped(b, sa), ClassInference::Single(c) if c == human),
            "session A: B is equal to A which is a Human → Single(Human), got {:?}",
            sem.infer_class_scoped(b, sa));
        // Base never saw these (un-promoted) → Unknown.
        assert!(matches!(sem.infer_class_scoped(b, Scope::Base), ClassInference::Unknown),
            "Base has no evidence for B → Unknown, got {:?}", sem.infer_class_scoped(b, Scope::Base));
        // A concurrent session is isolated → Unknown.
        assert!(matches!(sem.infer_class_scoped(b, sb), ClassInference::Unknown),
            "session B never asserted this → Unknown, got {:?}", sem.infer_class_scoped(b, sb));
    }

    #[test]
    fn scoped_infer_class_domain_path_is_session_local() {
        // The argument-domain inference path through infer_class is session-scoped
        // end-to-end: BOTH the relation atom `(mother Mary Jesus)` AND the domain
        // rule `(domain mother 1 Mother)` are transient session_a evidence.  Mary
        // classifies as Mother ONLY in session_a — never in Base or session_b,
        // even when session_b independently asserts a *conflicting* domain.
        use crate::semantics::types::{ClassInference, Scope};
        use crate::syntactic::caches::session::session_id;

        let mut kb = KnowledgeBase::new();
        kb.tell("(domain mother 1 Mother)(mother Mary Jesus)", "session_a");
        // session_b asserts the SAME relation atom but a DIFFERENT arg-1 domain —
        // if any cross-session leak existed, Mary's class would collide here.
        kb.tell("(domain mother 1 Caregiver)(mother Mary Jesus)", "session_b");

        let mary      = kb.symbol_id("Mary").unwrap();
        let mother    = kb.symbol_id("Mother").unwrap();
        let caregiver = kb.symbol_id("Caregiver").unwrap();
        let sa = Scope::Session(session_id("session_a"));
        let sb = Scope::Session(session_id("session_b"));
        let sem = &kb.layer.semantic;

        // session_a: Mary at arg-1 of `mother` → mother's session_a domain (Mother).
        assert!(matches!(sem.infer_class_scoped(mary, sa), ClassInference::Single(c) if c == mother),
            "session A: Mary is arg-1 of mother (domain Mother) → Single(Mother), got {:?}",
            sem.infer_class_scoped(mary, sa));
        // session_b: same arg position, but its OWN domain (Caregiver) — no leak
        // of session_a's Mother classification.
        assert!(matches!(sem.infer_class_scoped(mary, sb), ClassInference::Single(c) if c == caregiver),
            "session B: Mary → its own domain (Caregiver), not session A's Mother, got {:?}",
            sem.infer_class_scoped(mary, sb));
        // Base saw neither (both transient) → Unknown.
        assert!(matches!(sem.infer_class_scoped(mary, Scope::Base), ClassInference::Unknown),
            "Base has no transient evidence for Mary → Unknown, got {:?}", sem.infer_class_scoped(mary, Scope::Base));
    }

    #[test]
    fn scoped_domain_is_session_local() {
        // The common domain/range change: a session introduces a NEW relation
        // with its own `domain` rule.  That rule is effective ONLY in the
        // declaring session — never in Base or a concurrent session.
        use crate::semantics::types::{RelationDomain, Scope};
        use crate::syntactic::caches::session::session_id;

        let mut kb = KnowledgeBase::new();
        kb.tell("(domain likes 1 Animal)", "session_a");

        let likes  = kb.symbol_id("likes").unwrap();
        let animal = kb.symbol_id("Animal").unwrap();
        let sa = Scope::Session(session_id("session_a"));
        let sb = Scope::Session(session_id("session_b"));
        let sem = &kb.layer.semantic;

        // Visible in the declaring session …
        let d = sem.domain_scoped(likes, sa);
        assert_eq!(d.len(), 1);
        assert!(matches!(&d[0], RelationDomain::Domain(c) if *c == animal),
            "session A declared (domain likes 1 Animal) — must see it, got {d:?}");
        // … never in Base (un-promoted) …
        assert!(sem.domain(likes).is_empty(),
            "transient session domain must not leak into Base");
        // … nor in a concurrent session.
        assert!(sem.domain_scoped(likes, sb).is_empty(),
            "transient session domain must not leak across sessions");
    }

    #[test]
    fn scoped_base_domain_overrules_session() {
        // A global (axiom) domain rule always overrules a session assertion at
        // the same position — the session cannot redefine a Base-claimed slot.
        use crate::semantics::types::{RelationDomain, Scope};
        use crate::syntactic::caches::session::session_id;

        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "base.kif", "(domain likes 1 Animal)"); // promoted axiom
        kb.tell("(domain likes 1 Plant)", "session_a");     // conflicting overlay

        let likes  = kb.symbol_id("likes").unwrap();
        let animal = kb.symbol_id("Animal").unwrap();
        let sa = Scope::Session(session_id("session_a"));
        let sem = &kb.layer.semantic;

        let d = sem.domain_scoped(likes, sa);
        assert_eq!(d.len(), 1);
        assert!(matches!(&d[0], RelationDomain::Domain(c) if *c == animal),
            "Base (Animal) overrules the session's conflicting (Plant), got {d:?}");
    }

    #[test]
    fn scoped_range_is_session_local_and_base_overrules() {
        // Range mirrors domain: a session's transient `range` is effective only
        // when Base declares none, and only within that session.
        use crate::types::RelationRange;
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;

        let mut kb = KnowledgeBase::new();
        kb.tell("(range FatherOfFn Human)", "session_a");

        let fof   = kb.symbol_id("FatherOfFn").unwrap();
        let human = kb.symbol_id("Human").unwrap();
        let sa = Scope::Session(session_id("session_a"));
        let sb = Scope::Session(session_id("session_b"));
        let sem = &kb.layer.semantic;

        assert!(matches!(sem.range_scoped(fof, sa), RelationRange::Range(c) if c == human),
            "session A declared (range FatherOfFn Human) — must see it");
        assert!(matches!(sem.range(fof), RelationRange::Unknown),
            "transient session range must not leak into Base");
        assert!(matches!(sem.range_scoped(fof, sb), RelationRange::Unknown),
            "transient session range must not leak across sessions");
    }

    #[test]
    fn scoped_documentation_is_session_local() {
        // documentation/format/termFormat now ride the same scope filter as
        // domain/range: the default accessor is Base (promoted/global) only, and
        // a session's transient docs are visible solely through its own scope.
        use crate::semantics::types::Scope;
        use crate::syntactic::caches::session::session_id;

        let mut kb = KnowledgeBase::new();
        kb.tell(r#"(documentation Gizmo EnglishLanguage "a session gadget")"#, "session_a");

        let gizmo = kb.symbol_id("Gizmo").unwrap();
        let sa = Scope::Session(session_id("session_a"));
        let sb = Scope::Session(session_id("session_b"));
        let sem = &kb.layer.semantic;

        // Default (Base) does NOT see a session's transient (un-promoted) doc.
        assert!(sem.documentation(gizmo, None).is_empty(),
            "Base must not see a session's transient documentation");
        // The declaring session sees it.
        let d = sem.documentation_scoped(gizmo, None, sa);
        assert_eq!(d.len(), 1, "session A declared the doc → must see it");
        assert_eq!(d[0].text, "a session gadget");
        // A concurrent session does not.
        assert!(sem.documentation_scoped(gizmo, None, sb).is_empty(),
            "documentation must not leak across sessions");
    }

    #[test]
    fn session_referenced_revives_a_cached_empty_domain() {
        // The correctness gap the SessionReferenced wiring closes: a dedup
        // re-assert fires `SessionReferenced` (NOT `RelationAdded`), so a
        // previously-cached "empty" domain verdict for the newly-referencing
        // session must be targeted-invalidated.
        use crate::semantics::types::{RelationDomain, Scope};
        use crate::syntactic::caches::session::session_id;

        let mut kb = KnowledgeBase::new();
        kb.tell("(domain mother 1 Mother)", "session_a"); // transient root

        let mother   = kb.symbol_id("mother").unwrap();
        let mother_c = kb.symbol_id("Mother").unwrap();
        let sb = Scope::Session(session_id("session_b"));

        // Query B FIRST → empty, MEMOISING the empty verdict for B's scope.
        assert!(kb.layer.semantic.domain_scoped(mother, sb).is_empty(),
            "B has not referenced the domain axiom yet → empty");

        // B re-asserts the identical axiom: dedup → SessionReferenced, which must
        // drop B's stale empty entry (targeted, only B's scope for `mother`).
        kb.tell("(domain mother 1 Mother)", "session_b");

        let d = kb.layer.semantic.domain_scoped(mother, sb);
        assert_eq!(d.len(), 1, "B now references the axiom → must see the domain");
        assert!(matches!(&d[0], RelationDomain::Domain(c) if *c == mother_c));
    }

    #[test]
    fn session_retracted_drops_only_that_sessions_range_entry() {
        // SessionRetracted is a TARGETED invalidation: retracting session A drops
        // only A's scoped entries.  A shared edge that survives (still referenced
        // by B) keeps B's memo intact — not a coarse store-wide clear.
        use crate::types::RelationRange;
        use crate::semantics::types::{Scope, Scoped};
        use crate::syntactic::caches::session::session_id;

        let mut kb = KnowledgeBase::new();
        // Both sessions reference the SAME transient range sentence (dedup-shared).
        kb.tell("(range FatherOfFn Human)", "session_a");
        kb.tell("(range FatherOfFn Human)", "session_b");

        let fof   = kb.symbol_id("FatherOfFn").unwrap();
        let human = kb.symbol_id("Human").unwrap();
        let sa = Scope::Session(session_id("session_a"));
        let sb = Scope::Session(session_id("session_b"));

        // Memoise BOTH sessions' verdicts.
        assert!(matches!(kb.layer.semantic.range_scoped(fof, sa), RelationRange::Range(c) if c == human));
        assert!(matches!(kb.layer.semantic.range_scoped(fof, sb), RelationRange::Range(c) if c == human));
        assert!(kb.layer.semantic.range.peek(&Scoped { scope: sa, key: fof }).is_some());
        assert!(kb.layer.semantic.range.peek(&Scoped { scope: sb, key: fof }).is_some());

        // Retract A.  B still references the sentence, so it survives — the only
        // signal the range cache sees is `SessionRetracted{A}` (no RelationRemoved).
        kb.flush_session("session_a");

        assert!(kb.layer.semantic.range.peek(&Scoped { scope: sa, key: fof }).is_none(),
            "session A's range entry is dropped on retraction");
        assert!(kb.layer.semantic.range.peek(&Scoped { scope: sb, key: fof }).is_some(),
            "session B's entry SURVIVES A's retraction — targeted, not coarse");
        assert!(matches!(kb.layer.semantic.range_scoped(fof, sb), RelationRange::Range(c) if c == human),
            "B still resolves the range after A is gone");
    }

    #[test]
    fn tell_returns_ok_and_sids_on_valid_kif() {
        let mut kb = KnowledgeBase::new();
        let r = kb.tell("(subclass Dog Mammal)\n(subclass Cat Mammal)", "s");
        assert!(r.ok);
        assert_eq!(r.sids.len(), 2);
        assert!(r.diagnostics.is_empty());
    }

    #[test]
    fn tell_parse_error_sets_ok_false_and_populates_errors() {
        let mut kb = KnowledgeBase::new();
        let r = kb.tell("(subclass Dog Mammal", "s");
        assert!(!r.ok);
        assert!(r.has_errors());
    }

    // Duplicate formulas are de-duplicated silently by content addressing —
    // re-asserting an existing axiom is a no-op with no diagnostic.
    #[test]
    fn tell_duplicate_is_silent_noop() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "f.kif", "(subclass Dog Mammal)");
        let r = kb.tell("(subclass Dog Mammal)", "s2");
        assert!(r.ok && !r.has_errors(), "duplicate is non-fatal");
    }

    #[test]
    fn tell_adds_to_named_session() {
        let mut kb = KnowledgeBase::new();
        let r = kb.tell("(subclass Dog Mammal)", "my_session");
        assert!(r.ok);
        let session_sids = kb.session_sids("my_session");
        assert_eq!(session_sids.len(), r.sids.len());
        for sid in &r.sids {
            assert!(session_sids.contains(sid));
        }
    }

    #[test]
    fn tell_multiple_sessions_are_independent() {
        let mut kb = KnowledgeBase::new();
        kb.tell("(subclass Dog Mammal)", "session_a");
        kb.tell("(subclass Cat Mammal)", "session_b");
        assert_eq!(kb.session_sids("session_a").len(), 1);
        assert_eq!(kb.session_sids("session_b").len(), 1);
    }

    // ── flush_session ───────────────────────────────────────────────────────

    #[test]
    fn flush_session_removes_all_assertions() {
        let mut kb = KnowledgeBase::new();
        kb.tell("(subclass Dog Mammal)", "s");
        assert_eq!(kb.session_sids("s").len(), 1);
        kb.flush_session("s");
        assert_eq!(kb.session_sids("s").len(), 0);
    }

    #[test]
    fn flush_session_on_unknown_session_is_noop() {
        let mut kb = KnowledgeBase::new();
        kb.flush_session("does_not_exist"); // must not panic
    }

    // ── reload_kif ──────────────────────────────────────────────────────────

    #[test]
    fn reload_kif_false_new_file_is_ingested_and_promoted() {
        // Reload ingests the new file; promotion is a separate, explicit step
        // (never automatic).  The explicit `make_session_axiomatic` does the
        // lifting and indexes the axiom in SInE.
        let mut kb = KnowledgeBase::new();
        assert!(kb.file_roots("new.kif").is_empty());
        let r = kb.reload_kif("(subclass Dog Mammal)", &PathBuf::from("new.kif"), "new.kif");
        assert!(r.ok);
        assert_eq!(r.added(), 1);
        assert!(!kb.file_roots("new.kif").is_empty());
        assert_eq!(kb.layer.semantic.syntactic.sine.with_ref(|idx| idx.axiom_count()), 0,
            "reload alone is a pure ingest — nothing is promoted yet");
        kb.make_session_axiomatic("new.kif").expect("promote");
        assert_eq!(kb.layer.semantic.syntactic.sine.with_ref(|idx| idx.axiom_count()), 1,
            "explicit promotion lifts the new axiom into SInE");
    }

    #[test]
    fn reload_kif_false_existing_file_updates_syntactically_without_sine() {
        // Existing-file path (reconcile_syntactic_only): store is updated
        // but SInE is intentionally not touched.
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)");
        let sine_before = kb.layer.semantic.syntactic.sine.with_ref(|idx| idx.axiom_count());

        let r = kb.reload_kif("(subclass Dog Mammal)\n(subclass Cat Mammal)", &PathBuf::from("t.kif"), "test");
        assert!(r.ok);
        assert_eq!(r.added(), 1);
        assert_eq!(kb.file_roots("t.kif").len(), 2);
        assert_eq!(kb.layer.semantic.syntactic.sine.with_ref(|idx| idx.axiom_count()), sine_before,
            "syntactic-only path must not update SInE");
    }

    #[test]
    fn reload_kif_false_existing_noop_when_unchanged() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)");
        let r = kb.reload_kif("(subclass Dog Mammal)", &TEST_PATH, "test");
        assert!(r.ok);
        assert_eq!(r.added(), 0);
        assert_eq!(r.removed(), 0);
    }

    // ── reload_kif (validate=true) ──────────────────────────────────────────

    #[test]
    fn reload_kif_true_new_file_is_ingested_and_promoted() {
        // `validate=true` does not change the contract: reload ingests, the
        // caller lifts explicitly.  Promotion does the SInE indexing.
        let new_path = PathBuf::from("new.kif");
        let mut kb = KnowledgeBase::new();
        let r = kb.reload_kif("(subclass Dog Mammal)", &new_path, "new.kif");
        assert!(r.ok);
        assert_eq!(r.added(), 1);
        assert!(!kb.file_roots("new.kif").is_empty());
        kb.make_session_axiomatic("new.kif").expect("promote");
        assert_eq!(kb.layer.semantic.syntactic.sine.with_ref(|idx| idx.axiom_count()), 1);
    }

    #[test]
    fn reload_kif_noop_when_text_unchanged() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)\n(subclass Cat Mammal)");
        let r = kb.reload_kif("(subclass Dog Mammal)\n(subclass Cat Mammal)", &PathBuf::from("t.kif"), "test");
        assert_eq!(r.retained, 2);
        assert_eq!(r.added(), 0);
        assert_eq!(r.removed(), 0);
        assert!(r.is_noop());
    }

    #[test]
    fn reload_kif_detects_pure_addition() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)");
        let r = kb.reload_kif("(subclass Dog Mammal)\n(subclass Cat Mammal)", &PathBuf::from("t.kif"), "test");
        assert_eq!(r.retained, 1);
        assert_eq!(r.added(), 1);
        assert_eq!(r.removed(), 0);
    }

    #[test]
    fn reload_kif_detects_pure_removal() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)\n(subclass Cat Mammal)");
        let r = kb.reload_kif("(subclass Dog Mammal)", &PathBuf::from("t.kif"), "test");
        assert_eq!(r.retained, 1);
        assert_eq!(r.added(), 0);
        assert_eq!(r.removed(), 1);
    }

    #[test]
    fn reload_kif_detects_mixed_edit() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)\n(subclass Cat Mammal)");
        let r = kb.reload_kif("(subclass Dog Mammal)\n(subclass Whale Mammal)", &PathBuf::from("t.kif"), "test");
        assert_eq!(r.retained, 1);
        assert_eq!(r.added(), 1);
        assert_eq!(r.removed(), 1);
    }

    #[test]
    fn reload_kif_removed_axioms_drop_from_sine_index() {
        // Promote two file axioms, then re-ingest the SAME file with one dropped:
        // the source reconcile retracts `Cat` and the RootRemoved cascade
        // unindexes it from SInE.  (A *file* update is the reconcile path; `tell`
        // would now append.)
        let path = PathBuf::from("t.kif");
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)\n(subclass Cat Mammal)");
        assert_eq!(kb.layer.semantic.syntactic.sine.with_ref(|idx| idx.axiom_count()), 2);
        kb.reload_kif("(subclass Dog Mammal)", &path, "t.kif");
        kb.commit("t.kif");
        assert_eq!(kb.layer.semantic.syntactic.sine.with_ref(|idx| idx.axiom_count()), 1);
    }

    #[test]
    fn reload_kif_added_axioms_are_indexed_in_sine() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)");
        // Reload is a pure (syntactic) re-ingest — it does NOT auto-promote.  The
        // added `Whale` axiom is transient until the caller explicitly lifts the
        // session; only then does the `SessionAxiomatized → AxiomsPromoted`
        // cascade index it into SInE.
        let _r = kb.reload_kif("(subclass Dog Mammal)\n(subclass Whale Mammal)", &PathBuf::from("t.kif"), "t.kif");
        kb.make_session_axiomatic("t.kif").expect("promote");
        assert_eq!(kb.layer.semantic.syntactic.sine.with_ref(|idx| idx.axiom_count()), 2);
        assert!(kb.store_for_testing().sym_id("Whale").is_some(),
            "Whale should have been interned");
    }

    #[test]
    fn reload_kif_retained_sentences_keep_sids() {
        // SentenceId stability is the core contract that makes reload cheaper
        // than wipe-and-reload: SInE triggers, fingerprint keys, and LMDB
        // rows all stay valid without rehashing.
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)\n(subclass Cat Mammal)");
        let old: HashSet<_> = kb.file_roots("t.kif").into_iter().collect();
        let _r = kb.reload_kif("(subclass Dog Mammal)\n(subclass Whale Mammal)", &PathBuf::from("t.kif"), "test");
        kb.commit("t.kif");
        let new: HashSet<_> = kb.file_roots("t.kif").into_iter().collect();
        // The retained `(subclass Dog Mammal)` sentence must keep its SentenceId,
        // so its SID appears in both root sets (the intersection is exactly it).
        assert_eq!(old.intersection(&new).count(), 1,
            "retained sentence must keep its SentenceId");
    }

    #[test]
    fn reload_kif_parse_error_aborts_without_mutation() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)");
        let axioms_before = kb.layer.semantic.syntactic.sine.with_ref(|idx| idx.axiom_count());
        let r = kb.reload_kif("(subclass Dog Mammal", &TEST_PATH, "test");
        assert!(r.has_errors());
        assert_eq!(r.retained, 0);
        assert_eq!(r.added(), 0);
        assert_eq!(r.removed(), 0);
        assert_eq!(kb.layer.semantic.syntactic.sine.with_ref(|idx| idx.axiom_count()), axioms_before);
    }

    #[test]
    fn reload_kif_alpha_equivalent_edit_is_remove_and_add() {
        // compute_file_diff uses structural (non-alpha-equivalent) fingerprints,
        // so renaming ?X to ?Y counts as remove+add at the file-diff level
        // even though the formula is logically identical.  This is a FILE update
        // (reconcile); two reloads of the same path diff against each other.
        let path = PathBuf::from("t.kif");
        let mut kb = KnowledgeBase::new();
        kb.reload_kif("(=> (P ?X) (Q ?X))", &path, "t.kif");
        let r = kb.reload_kif("(=> (P ?Y) (Q ?Y))", &path, "t.kif");
        assert_eq!(r.retained, 0);
        assert_eq!(r.added(), 1);
        assert_eq!(r.removed(), 1);
    }

    // ── truncation: an empty file zeroes the source ─────────────────────────

    #[test]
    fn truncate_removes_every_axiom_of_the_file() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)\n(subclass Cat Mammal)");
        assert_eq!(kb.file_roots("t.kif").len(), 2);

        // An empty source diffs against the file's prior formulas: everything
        // it used to contribute is recycled, nothing is added.
        let r = kb.load(SourceFile::truncate(PathBuf::from("t.kif")), "t.kif");
        assert!(r.ok, "truncate failed: {:?}", r.diagnostics);
        assert_eq!(r.added(), 0);
        assert_eq!(r.removed(), 2, "both axioms should be recycled");
        assert!(kb.file_roots("t.kif").is_empty(), "file should own nothing after truncation");
    }

    #[test]
    fn reloading_with_empty_text_zeroes_the_file() {
        // Same outcome by the route a caller is likelier to take: re-ingesting
        // the same path with empty contents rather than building a truncate.
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)\n(instance Rex Dog)");
        assert_eq!(kb.file_roots("t.kif").len(), 2);

        let r = kb.reload_kif("", &PathBuf::from("t.kif"), "t.kif");
        assert!(r.ok, "empty reload failed: {:?}", r.diagnostics);
        assert_eq!(r.removed(), 2);
        kb.commit("t.kif");                    // staged removals become real
        assert!(kb.file_roots("t.kif").is_empty());
    }

    #[test]
    fn remove_file_drops_the_files_axioms() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)\n(subclass Cat Mammal)");
        kb.remove_file("t.kif");
        assert!(kb.file_roots("t.kif").is_empty());
    }

    #[test]
    fn truncating_one_file_keeps_a_sentence_another_file_still_owns() {
        // Sentences are content-addressed, so an axiom present in two files is
        // one sid with two owners. Truncating one owner must not retract it —
        // this is the refcounting `remove_file` documents.
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "a.kif", "(subclass Dog Mammal)");
        load_file(&mut kb, "b.kif", "(subclass Dog Mammal)\n(subclass Cat Mammal)");

        kb.remove_file("a.kif");

        assert!(kb.file_roots("a.kif").is_empty(), "a.kif should own nothing");
        assert_eq!(kb.file_roots("b.kif").len(), 2,
            "b.kif still owns the shared axiom and its own");
        assert!(kb.store_for_testing().sym_id("Dog").is_some(),
            "the shared axiom must survive: b.kif still references it");
    }

    #[test]
    fn truncate_then_reload_restores_the_file() {
        // Truncation is not destructive to the path: re-ingesting the original
        // text brings the axioms back as ordinary additions.
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif", "(subclass Dog Mammal)\n(subclass Cat Mammal)");
        kb.remove_file("t.kif");
        assert!(kb.file_roots("t.kif").is_empty());

        let r = kb.reload_kif("(subclass Dog Mammal)\n(subclass Cat Mammal)",
                              &PathBuf::from("t.kif"), "t.kif");
        assert!(r.ok, "reload after truncate failed: {:?}", r.diagnostics);
        assert_eq!(r.added(), 2);
        assert_eq!(r.removed(), 0);
        assert_eq!(kb.file_roots("t.kif").len(), 2);
    }

    #[test]
    fn revalidating_a_buffer_leaves_the_file_untouched() {
        // How the editor revalidates a buffer with full KB context and without
        // polluting it: stage the edited text as a diff against the file, judge
        // only the sentences the diff added, then stage the original back so the
        // additions are diffed away again.
        let mut kb = KnowledgeBase::new();
        let orig = "(subclass Dog Mammal)\n(instance Rex Dog)";
        load_file(&mut kb, "t.kif", orig);
        let before = kb.file_roots("t.kif").len();

        let edited = format!("{orig}\n(documentation ZzGhost EnglishLanguage \"half typed\")");
        let staged = kb.reload_kif(&edited, &PathBuf::from("t.kif"), "t.kif");
        assert_eq!(staged.added(), 1, "only the edited line is staged, not the file");

        // Semantic checks resolve against the whole KB here, which is the point.
        let mut diags = Vec::new();
        for sid in &staged.sids { diags.extend(kb.validate_sentence(*sid)); }

        let back = kb.reload_kif(orig, &PathBuf::from("t.kif"), "t.kif");
        kb.commit("t.kif");
        assert_eq!(back.removed(), 1, "the staged addition is diffed back out");

        assert_eq!(kb.file_roots("t.kif").len(), before, "file restored exactly");
        let opts = crate::kb::search::SearchOpts { kind: None, language: None, limit: None };
        assert!(kb.search("half typed", &opts).is_empty(), "no searchable ghost remains");
        assert!(kb.symbol_id("ZzGhost").is_none(), "the buffer's symbol is not left interned");
    }

    #[test]
    fn removal_keeps_variable_symbols_of_other_live_sentences() {
        use std::collections::HashSet;
        // Two rules whose scope-qualified variables (X__<scope>) are interned
        // into the symbol table. Removing one sentence must not evict the
        // OTHER rule's variable symbols — the over-prune that surfaced when the
        // editor revalidated a rule-heavy constituent.
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif",
            "(=> (instance ?X Dog) (instance ?X Mammal))\n             (=> (instance ?Y Cat) (instance ?Y Mammal))\n             (instance Rex Dog)");

        let vars_before: HashSet<String> = kb.iter_symbols()
            .map(|(_, n)| n).filter(|n| n.contains("__")).collect();
        assert!(vars_before.len() >= 2, "expected scope-qualified var symbols, got {vars_before:?}");

        // Remove one sentence (the atomic fact) — a real FormulaRemoved batch.
        let r = kb.reload_kif(
            "(=> (instance ?X Dog) (instance ?X Mammal))\n             (=> (instance ?Y Cat) (instance ?Y Mammal))",
            &PathBuf::from("t.kif"), "t.kif");
        assert_eq!(r.removed(), 1);
        kb.commit("t.kif");

        let vars_after: HashSet<String> = kb.iter_symbols()
            .map(|(_, n)| n).filter(|n| n.contains("__")).collect();
        // Both rules survive, so their variable symbols must survive too.
        assert_eq!(vars_before, vars_after,
            "variables of still-live rules must not be pruned by an unrelated removal");
    }

    #[test]
    fn removing_a_rule_evicts_only_its_own_variable_symbols() {
        use std::collections::HashSet;
        // The complement: a removed rule's OWN variables should still be
        // reclaimed, so the fix keeps legitimate orphan pruning working.
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "t.kif",
            "(=> (instance ?X Dog) (instance ?X Mammal))\n             (=> (instance ?Y Cat) (instance ?Y Mammal))");
        let before: HashSet<String> = kb.iter_symbols().map(|(_,n)|n).filter(|n|n.contains("__")).collect();

        // Drop the first rule entirely.
        let r = kb.reload_kif("(=> (instance ?Y Cat) (instance ?Y Mammal))",
            &PathBuf::from("t.kif"), "t.kif");
        assert_eq!(r.removed(), 1);
        kb.commit("t.kif");

        let after: HashSet<String> = kb.iter_symbols().map(|(_,n)|n).filter(|n|n.contains("__")).collect();
        assert!(after.len() < before.len(),
            "the removed rule's own variable symbols should be reclaimed ({before:?} -> {after:?})");
    }

    #[test]
    fn symbol_is_variable_discriminates_scope_qualified_names() {
        let kb = KnowledgeBase::new();
        // Scope-qualified variables (what the store interns) are variables.
        for v in ["X__0", "X__1", "OBJ1__42", "ROW__3", "FOO__BAR__7"] {
            assert!(kb.symbol_is_variable(v), "{v} should read as a variable");
        }
        // Real ontology terms are not — including ones with a double underscore
        // but no trailing scope number.
        for t in ["Human", "instance", "subclass", "Mid-level", "foo__bar", "A__", "__3"] {
            assert!(!kb.symbol_is_variable(t), "{t} should read as a term");
        }
    }

    // ── reload_kifs (batch) ─────────────────────────────────────────────────

    #[test]
    fn reload_kifs_processes_multiple_files_independently() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "a.kif", "(subclass Dog Mammal)");
        load_file(&mut kb, "b.kif", "(subclass Cat Mammal)");

        let results = kb.reload_kifs([
            ("a.kif", "(subclass Dog Mammal)\n(subclass Wolf Mammal)"),
            ("b.kif", "(subclass Cat Mammal)\n(subclass Lion Mammal)"),
        ], "Test");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].added(), 1, "a.kif: one addition");
        assert_eq!(results[1].added(), 1, "b.kif: one addition");
        // The two prior files were promoted by `load_file`; lift the reload's
        // session to index the two new axioms (Wolf, Lion) → 4 total.
        kb.make_session_axiomatic("Test").expect("promote");
        assert_eq!(kb.layer.semantic.syntactic.sine.with_ref(|idx| idx.axiom_count()), 4);
    }

    #[test]
    fn reload_kifs_parse_error_in_one_file_does_not_abort_others() {
        let mut kb = KnowledgeBase::new();
        load_file(&mut kb, "a.kif", "(subclass Dog Mammal)");
        load_file(&mut kb, "b.kif", "(subclass Cat Mammal)");

        let results = kb.reload_kifs([
            ("a.kif", "(subclass Dog Mammal"),           // parse error
            ("b.kif", "(subclass Cat Mammal)\n(subclass Lion Mammal)"),
        ],"Test");
        assert_eq!(results.len(), 2);
        assert!(!results[0].ok, "a.kif should report parse error");
        assert!(results[1].ok, "b.kif should succeed despite a.kif error");
        assert_eq!(results[1].added(), 1);
    }

    #[test]
    fn reload_kifs_new_files_in_batch_are_indexed_in_sine() {
        let mut kb = KnowledgeBase::new();
        let results = kb.reload_kifs([
            ("a.kif", "(subclass Dog Mammal)"),
            ("b.kif", "(subclass Cat Mammal)"),
        ], "Test");
        assert!(results.iter().all(|r| r.ok));
        // Batch ingest doesn't auto-promote either; lift the batch's session.
        kb.make_session_axiomatic("Test").expect("promote");
        assert_eq!(kb.layer.semantic.syntactic.sine.with_ref(|idx| idx.axiom_count()), 2);
    }

    #[test]
    fn reload_kifs_returns_one_result_per_input_file_in_order() {
        let mut kb = KnowledgeBase::new();
        let results = kb.reload_kifs([
            ("x.kif", "(subclass A B)"),
            ("y.kif", "(subclass C D)"),
            ("z.kif", "(subclass E F)"),
        ], "Test");
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].added(), 1);
        assert_eq!(results[1].added(), 1);
        assert_eq!(results[2].added(), 1);
    }
}