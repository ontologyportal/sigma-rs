//! Unified cross-layer change-event model for the reactive cache graph.

use std::sync::Arc;

use crate::{Sentence, types::{SentenceId, SymbolId}};

/// A single typed change event flowing through the reactive graph.
///
/// Batches are passed as `&[Event]`; a reaction returns [`Vec<Event>`] (the
/// follow-on events it produces).  Events are id-based and small, and their
/// application is commutative (targets are set-shaped) so a level can be
/// applied in parallel order-free.
#[derive(Debug, Clone)]
pub(crate) enum Event {
    /// New source file / session is ingested. Need parsing, will trigger 
    /// fingerprint deduplication 
    SourceAdded { session: Arc<String>, file: crate::types::SourceFile, staged: bool },
    /// New formulas parsed in a file/session context. All start as assertions.
    FormulaAdded { node: u64, session: Arc<String> },
    /// Formulas retracted (the API resolves file/session → sids first).
    FormulaRemoved { node: u64 },
    /// `session` newly references an *already-present* formula `node`.  The
    /// sentence-store reactor resolves the fingerprint to its root sids, records
    /// the new session membership, and surfaces `SessionReferenced` so
    /// scope-bearing indices learn the new scope.
    FormulaReferenced { node: u64, session: Arc<String> },
    /// Formulas a file reconcile carried over **unchanged** (their fingerprint was
    /// present in both the prior and the new parse).  Informational: the router
    /// collects it into `emitted` so `reload_kif` can report the retained count.
    FormulasUnchanged { nodes: Vec<u64> },
    /// A staged file reconcile deferred these formulas' removal into the
    /// recycle bin (they stay live until `commit`).  Informational: `stage()`
    /// reads it so the reported diff counts staged removals as removed.
    FormulasRecycled { nodes: Vec<u64> },
    /// A whole session's assertions are lifted to axioms.
    SessionAxiomatized { session: String },
    /// A non-axiomatic session is retracted wholesale. The session cache drops
    /// the session's bookkeeping; the actual sentence removal is source-driven.
    /// Axiomatic sessions cannot be retracted wholesale (individual axioms still
    /// retract via `RootRemoved`).
    SessionRetracted { session: String },
    /// `session` newly references already-interned root sentences `sids` into a
    /// session that did not previously own them.  Scope-bearing indices that
    /// snapshot a sentence's session set at add-time (`tax_edges`) use this to
    /// extend that snapshot; live-reading consumers (`inferred_class`) use it to
    /// invalidate the session's stale entries.  Carries no semantics for sids
    /// that aren't taxonomy edges — consumers filter.
    SessionReferenced { session: String, sids: Vec<SentenceId> },
    /// A conjecture is asked of the [`crate::KnowledgeBase`]
    QuestionAsked { conjecture: crate::parse::ast::AstNode, session: String },

    // Tier 1 — sentence-store reactor output
    /// The sentence store changed: which sentences and symbols were
    /// added/removed, and which symbols were merely *touched* (already present,
    /// occurrence count unchanged-to-nonzero). Consumed by `occurrences`,
    /// `head_index`, the doc caches, and the detectors.
    SentencesChanged {
        added:         Vec<SentenceId>,
        removed:       Vec<SentenceId>,
        syms_added:    Vec<SymbolId>,
        syms_removed:  Vec<SymbolId>,
        syms_modified: Vec<SymbolId>,
    },
    /// A session's sentences became axioms. Consumed by `axiom_index`, `sine`.
    AxiomsPromoted { sids: Vec<SentenceId> },

