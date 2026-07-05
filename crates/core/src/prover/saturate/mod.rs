// crates/core/src/saturate/mod.rs
//
// Native saturation prover — the alternative TOP layer of the KB stack.
//
// Where `TranslationLayer` renders the KB to TPTP for an external prover
// (Vampire/E subprocess), `ProverLayer` proves natively: clausify stored
// sentences in-process, retrieve literals through the residue index, and
// run a given-clause refutation loop with theory-oracle discharge against
// the semantic layer's taxonomy / subrelation / transitivity closures.
// `KnowledgeBase<ProverLayer>` swaps it in via the `TopLayer` seam — the
// syntactic and semantic layers underneath are shared, unchanged.
//
// Gated on the `native-prover` feature (no new dependencies, independent
// of `ask`).  Submodules: `clause` (Term/AtomTable/PClause), `clausify`
// (the native clausifier), `canon` (canonical clause form), `caches`
// (the lazy clause store).  The residue index, theory oracle, and
// given-clause loop land in subsequent phases.

pub(crate) mod clause;
pub(crate) mod clausify;
pub(crate) mod canon;
pub(crate) mod caches;
pub(crate) mod hash64;
pub(crate) mod kbo;
pub(crate) mod terms;
pub(crate) mod index;
pub(crate) mod unify;
pub(crate) mod units;
pub(crate) mod oracle;
pub(crate) mod theory;
pub(crate) mod temporal;
pub(crate) mod eventcalc;
pub(crate) mod model;
pub(crate) mod prover;
pub(crate) mod proof;
pub(crate) mod schema;
mod prove;
mod consistency;
pub mod strategy;

#[cfg(test)]
mod tests;

use super::ProvingLayer;
pub(crate) use super::Conjecture;

use std::sync::Arc;

use crate::cache::{Cache, CacheConfig, WholeCache};
use crate::layer::{Layer, NoLayer, TopLayer};
use crate::prover::saturate::prover::NativeOpts;
use crate::semantics::SemanticLayer;
use crate::types::SentenceId;

use clause::{AtomTable, PClause};
use caches::clause_store::ClauseStore;
use caches::model_registry::ModelRegistry;

pub(crate) use caches::fingerprint::{AtomInfo, slot_term_seat_coin, arity_tag, AtomInfos};

/// The native-prover top layer.  Owns the semantic stack plus the
/// prover-local clause state; the residue index and given-clause loop
/// accrete here in later phases.
#[derive(Debug)]
pub struct ProverLayer {
    /// Inner layer: the semantic layer this prover queries for
    /// taxonomy / subrelation / transitivity discharge.
    pub(crate) semantic: SemanticLayer,

    /// Prover-local atom storage: canonical atom `Sentence`s by content
    /// hash.  NOT the shared sentence store — derived literals churn too
    /// fast for refcounted ingest (plan D5).  Interior-mutable; interns
    /// happen behind `&self` from cache `generate` and the prover loop.
    pub(crate) atoms: AtomTable,

    /// Lazy root → canonical clauses cache.  See
    /// [`caches::clause_store`].
    pub(crate) clause_store: Cache<ClauseStore>,

    /// Whole-KB inductive-definition model program (Phase 5): extracted
    /// Datalog rules + role schemas + cluster partition + monotone fragment,
    /// built once and reactor-invalidated on root changes.  See
    /// [`caches::model_registry`].  Not yet consulted by the prover (slice 1).
    pub(crate) model_program: WholeCache<ModelRegistry>,

    /// Memoized per-atom residue facts (mask, fingerprint, seat coins) —
    /// content-addressed, so background atoms are fingerprinted once,
    /// ever, across every problem.  See [`fingerprint`].
    pub(crate) atom_infos: AtomInfos,

    /// The schema-channel pattern table: precomputed fingerprints of
    /// theory-rule clause shapes (symmetry / transitivity / … and
    /// their second-order metaschemas).  See [`schema`].
    pub(crate) schema: schema::SchemaTable,