    // ---- syntactic (Phase-1 transitional; superseded by Tier 0/1 above) ----
    /// A parsed root node was admitted (id already minted).
    RootAdded { sid: SentenceId },
    /// A root was retracted (file unload / reconcile remove).
    ///
    /// Carries the removed sentences so consumers don't have to read the body
    /// back from the store, which removal has already torn out of the map.
    RootRemoved { sid: SentenceId, sentences: Vec<Sentence> },
    /// A new relation was added (a sentence with a symbol head).
    RelationAdded { sid: SentenceId, head_id: SymbolId },
    /// A relation was removed (a sentence with a symbol head).
    RelationRemoved { sid: SentenceId, sentence: Sentence },
    /// A synthetic sentence was produced by normalization, derived from `origin`.
    SyntheticAdded { sid: SentenceId, origin: SentenceId },
    /// A synthetic sentence was dropped (its `origin` was removed).
    SyntheticRemoved { sid: SentenceId, origin: SentenceId },
    /// A sentence now contributes these symbol uses (reverse-index maintenance).
    SymbolsIntroduced { sid: SentenceId, syms: Vec<SymbolId> },
    /// A sentence no longer contributes these symbol uses.
    SymbolsRetracted { sid: SentenceId, syms: Vec<SymbolId> },
    /// Trigger the orphan-symbol prune after a removal batch (scan-and-prune).
    Prune,

    // ---- semantic classification (middle layer) ----
    // These carry the affected `SymbolId`s that the consuming caches evict.
    /// Taxonomy edges changed; `syms` = symbols whose taxonomy-derived caches
    /// must drop (`SemanticDelta::taxonomy_affected_symbols`).
    TaxonomyChanged { syms: Vec<SymbolId> },
    /// Relation domain/range changed; `syms` = the affected relation symbols
    /// (added ∪ removed domain/range).
    DomainRangeChanged { syms: Vec<SymbolId> },
    /// Non-taxonomy, non-domain/range roots changed; `syms` = all symbols those
    /// sentences mention (`SemanticDelta::all_affected_symbols`).
    OtherRootsChanged { syms: Vec<SymbolId> },

    // ---- batch markers (translation fast path) ----
    /// The batch is a pure addition (no taxonomy edge / domain-range / other
    /// root was removed) — enables the formula/relation cache fast path.
    PureAddition,
    /// An implication-shaped root was added (rewrite pass must re-run).
    ImplicationsAdded,

    /// A numeric sort class was removed
    NumericSortRemoved(SymbolId),
    /// A numeric sort class was added
    NumericSortAdded(SymbolId),

    // ---- validation (user-triggered) ----
    /// Validate *every* root sentence: the `semantic::validation` cache runs the
    /// semantic validator over each root and memoises the result.
    ValidateKB,
    /// Validate a single sentence and memoise its result in the
    /// `semantic::validation` cache.
    ValidateSentence { sid: SentenceId },
    /// Validate every sentence belonging to `session`, reasoning in that
    /// session's scope (`Base` ∪ the session's transient overlay) rather than
    /// globally — so a session's own transient taxonomy/type declarations are
    /// visible to the validator.  `ValidateKB` / `ValidateSentence` stay global.
    ValidateSession { session: String },

    // ---- diagnostics (emitted by any reactor, consumed by none) ----
    /// A warning or non-fatal error surfaced by a reactor.  No reactor
    /// `consumes` it (it never participates in the schedule); the router peels
    /// these off the emitted stream into [`RouteOutcome::errors`].
    ///
    /// [`RouteOutcome::errors`]: crate::cache::router::RouteOutcome::errors
    Diagnostic(crate::Diagnostic),
}

/// Lightweight discriminant used by the static graph: caches declare the
/// `EventKind`s they `consumes`/`produces`, never the full `Event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum EventKind {
    /// New source file is ingested
    SourceAdded,
    /// New formula parsed in a file/session context. All start as assertions.
    FormulaAdded,
    /// Formula retracted (the API resolves file/session → sids first).
    FormulaRemoved,
    /// A session newly references an already-present formula (cross-session dedup).
    FormulaReferenced,
    /// Formulas carried over unchanged by a file reconcile (informational tally).
    FormulasUnchanged,
    /// Formulas whose removal was deferred into the recycle bin by a staged reconcile.
    FormulasRecycled,
    /// A whole session's assertions are lifted to axioms.
    SessionAxiomatized,
    /// A non-axiomatic session is retracted wholesale.
    SessionRetracted,
    /// A session newly references already-interned roots (dedup re-assert).
    SessionReferenced,
    /// A conjecture is asked of the knowledge base.
    QuestionAsked,
    /// The sentence store changed (sentences and symbols added/removed/touched).
    SentencesChanged,
    /// A session's sentences became axioms.
    AxiomsPromoted,
    /// A parsed root node was admitted.
    RootAdded,
    /// A root was retracted.
    RootRemoved,
    /// A relation (symbol-headed sentence) was added.
    RelationAdded,
    /// A relation (symbol-headed sentence) was removed.
    RelationRemoved,
    /// A synthetic sentence was produced by normalization.
    SyntheticAdded,
    /// A synthetic sentence was dropped.
    SyntheticRemoved,
    /// A sentence contributed new symbol uses.
    SymbolsIntroduced,
    /// A sentence's symbol uses were retracted.
    SymbolsRetracted,
    /// Trigger the orphan-symbol prune after a removal batch.
    Prune,
    /// Taxonomy edges changed.
    TaxonomyChanged,
    /// Relation domain/range changed.
    DomainRangeChanged,
    /// Non-taxonomy, non-domain/range roots changed.
    OtherRootsChanged,
    /// The batch is a pure addition (enables cache fast paths).
    PureAddition,
    /// An implication-shaped root was added.
    ImplicationsAdded,
    /// A numeric sort class was removed.
    NumericSortRemoved,
    /// A numeric sort class was added.
    NumericSortAdded,
    /// Validate every root sentence.
    ValidateKB,
    /// Validate a single sentence.
    ValidateSentence,
    /// Validate every sentence in a session's scope.
    ValidateSession,
    /// A warning or non-fatal error surfaced by a reactor.
    Diagnostic,
}

impl Event {
    /// Returns the lightweight [`EventKind`] discriminant for this event.
    pub(crate) fn kind(&self) -> EventKind {
        match self {
            Event::SourceAdded { .. } => EventKind::SourceAdded,
            Event::FormulaAdded { .. } => EventKind::FormulaAdded,
            Event::FormulaReferenced { .. } => EventKind::FormulaReferenced,
            Event::FormulasUnchanged { .. } => EventKind::FormulasUnchanged,
            Event::FormulasRecycled { .. } => EventKind::FormulasRecycled,
            Event::FormulaRemoved { .. } => EventKind::FormulaRemoved,
            Event::SessionAxiomatized { .. } => EventKind::SessionAxiomatized,
            Event::SessionRetracted { .. } => EventKind::SessionRetracted,
            Event::SessionReferenced { .. } => EventKind::SessionReferenced,
            Event::QuestionAsked { .. } => EventKind::QuestionAsked,
            Event::SentencesChanged { .. } => EventKind::SentencesChanged,
            Event::AxiomsPromoted { .. } => EventKind::AxiomsPromoted,
            Event::RootAdded { .. } => EventKind::RootAdded,
            Event::RootRemoved { .. } => EventKind::RootRemoved,
            Event::RelationAdded { .. } => EventKind::RelationAdded,
            Event::RelationRemoved { .. } => EventKind::RelationRemoved,
            Event::SyntheticAdded { .. } => EventKind::SyntheticAdded,
            Event::SyntheticRemoved { .. } => EventKind::SyntheticRemoved,
            Event::SymbolsIntroduced { .. } => EventKind::SymbolsIntroduced,
            Event::SymbolsRetracted { .. } => EventKind::SymbolsRetracted,
            Event::Prune => EventKind::Prune,
            Event::TaxonomyChanged { .. } => EventKind::TaxonomyChanged,
            Event::DomainRangeChanged { .. } => EventKind::DomainRangeChanged,
            Event::OtherRootsChanged { .. } => EventKind::OtherRootsChanged,
            Event::PureAddition => EventKind::PureAddition,
            Event::ImplicationsAdded => EventKind::ImplicationsAdded,
            Event::ValidateKB => EventKind::ValidateKB,
            Event::ValidateSentence { .. } => EventKind::ValidateSentence,
            Event::ValidateSession { .. } => EventKind::ValidateSession,
            Event::Diagnostic(_) => EventKind::Diagnostic,
            Event::NumericSortRemoved(_) => EventKind::NumericSortRemoved,
            Event::NumericSortAdded(_) => EventKind::NumericSortAdded,
        }
    }
}

// ---------------------------------------------------------------------------
// Static reactor graph
// ---------------------------------------------------------------------------