    /// The Knuth–Bendix reduction ordering and its content-addressed
    /// per-atom weight/variable memo.  Layer-fixed (the memo is sound to
    /// share); the keystone the equality machinery will consume.  See
    /// [`kbo`].
    pub(crate) kbo: kbo::KboOrdering,

    /// Ground-term facts memo (size / depth / symbol Bloom / KBO
    /// weight), keyed by content hash — the prover-side tier of the
    /// two-tier ground-term identity design.  Content-addressed and
    /// pure, so layer-shared across runs like `atom_infos` beside it.
    /// Only ever consulted while `Strategy.demod` is on.  See [`terms`].
    pub(crate) term_facts: terms::TermFactsTable,

    /// Frozen background problem bases, keyed by a fingerprint of
    /// everything that shaped them (scope, selection, session content,
    /// conjecture, whole-KB root set, make-affecting opts).  A hit
    /// skips the theory pre-pass and background clause loading for
    /// repeat queries (serve / SDK loops / retried autoscale budgets).
    /// Capped — see `kb/prove_native.rs`.
    pub(crate) bg_snapshots:
        dashmap::DashMap<u64, std::sync::Arc<prover::ProverSnapshot>>,
}

impl ProverLayer {
    pub(crate) fn new(semantic: SemanticLayer) -> Self {
        Self::with_config(semantic, &CacheConfig::default())
    }

    /// Construct a `ProverLayer` whose caches share `cfg`.
    pub(crate) fn with_config(semantic: SemanticLayer, cfg: &CacheConfig) -> Self {
        let atoms = AtomTable::default();
        // The pattern exemplars intern into the layer's own atom table
        // (a few dozen atoms, once per layer).
        let schema = schema::SchemaTable::build(&atoms, &semantic.syntactic);
        Self {
            semantic,
            atoms,
            clause_store: Cache::new(cfg, ClauseStore),
            model_program: WholeCache::new(cfg, ModelRegistry),
            atom_infos:   AtomInfos::default(),
            schema,
            kbo:          kbo::KboOrdering::new(),
            term_facts:   terms::TermFactsTable::default(),
            bg_snapshots: dashmap::DashMap::new(),
        }
    }

    /// The memoized residue facts of `atom`.
    pub(crate) fn atom_info(&self, atom: clause::AtomId) -> Arc<AtomInfo> {
        self.atom_infos.info(atom, &self.atoms, &self.semantic.syntactic)
    }

    /// The canonical clauses of a stored root sentence (clausified on
    /// first request, cached until the root is retracted).
    pub(crate) fn clauses_for(&self, root: SentenceId) -> Arc<Vec<PClause>> {
        self.clause_store.get(self, root)
    }

    /// `true` when `root` FAILED to load as an input: it clausified to
    /// nothing for a shape/capacity reason (unsupported shape, CNF blow-up,
    /// over-cap clause) or vanished from the store — as opposed to the sound
    /// empty results (tautology deletion / dedup).  Cheap: the loss
    /// re-clausification only runs for roots whose
    /// [`clauses_for`](Self::clauses_for) came back empty.  Feeds the
    /// input-completeness gate: a failed input root poisons any confident
    /// Disproved/Satisfiable verdict.
    pub(crate) fn root_load_failed(&self, root: SentenceId) -> bool {
        if !self.clauses_for(root).is_empty() {
            return false;
        }
        let syn = &self.semantic.syntactic;
        let Some(sent) = syn.sentence(root) else { return true };
        clausify::clausify_sentence_lossy(syn, &self.atoms, &sent, root, false).1
    }
}

impl TopLayer for ProverLayer {
    fn from_semantic(semantic: SemanticLayer) -> Self { Self::new(semantic) }
    fn semantic(&self) -> &SemanticLayer { &self.semantic }
    fn semantic_mut(&mut self) -> &mut SemanticLayer { &mut self.semantic }
}

impl Layer for ProverLayer {
    type Inner = SemanticLayer;
    type Outer = NoLayer;

    fn inner(&self) -> Option<&SemanticLayer> { Some(&self.semantic) }
    fn outer(&self) -> Option<&NoLayer> { None }