/// One cache/reactor's static declaration of the event kinds it consumes and
/// the kinds it may produce.  Capability (may-consume / may-produce), not
/// runtime occurrence — the graph is conservative.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReactorDecl {
    /// The reactor's cache name.
    pub name: &'static str,
    /// Event kinds this reactor may consume.
    pub consumes: &'static [EventKind],
    /// Event kinds this reactor may produce.
    pub produces: &'static [EventKind],
    /// Cache names this reactor reads — data-dependency edges (the reactor whose
    /// `name` equals a read cache is ordered before this one).
    pub reads: &'static [&'static str],
}

/// A dependency cycle in the reactor graph — a hard configuration error,
/// surfaced at startup / in a unit test rather than at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CycleError {
    /// Names of the reactors that remain in the unresolved cycle.
    pub names: Vec<&'static str>,
}

/// Build the level schedule from reactor declarations, as cohorts of **decl
/// indices** (positions in `decls`).
///
/// Edge `A → B` exists when `A.produces ∩ B.consumes ≠ ∅` (A must run before
/// B).  Returns the topological *levels*: each inner `Vec` is a cohort of
/// reactors with no inter-dependencies (safe to run in parallel), and earlier
/// levels produce the events later levels consume.
///
/// Returns `Err(CycleError)` if the graph is cyclic — including a reactor that
/// both produces and consumes the same kind (a self-loop).
pub(crate) fn build_schedule_indexed(
    decls: &[ReactorDecl],
) -> Result<Vec<Vec<usize>>, CycleError> {
    let n = decls.len();

    // A self-loop (produces a kind it also consumes) is a degenerate cycle.
    for d in decls {
        if d.produces.iter().any(|k| d.consumes.contains(k)) {
            return Err(CycleError { names: vec![d.name] });
        }
    }

    // Build adjacency (out[a] = nodes that depend on a) and in-degrees.
    // Two edge kinds, both meaning "a must run before b":
    //   * EVENT — `a.produces ∩ b.consumes ≠ ∅` (b consumes what a emits).
    //   * DATA  — `b.reads` names the cache `a` writes (`a.name`); the writer of
    //             a cache runs before any reactor that reads it.
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg: Vec<usize> = vec![0; n];
    for a in 0..n {
        for b in 0..n {
            if a == b {
                continue;
            }
            let event_dep = decls[a]
                .produces
                .iter()
                .any(|k| decls[b].consumes.contains(k));
            // `a` writes its own cache `a.name`; `b` reads it iff `b.reads` lists it.
            let data_dep = decls[b].reads.contains(&decls[a].name);
            if event_dep || data_dep {
                out[a].push(b);
                indeg[b] += 1;
            }
        }
    }

    // Kahn's algorithm, emitting one parallel cohort per iteration.
    let mut placed = vec![false; n];
    let mut remaining = n;
    let mut levels: Vec<Vec<usize>> = Vec::new();
    while remaining > 0 {
        let level: Vec<usize> = (0..n).filter(|&i| !placed[i] && indeg[i] == 0).collect();
        if level.is_empty() {
            // No zero-in-degree node left ⇒ the unplaced nodes form a cycle.
            let names = (0..n).filter(|&i| !placed[i]).map(|i| decls[i].name).collect();
            return Err(CycleError { names });
        }
        for &i in &level {
            placed[i] = true;
            remaining -= 1;
        }
        for &i in &level {
            for &b in &out[i] {
                indeg[b] -= 1;
            }
        }
        levels.push(level);
    }

    Ok(levels)
}

/// [`build_schedule_indexed`] with the cohorts mapped back to reactor *names*.
#[cfg(test)]
pub(crate) fn build_schedule(
    decls: &[ReactorDecl],
) -> Result<Vec<Vec<&'static str>>, CycleError> {
    build_schedule_indexed(decls).map(|levels| {
        levels
            .into_iter()
            .map(|lvl| lvl.into_iter().map(|i| decls[i].name).collect())
            .collect()
    })
}