    fn schedule_cell(&self) -> &'static crate::layer::ScheduleCell {
        static CELL: crate::layer::ScheduleCell = std::sync::OnceLock::new();
        &CELL
    }

    fn cache_config(&self) -> &CacheConfig { self.semantic.cache_config() }

    fn own_reactors(&self) -> Vec<crate::cache::router::ReactorEntry<'_>> {
        use crate::cache::router::bind;
        vec![
            bind(&self.clause_store, self),
            bind(&self.model_program, self),
        ]
    }
}

impl ProvingLayer for ProverLayer {
    type Opts = NativeOpts;

    /// Interns into the prover-local atom table.  Forwards to the inherent
    /// `&self` [`intern_conjecture_native`](ProverLayer::intern_conjecture_native);
    /// the whole `ProvingLayer` impl is `&self` (sweep-safe).
    fn intern_conjecture(&self, asts: &[crate::AstNode])
        -> Vec<(std::sync::Arc<crate::types::Sentence>, SentenceId)> {
        self.intern_conjecture_native(asts)
    }

    /// Native step-exhaustion (`GaveUp`) means the search space was too big —
    /// narrow like a timeout, not widen (the planner reads `GaveUp` as
    /// prover-incompleteness on the TPTP path).
    fn remap(
        _status: super::result::ProverStatus,
        term:    Option<super::result::TerminationReason>,
    ) -> Option<super::result::TerminationReason> {
        match term {
            Some(super::result::TerminationReason::GaveUp) =>
                Some(super::result::TerminationReason::TimeLimit),
            other => other,
        }
    }

    fn prove_once(
        &self,
        conj:   &super::Conjecture,
        params: crate::SineParams,
        slice:  u32,
        opts:   &NativeOpts,
        ctx:    &crate::ProveCtx,
    ) -> (super::result::ProverResult, usize) {
        // `prove_one_driver` is `&self` — sweep-safe; nothing here mutates.
        self.prove_one_driver(conj, params, slice, opts, ctx)
    }

    /// TPTP-regime strategy schedule (see `prove.rs`'s
    /// `run_portfolio_schedule` / `strategy::Strategy::tptp_lanes`): engages
    /// only when `opts` was configured for a standalone TPTP problem
    /// (`set_tptp_problem` swapped in `Strategy::tptp()`, the sole source of
    /// `full_saturation`) and `SIGMA_NO_PORTFOLIO` isn't set, so the KIF/SUMO
    /// path (`full_saturation` off) always falls through to `None` — the
    /// trait default's plain `drive` loop, byte-identical to before this
    /// hook existed.
    fn try_portfolio(
        &self,
        conj:          &super::Conjecture,
        total_timeout: u32,
        opts:          &NativeOpts,
        ctx:           &crate::ProveCtx,
    ) -> Option<super::result::ProverResult> {
        if !opts.strategy.full_saturation || std::env::var_os("SIGMA_NO_PORTFOLIO").is_some() {
            return None;
        }
        Some(self.run_portfolio_schedule(conj, total_timeout, opts, ctx))
    }

    fn check_consistency(&self, opts: &NativeOpts, ctx: &crate::ProveCtx)
        -> super::result::ProverResult {
        // `limit = 1`: stop at the first contradiction — the cross-backend
        // consistency contract.  The enumerating audit calls the same driver
        // (`check_consistency_driver`) with a larger limit and a `focus`.
        self.check_consistency_driver(opts.session.as_deref(), &[], opts.selection, opts.clone(), ctx, 1)
    }

    /// Enumerate up to `limit` distinct contradictions over `focus` — the
    /// native driver behind `KnowledgeBase::audit_consistency`.
    fn audit_consistency(
        &self,
        focus:  &[SentenceId],
        opts:   &NativeOpts,
        limit:  usize,
        ctx:    &crate::ProveCtx,
    ) -> super::result::ProverResult {
        self.check_consistency_driver(opts.session.as_deref(), focus, opts.selection, opts.clone(), ctx, limit)
    }
}

// `Conjecture` + `collect_ast_symbols` now live on the shared prover module
// (`crate::prover::Conjecture`) so both backends share one type; see the
// `pub(crate) use super::Conjecture` re-export above.