/// The cross-layer reactor graph.
///
/// Declares the logical dataflow of an ingest change:
/// `ingest` injects root events → the `classify` reactor derives the
/// semantic/translation events → cache and structural reactors consume them.
/// Fed to [`build_schedule`] to validate acyclicity and document the cohort
/// (parallel-level) ordering.
pub(crate) fn kb_reactor_graph() -> Vec<ReactorDecl> {
    use EventKind::*;
    vec![
        // Sources / derivation.
        ReactorDecl { name: "ingest",   consumes: &[],                       produces: &[RootAdded, RootRemoved], reads: &[] },
        ReactorDecl { name: "classify", consumes: &[RootAdded, RootRemoved], produces: &[TaxonomyChanged, DomainRangeChanged, OtherRootsChanged, PureAddition, ImplicationsAdded], reads: &[] },
        // Structural (mutate live state read by caches).
        ReactorDecl { name: "taxonomy_apply",      consumes: &[TaxonomyChanged],                                  produces: &[], reads: &[] },
        ReactorDecl { name: "trans::prime_caches", consumes: &[TaxonomyChanged],                                  produces: &[], reads: &[] },
        ReactorDecl { name: "trans::rewrite_dirty", consumes: &[TaxonomyChanged, ImplicationsAdded, PureAddition], produces: &[], reads: &[] },
        // Semantic caches.
        ReactorDecl { name: "semantic::is_instance",   consumes: &[TaxonomyChanged],                     produces: &[], reads: &[] },
        ReactorDecl { name: "semantic::is_class",      consumes: &[TaxonomyChanged],                     produces: &[], reads: &[] },
        ReactorDecl { name: "semantic::is_relation",   consumes: &[TaxonomyChanged],                     produces: &[], reads: &[] },
        ReactorDecl { name: "semantic::is_predicate",  consumes: &[TaxonomyChanged],                     produces: &[], reads: &[] },
        ReactorDecl { name: "semantic::is_function",   consumes: &[TaxonomyChanged],                     produces: &[], reads: &[] },
        ReactorDecl { name: "semantic::has_ancestor",  consumes: &[TaxonomyChanged],                     produces: &[], reads: &[] },
        ReactorDecl { name: "semantic::domain",        consumes: &[TaxonomyChanged, RelationAdded, RelationRemoved], produces: &[DomainRangeChanged], reads: &[] },
        ReactorDecl { name: "semantic::range",         consumes: &[TaxonomyChanged, DomainRangeChanged], produces: &[], reads: &[] },
        ReactorDecl { name: "semantic::inferred_class",consumes: &[TaxonomyChanged, DomainRangeChanged], produces: &[], reads: &[] },
        ReactorDecl { name: "semantic::validate",      consumes: &[ValidateKB, ValidateSentence, ValidateSession, RootAdded, RootRemoved], produces: &[], reads: &[] },
        ReactorDecl { name: "semantic::arity",         consumes: &[TaxonomyChanged, OtherRootsChanged],  produces: &[], reads: &[] },
        ReactorDecl { name: "semantic::documentation", consumes: &[TaxonomyChanged, OtherRootsChanged],  produces: &[], reads: &[] },
        ReactorDecl { name: "semantic::term_format",   consumes: &[TaxonomyChanged, OtherRootsChanged],  produces: &[], reads: &[] },
        ReactorDecl { name: "semantic::format",        consumes: &[TaxonomyChanged, OtherRootsChanged],  produces: &[], reads: &[] },
        // Translation caches.
        ReactorDecl { name: "trans::symbol_sort",     consumes: &[DomainRangeChanged],                            produces: &[], reads: &[] },
        ReactorDecl { name: "trans::sort_annotations",consumes: &[DomainRangeChanged],                            produces: &[], reads: &[] },
        ReactorDecl { name: "trans::relation_sorts",  consumes: &[TaxonomyChanged, DomainRangeChanged, PureAddition], produces: &[], reads: &[] },
        ReactorDecl { name: "trans::formulas_tff",    consumes: &[TaxonomyChanged, DomainRangeChanged, PureAddition], produces: &[], reads: &[] },
        ReactorDecl { name: "trans::formulas_fof",    consumes: &[TaxonomyChanged, DomainRangeChanged, PureAddition], produces: &[], reads: &[] },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(
        name: &'static str,
        consumes: &'static [EventKind],
        produces: &'static [EventKind],
    ) -> ReactorDecl {
        ReactorDecl { name, consumes, produces, reads: &[] }
    }

    #[test]
    fn empty_graph_is_empty_schedule() {
        assert_eq!(build_schedule(&[]), Ok(Vec::new()));
    }

    #[test]
    fn producer_runs_before_consumer() {
        // A produces RootAdded(-derived) SymbolsIntroduced; B consumes it.
        let decls = [
            decl("B", &[EventKind::SymbolsIntroduced], &[]),
            decl("A", &[EventKind::RootAdded], &[EventKind::SymbolsIntroduced]),
        ];
        let schedule = build_schedule(&decls).unwrap();
        assert_eq!(schedule, vec![vec!["A"], vec!["B"]]);
    }

    #[test]
    fn independent_reactors_share_a_level() {
        let decls = [
            decl("X", &[EventKind::RootAdded], &[]),
            decl("Y", &[EventKind::RootRemoved], &[]),
        ];
        let schedule = build_schedule(&decls).unwrap();
        assert_eq!(schedule.len(), 1);
        assert_eq!(schedule[0].len(), 2);
    }

    #[test]
    fn three_level_chain() {
        let decls = [
            decl("store", &[EventKind::RootAdded], &[EventKind::SymbolsIntroduced]),
            decl("index", &[EventKind::SymbolsIntroduced], &[EventKind::TaxonomyChanged]),
            decl("taxonomy", &[EventKind::TaxonomyChanged], &[]),
        ];
        let schedule = build_schedule(&decls).unwrap();
        assert_eq!(schedule, vec![vec!["store"], vec!["index"], vec!["taxonomy"]]);
    }

    #[test]
    fn cycle_is_rejected() {
        // A → B (A produces X, B consumes X) and B → A (B produces Y, A consumes Y).
        let decls = [
            decl("A", &[EventKind::DomainRangeChanged], &[EventKind::SymbolsIntroduced]),
            decl("B", &[EventKind::SymbolsIntroduced], &[EventKind::DomainRangeChanged]),
        ];
        let err = build_schedule(&decls).unwrap_err();
        assert_eq!(err.names.len(), 2);
    }

    #[test]
    fn kb_cross_layer_graph_is_acyclic_with_expected_cohorts() {
        let graph = kb_reactor_graph();
        let schedule =
            build_schedule(&graph).expect("cross-layer reactor graph must be acyclic");

        // Four cohorts, driven purely by the event interface:
        //   0. ingest                       — the only source (consumes nothing)
        //   1. classify + validate          — both consume ingest's RootAdded/
        //                                      RootRemoved directly
        //   2. taxonomy/domain/arity/…       — consume classify's derived events
        //                                      (`domain` re-emits DomainRangeChanged)
        //   3. range/inferred_class/sorts/…  — consume the DomainRangeChanged that
        //                                      `domain` (cohort 2) produces
        assert_eq!(schedule.len(), 4, "expected ingest → classify+validate → caches → domain/range-derived");

        // Cohort 0: ingest alone.
        assert_eq!(schedule[0], vec!["ingest"]);

        // Cohort 1: the two direct consumers of root churn.
        assert!(schedule[1].contains(&"classify"));
        assert!(schedule[1].contains(&"semantic::validate"));

        // Cohort 2: structural + first-order caches off classify's events,
        // including `domain` (which itself emits DomainRangeChanged).
        assert!(schedule[2].contains(&"taxonomy_apply"));
        assert!(schedule[2].contains(&"trans::prime_caches"));
        assert!(schedule[2].contains(&"semantic::is_instance"));
        assert!(schedule[2].contains(&"semantic::domain"));

        // Cohort 3: the caches that depend on `domain`'s DomainRangeChanged.
        assert!(schedule[3].contains(&"semantic::range"));
        assert!(schedule[3].contains(&"semantic::inferred_class"));
        assert!(schedule[3].contains(&"trans::formulas_tff"));

        // `domain` must precede `range` (the DomainRangeChanged edge).
        let cohort_of = |name| schedule.iter().position(|c| c.contains(&name)).unwrap();
        assert!(cohort_of("semantic::domain") < cohort_of("semantic::range"));
    }

    #[test]
    fn self_loop_is_rejected() {
        let decls = [decl(
            "loop",
            &[EventKind::SymbolsIntroduced],
            &[EventKind::SymbolsIntroduced],
        )];
        assert_eq!(
            build_schedule(&decls),
            Err(CycleError { names: vec!["loop"] })
        );
    }
}
