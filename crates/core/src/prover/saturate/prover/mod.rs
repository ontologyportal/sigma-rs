// crates/core/src/saturate/prover.rs
//
// The given-clause refutation loop (prototype §7) under set of support.
//
// Background KB clauses are pre-activated (indexed) and never selected
// as given; the support set — problem hypotheses plus the negated
// conjecture — drives all inference (Wos 1965; sound when the
// background is satisfiable).  Support *units* feed the oracle and
// partner passively; support *rules* and derived clauses compete in
// the passive queue by lineage tier and weight (age/weight alternation
// prevents starvation).
//
// Per given clause: oracle re-simplification, factoring, unit
// paramodulation, then binary resolution on the literal with the
// fewest index candidates (linear-resolution flavor — skipped literals
// reappear in resolvents, and stay indexed as passive partners).

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::time::Instant;

use smallvec::SmallVec;

use crate::parse::OpKind;
use crate::prover::CommonProverOpts;
use crate::semantics::types::Scope;
use crate::types::{Element, Literal, SentenceId, Symbol, SymbolId};
use crate::SineParams;

use super::ProverLayer;
use super::canon::{blank_key, canonical_clause};
use super::clause::{AtomId, ClauseKey, PClause, PLit, Term};
use super::AtomInfo;
use super::index::{EntryRef, LiteralIndex};
use super::oracle::{SemanticOracle, Witness};
use super::hash64::{Map64, Set64};
use super::schema::{SchemaHit, SchemaKind};
use super::strategy::Strategy;
use super::unify::{apply, apply_off, match_one_way, shift_slots, slot_atom, unify, unify_off, Subst};
use super::units::UnitStores;

// Clause lineage tiers (queue priority: conjecture line first).
pub(crate) const CONJECTURE: u8 = 0;
pub(crate) const SUPPORT: u8 = 1;
pub(crate) const BACKGROUND: u8 = 2;
// Search-shaping tunables (queue ratios, generation caps, forward
// closure, channel switches) live in [`Strategy`] — one serializable
// struct per portfolio lane, threaded in through `NativeOpts`.
/// Slot offset separating a unit pattern's variables from a target
/// literal's during one-way matching (target slots are shifted here;
/// the matcher never indexes them, so no substitution covers them).
const MATCH_TARGET_OFF: u64 = 4096;
/// Slot offset for partner units in the forward closure's join — kept
/// small because the join *unifies* (binds both sides), so the
/// substitution vector must span `JOIN_UNIT_OFF + 256`.  Premise slots
/// stay below 258 (canonical cap + shift), well under it.
const JOIN_UNIT_OFF: u64 = 512;

/// The native prover layer's single consolidated params struct — the shared
/// cross-backend inputs (SInE `selection`, `session`, wall-clock budget) folded
/// together with the native engine's own run tunables.  Implements
/// [`CommonProverOpts`] so the backend-agnostic [`ProvingLayer::prove`] loop
/// reads selection / timeout off it.  (The external layer's peer is
/// `ExternalOpts`.)
#[derive(Debug, Clone)]
pub struct NativeOpts {
    /// SInE axiom-selection seed (the autoscaling loop's base selection).
    pub selection: SineParams,
    /// Optional in-memory session whose assertions ride in as force-included
    /// hypotheses and seed SInE alongside the conjecture.
    pub session: Option<String>,
    pub max_steps: usize,
    pub max_lits: usize,
    /// Wall-clock budget in seconds (0 = unlimited).
    pub time_limit_secs: u64,
    /// Run the bounded forward closure before the main loop.
    pub forward_close: bool,
    /// Collect per-mechanism timing inside the saturation loop
    /// (re-simplify / factor / eq-resolve / paramodulate / resolve).
    /// Off by default — the timers cost a few clock reads per
    /// given-clause step.
    pub profile: bool,
    /// Render the refutation into `proof_kif` (KIF ASTs with original
    /// source formulas, skolem relabeling — the `--proof` experience).
    /// Off by default: the raw derivation DAG always exists in the
    /// prover arena and the status mapping reads it directly, so
    /// callers that never display a transcript skip the rendering
    /// entirely.  The arena drops when the call returns — this is
    /// render-now-or-never, not deferred rendering.
    pub want_proof: bool,
    /// Search-shaping knobs (queue, caps, channels, selection repair)
    /// — the portfolio axis.  See [`Strategy`].
    pub strategy: Strategy,
    /// Cooperative cancellation: when set, the saturation loop (and
    /// the forward closure) poll it each step and stop with
    /// `TimedOut` once it reads `true`.  A portfolio runner hands
    /// every lane the same flag and raises it when one lane returns a
    /// conclusive verdict.
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Interactive single-step: pause at each given-clause and each
    /// inference (`make`), printing a readable view and blocking on
    /// stdin.  Equivalent to setting `SIGMA_STEP`; intended for a single
    /// problem on one thread.  See the `stepdbg` module.
    pub step: bool,
}

impl Default for NativeOpts {
    fn default() -> Self {
        Self {
            selection: SineParams::default(), session: None,
            max_steps: 4000, max_lits: 8, time_limit_secs: 30,
            forward_close: true, profile: false, want_proof: false,
            strategy: Strategy::default(), cancel: None, step: false,
        }
    }
}

impl CommonProverOpts for NativeOpts {
    fn selection(&self) -> SineParams { self.selection }
    fn timeout(&self) -> u64 { self.time_limit_secs }
    fn set_timeout(&mut self, secs: u64) { self.time_limit_secs = secs; }
    fn set_session(&mut self, session: Option<String>) { self.session = session; }
    /// Standalone TPTP problem: swap in the complete-calculus,
    /// full-saturation strategy ([`Strategy::tptp`]) — set-of-support
    /// tiering can't prove axiom-case-split Theorems, and an incomplete
    /// calculus must not certify "no" on saturation.
    fn set_tptp_problem(&mut self) {
        self.strategy = Strategy::tptp();
    }
}

impl NativeOpts {
    /// `true` once the caller's cancellation flag is raised.
    #[inline]
    fn cancelled(&self) -> bool {
        self.cancel.as_ref()
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
    }
}

/// One clause in the prover's arena.
#[derive(Debug, Clone)]
pub(crate) struct ClauseRec {
    pub(crate) id: u32,
    /// Canonical literals (atom ids in the layer's `AtomTable`).
    pub(crate) lits: SmallVec<[PLit; 4]>,
    /// The same literals in slot-variable term form (lifted once).
    pub(crate) terms: Vec<(bool, Term)>,
    pub(crate) nvars: u32,
    pub(crate) key: ClauseKey,
    /// Parent clause ids (derivation DAG).
    pub(crate) parents: Vec<u32>,
    /// Witnessing stored facts (oracle discharges) — proof-step
    /// premises with file:line provenance.
    pub(crate) fact_parents: Vec<SentenceId>,
    /// The stored root this clause was clausified from, when it is an
    /// input (axiom/hypothesis) clause.
    pub(crate) source: Option<SentenceId>,
    pub(crate) rule: &'static str,
    pub(crate) tier: u8,
    pub(crate) weight: u64,
    pub(crate) activated: bool,
    /// Bit `i` set ⇔ literal `i` is maximal under the KBO literal
    /// ordering (the ordered-inference eligibility set).  All-ones when
    /// maximality is not needed (the unordered default), so consumers can
    /// AND against it unconditionally.
    pub(crate) max_mask: u64,
    /// Human-readable justifications (oracle discharges, unit refutations).
    pub(crate) notes: Vec<String>,
}

/// Verdict of the ground-equality decision procedure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EqDecision {
    /// Provably equal (closure / identical / equal numeric values).
    Entailed,
    /// Provably UNEQUAL (distinct numeric literal values only).
    Refuted,
    /// Neither provable — ordinary search decides.
    Unknown,
}

/// The given literal's decode-relevant facts, hoisted out of the
/// per-partner loop (see `decode_given_shape`).
struct DecodeShape {
    m: u32,
    open_slots: SmallVec<[(u8, u64); 2]>,
    base_residue: u64,
    s3: u64,
    arity: u8,
    g_nvars: u32,
    g_tier: u8,
}

/// Why `run` stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunVerdict {
    /// Refutation found: the id of the empty clause.
    Refutation(u32),
    /// Passive queue exhausted with no refutation.
    Saturated,
    /// Step budget exhausted.
    StepsExhausted,
    /// Wall-clock budget exhausted.
    TimedOut,
}

#[derive(Debug, Default)]
pub(crate) struct ProverStats {
    pub(crate) resolvents: u64,
    pub(crate) oracle_discharges: u64,
    pub(crate) oracle_subsumed: u64,
    pub(crate) unit_subsumed: u64,
    pub(crate) unit_simplified: u64,
    /// Subterm rewrites performed by forward demodulation.
    pub(crate) demod_rewrites: u64,
    /// New clauses dropped by forward (multi-literal) subsumption.
    pub(crate) subsumed: u64,
    pub(crate) discarded_deep: u64,
    pub(crate) discarded_long: u64,
    /// Some clause carried an equality literal — the "problem contains
    /// equality" signal for strict saturation verdicts.  Only tracked
    /// when `Strategy.strict_saturation` (sticky bit, one scan per make).
    pub(crate) saw_equality: bool,
    /// Superposition generation truncated by `para_cap` — inferences
    /// were never made, so a later saturation is not refutation-complete.
    pub(crate) gen_capped: u64,
    /// Maximal positive equality literals the superposition indexes had
    /// to skip because KBO could not orient them — the calculus only
    /// superposes FROM oriented equations, so each is a completeness
    /// loss strict saturation must know about.
    pub(crate) unorientable_eqs: u64,
    pub(crate) forward_closed: u64,
    /// Oriented equations produced by Phase-6 background completion.
    pub(crate) bg_completed: u64,
    /// Resolutions whose bindings were extracted algebraically from the
    /// power-sum residual (no unification walk).
    pub(crate) decoded_resolutions: u64,
    // -- candidate-verification profile (attempts vs successes per site,
    //    plus how many attempts had a ground candidate — the decode
    //    fast-path's entry condition).  Sized for ranking where the
    //    algebraic calculus could take more load.
    pub(crate) resolve_unify_attempts: u64,
    pub(crate) resolve_unify_hits: u64,
    pub(crate) resolve_ground_partner: u64,
    pub(crate) fc_unify_attempts: u64,
    pub(crate) fc_unify_hits: u64,
    pub(crate) fc_ground_candidate: u64,
    pub(crate) open_match_attempts: u64,
    pub(crate) open_match_hits: u64,
    /// Candidates refuted by THE KEY EQUATION before the match walk.
    pub(crate) open_match_prefiltered: u64,
    pub(crate) factor_attempts: u64,
    pub(crate) factor_hits: u64,
    /// Pairs refuted by per-seat coin comparison before unification.
    pub(crate) factor_prefiltered: u64,
    // -- saturation-loop mechanism timing (populated when opts.profile;
    //    one Instant pair per mechanism per given-clause step).
    pub(crate) t_resimplify: std::time::Duration,
    pub(crate) t_factors: std::time::Duration,
    pub(crate) t_eq_resolve: std::time::Duration,
    pub(crate) t_paramod: std::time::Duration,
    pub(crate) t_resolve: std::time::Duration,
    /// Empty clauses whose lineage never touches the negated
    /// conjecture: the INPUTS are contradictory (SUMO is, in places —
    /// e.g. Merge's species-inheritance axiom vs the Man/Woman
    /// partition).  Logged and skipped under the paraconsistent
    /// set-of-support discipline, never exploited.
    pub(crate) input_contradictions: u64,
    // -- schema channel (theory-rule shape recognition; see schema.rs).
    /// Probe hits (verified — not raw table matches).
    pub(crate) schema_hits: u64,
    /// Clauses absorbed outright (symmetry rules + the symmetry
    /// metaschema; their inferential role is fully replaced).
    pub(crate) schema_absorbed: u64,
    /// Ground symmetric-relation literals whose arguments were swapped
    /// into canonical order.
    pub(crate) sym_oriented: u64,
    /// Resolutions that succeeded through the symmetric argument-swap
    /// retry (`resolve_sym` steps).
    pub(crate) sym_resolutions: u64,
    pub(crate) mined_symmetric: u64,
    pub(crate) mined_transitive: u64,
    /// Antisymmetry / irreflexivity / inverse-pair sightings (registered
    /// for future consumers; no behavior change yet).
    pub(crate) mined_other: u64,

    // -- model-discharge path counters (SIGMA_STATS instrumentation only;
    //    zero behavior change — see discharge_models / discharge_model_joins
    //    / lit_pattern).  All zero unless SIGMA_MODEL is set.
    /// Conjecture atoms seen while scanning for goal patterns, summed across
    /// `discharge_models` + `discharge_model_joins`.
    pub(crate) model_atoms_seen: u64,
    /// Atoms rejected by `lit_pattern` (non-flat / no-args / non-`App` head)
    /// while scanning conjecture literals for goal patterns.
    pub(crate) model_atoms_rejected: u64,
    /// Goal argument positions collapsed to `DTerm::Var(0)` at the
    /// prover-to-model bridge because the argument is a compound term (not
    /// a bare `Term::Sym`).
    pub(crate) model_arg_collapsed_compound: u64,
    /// Goal argument positions collapsed to `DTerm::Var(0)` at the bridge
    /// because the same source variable appears in more than one argument
    /// position (repeated-variable collapse -- `DTerm::Var(0)` cannot
    /// distinguish them, so the join loses the co-reference constraint).
    pub(crate) model_arg_collapsed_repeated_var: u64,
    /// Conjecture atoms `discharge_models`/`discharge_model_joins` obtained
    /// at least one answer/witness for.
    pub(crate) model_atoms_answered: u64,
    /// Conjecture atoms that were dispatched to `ModelProgram::answer` but
    /// came back with no rows (or the call bailed) -- no witness found.
    pub(crate) model_atoms_unanswered: u64,
    /// `ModelProgram::answer` bail reasons, summed across both discharge
    /// passes (see `model::ModelStats`).
    pub(crate) model_unsafe_bails: u64,
    pub(crate) model_unstratifiable_bails: u64,
    /// Tuple-budget AND wall-clock-deadline overflows, combined -- the
    /// evaluator's `ModelError::Overflow` does not distinguish them (see
    /// `model/seminaive.rs`); splitting would need a second return channel
    /// through `evaluate_within`, not attempted here.
    pub(crate) model_budget_or_deadline_overflows: u64,
    pub(crate) model_undefined_relation: u64,

    // -- forward-demodulation duplicate-hit probe (Part 2; only active when
    //    Strategy.demod is on).
    /// Calls into `demodulate()` that were eligible to attempt a rewrite
    /// (demod on, at least one active unit equation) -- one per literal
    /// visited in `make`.
    pub(crate) demod_rewrite_attempts: u64,
    /// Of those, how many actually rewrote the literal (n >= 1 subterm
    /// rewrites applied) -- a clause-level count, NOT a subterm-rewrite
    /// count (that is `demod_rewrites` above).
    pub(crate) demod_rewrites_applied: u64,
    /// Of the clauses whose literals were rewritten by demod, how many
    /// ended up being exact duplicates of an already-known clause (probed
    /// via the same `ClauseKey`/`self.seen` dedup path `push()` uses).
    /// Measures the potential payoff of a rewrite-delta pre-probe.
    pub(crate) demod_dup_hits: u64,

    // -- proof-DAG discharge-rule reach (counted once per completed proof
    //    extraction, at refutation time).
    pub(crate) proof_tag_model: u64,
    pub(crate) proof_tag_model_join: u64,
    pub(crate) proof_tag_join: u64,
    pub(crate) proof_tag_event_calculus: u64,
    pub(crate) proof_tag_oracle: u64,
}

/// A frozen background problem base: everything `ask_native_once`
/// computes BEFORE support/conjecture loading, detached from the
/// layer borrow so it can live in `ProverLayer::bg_snapshots`.
/// Rehydration is a deep clone — a few ms against the ~60 ms+ of
/// pre-pass + clause-pipeline + indexing it replaces.
#[derive(Debug, Clone)]
pub(crate) struct ProverSnapshot {
    /// Roots whose pre-pass + clauses are IN THE ARENA (indexes may
    /// cover a subset after a narrowed rehydration — `retain_background`
    /// rebuilds them from the arena for any subset of these).
    pub(crate) loaded_roots: std::collections::HashSet<SentenceId>,
    clauses: Vec<ClauseRec>,
    seen: Set64<ClauseKey>,
    idx: LiteralIndex,
    units: UnitStores,
    support_seeds: Vec<(AtomId, u32)>,
    eq_terms: Map64<u64, Term>,
    lists_done: Set64<u64>,
    pending_list_units: Vec<Term>,
    has_compound_eqs: bool,
    antisym_mined: Map64<SymbolId, Option<SentenceId>>,
    irrefl_mined: Map64<SymbolId, Option<SentenceId>>,
    inverse_mined: Vec<(SymbolId, SymbolId, Option<SentenceId>)>,
    sym_swap_memo: Map64<AtomId, (u64, Option<AtomId>)>,
    seq: u64,
    tick: u64,
    oracle: super::oracle::OracleSnapshot,
}

pub(crate) struct NativeProver<'a> {
    layer: &'a ProverLayer,
    scope: Scope,
    pub(crate) oracle: SemanticOracle<'a>,
    pub(crate) opts: NativeOpts,
    /// Per-prover KBO when `Strategy.prec_seed != 0` (a permuted symbol
    /// precedence); `None` ⇒ use the shared, warm `layer.kbo` unchanged.
    prec: Option<super::kbo::KboOrdering>,
    pub(crate) clauses: Vec<ClauseRec>,
    seen: Set64<ClauseKey>,
    idx: LiteralIndex,
    /// Subterm-position index of active clauses' maximal literals — the
    /// superposition "into" targets (probe with an equation lhs).  Empty
    /// unless `Strategy.superposition`; rebuilt from the arena on snapshot
    /// hydrate / background masking (reconstructible, not frozen).
    term_idx: super::index::TermIndex,
    /// Active oriented maximal positive equality literals `(clause, lit)`
    /// with `s ≻ t` — the superposition equations (the "from" side).
    active_eqns: Vec<(u32, u8)>,
    units: UnitStores,
    /// Ground positive support units — the forward closure's seeds.
    support_seeds: Vec<(AtomId, u32)>,
    h_weight: BinaryHeap<Reverse<(u64, u64, u32)>>,
    h_age: BinaryHeap<Reverse<(u64, u32)>>,
    popped: Set64<u32>,
    /// Equality-class key → renderable Term for every constant the
    /// equality machinery has touched (symbols INCLUDING prover-local
    /// skolems, numeric literals).  Backs `normalize_eq`'s
    /// representative rewriting and the FD-equality unit builder — the
    /// store's symbol cache knows neither skolems nor numbers.
    eq_terms: Map64<u64, Term>,
    /// Suppressed input contradictions (empty clauses with no
    /// conjecture in their lineage), kept for transcript extraction —
    /// every entry is a complete, citable proof that the axioms /
    /// hypotheses contradict each other.  Capped; the count lives in
    /// `stats.input_contradictions`.
    pub(crate) input_contradiction_ids: Vec<u32>,
    /// Audit mode: suppress-and-collect EVERY empty clause and keep
    /// searching — the consistency-audit enumeration behavior (no
    /// single contradiction ends the run; saturation/timeout does).
    audit: bool,
    /// Collection cap for `input_contradiction_ids`.
    contradiction_cap: usize,
    /// Ground `(ListFn …)` terms whose theory units have been
    /// synthesized (keyed by content hash) — each ground list gets its
    /// extension exactly once.
    lists_done: Set64<u64>,
    /// Synthesized list-theory unit terms awaiting `make` (can't make
    /// from within make): `(inList mᵢ L)` and
    /// `(equal mᵢ (ListOrderFn L i))` per member.
    pending_list_units: Vec<Term>,
    /// Set when any equality class contains a COMPOUND term — gates
    /// the per-App key lookup in `normalize_eq` (hashing every ground
    /// subterm of every literal is wasted work until a compound
    /// equality exists).
    has_compound_eqs: bool,
    /// Whether conjecture clauses were loaded — when true, only
    /// conjecture-rooted refutations count (paraconsistent set of
    /// support); `check_consistency` runs without a conjecture and
    /// accepts any refutation.
    has_conjecture: bool,
    /// Reusable substitution buffer for the unify/match hot loops —
    /// `vec![None; n]` per attempt was ~45% of prover CPU (allocator
    /// traffic).  Each site clears exactly the slot range it used.
    scratch: Subst,
    /// Rule-mined antisymmetric / irreflexive relations and inverse
    /// pairs (schema channel) — recognized and recorded; runtime
    /// consumers land separately (antisymmetry → pending equalities,
    /// irreflexivity → disequalities, inverse → predicate collapse).
    antisym_mined: Map64<SymbolId, Option<SentenceId>>,
    irrefl_mined: Map64<SymbolId, Option<SentenceId>>,
    inverse_mined: Vec<(SymbolId, SymbolId, Option<SentenceId>)>,
    /// Roots loaded as BACKGROUND (snapshot coverage bookkeeping).
    bg_roots: std::collections::HashSet<SentenceId>,
    /// Memo for [`Self::symmetric_swapped_info`]: atom → swapped atom
    /// (`None` = not symmetric-swappable).  Entries for non-symmetric
    /// heads are epoch-guarded like the oracle's holds memo — a
    /// relation can BECOME symmetric mid-run.
    sym_swap_memo: Map64<AtomId, (u64, Option<AtomId>)>,
    /// OR of the conjecture atoms' leaf signatures — the goal profile
    /// every queued clause is scored against.  0 = no conjecture
    /// (consistency audits) ⇒ the distance factor is inert.
    conj_sig: u64,
    seq: u64,
    tick: u64,
    pub(crate) stats: ProverStats,
}

impl<'a> NativeProver<'a> {
    pub(crate) fn new(layer: &'a ProverLayer, scope: Scope, opts: NativeOpts) -> Self {
        let prec = (opts.strategy.prec_seed != 0)
            .then(|| super::kbo::KboOrdering::with_prec_seed(opts.strategy.prec_seed));
        Self {
            layer,
            scope,
            oracle: SemanticOracle::new(&layer.semantic, scope),
            opts,
            prec,
            clauses: Vec::new(),
            seen: Set64::default(),
            idx: LiteralIndex::default(),
            term_idx: super::index::TermIndex::default(),
            active_eqns: Vec::new(),
            units: UnitStores::default(),
            support_seeds: Vec::new(),
            h_weight: BinaryHeap::new(),
            h_age: BinaryHeap::new(),
            popped: Set64::default(),
            eq_terms: Map64::default(),
            input_contradiction_ids: Vec::new(),
            audit: false,
            contradiction_cap: 8,
            lists_done: Set64::default(),
            pending_list_units: Vec::new(),
            has_compound_eqs: false,
            has_conjecture: false,
            scratch: Vec::new(),
            antisym_mined: Map64::default(),
            irrefl_mined: Map64::default(),
            inverse_mined: Vec::new(),
            bg_roots: std::collections::HashSet::new(),
            sym_swap_memo: Map64::default(),
            conj_sig: 0,
            seq: 0,
            tick: 0,
            stats: ProverStats::default(),
        }
    }

    /// Capture the prover's owned state after BACKGROUND loading —
    /// clause arena, indexes, dedup set, oracle products of the input
    /// pre-pass — for reuse by later runs over an identical problem
    /// base (see `ProverLayer::bg_snapshots`).  Must be taken BEFORE
    /// support/conjecture loading: the queue is asserted empty (the
    /// background tier is pre-activated, never queued).
    pub(crate) fn freeze(&self) -> ProverSnapshot {
        debug_assert!(
            self.h_weight.is_empty() && self.h_age.is_empty(),
            "freeze must precede support/conjecture loading"
        );
        ProverSnapshot {
            loaded_roots: self.bg_roots.clone(),
            clauses: self.clauses.clone(),
            seen: self.seen.clone(),
            idx: self.idx.clone(),
            units: self.units.clone(),
            support_seeds: self.support_seeds.clone(),
            eq_terms: self.eq_terms.clone(),
            lists_done: self.lists_done.clone(),
            pending_list_units: self.pending_list_units.clone(),
            has_compound_eqs: self.has_compound_eqs,
            antisym_mined: self.antisym_mined.clone(),
            irrefl_mined: self.irrefl_mined.clone(),
            inverse_mined: self.inverse_mined.clone(),
            sym_swap_memo: self.sym_swap_memo.clone(),
            seq: self.seq,
            tick: self.tick,
            oracle: self.oracle.snapshot(),
        }
    }

    /// Rehydrate a prover from a frozen background: a fresh instance
    /// whose pre-pass + background loading already happened.  Per-run
    /// state (queues, stats, conjecture flags, goal profile) starts
    /// clean.
    pub(crate) fn from_snapshot(
        layer: &'a ProverLayer,
        scope: Scope,
        opts:  NativeOpts,
        snap:  &ProverSnapshot,
    ) -> Self {
        let mut p = Self::new(layer, scope, opts);
        p.oracle = SemanticOracle::from_snapshot(&layer.semantic, scope, &snap.oracle);
        p.bg_roots = snap.loaded_roots.clone();
        p.clauses = snap.clauses.clone();
        p.seen = snap.seen.clone();
        p.idx = snap.idx.clone();
        p.units = snap.units.clone();
        p.support_seeds = snap.support_seeds.clone();
        p.eq_terms = snap.eq_terms.clone();
        p.lists_done = snap.lists_done.clone();
        p.pending_list_units = snap.pending_list_units.clone();
        p.has_compound_eqs = snap.has_compound_eqs;
        p.antisym_mined = snap.antisym_mined.clone();
        p.irrefl_mined = snap.irrefl_mined.clone();
        p.inverse_mined = snap.inverse_mined.clone();
        p.sym_swap_memo = snap.sym_swap_memo.clone();
        p.seq = snap.seq;
        p.tick = snap.tick;
        // The superposition indexes aren't frozen (reconstructible from the
        // arena); rebuild them for the hydrated background.
        if p.opts.strategy.superposition {
            p.rebuild_superposition_index();
        }
        p
    }

    /// Re-derive the retrieval surfaces (literal index, unit stores,
    /// dedup set) for a SUBSET of the frozen background — the
    /// contraction half of cross-slice reuse.  Clauses from roots
    /// outside `keep` stay in the arena (ids are stable, so parent /
    /// proof references keep working) but vanish from every probe, so
    /// they can never be resolution partners — exactly a narrower
    /// slice's search space.  The ORACLE deliberately keeps the
    /// superset's theory (equalities / FD / closures contributed by
    /// masked axioms): every discharge still cites real KB axioms, so
    /// narrowing stays sound — it is a search heuristic, not a
    /// semantic restriction.  Synthesized theory clauses (subrel
    /// schema, `source == None`) are always kept.
    pub(crate) fn retain_background(&mut self, keep: &std::collections::HashSet<SentenceId>) {
        self.idx = LiteralIndex::default();
        self.units = UnitStores::default();
        self.seen = Set64::default();
        self.support_seeds.clear();
        let n = self.clauses.len() as u32;
        let layer = self.layer;
        let src = move |a| layer.atom_info(a);
        for id in 0..n {
            let (kept, key) = {
                let c = &self.clauses[id as usize];
                let kept = c.activated
                    && match c.source {
                        Some(sid) => keep.contains(&sid),
                        None => true,
                    };
                (kept, c.key)
            };
            if !kept {
                continue;
            }
            self.seen.insert(key);
            let lits = self.clauses[id as usize].lits.clone();
            for (i, l) in lits.iter().enumerate() {
                self.idx.add(EntryRef { clause: id, lit: i as u8 }, l.pos, l.atom, &src);
            }
            if lits.len() == 1 {
                let nv = self.clauses[id as usize].nvars;
                self.units.add_unit(
                    id, lits[0].pos, lits[0].atom, nv,
                    &layer.atom_infos, &layer.atoms, &layer.semantic.syntactic);
            }
        }
        if self.opts.strategy.superposition {
            self.rebuild_superposition_index();
        }
    }

    /// Test-only retrieval probe: is `(pos, atom)` an active ground
    /// unit?  (The mask tests need to observe the index surface.)
    #[cfg(test)]
    pub(crate) fn test_ground_unit(&self, pos: bool, atom: AtomId) -> bool {
        self.units.ground_unit(pos, atom).is_some()
    }

    fn syn(&self) -> &crate::syntactic::SyntacticLayer { &self.layer.semantic.syntactic }

    /// The reduction ordering for this run — the per-prover permuted KBO
    /// when `prec_seed != 0`, else the shared layer KBO (warm memo).
    #[inline]
    fn kbo(&self) -> &super::kbo::KboOrdering {
        self.prec.as_ref().unwrap_or(&self.layer.kbo)
    }

    /// The owning layer (proof extraction resolves atoms through it).
    pub(crate) fn layer(&self) -> &'a ProverLayer { self.layer }

    /// Borrow the reusable substitution buffer, sized to at least `n`
    /// all-`None` slots.  Return it with [`Self::put_scratch`], which
    /// re-establishes the all-`None` invariant for the used range.
    /// (take/put rather than `&mut` so the buffer doesn't pin `self`
    /// across `make`/index calls.)
    fn take_scratch(&mut self, n: usize) -> Subst {
        let mut s = std::mem::take(&mut self.scratch);
        if s.len() < n { s.resize(n, None); }
        s
    }

    fn put_scratch(&mut self, mut s: Subst, used: usize) {
        let end = used.min(s.len());
        for slot in &mut s[..end] { *slot = None; }
        self.scratch = s;
    }

    /// Human-readable discharge notes are RENDERING payload (proof /
    /// contradiction transcripts).  Building them means `term_kif` /
    /// `witnesses_kif` String construction per discharge — tens of
    /// thousands per hard run — so they exist only when a transcript
    /// can surface them.  The proof DAG itself (parents/fact_parents)
    /// is always recorded.
    fn want_notes(&self) -> bool {
        self.opts.want_proof || self.audit
    }

    /// Rewrite every ground constant in `t` to its equality-class
    /// representative, IN PLACE — touched nodes are replaced, untouched
    /// subtrees are never rebuilt, and the no-equalities case (most
    /// runs, most literals) is a single branch.
    fn normalize_eq(&self, t: &mut Term) {
        if !self.oracle.has_equalities() {
            return;
        }
        self.normalize_eq_rec(t);
    }

    fn normalize_eq_rec(&self, t: &mut Term) {
        match t {
            Term::Sym(_) | Term::Lit(Literal::Number(_)) => {
                let Some(key) = eq_key(t) else { return };
                let rep = self.oracle.eq_rep(key);
                if rep == key {
                    return;
                }
                if let Some(r) = self.eq_terms.get(&rep) {
                    *t = r.clone();
                } else if let Some(sym) = self.syn().sym_name(rep) {
                    *t = Term::Sym(sym);
                }
            }
            Term::App(elems) => {
                for e in elems.iter_mut() {
                    self.normalize_eq_rec(e);
                }
                if self.has_compound_eqs
                    && t.is_ground()
                    && term_size(t) <= self.opts.strategy.max_term_size
                {
                    let key = self.layer.atoms.intern_atom(t);
                    let rep = self.oracle.eq_rep(key);
                    if rep != key {
                        if let Some(r) = self.eq_terms.get(&rep) {
                            *t = r.clone();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Forward demodulation: rewrite `t` to KBO normal form using the
    /// active unit equations, oriented by the reduction ordering.  For a
    /// unit `(equal l r)` we rewrite a subterm matching `l` to `r` ONLY
    /// when `l >_kbo r` — so every rewrite is strictly downhill in a
    /// well-founded order (it terminates) and sound (equals for equals).
    /// Unlike `paramodulants` (which UNIFIES and keeps the parent), this
    /// MATCHES one-way (binds the rule's variables only) and the
    /// rewritten clause replaces the original — a simplification.
    /// Demodulator clause ids are pushed to `used` for the proof DAG.
    fn demodulate(&self, t: &mut Term, used: &mut Vec<u32>) -> u64 {
        if !self.opts.strategy.demod || self.units.equals.is_empty() {
            return 0;
        }
        // Cap total rewrites per term — a guard, not the terminator
        // (KBO already guarantees termination); bounds pathological
        // fan-out on huge clauses.  Parameterized (default 64).
        let demod_cap = self.opts.strategy.demod_cap.max(1);
        let mut rewrites = 0u64;
        'fixpoint: loop {
            if rewrites >= demod_cap {
                break;
            }
            // Shift the demodulator's slots clear of the target's, so
            // one-way matching never confuses a rule variable with a
            // target variable (mirrors `paramodulants`' offset trick).
            let off = max_slot(t).map_or(0, |m| m + 1);
            for (cid, l, r) in self.units.equals.iter() {
                // A bare-variable left side rewrites everything — never a
                // sound demodulator; skip (it is also never KBO-greater).
                if matches!(l, Term::Var(_)) {
                    continue;
                }
                if !self.demod_oriented(l, r) {
                    continue;
                }
                let l2 = shift_slots(l, off);
                let r2 = shift_slots(r, off);
                let nslots = max_slot(&l2).unwrap_or(off) + 1;
                for (path, sub) in positions(t) {
                    let mut s: Subst = vec![None; nslots as usize];
                    if match_one_way(&l2, &sub, &mut s) {
                        let rr = apply(&r2, &s);
                        *t = replace(t, &path, &rr);
                        used.push(*cid);
                        rewrites += 1;
                        // The term changed; restart the scan from the top
                        // (a rewrite can expose new redexes / new `off`).
                        continue 'fixpoint;
                    }
                }
            }
            break;
        }
        rewrites
    }

    /// Whether `(equal l r)` is a sound left-to-right demodulator: `l`
    /// strictly greater than `r` in the layer's KBO.  Stable under
    /// substitution, so the single check licenses rewriting every
    /// matched instance.  Both sides intern (content-addressed, cheap)
    /// and the comparison is memoized.
    fn demod_oriented(&self, l: &Term, r: &Term) -> bool {
        let la = self.layer.atoms.intern_atom(l);
        let ra = self.layer.atoms.intern_atom(r);
        matches!(
            self.kbo().compare(la, ra, &self.layer.atoms, self.syn()),
            super::kbo::KboCmp::Greater
        )
    }

    /// Synthesize the concrete subrelation rule `(=> (R ?x ?y) (S ?x ?y))`
    /// for every `(subrelation R S)` ground fact in `clauses`, adding it
    /// as an activated BACKGROUND clause.  This is the first-order
    /// instantiation of SUMO's second-order subrelation schema for the
    /// relations actually present: it lets resolution chain subrelation
    /// inheritance directly (binding open `(part ?N ?A)` literals from
    /// `(component ?N ?A)` facts) instead of branching through the
    /// predicate-variable seat-0 bucket, which otherwise explodes.
    pub(crate) fn synthesize_subrelation_rules(&mut self, clauses: &[PClause]) {
        let subrel = self.oracle.roles().subrelation;
        let mut pairs: Vec<(Symbol, Symbol)> = Vec::new();
        for c in clauses {
            if c.lits.len() != 1 || !c.lits[0].pos {
                continue;
            }
            let Some(sent) = self.layer.atoms.resolve(c.lits[0].atom, self.syn()) else { continue };
            if sent.elements.len() != 3 {
                continue;
            }
            match (sent.elements.first(), &sent.elements[1], &sent.elements[2]) {
                (Some(Element::Symbol(h)), Element::Symbol(r), Element::Symbol(s))
                    if h.id() == subrel && r.id() != s.id() =>
                {
                    pairs.push((r.0.clone(), s.0.clone()));
                }
                _ => {}
            }
        }
        for (r, s) in pairs {
            if std::env::var_os("SIGMA_ORACLE_TRACE").is_some() {
                eprintln!("SUBREL-SCHEMA {} -> {}", r.name(), s.name());
            }
            let body = Term::App(vec![Term::Sym(r), Term::Var(0), Term::Var(1)]);
            let head = Term::App(vec![Term::Sym(s), Term::Var(0), Term::Var(1)]);
            if let Some(id) =
                self.make(vec![(false, body), (true, head)], vec![], "subrel_schema", BACKGROUND, None, false)
            {
                let key = self.clauses[id as usize].key;
                if self.clauses[id as usize].lits.len() <= self.opts.max_lits
                    && self.seen.insert(key)
                {
                    self.activate(id);
                }
            }
        }
    }

    /// Event-oracle (fix B): discharge multi-premise Horn rules by an
    /// indexed nested-loop JOIN over ground facts, emitting only the
    /// satisfied head unit.  Theory body literals (taxonomy / temporal)
    /// are decided through the oracle rather than resolved against the
    /// generative axioms that produce their facts — so a rule body over
    /// high-frequency relations (`instance`/`agent`/`temporalPart`)
    /// becomes a bounded ground join instead of a saturating cascade.
    ///
    /// Only "conclusion" rules run: the head relation must have no ground
    /// facts of its own and not be a theory relation — this selects
    /// derived-only heads (`breaksLaw`, `goesToJail`) and excludes SUMO's
    /// generative rules (whose heads are `instance`/attributes/…).  A
    /// bounded fixpoint feeds each emitted head back as a fact so chained
    /// rules (`breaksLaw ⇒ goesToJail`) fire on later rounds.
    ///
    /// Gated by `SIGMA_RULE_JOIN`; a no-op when unset (default off, so the
    /// saturation baseline is byte-identical).
    pub(crate) fn discharge_horn_joins(&mut self) {
        if std::env::var_os("SIGMA_RULE_JOIN").is_none() {
            return;
        }
        let roles = self.oracle.roles();
        let tids = super::temporal::TemporalRelIds::standard();
        let trace = std::env::var_os("SIGMA_ORACLE_TRACE").is_some();

        // Conclusion rules from the clause set: one positive head + ≥1
        // negative body literal, all symbol-headed, the head a non-theory
        // relation with NO ground facts of its own (so SUMO's generative
        // rules — heads over `instance`/attributes/… — are excluded).
        // Ground facts come from the STORE (the whole KB), not the
        // SInE-selected clause set — the join is a semantic discharge and
        // must see facts the search heuristic dropped.
        struct JoinRule {
            /// The Horn-rule clause id — a proof-DAG parent of every head
            /// the rule discharges (renders as "by axiom …").
            id:   u32,
            body: Vec<(SymbolId, Vec<Term>)>,
            head: Term,
        }
        // A conjunctive-query goal: the all-negative negated conjecture
        // `¬R1 ∨ … ∨ ¬Rn` of `∃X⃗.(R1 ∧ … ∧ Rn)`.  `lits` are the (positive)
        // atom terms; a binding satisfying all of them against ground facts
        // makes every Ri true, and emitting those ground atoms collapses
        // the clause to empty (the query is answered).
        struct JoinQuery {
            lits: Vec<Term>,
        }
        let mut rules: Vec<JoinRule> = Vec::new();
        let mut queries: Vec<JoinQuery> = Vec::new();
        let mut needed: HashSet<SymbolId> = HashSet::new();
        for c in &self.clauses {
            let mut head: Option<&Term> = None;
            let mut two_pos = false;
            for (pos, t) in &c.terms {
                if *pos {
                    if head.is_some() {
                        two_pos = true;
                        break;
                    }
                    head = Some(t);
                }
            }
            if two_pos {
                continue;
            }
            let Some(head) = head else { continue };
            if !c.terms.iter().any(|(p, _)| !*p) {
                continue; // no body
            }
            let rule_id = c.id;
            let Some((head_rel, _)) = lit_pattern(head) else { continue };
            if is_theory_rel(head_rel, &roles, &tids) {
                continue;
            }
            if !self.syn().by_head_id(&head_rel).is_empty() {
                continue; // head relation has asserted facts ⇒ generative, skip
            }
            let mut body: Vec<(SymbolId, Vec<Term>)> = Vec::new();
            let mut ok = true;
            for (p, t) in &c.terms {
                if *p {
                    continue;
                }
                match lit_pattern(t) {
                    Some((r, a)) => {
                        if !is_theory_rel(r, &roles, &tids) {
                            needed.insert(r);
                        }
                        body.push((r, a));
                    }
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                if trace {
                    eprintln!(
                        "RULE-JOIN rule head={} ({} body lits)",
                        term_kif(head, self.syn()),
                        body.len(),
                    );
                }
                rules.push(JoinRule { id: rule_id, body, head: head.clone() });
            }
        }

        // Conjunctive-query goals: an all-negative conjecture clause is the
        // negated `∃X⃗.(R1∧…∧Rn)` — discharge it as a join over ground facts.
        for c in &self.clauses {
            if c.tier != CONJECTURE || c.terms.is_empty() {
                continue;
            }
            if c.terms.iter().any(|(p, _)| *p) {
                continue; // all-negative only (a pure query, no head)
            }
            let mut lits: Vec<Term> = Vec::with_capacity(c.terms.len());
            let mut ok = true;
            for (_p, t) in &c.terms {
                match lit_pattern(t) {
                    Some((r, _)) => {
                        if !is_theory_rel(r, &roles, &tids) {
                            needed.insert(r);
                        }
                        lits.push(t.clone());
                    }
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok && !lits.is_empty() {
                if trace {
                    let desc: Vec<String> = lits.iter().filter_map(|t| {
                        lit_pattern(t).map(|(r, a)| format!(
                            "{}/{}{}",
                            term_kif(t, self.syn()).split_whitespace().next().unwrap_or("?")
                                .trim_start_matches('('),
                            a.len(),
                            if is_theory_rel(r, &roles, &tids) { "[theory]" }
                            else if self.syn().by_head_id(&r).is_empty() { "[nofacts]" }
                            else { "[facts]" },
                        ))
                    }).collect();
                    eprintln!("RULE-JOIN query [{}]", desc.join(", "));
                }
                queries.push(JoinQuery { lits });
            }
        }

        if rules.is_empty() && queries.is_empty() {
            return;
        }

        // Pull ground facts for every non-theory body relation directly
        // from the store (the join's generators).
        let mut facts: HashMap<SymbolId, Vec<JoinFact>> = HashMap::new();
        for rel in needed {
            let f = self.store_facts(rel);
            if !f.is_empty() {
                facts.insert(rel, f);
            }
        }
        // A "genuine" query is ground-answerable: every conjunct is a
        // theory relation (oracle-decided) or has ground facts.  When one
        // is present the problem is a database-style QA query, and the
        // conclusion-rule pass is irrelevant noise that floods the search
        // — suppress it.  (A Horn-chain goal like `¬goesToJail` is also an
        // all-negative clause, but its relation has NO facts, so it is NOT
        // genuine and rule mode stays on — keeping the jail proof intact.)
        let suppress_rules = queries.iter().any(|q| {
            q.lits.iter().all(|lit| match lit_pattern(lit) {
                Some((r, _)) => is_theory_rel(r, &roles, &tids) || facts.contains_key(&r),
                None => false,
            })
        });
        if trace {
            eprintln!(
                "RULE-JOIN scan: {} generator relations, {} ground facts, {} conclusion rules, \
                 {} queries, suppress_rules={}",
                facts.len(),
                facts.values().map(Vec::len).sum::<usize>(),
                rules.len(),
                queries.len(),
                suppress_rules,
            );
        }

        // Bounded fixpoint: emit satisfied heads, feed them back as facts
        // so chained conclusion rules fire on the next round.
        let mut emitted: HashSet<AtomId> = HashSet::new();
        let mut budget = 4096usize;
        for _round in 0..64 {
            // Rebuild the seat index from the current facts (rule mode may
            // have fed emitted heads back as facts last round).
            let seat_idx = build_seat_index(&facts);
            // (head, fact_parent sids, clause-parent ids) for each
            // satisfied head this round — collected with only `&self`
            // before the mutating emit pass below.
            let mut produced: Vec<(Term, Vec<SentenceId>, Vec<u32>)> = Vec::new();
            for r in rules.iter().take(if suppress_rules { 0 } else { rules.len() }) {
                let mut sols: Vec<HashMap<SymbolId, Term>> = Vec::new();
                self.join_rec(
                    &r.body,
                    &(0..r.body.len()).collect::<Vec<_>>(),
                    &HashMap::new(),
                    &facts,
                    &seat_idx,
                    &roles,
                    &tids,
                    &mut sols,
                    &mut budget,
                );
                for sol in sols {
                    let h = subst(&r.head, &sol);
                    if !h.is_ground() {
                        continue;
                    }
                    let (fact_sids, mut cparents) = self.collect_provenance(&r.body, &sol, &facts);
                    cparents.insert(0, r.id); // the rule itself
                    produced.push((h, fact_sids, cparents));
                }
            }
            // Conjunctive-query goals: one satisfying binding answers the
            // query — emit the ground instance of every conjunct as a
            // positive unit, which resolves against the all-negative goal
            // clause to the empty clause.
            for q in &queries {
                let body: Vec<(SymbolId, Vec<Term>)> =
                    q.lits.iter().filter_map(lit_pattern).collect();
                if body.len() != q.lits.len() {
                    continue;
                }
                let mut sols: Vec<HashMap<SymbolId, Term>> = Vec::new();
                self.join_rec(
                    &body,
                    &(0..body.len()).collect::<Vec<_>>(),
                    &HashMap::new(),
                    &facts,
                    &seat_idx,
                    &roles,
                    &tids,
                    &mut sols,
                    &mut budget,
                );
                if let Some(sol) = sols.first() {
                    let (fact_sids, _) = self.collect_provenance(&body, sol, &facts);
                    for lit in &q.lits {
                        let g = subst(lit, sol);
                        if g.is_ground() {
                            // Resolution against the negated goal supplies
                            // the conjecture lineage; no clause parent here.
                            produced.push((g, fact_sids.clone(), Vec::new()));
                        }
                    }
                }
            }
            let mut progress = false;
            for (h, fact_sids, cparents) in produced {
                let aid = self.layer.atoms.intern_atom(&h);
                if !emitted.insert(aid) {
                    continue;
                }
                if trace {
                    eprintln!("RULE-JOIN emit {}", term_kif(&h, self.syn()));
                }
                let head_for_fact = lit_pattern(&h);
                if let Some(id) =
                    self.make(vec![(true, h)], cparents, "rule_join", SUPPORT, None, true)
                {
                    self.clauses[id as usize].fact_parents.extend(fact_sids);
                    let key = self.clauses[id as usize].key;
                    if self.seen.insert(key) {
                        if let Some((rel, args)) = head_for_fact {
                            facts.entry(rel).or_default().push(JoinFact {
                                args,
                                src: FactSrc::Emitted(id),
                            });
                        }
                        self.activate(id);
                        self.push(Some(id));
                        progress = true;
                    }
                }
            }
            if !progress {
                break;
            }
        }
    }

    /// Discrete Event Calculus discharge (gated `SIGMA_EC`; a no-op when
    /// unset, and a no-op on any KB without a DEC narrative — so SUMO and
    /// every non-EC corpus are unaffected).
    ///
    /// The CSR event-calculus problems load the standard DEC frame axioms
    /// (DEC1–DEC12) plus a per-problem narrative defining
    /// `happens`/`initiates`/`terminates` by `<=>` enumeration.  Ordinary
    /// resolution explodes on the `~∃Event` inertia conditions, so instead we
    /// read the narrative into effect tables, forward-simulate the complete
    /// fluent state over the ground timeline, and emit each `(fluent, time)`
    /// as a ground `holdsAt` / `~holdsAt` unit.  Those units resolve directly
    /// against the (negated) conjecture — a decision procedure standing in for
    /// the frame-axiom search.  Complete-state (DEC7 negative inertia) means
    /// negative `holdsAt` queries are decided too.
    pub(crate) fn discharge_event_calculus(&mut self) {
        if std::env::var_os("SIGMA_EC").is_none() {
            return;
        }
        let trace = std::env::var_os("SIGMA_ORACLE_TRACE").is_some();
        let Some((nar, names)) = super::eventcalc::parse_narrative(self.syn()) else {
            return;
        };
        let holds_at = Symbol::from("holdsAt");
        // The complete fluent state. Default path: the bespoke forward
        // simulator. `SIGMA_EC_MODEL` routes the SAME narrative through the
        // generic Datalog(¬) model kernel instead (`narrative_to_program` →
        // perfect model), to validate end-to-end that the generic engine
        // solves what the bespoke oracle solves. Emission below is shared, so
        // any difference is purely in how the state is computed.
        let state: HashMap<(SymbolId, SymbolId), bool> =
            if std::env::var_os("SIGMA_EC_MODEL").is_some() {
                let prog = super::model::narrative_to_program(&nar);
                let Ok(model) = prog.evaluate() else {
                    if trace { eprintln!("EC[model]: program not stratified/safe — bailing"); }
                    return;
                };
                let rel = model.get(&holds_at.id()).cloned().unwrap_or_default();
                // Reconstruct complete state over the fluent×time grid
                // (closed-world: a cell absent from the relation is false).
                let fluents: HashSet<SymbolId> = nar.initiates.iter()
                    .chain(nar.terminates.iter())
                    .map(|e| e.fluent)
                    .chain(nar.initial.keys().copied())
                    .collect();
                let mut st = HashMap::new();
                for &f in &fluents {
                    for &t in &nar.times {
                        st.insert((f, t), rel.contains(&vec![f, t]));
                    }
                }
                if trace { eprintln!("EC[model]: kernel perfect model, {} state cells", st.len()); }
                st
            } else {
                super::eventcalc::simulate(&nar)
            };
        if trace {
            eprintln!(
                "EC: {} times, {} initiates, {} terminates, {} state cells",
                nar.times.len(), nar.initiates.len(), nar.terminates.len(), state.len(),
            );
        }
        // Emit each simulated state cell as a ground `holdsAt` / `~holdsAt`
        // unit.  Each is BOTH queued for selection (`push`) and indexed as a
        // resolution / unit-simplification partner (`activate`) — so the
        // complementary conjecture literal is discharged whichever clause the
        // given-clause loop reaches first.
        let mut pushed = 0usize;
        for (&(fluent, time), &holds) in &state {
            let (Some(fl), Some(t)) = (names.get(&fluent), names.get(&time)) else {
                continue;
            };
            let atom = Term::App(vec![
                Term::Sym(holds_at.clone()),
                Term::Sym(fl.clone()),
                Term::Sym(t.clone()),
            ]);
            if let Some(id) =
                self.make(vec![(holds, atom)], Vec::new(), "event_calculus", SUPPORT, None, true)
            {
                if self.push(Some(id)).is_some() {
                    pushed += 1;
                }
                self.activate(id);
            }
        }
        if trace {
            eprintln!("EC: {} state cells → {} pushed/activated", state.len(), pushed);
        }
    }

    /// Generic inductive-definition model discharge (Phase 5, slice 2; gated
    /// `SIGMA_MODEL`, default-off — runs ALONGSIDE the bespoke oracles for
    /// the parity diff).  Consults the KB-lifetime model registry: evaluates
    /// the **monotone** (negation-free) fragment — a sound positive model for
    /// every predicate — and emits the entailed ground facts that match the
    /// conjecture's atoms, which resolve against the (negated) goal.
    ///
    /// Positive-only here (monotone is a sound under-approximation); negative /
    /// complete decisions from stratifiable clusters are a later slice.  No-op
    /// when the conjecture's relations aren't defined in the program — so SUMO
    /// non-taxonomy queries pay only a cheap miss.
    pub(crate) fn discharge_models(&mut self) {
        if std::env::var_os("SIGMA_MODEL").is_none() {
            return;
        }
        let trace = std::env::var_os("SIGMA_ORACLE_TRACE").is_some();
        let mp = self.layer.model_program();

        // The conjecture's atom patterns (relation + argument terms).  Read
        // from `lits` (slot-form `terms` can be empty for already-simplified
        // clauses); resolve each atom to a term.
        let mut patterns: Vec<(SymbolId, Vec<Term>)> = Vec::new();
        for c in &self.clauses {
            if c.tier != CONJECTURE {
                continue;
            }
            for l in &c.lits {
                if let Some(t) = slot_atom(&self.layer.atoms, self.syn(), l.atom, 0) {
                    self.stats.model_atoms_seen += 1;
                    match lit_pattern(&t) {
                        Some(p) => patterns.push(p),
                        None => self.stats.model_atoms_rejected += 1,
                    }
                }
            }
        }
        if patterns.is_empty() {
            return;
        }
        let goal_preds: HashSet<SymbolId> = patterns.iter().map(|(r, _)| *r).collect();
        // Cheap skip: does the program define/store any goal relation?
        let defines = mp.monotone.rules.iter().any(|r| goal_preds.contains(&r.head.pred))
            || mp.monotone.edb.keys().any(|p| goal_preds.contains(p));
        if !defines {
            return;
        }
        if trace {
            let prog_facts: usize = mp.monotone.edb.values().map(|s| s.len()).sum();
            eprintln!("MODEL: program {} monotone rules, {prog_facts} edb facts; {} goal atoms",
                mp.monotone.rules.len(), patterns.len());
        }
        // Per conjecture atom: demand-scope (dependency cone) + magic-set
        // rewrite on the atom's CONSTANTS (slice 4b), evaluate the demanded
        // slice, and collect the entailed answers.  This keeps a dense relation
        // (OpenCyc `genls`) affordable — only the facts reachable from the
        // conjecture's constants are derived.
        // Hard wall-clock cap on model materialization across all goal atoms,
        // so a slow/zero-value model build (e.g. a dense OpenCyc cone that
        // emits nothing) can never eat the prover's time budget — it bails and
        // resolution proceeds.
        let deadline = Instant::now() + std::time::Duration::from_millis(800);
        let mut to_emit: Vec<(SymbolId, Vec<SymbolId>)> = Vec::new();
        let mut model_stats = super::model::ModelStats::default();
        for (rel, args) in &patterns {
            let dargs = self.bridge_dargs(args);
            let answered = mp.answer_stats(*rel, &dargs, Some(deadline), &mut model_stats);
            if let Some(rows) = answered {
                self.stats.model_atoms_answered += 1;
                for row in rows {
                    to_emit.push((*rel, row));
                }
            } else {
                self.stats.model_atoms_unanswered += 1;
            }
        }
        self.merge_model_stats(&model_stats);

        let mut emitted = 0usize;
        for (rel, row) in to_emit {
            let Some(relname) = self.syn().sym_name(rel) else { continue };
            let mut elems = vec![Term::Sym(relname)];
            let mut ok = true;
            for v in &row {
                match self.syn().sym_name(*v) {
                    Some(s) => elems.push(Term::Sym(s)),
                    None => { ok = false; break; }
                }
            }
            if !ok {
                continue;
            }
            if let Some(id) =
                self.make(vec![(true, Term::App(elems))], Vec::new(), "model", SUPPORT, None, true)
            {
                if self.push(Some(id)).is_some() {
                    emitted += 1;
                }
                self.activate(id);
            }
        }
        if trace {
            eprintln!("MODEL: {emitted} positive units emitted over {} goal relations", goal_preds.len());
        }
    }

    /// Bridge one conjecture atom's argument terms to model-side
    /// [`DTerm`](super::model::DTerm)s: bare symbols become constants,
    /// everything else collapses to the wildcard `DTerm::Var(0)`.  The
    /// collapse LOSES a constraint when the argument is a compound term
    /// or a variable that co-occurs in another position (the join can no
    /// longer enforce the co-reference) — both are counted into the
    /// `model_arg_collapsed_*` stats.
    fn bridge_dargs(&mut self, args: &[Term]) -> Vec<super::model::DTerm> {
        args.iter()
            .map(|t| match t {
                Term::Sym(s) => super::model::DTerm::Const(s.id()),
                Term::Var(v) => {
                    let repeats = args.iter()
                        .filter(|o| matches!(o, Term::Var(ov) if ov == v))
                        .count();
                    if repeats > 1 {
                        self.stats.model_arg_collapsed_repeated_var += 1;
                    }
                    super::model::DTerm::Var(0)
                }
                _ => {
                    self.stats.model_arg_collapsed_compound += 1;
                    super::model::DTerm::Var(0)
                }
            })
            .collect()
    }

    /// Fold one discharge pass's [`ModelStats`](super::model::ModelStats)
    /// bail-reason breakdown into the prover's per-run counters (the
    /// `answered` count is tracked per-atom by the caller instead).
    fn merge_model_stats(&mut self, ms: &super::model::ModelStats) {
        self.stats.model_unsafe_bails += u64::from(ms.unsafe_bails);
        self.stats.model_unstratifiable_bails += u64::from(ms.unstratifiable_bails);
        self.stats.model_budget_or_deadline_overflows += u64::from(ms.budget_overflows);
        self.stats.model_undefined_relation += u64::from(ms.undefined_relation);
    }

    /// Conjunctive-query goal discharge over the inductive model (gated
    /// `SIGMA_MODEL`).  The per-atom [`discharge_models`] emits each
    /// conjecture atom's model answers as *isolated* units and leaves the
    /// cross-atom JOIN to resolution — which explodes on the large
    /// existential conjunctive queries of the CSR QA family (`∃X⃗.(R1∧…∧Rn)`
    /// with 8–10 shared variables): saturation has to reconstruct the join by
    /// hand.  This pass instead evaluates the whole conjunction as one indexed
    /// join ([`join_rec`]) over `store ∪ model-derived` facts, and on the
    /// first satisfying binding emits the ground conjuncts — collapsing the
    /// all-negative goal clause to empty without the resolution blow-up.
    ///
    /// Sound: each emitted unit is a ground instance entailed by the
    /// (monotone, under-approximating) model or the store; the binding is a
    /// real witness for the existential.  A no-op unless the conjecture is an
    /// all-negative conjunction of ≥2 model-/store-defined relations, so
    /// non-QA queries pay only a cheap miss.  Runs AFTER `discharge_models`,
    /// so the bespoke per-atom path (which already closes e.g. CSR116+5) is
    /// untouched; this only adds closures it was missing.
    pub(crate) fn discharge_model_joins(&mut self) {
        if std::env::var_os("SIGMA_MODEL").is_none() {
            return;
        }
        let trace = std::env::var_os("SIGMA_ORACLE_TRACE").is_some();
        let roles = self.oracle.roles();
        let tids = super::temporal::TemporalRelIds::standard();
        let mp = self.layer.model_program();

        // 1) Conjunctive-query goals: all-negative conjecture clauses with ≥2
        //    literals.  Read atom terms from `lits` (slot-form `terms` can be
        //    empty for already-simplified clauses — the same reason
        //    discharge_models reads lits, and why the store-only RULE_JOIN
        //    misses these conjectures entirely).
        let mut queries: Vec<Vec<Term>> = Vec::new();
        let mut needed: HashSet<SymbolId> = HashSet::new();
        for c in &self.clauses {
            if c.tier != CONJECTURE || c.lits.len() < 2 {
                continue;
            }
            if c.lits.iter().any(|l| l.pos) {
                continue; // a pure query is all-negative (no positive head)
            }
            let mut lits: Vec<Term> = Vec::with_capacity(c.lits.len());
            let mut ok = true;
            for l in &c.lits {
                match slot_atom(&self.layer.atoms, self.syn(), l.atom, 0) {
                    Some(t) if lit_pattern(&t).is_some() => lits.push(t),
                    _ => { ok = false; break; }
                }
            }
            if ok && lits.len() >= 2 {
                for t in &lits {
                    if let Some((r, _)) = lit_pattern(t) {
                        needed.insert(r);
                    }
                }
                queries.push(lits);
            }
        }
        if queries.is_empty() {
            return;
        }

        // 2) Generator facts per body relation: store atoms PLUS model-derived
        //    tuples.  The join's variables connect conjuncts, so a fact
        //    demanded for one conjunct (e.g. the derived `subr(_, rprs_0)`
        //    closure) becomes reachable through another conjunct's binding.
        //    Two materialization strategies, in order of cost:
        //      a) the FULL positive model (IDB closure + transitivity) — exact,
        //         but bails on a dense KB (e.g. a big transitive `sub`);
        //      b) per-atom demand-scoped `mp.answer`, magic-set-seeded on each
        //         conjunct's *constants* — bounded even when (a) blows up, and
        //         it is what materializes a constant-seeded IDB slice like
        //         `subr(_, rprs_0)`.
        //    We union both: (a) when it fits, (b) always (cheap, demand-scoped).
        //    Theory relations are oracle-decided, never enumerated.
        let deadline = Instant::now() + std::time::Duration::from_millis(1500);
        const MAX_FACTS_PER_REL: usize = 50_000;
        let full_model = mp.positive_model();
        let mut facts: HashMap<SymbolId, Vec<JoinFact>> = HashMap::new();
        for &rel in &needed {
            if is_theory_rel(rel, &roles, &tids) {
                continue;
            }
            let mut f = self.store_facts(rel);
            let mut push_row = |f: &mut Vec<JoinFact>, row: &[SymbolId]| {
                if f.len() >= MAX_FACTS_PER_REL {
                    return;
                }
                let aargs: Vec<Term> = row
                    .iter()
                    .filter_map(|v| self.syn().sym_name(*v).map(Term::Sym))
                    .collect();
                if aargs.len() == row.len() && !f.iter().any(|jf| jf.args == aargs) {
                    f.push(JoinFact { args: aargs, src: FactSrc::Model });
                }
            };
            // (a) full model, when it materialized.
            if let Some(model) = full_model.as_ref().and_then(|m| m.get(&rel)) {
                for row in model {
                    push_row(&mut f, row);
                }
            }
            // (b) per-atom demand-scoped answers, seeded on the conjuncts'
            //     constants — derives constant-bound IDB slices the full model
            //     bailed on.
            for lits in &queries {
                for t in lits {
                    let Some((r, args)) = lit_pattern(t) else { continue };
                    if r != rel {
                        continue;
                    }
                    let dargs: Vec<super::model::DTerm> = args
                        .iter()
                        .map(|a| match a {
                            Term::Sym(s) => super::model::DTerm::Const(s.id()),
                            _ => super::model::DTerm::Var(0),
                        })
                        .collect();
                    if let Some(rows) = mp.answer(rel, &dargs, Some(deadline)) {
                        for row in &rows {
                            push_row(&mut f, row);
                        }
                    }
                }
            }
            if !f.is_empty() {
                facts.insert(rel, f);
            }
        }
        if facts.is_empty() {
            return;
        }
        if trace {
            eprintln!(
                "MODEL-JOIN: {} queries, {} generator relations, {} facts",
                queries.len(),
                facts.len(),
                facts.values().map(Vec::len).sum::<usize>(),
            );
        }

        // 3) Join each query; on the first satisfying binding, collect the
        //    ground conjuncts to emit.
        let seat_idx = build_seat_index(&facts);
        let mut budget = 200_000usize;
        let mut produced: Vec<(Term, Vec<SentenceId>)> = Vec::new();
        for lits in &queries {
            let body: Vec<(SymbolId, Vec<Term>)> =
                lits.iter().filter_map(lit_pattern).collect();
            if body.len() != lits.len() {
                continue;
            }
            let mut sols: Vec<HashMap<SymbolId, Term>> = Vec::new();
            self.join_rec(
                &body,
                &(0..body.len()).collect::<Vec<_>>(),
                &HashMap::new(),
                &facts,
                &seat_idx,
                &roles,
                &tids,
                &mut sols,
                &mut budget,
            );
            if let Some(sol) = sols.first() {
                let (fact_sids, _) = self.collect_provenance(&body, sol, &facts);
                for lit in lits {
                    let g = subst(lit, sol);
                    if g.is_ground() {
                        produced.push((g, fact_sids.clone()));
                    }
                }
                if trace {
                    eprintln!("MODEL-JOIN: query of {} atoms satisfied", lits.len());
                }
            }
        }
        drop(mp);

        // 4) Emit the witness conjuncts as positive units — each resolves a
        //    literal of the all-negative goal clause, collapsing it to empty.
        let mut emitted = 0usize;
        let mut seen_emit: HashSet<AtomId> = HashSet::new();
        for (h, fact_sids) in produced {
            let aid = self.layer.atoms.intern_atom(&h);
            if !seen_emit.insert(aid) {
                continue;
            }
            if let Some(id) =
                self.make(vec![(true, h)], Vec::new(), "model_join", SUPPORT, None, true)
            {
                self.clauses[id as usize].fact_parents.extend(fact_sids);
                self.activate(id);
                if self.push(Some(id)).is_some() {
                    emitted += 1;
                }
            }
        }
        if trace {
            eprintln!("MODEL-JOIN: {emitted} witness units emitted");
        }
    }

    /// Goal-directed backward chaining / connection search (gated
    /// `SIGMA_BACKWARD`, default-off).  The forward given-clause loop is
    /// blind to *which* axioms lead to the goal; on a constant-rich
    /// conjecture over a 10k-axiom theory it floods.  This pass instead
    /// drives **from** the negated conjecture: select a goal literal, find an
    /// axiom whose head literal structurally matches it, resolve, and recurse
    /// on the axiom's body — iterative-deepening DFS, most-constrained literal
    /// first (sideways information passing).  Every step is a real `resolve`
    /// (sound binary resolution), so a derived empty clause is a genuine
    /// refutation; on success the empty clause is pushed and the normal loop
    /// reports it.  Handles existential/Skolem rule heads naturally — matching
    /// a goal atom against an existential conclusion just unifies the goal
    /// term with the head's Skolem term.  Definite-clause (Horn) focused: only
    /// negative goal literals are expanded, so a resolvent that gains a
    /// positive literal (non-definite partner) is not pursued — a prototype
    /// limitation, not unsoundness.
    pub(crate) fn discharge_backward(&mut self) {
        if std::env::var_os("SIGMA_BACKWARD").is_none() {
            return;
        }
        let trace = std::env::var_os("SIGMA_ORACLE_TRACE").is_some();

        // Goal clauses: all-negative conjecture clauses (the negated `∃`).
        let goals: Vec<u32> = self
            .clauses
            .iter()
            .filter(|c| {
                c.tier == CONJECTURE && !c.terms.is_empty() && c.terms.iter().all(|(p, _)| !*p)
            })
            .map(|c| c.id)
            .collect();
        if goals.is_empty() {
            return;
        }

        // Head/conclusion index: predicate → positive-literal occurrences
        // across ALL loaded clauses (axiom heads + ground unit facts).  Built
        // once; resolvents are never added (we chain the goal against axioms,
        // not against derived clauses).
        let mut head_index: HashMap<SymbolId, Vec<(u32, usize)>> = HashMap::new();
        for c in &self.clauses {
            for (i, (pos, t)) in c.terms.iter().enumerate() {
                if *pos {
                    if let Some((p, _)) = lit_pattern(t) {
                        head_index.entry(p).or_default().push((c.id, i));
                    }
                }
            }
        }
        if trace {
            let total: usize = head_index.values().map(Vec::len).sum();
            eprintln!(
                "BACKWARD: {} goal clause(s), {} clauses, {} head predicates, {} positive-head occurrences",
                goals.len(),
                self.clauses.len(),
                head_index.len(),
                total,
            );
            // Per goal-literal candidate counts (where the search would branch
            // or die) — the diagnostic for an unreachable conjunct.
            for &g in &goals {
                for (pos, t) in &self.clauses[g as usize].terms {
                    if *pos {
                        continue;
                    }
                    if let Some((pred, gargs)) = lit_pattern(t) {
                        let n = head_index
                            .get(&pred)
                            .map(|v| {
                                v.iter()
                                    .filter(|&&(cid, pi)| {
                                        lit_pattern(&self.clauses[cid as usize].terms[pi].1)
                                            .is_some_and(|(_, pa)| {
                                                structurally_compatible(&gargs, &pa)
                                            })
                                    })
                                    .count()
                            })
                            .unwrap_or(0);
                        eprintln!(
                            "BACKWARD:   goal lit {}/{} -> {} candidate head(s)",
                            self.syn().sym_name(pred).map(|s| s.to_string()).unwrap_or_default(),
                            gargs.len(),
                            n,
                        );
                    }
                }
            }
        }

        // Depth bounds resolution STEPS along one DFS path.  A goal with N
        // literals needs ≥N resolutions just to discharge each against a fact,
        // plus the rule-chain depth — so the bound scales with the goal width,
        // not the (small) proof depth.  Single deep DFS, node-budgeted (cheaper
        // than iterative deepening, which re-materializes resolvents each round).
        // Best-effort: bounded by a wall-clock deadline (each DFS node
        // materializes a real resolvent, so the node count is a poor bound)
        // plus a node backstop.  Returns promptly either way.
        let ms: u64 = std::env::var("SIGMA_BACKWARD_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(800);
        let deadline = Instant::now() + std::time::Duration::from_millis(ms);
        let mut budget = std::env::var("SIGMA_BACKWARD_NODES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(200_000usize);
        for &g in &goals {
            let width = self.clauses[g as usize].terms.len() as u32;
            let max_depth = width.saturating_mul(2).saturating_add(16).min(64);
            if self.backward_dfs(g, max_depth, &head_index, &mut budget, deadline) {
                if trace {
                    eprintln!("BACKWARD: refutation found (depth budget {max_depth})");
                }
                return; // empty clause pushed; the loop reports it
            }
            if budget == 0 {
                if trace {
                    eprintln!("BACKWARD: node budget exhausted");
                }
                return;
            }
        }
        if trace {
            eprintln!("BACKWARD: no refutation found");
        }
    }

    /// One depth-bounded backward step (see [`discharge_backward`]).  Returns
    /// `true` iff an empty clause was derived (and pushed) on this branch.
    fn backward_dfs(
        &mut self,
        goal0: u32,
        depth0: u32,
        head_index: &HashMap<SymbolId, Vec<(u32, usize)>>,
        budget: &mut usize,
        deadline: Instant,
    ) -> bool {
        let empty: Vec<(u32, usize)> = Vec::new();
        // Forced-move PROPAGATION loop: a goal literal with exactly one
        // structurally-compatible head is forced (no choice), so commit to it
        // without a backtrack point — this is the connection calculus's
        // reduction step and collapses the wide goal (each ground-fact-only
        // literal, once its variables are bound by a sibling, becomes
        // single-candidate and discharges deterministically).
        let mut goal = goal0;
        let mut depth = depth0;
        loop {
            if self.clauses[goal as usize].terms.is_empty() {
                self.push(Some(goal)); // the empty clause — refutation
                return true;
            }
            if depth == 0 || *budget == 0 || Instant::now() >= deadline {
                return false;
            }

            // Candidate heads for every negative (goal) literal.
            let goal_terms = self.clauses[goal as usize].terms.clone();
            let mut lit_cands: Vec<(usize, Vec<(u32, usize)>)> = Vec::new();
            for (gi, (pos, t)) in goal_terms.iter().enumerate() {
                if *pos {
                    continue; // definite-clause focus: expand negative literals
                }
                let Some((pred, gargs)) = lit_pattern(t) else { continue };
                let mut cands: Vec<(u32, usize)> = Vec::new();
                for &(cid, pi) in head_index.get(&pred).unwrap_or(&empty) {
                    if cid == goal {
                        continue;
                    }
                    if let Some((_, pa)) = lit_pattern(&self.clauses[cid as usize].terms[pi].1) {
                        if structurally_compatible(&gargs, &pa) {
                            cands.push((cid, pi));
                        }
                    }
                }
                if cands.is_empty() {
                    return false; // unsatisfiable goal literal → dead branch
                }
                lit_cands.push((gi, cands));
            }
            if lit_cands.is_empty() {
                return false; // only positive literals left (non-definite)
            }

            // Forced move (single candidate): commit and re-loop, no branching.
            if let Some((gi, cands)) = lit_cands.iter().find(|(_, c)| c.len() == 1) {
                let (partner, pi) = cands[0];
                *budget -= 1;
                match self.resolve(goal, *gi, partner, pi) {
                    Some(r) => {
                        goal = r;
                        depth -= 1;
                        continue;
                    }
                    None => return false, // the only option didn't unify → dead
                }
            }

            // Otherwise branch on the most-constrained literal, trying
            // ground-unit-clause partners (leaf closures) before rule partners.
            let (gi, mut cands) = lit_cands
                .into_iter()
                .min_by_key(|(_, c)| c.len())
                .unwrap();
            cands.sort_by_key(|&(cid, _)| usize::from(self.clauses[cid as usize].terms.len() > 1));
            for (partner, pi) in cands {
                if *budget == 0 || Instant::now() >= deadline {
                    return false;
                }
                *budget -= 1;
                if let Some(r) = self.resolve(goal, gi, partner, pi) {
                    if self.backward_dfs(r, depth - 1, head_index, budget, deadline) {
                        return true;
                    }
                }
            }
            return false;
        }
    }

    /// Re-walk a satisfied rule body under its complete binding to gather
    /// proof provenance: store facts and oracle witnesses become
    /// `fact_parents` (cited axiom steps); previously-emitted heads become
    /// clause parents (so chained `rule_join` steps form a connected DAG).
    fn collect_provenance(
        &self,
        body:    &[(SymbolId, Vec<Term>)],
        binding: &HashMap<SymbolId, Term>,
        facts:   &HashMap<SymbolId, Vec<JoinFact>>,
    ) -> (Vec<SentenceId>, Vec<u32>) {
        let mut fact_sids: Vec<SentenceId> = Vec::new();
        let mut cparents:  Vec<u32> = Vec::new();
        for (rel, args) in body {
            let sargs: Vec<Term> = args.iter().map(|a| subst(a, binding)).collect();
            // A directly-matched generator fact (store or emitted head).
            if let Some(jf) = facts
                .get(rel)
                .and_then(|v| v.iter().find(|jf| jf.args == sargs))
            {
                match jf.src {
                    FactSrc::Store(sid) => fact_sids.push(sid),
                    FactSrc::Emitted(cid) => cparents.push(cid),
                    FactSrc::Model => {} // model-derived: no citable store sid
                }
                continue;
            }
            // Otherwise a binary literal the oracle decided (taxonomy /
            // subrelation / transitive): cite its witness facts.
            if sargs.len() == 2 {
                if let (Some(x), Some(y)) = (sym_of(&sargs[0]), sym_of(&sargs[1])) {
                    let mut why: Vec<Witness> = Vec::new();
                    if self.oracle.holds(*rel, x, y, Some(&mut why)) {
                        fact_sids.extend(why.iter().filter_map(|w| w.sid));
                    }
                }
            }
        }
        fact_sids.sort_unstable();
        fact_sids.dedup();
        cparents.sort_unstable();
        cparents.dedup();
        (fact_sids, cparents)
    }

    /// Ground argument tuples of every `(rel …)` atom asserted in the
    /// store (base ∪ session), regardless of SInE selection — the join's
    /// generator facts.  Only all-leaf (symbol / literal) argument lists
    /// are returned; atoms with variable, operator, or compound arguments
    /// are skipped (a generator must bind variables to ground fillers).
    fn store_facts(&self, rel: SymbolId) -> Vec<JoinFact> {
        let mut out = Vec::new();
        for sid in self.syn().by_head_id(&rel) {
            let Some(s) = self.syn().sentence(sid) else { continue };
            if s.elements.len() < 2 {
                continue;
            }
            let mut args = Vec::with_capacity(s.elements.len() - 1);
            let mut ok = true;
            for el in &s.elements[1..] {
                match el {
                    Element::Symbol(sym) => args.push(Term::Sym(sym.0.clone())),
                    Element::Literal(l) => args.push(Term::Lit(l.clone())),
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                out.push(JoinFact { args, src: FactSrc::Store(sid) });
            }
        }
        out
    }

    /// Recursive ground-fact join over a Horn rule body.  At each step:
    /// discharge any fully-ground literal (a check, no branching) via the
    /// oracle / fact membership; otherwise expand the most selective
    /// non-theory generator literal over its candidate facts.  Open
    /// theory literals are never enumerated (the join bails on a branch
    /// that leaves only those) — best-effort, escalating the rest to
    /// ordinary resolution.
    #[allow(clippy::too_many_arguments)]
    fn join_rec(
        &self,
        body: &[(SymbolId, Vec<Term>)],
        pending: &[usize],
        binding: &HashMap<SymbolId, Term>,
        facts: &HashMap<SymbolId, Vec<JoinFact>>,
        seat_idx: &SeatIndex,
        roles: &crate::semantics::roles::TaxonomyRoles,
        tids: &super::temporal::TemporalRelIds,
        out: &mut Vec<HashMap<SymbolId, Term>>,
        budget: &mut usize,
    ) {
        if *budget == 0 {
            return;
        }
        if pending.is_empty() {
            *budget -= 1;
            out.push(binding.clone());
            return;
        }
        // 1) Fully-ground literal under the current binding: a check.
        for &li in pending {
            let (rel, args) = &body[li];
            let sargs: Vec<Term> = args.iter().map(|a| subst(a, binding)).collect();
            if sargs.iter().all(Term::is_ground) {
                if !self.ground_lit_holds(*rel, &sargs, facts) {
                    return; // dead branch
                }
                let rest: Vec<usize> = pending.iter().copied().filter(|&x| x != li).collect();
                self.join_rec(body, &rest, binding, facts, seat_idx, roles, tids, out, budget);
                return;
            }
        }
        // 2) Expand the most selective generator GIVEN the current binding.
        //    Narrow each candidate conjunct's facts via the seat index on
        //    its already-bound seats (sideways information passing), and
        //    pick the conjunct with the fewest candidates — so the join
        //    follows the constrained path instead of materializing a
        //    cross-product.  A bound seat with no matching fact (count 0)
        //    makes the whole branch dead.  `None` candidate set ⇒ no seat
        //    bound yet ⇒ full scan of the relation.
        let mut pick: Option<(usize, Option<Vec<u32>>, usize)> = None;
        for &li in pending {
            let (rel, args) = &body[li];
            if is_theory_rel(*rel, roles, tids) {
                continue;
            }
            let Some(rel_facts) = facts.get(rel) else { continue };
            let mut narrowed: Option<&Vec<u32>> = None;
            let mut dead = false;
            for (seat, a) in args.iter().enumerate() {
                if let Some(k) = seat_key(&subst(a, binding)) {
                    match seat_idx.get(&(*rel, seat as u8, k)) {
                        Some(list) => {
                            if narrowed.map_or(true, |c| list.len() < c.len()) {
                                narrowed = Some(list);
                            }
                        }
                        None => {
                            dead = true;
                            break;
                        }
                    }
                }
            }
            let count = if dead { 0 } else { narrowed.map_or(rel_facts.len(), |c| c.len()) };
            if pick.as_ref().map_or(true, |(_, _, bn)| count < *bn) {
                let cands = if dead { Some(Vec::new()) } else { narrowed.cloned() };
                pick = Some((li, cands, count));
            }
        }
        let Some((li, cand_idxs, _)) = pick else { return }; // only open theory lits left
        let (rel, args) = &body[li];
        let rest: Vec<usize> = pending.iter().copied().filter(|&x| x != li).collect();
        let pargs: Vec<Term> = args.iter().map(|a| subst(a, binding)).collect();
        let Some(rel_facts) = facts.get(rel) else { return };
        // Iterate either the index-narrowed candidates or the full relation.
        match cand_idxs {
            Some(idxs) => {
                for &fi in &idxs {
                    let jf = &rel_facts[fi as usize];
                    let mut b2 = binding.clone();
                    if match_args(&pargs, &jf.args, &mut b2) {
                        self.join_rec(body, &rest, &b2, facts, seat_idx, roles, tids, out, budget);
                        if *budget == 0 {
                            return;
                        }
                    }
                }
            }
            None => {
                for jf in rel_facts {
                    let mut b2 = binding.clone();
                    if match_args(&pargs, &jf.args, &mut b2) {
                        self.join_rec(body, &rest, &b2, facts, seat_idx, roles, tids, out, budget);
                        if *budget == 0 {
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Decide a fully-ground body literal.  Generator facts (store atoms
    /// + previously-emitted heads) are consulted first by exact match —
    /// this is what lets a chained rule see an earlier round's head.
    /// Binary atoms then fall through to the oracle (taxonomy, temporal,
    /// subrelation-inherited and transitive edges).
    fn ground_lit_holds(
        &self,
        rel: SymbolId,
        args: &[Term],
        facts: &HashMap<SymbolId, Vec<JoinFact>>,
    ) -> bool {
        if facts.get(&rel).is_some_and(|v| {
            v.iter().any(|jf| {
                jf.args.len() == args.len() && jf.args.iter().zip(args).all(|(a, b)| a == b)
            })
        }) {
            return true;
        }
        if args.len() == 2 {
            if let (Some(x), Some(y)) = (sym_of(&args[0]), sym_of(&args[1])) {
                return self.oracle.holds(rel, x, y, None);
            }
        }
        false
    }

    /// Shape-recognize the taxonomy roles (`Strategy.recognize_roles`)
    /// and install them on BOTH the semantic layer (so `tax_edges` /
    /// `parents_of` / `trans_reach` reclassify the renamed vocabulary)
    /// and the oracle.  Must run BEFORE background loading and the
    /// equality/schema pre-pass: the recognized ids drive exhaustive-set,
    /// disjointness, and every `holds` decision from the first clause.
    /// A no-op on SUMO (recovers the same ids the names hash to).
    pub(crate) fn recognize_roles(&mut self, _roots: &[SentenceId]) {
        // The semantic layer is the shared source of truth: recognize +
        // rebuild the taxonomy there, then seed the oracle from the same
        // installed roles.
        self.layer.semantic.ensure_taxonomy_roles();
        let roles = self.layer.semantic.recognized_roles().unwrap_or_default();
        if std::env::var_os("SIGMA_ORACLE_TRACE").is_some() {
            eprintln!(
                "ROLES instance={:#x} subclass={:#x} subrelation={:#x} \
                 transitive={:#x} symmetric={:#x} domain={:#x} range={:#x} \
                 disjoint={:#x} partition={:#x}",
                roles.instance, roles.subclass, roles.subrelation,
                roles.transitive, roles.symmetric, roles.domain, roles.range,
                roles.disjoint, roles.partition,
            );
        }
        self.oracle.set_roles(roles, &self.layer.semantic);
    }

    /// Recognize functional-dependency axioms among `clauses` and
    /// register them with the oracle's FD congruence:
    ///
    /// 1. Uniqueness clauses `¬G… ∨ ¬R(u₁,v₁) ∨ ¬R(u₂,v₂) ∨ v₁ = v₂`
    ///    — two same-relation atoms sharing a key variable at one
    ///    position, equating the other position's variables; any
    ///    remaining negative `(instance ?x C)` literals over the key /
    ///    value variables become sort GUARDS (TQG14's nucleus axiom:
    ///    `part` keyed on the whole, guarded by `AtomicNucleus`).
    /// 2. `(instance R SingleValuedRelation)` declarations — the
    ///    unguarded arg1-determines-arg2 case.
    pub(crate) fn mine_fd_relations(&mut self, clauses: &[PClause], root: SentenceId) {
        use super::oracle::FdDecl;
        let instance = self.oracle.roles().instance;
        let single_valued = Symbol::hash_name("SingleValuedRelation");

        'clauses: for c in clauses {
            // -- Declaration form.
            if c.lits.len() == 1 && c.lits[0].pos {
                let Some(sent) = self.layer.atoms.resolve(c.lits[0].atom, self.syn()) else { continue };
                if sent.elements.len() == 3 {
                    if let (Some(Element::Symbol(h)), Element::Symbol(r), Element::Symbol(cl)) =
                        (sent.elements.first(), &sent.elements[1], &sent.elements[2])
                    {
                        if h.id() == instance && cl.id() == single_valued {
                            self.oracle.register_fd(r.id(), FdDecl {
                                key_pos: 1,
                                key_guards: Vec::new(),
                                val_guards: Vec::new(),
                                axiom: Some(root),
                            });
                        }
                    }
                }
                continue;
            }

            // -- Uniqueness-clause form.
            if c.lits.len() < 3 || c.lits.len() > 8 {
                continue;
            }
            // Exactly one positive literal: (equal ?a ?b).
            let mut pos_iter = c.lits.iter().filter(|l| l.pos);
            let (Some(pos), None) = (pos_iter.next(), pos_iter.next()) else { continue };
            let Some(eq_sent) = self.layer.atoms.resolve(pos.atom, self.syn()) else { continue };
            if eq_sent.elements.len() != 3
                || !matches!(eq_sent.elements.first(), Some(Element::Op(OpKind::Equal)))
            {
                continue;
            }
            let (Element::Variable { id: va, .. }, Element::Variable { id: vb, .. }) =
                (&eq_sent.elements[1], &eq_sent.elements[2]) else { continue };
            let (va, vb) = (*va, *vb);
            if va == vb { continue; }

            // Negative literals: two same-relation binary atoms over
            // the equated variables + instance guards.  Anything else
            // disqualifies the clause.
            let mut rel_atoms: Vec<(SymbolId, u64, u64)> = Vec::new(); // (rel, arg1 var, arg2 var)
            let mut guards: Vec<(u64, SymbolId)> = Vec::new();         // (var, class)
            for l in c.lits.iter().filter(|l| !l.pos) {
                let Some(sent) = self.layer.atoms.resolve(l.atom, self.syn()) else { continue 'clauses };
                if sent.elements.len() != 3 { continue 'clauses; }
                let Some(Element::Symbol(h)) = sent.elements.first() else { continue 'clauses };
                match (&sent.elements[1], &sent.elements[2]) {
                    (Element::Variable { id: x, .. }, Element::Symbol(class)) if h.id() == instance => {
                        guards.push((*x, class.id()));
                    }
                    (Element::Variable { id: x, .. }, Element::Variable { id: y, .. }) => {
                        rel_atoms.push((h.id(), *x, *y));
                    }
                    _ => continue 'clauses,
                }
            }
            if rel_atoms.len() != 2 || rel_atoms[0].0 != rel_atoms[1].0 {
                continue;
            }
            let rel = rel_atoms[0].0;
            let ((_, x1, y1), (_, x2, y2)) = (rel_atoms[0], rel_atoms[1]);
            // Orientation: key var shared at one position, the
            // equated pair at the other.
            let (key_pos, key_var) = if x1 == x2 && {
                let vals = [y1, y2];
                vals.contains(&va) && vals.contains(&vb)
            } {
                (1u8, x1)
            } else if y1 == y2 && {
                let vals = [x1, x2];
                vals.contains(&va) && vals.contains(&vb)
            } {
                (2u8, y1)
            } else {
                continue;
            };
            let key_guards: Vec<SymbolId> = guards.iter()
                .filter(|(v, _)| *v == key_var).map(|(_, c)| *c).collect();
            let val_guards_a: Vec<SymbolId> = guards.iter()
                .filter(|(v, _)| *v == va).map(|(_, c)| *c).collect();
            let val_guards_b: Vec<SymbolId> = guards.iter()
                .filter(|(v, _)| *v == vb).map(|(_, c)| *c).collect();
            // Sound only if both equated sides carry the SAME guard
            // set (the axiom constrains both symmetrically).
            let mut ga = val_guards_a.clone(); ga.sort_unstable();
            let mut gb = val_guards_b.clone(); gb.sort_unstable();
            if ga != gb { continue; }
            // Guards on unrelated variables would make the clause more
            // restrictive than our check — disqualify.
            if guards.iter().any(|(v, _)| *v != key_var && *v != va && *v != vb) {
                continue;
            }
            self.oracle.register_fd(rel, FdDecl {
                key_pos,
                key_guards,
                val_guards: val_guards_a,
                axiom: Some(root),
            });
        }
    }

    /// Schema-channel pre-pass: probe every input clause of a root
    /// against the pattern table and register what it states (mined
    /// symmetric / transitive / antisymmetric / irreflexive relations,
    /// inverse pairs).  Registration only — absorption happens when the
    /// same clause flows through `make`, which re-probes.  Running this
    /// over ALL roots before any clause is made means orientation and
    /// the oracle closures are active from the first input clause.
    pub(crate) fn mine_schema(&mut self, clauses: &[PClause], root: SentenceId) {
        if !self.opts.strategy.schema {
            return;
        }
        for pc in clauses {
            if pc.lits.len() > 4 || pc.nvars == 0 {
                continue;
            }
            if let Some(hit) = self.layer.schema.probe(&pc.lits, &self.layer.atoms, self.syn()) {
                self.apply_schema_hit(&hit, Some(root));
            }
        }
    }

    /// Act on a verified schema hit: register with the matching theory
    /// registry, and say whether the clause should be ABSORBED (dropped
    /// — its inferential role fully replaced).  Absorption is earned
    /// per pattern:
    ///
    /// * Symmetry rule + symmetry metaschema: YES.  Ground orientation
    ///   collapses both argument orders to one canonical form; the
    ///   symmetric retrieval retry (`resolve`) and the oracle's
    ///   reversed-edge check cover open literals and stored facts.
    /// * Transitivity (rule AND metaschema): NO.  The oracle closure
    ///   discharges ground transitive queries, but saturation still
    ///   needs the clause to ENUMERATE compositions into open goals
    ///   (`¬R(a,?z)` has no closure analogue) — absorbing it would be
    ///   an enumeration-completeness hole.  Registration alone buys the
    ///   ground discharges.
    /// * Antisymmetry / irreflexivity / inverse: NO — recognized and
    ///   recorded; their consumers land separately.
    fn apply_schema_hit(&mut self, hit: &SchemaHit, source: Option<SentenceId>) -> bool {
        let trace = std::env::var_os("SIGMA_ORACLE_TRACE").is_some();
        match hit.kind {
            SchemaKind::Symmetry => {
                let Some(rel) = &hit.rel else { return false };
                if trace {
                    eprintln!("SCHEMA symmetric {}", rel.name());
                }
                self.stats.mined_symmetric += 1;
                self.oracle.register_symmetric(rel.id(), source);
                true
            }
            SchemaKind::SymMetaschema => {
                if trace {
                    eprintln!("SCHEMA symmetry-metaschema absorbed");
                }
                true
            }
            SchemaKind::EqSubstitution => {
                // Substitution of equals is what paramodulation and the
                // ground-equality congruence closure (normalize_eq,
                // compound equality keys, FD pipeline) already do; the
                // axiomatic spelling only multiplies every equality
                // unit by every R-fact.  Absorb.
                if trace {
                    let name = hit.rel.as_ref().map(|r| r.name().to_string());
                    eprintln!("SCHEMA eq-substitution absorbed ({name:?})");
                }
                true
            }
            SchemaKind::Transitivity => {
                let Some(rel) = &hit.rel else { return false };
                if trace {
                    eprintln!("SCHEMA transitive {}", rel.name());
                }
                self.stats.mined_transitive += 1;
                self.oracle.register_transitive(rel.id(), source);
                false
            }
            SchemaKind::TransMetaschema => false,
            SchemaKind::Antisymmetry => {
                if let Some(rel) = &hit.rel {
                    self.stats.mined_other += 1;
                    self.antisym_mined.entry(rel.id()).or_insert(source);
                }
                false
            }
            SchemaKind::Irreflexivity => {
                if let Some(rel) = &hit.rel {
                    self.stats.mined_other += 1;
                    self.irrefl_mined.entry(rel.id()).or_insert(source);
                }
                false
            }
            SchemaKind::Inverse => {
                if let (Some(r1), Some(r2)) = (&hit.rel, &hit.rel2) {
                    self.stats.mined_other += 1;
                    self.inverse_mined.push((r1.id(), r2.id(), source));
                }
                false
            }
        }
    }

    /// Install the conjecture's leaf-signature profile (called BEFORE
    /// background loading, so every input clause is scored too).
    /// Liu & Xu's premise-selection insight, transplanted to the queue:
    /// relevance is structural closeness to the goal, and closeness is
    /// shared ground content.  Here that is one OR per conjecture atom.
    pub(crate) fn set_goal(&mut self, clauses: &[PClause]) {
        for pc in clauses {
            for l in &pc.lits {
                self.conj_sig |= self.layer.atom_info(l.atom).leaf_sig;
            }
        }
    }

    /// The conjecture-distance weight factor for a clause's canonical
    /// literals: 1 (every ground leaf also occurs in the conjecture) up
    /// to `1 + goal_dist_w` (no shared content).  Leafless clauses
    /// (fully open schemas) score neutral — variable counts already
    /// charge them.  Cost: one memoized info probe per literal, one
    /// AND + two popcounts per clause.
    pub(crate) fn goal_distance_factor(&self, lits: &[PLit], tier: u8) -> u64 {
        if !self.opts.strategy.goal_dist || self.conj_sig == 0 || tier == CONJECTURE {
            return 1;
        }
        let mut sig = 0u64;
        for l in lits {
            sig |= self.layer.atom_info(l.atom).leaf_sig;
        }
        if sig == 0 {
            return 1;
        }
        let total = u64::from(sig.count_ones());
        let hits = u64::from((sig & self.conj_sig).count_ones());
        1 + self.opts.strategy.goal_dist_w * (total - hits) / total
    }

    /// Whether any active ordered inference needs per-clause maximality.
    #[inline]
    fn needs_maximality(&self) -> bool {
        self.opts.strategy.ordered_resolution
            || self.opts.strategy.superposition
            || self.opts.strategy.eq_factoring
    }

    /// Bitmask of the clause's KBO-maximal literals (ordered-inference
    /// eligibility).  All-ones when maximality isn't needed (the
    /// unordered default) or the clause is a unit — so consumers AND
    /// against it for free.  Literal `i` is maximal iff no other literal
    /// strictly dominates it under [`super::kbo::KboOrdering::compare_lits`].
    fn maximal_literals(&self, lits: &[PLit]) -> u64 {
        if !self.needs_maximality() || lits.len() <= 1 {
            return !0u64;
        }
        let kbo = self.kbo();
        let atoms = &self.layer.atoms;
        let syn = self.syn();
        let mut mask = 0u64;
        for (i, li) in lits.iter().enumerate().take(64) {
            let dominated = lits.iter().enumerate().any(|(j, lj)| {
                i != j
                    && kbo.compare_lits(lj.pos, lj.atom, li.pos, li.atom, atoms, syn)
                        == super::kbo::KboCmp::Greater
            });
            if !dominated {
                mask |= 1u64 << i;
            }
        }
        mask
    }

    /// The id of an ACTIVE clause that forward-subsumes the candidate
    /// (`lits`/`terms`), or `None`.  Candidates are found via the literal
    /// index — every literal of a subsumer is a generalization of one of
    /// ours, so a subsumer must have at least one literal the index
    /// returns for ours — then verified exactly by [`clause_subsumes`].
    /// Gated by `Strategy.subsumption`.
    fn forward_subsumed(&mut self, lits: &[PLit], terms: &[(bool, Term)]) -> Option<u32> {
        if !self.opts.strategy.subsumption || lits.is_empty() {
            return None;
        }
        let layer = self.layer;
        let src = move |a| layer.atom_info(a);
        let mut cand: Set64<u32> = Set64::default();
        for l in lits {
            let info = self.layer.atom_info(l.atom);
            for at in self.idx.probe(l.pos, &info, &src) {
                cand.insert(at.clause);
            }
        }
        for cid in cand {
            let c = &self.clauses[cid as usize];
            if c.lits.len() <= terms.len() && clause_subsumes(&c.terms, terms) {
                return Some(cid);
            }
        }
        None
    }

    /// The argument-swapped form of a symmetric-relation literal, with
    /// the relation id — `None` when the literal's head is not a known
    /// symmetric relation (or the swap is the identity).
    fn symmetric_swap_term(&self, t: &Term) -> Option<(SymbolId, Term)> {
        if !self.opts.strategy.schema {
            return None;
        }
        let Term::App(elems) = t else { return None };
        if elems.len() != 3 || elems[1] == elems[2] {
            return None;
        }
        let Term::Sym(h) = &elems[0] else { return None };
        if !self.oracle.is_symmetric(h.id()) {
            return None;
        }
        Some((
            h.id(),
            Term::App(vec![elems[0].clone(), elems[2].clone(), elems[1].clone()]),
        ))
    }

    /// Residue facts of a symmetric-headed literal's argument-swapped
    /// atom — the second probe of the symmetric dual retrieval.  `None`
    /// for non-symmetric heads and palindromic atoms (swap = identity).
    /// Memoized per atom: positive entries are permanent (symmetry only
    /// accrues within a run), negative entries expire on oracle epoch
    /// (a relation can become symmetric mid-run).
    fn symmetric_swapped_info(&mut self, atom: AtomId) -> Option<std::sync::Arc<AtomInfo>> {
        if !self.opts.strategy.schema {
            return None;
        }
        if let Some(&(ep, cached)) = self.sym_swap_memo.get(&atom) {
            match cached {
                Some(id) => return Some(self.layer.atom_info(id)),
                None if ep == self.oracle.epoch() => return None,
                None => {}
            }
        }
        let swapped = (|| {
            let sent = self.layer.atoms.resolve(atom, self.syn())?;
            if sent.elements.len() != 3 {
                return None;
            }
            let Element::Symbol(h) = sent.elements.first()? else { return None };
            if !self.oracle.is_symmetric(h.id()) {
                return None;
            }
            let mut sw = (*sent).clone();
            sw.elements.swap(1, 2);
            let id = self.layer.atoms.intern_sentence(sw);
            (id != atom).then_some(id) // palindromes swap to themselves
        })();
        self.sym_swap_memo.insert(atom, (self.oracle.epoch(), swapped));
        swapped.map(|id| self.layer.atom_info(id))
    }

    /// Queue theory units for every NEW ground `(ListFn …)` subterm of
    /// `t`: `(inList mᵢ L)` and `(equal mᵢ (ListOrderFn L i))` (1-based,
    /// SUMO's ListOrderFn convention).
    fn collect_ground_lists(&mut self, t: &Term) {
        let Term::App(elems) = t else { return };
        for el in elems {
            self.collect_ground_lists(el);
        }
        if !matches!(elems.first(), Some(Term::Sym(h)) if &*h.name() == "ListFn") {
            return;
        }
        if elems.len() < 2 || elems.len() > 16 || !t.is_ground() {
            return;
        }
        // LEAF members only: a ground list of compounds (nested lists,
        // function terms) is the signature of SUMO's generative list
        // machinery — synthesizing its extension feeds the loop that
        // grows new lists forever.
        if elems.iter().skip(1).any(|m| matches!(m, Term::App(_))) {
            return;
        }
        // Global cap: list theory is for the handful of lists a
        // problem mentions, not a list-universe enumeration.
        if self.lists_done.len() >= 64 {
            return;
        }
        let key = self.layer.atoms.intern_atom(t);
        if !self.lists_done.insert(key) {
            return;
        }
        let in_list = crate::types::Symbol::from("inList");
        let order_fn = crate::types::Symbol::from("ListOrderFn");
        for (i, m) in elems.iter().enumerate().skip(1) {
            self.pending_list_units.push(Term::App(vec![
                Term::Sym(in_list.clone()), m.clone(), t.clone(),
            ]));
            self.pending_list_units.push(Term::App(vec![
                Term::Op(OpKind::Equal),
                m.clone(),
                Term::App(vec![
                    Term::Sym(order_fn.clone()),
                    t.clone(),
                    Term::Lit(Literal::Number(i.to_string())),
                ]),
            ]));
        }
    }

    /// Surface FD-derived equalities as activated `(equal a b)` unit
    /// clauses, with the deriving clauses + uniqueness axiom as proof
    /// parents — so they resolve and paramodulate like any equality
    /// and the transcript shows where they came from.
    pub(crate) fn drain_fd_equalities(&mut self) {
        // Ground-list theory units → activated background facts.
        let list_units = std::mem::take(&mut self.pending_list_units);
        for term in list_units {
            let made = self.make(
                vec![(true, term)], Vec::new(), "list_theory", BACKGROUND, None, true);
            let Some(id) = made else { continue };
            let key = self.clauses[id as usize].key;
            if self.seen.insert(key) {
                self.activate(id);
                self.push(Some(id));
            }
        }
        // Exhaustiveness-derived positive facts → activated units.
        for (rel, x, y, just) in self.oracle.take_pending_facts() {
            let term_of = |key: u64| -> Option<Term> {
                self.eq_terms.get(&key).cloned()
                    .or_else(|| self.syn().sym_name(key).map(Term::Sym))
            };
            let (Some(tr), Some(tx), Some(ty)) = (term_of(rel), term_of(x), term_of(y))
            else { continue };
            let term = Term::App(vec![tr, tx, ty]);
            let made = self.make(
                vec![(true, term)], just.clause_parents.clone(),
                "exhaustive", SUPPORT, None, true);
            let Some(id) = made else { continue };
            self.clauses[id as usize].fact_parents.extend(just.fact_sids.iter().copied());
            if let Some(ax) = just.axiom {
                self.clauses[id as usize].fact_parents.push(ax);
            }
            let key = self.clauses[id as usize].key;
            if self.seen.insert(key) {
                self.activate(id);
                self.push(Some(id));
            }
        }
        for (a, b, just) in self.oracle.take_pending_eq() {
            let term_of = |key: u64| -> Option<Term> {
                self.eq_terms.get(&key).cloned()
                    .or_else(|| self.syn().sym_name(key).map(Term::Sym))
            };
            let (Some(ta), Some(tb)) = (term_of(a), term_of(b)) else { continue };
            let term = Term::App(vec![Term::Op(OpKind::Equal), ta, tb]);
            let made = self.make(
                vec![(true, term)], just.clause_parents.clone(),
                "fd_congruence", SUPPORT, None, true);
            let Some(id) = made else { continue };
            self.clauses[id as usize].fact_parents.extend(just.fact_sids.iter().copied());
            if let Some(ax) = just.axiom {
                self.clauses[id as usize].fact_parents.push(ax);
            }
            let key = self.clauses[id as usize].key;
            if self.seen.insert(key) {
                self.activate(id);
                self.push(Some(id));
            }
        }
    }

    /// Congruence-closure pre-pass: union every ground `(equal a b)`
    /// unit in `clauses` into the oracle's equality closure, so the
    /// later `make` of every clause normalizes against the complete
    /// closure.  Must run before any non-equality clause is added.
    pub(crate) fn register_equalities(&mut self, clauses: &[PClause]) {
        for c in clauses {
            if c.lits.len() != 1 || !c.lits[0].pos {
                continue;
            }
            if let Some((ta, tb, ka, kb)) = self.ground_equality(c.lits[0].atom) {
                self.register_equality(ta, tb, ka, kb);
            }
        }
    }

    /// Equality-class key of any GROUND term: leaf keys from `eq_key`,
    /// compounds by content hash (interning into the prover-local
    /// AtomTable — the same id `Element::Sub` carries, so store-side
    /// and prover-side spellings of one subterm share a class).
    fn term_eq_key(&self, t: &Term) -> Option<u64> {
        if let Some(k) = eq_key(t) {
            return Some(k);
        }
        match t {
            Term::App(_) if t.is_ground() => Some(self.layer.atoms.intern_atom(t)),
            _ => None,
        }
    }

    /// Union one ground equality, remembering renderable terms for both
    /// keys and preferring a NUMERIC literal as the class root (so
    /// `normalize_eq` rewrites symbols TO numbers, keeping arithmetic
    /// comparisons decidable downstream).
    fn register_equality(&mut self, ta: Term, tb: Term, ka: u64, kb: u64) {
        if matches!(ta, Term::App(_)) || matches!(tb, Term::App(_)) {
            self.has_compound_eqs = true;
        }
        self.eq_terms.entry(ka).or_insert(ta);
        self.eq_terms.entry(kb).or_insert(tb);
        // Root preference: number > symbol > compound.  Rewriting
        // TOWARD numbers keeps comparisons decidable; rewriting toward
        // symbols keeps terms from GROWING (a compound root would make
        // normalize_eq inflate every occurrence of the symbol into the
        // compound — and a compound containing a ground list regrows
        // new lists forever).
        fn rank(t: &Term) -> u8 {
            match t {
                Term::Lit(Literal::Number(_)) => 0,
                Term::Sym(_) | Term::Lit(_) | Term::Op(_) => 1,
                Term::Var(_) | Term::App(_) => 2,
            }
        }
        let (ra, rb) = (rank(&self.eq_terms[&ka]), rank(&self.eq_terms[&kb]));
        let (root, child) = match ra.cmp(&rb) {
            std::cmp::Ordering::Less => (ka, kb),
            std::cmp::Ordering::Greater => (kb, ka),
            std::cmp::Ordering::Equal => {
                self.oracle.add_equality(ka, kb);
                return;
            }
        };
        self.oracle.add_equality_rooted(root, child);
    }

    /// Whether `t` is a symbol-headed relation atom the oracle can prove
    /// ill-sorted (an argument disjoint from its declared domain).
    fn atom_ill_sorted(&self, t: &Term) -> bool {
        let Term::App(elems) = t else { return false };
        let Some(Term::Sym(rel)) = elems.first() else { return false };
        let args: Vec<Option<SymbolId>> = elems
            .iter()
            .skip(1)
            .map(|e| match e {
                Term::Sym(s) => Some(s.id()),
                _ => None,
            })
            .collect();
        self.oracle.ill_sorted(rel.id(), &args)
    }

    /// For a ground `(equal l r)` atom term, whether the oracle entails
    /// it — symbol equality through reflexivity / equality-class /
    /// subclass antisymmetry (with witnesses), compound equality
    /// structurally (content-addressed).  `None` if `t` is not a ground
    /// equality atom.
    fn ground_equality_holds(&self, t: &Term) -> Option<(EqDecision, Vec<Witness>, Vec<u32>)> {
        let Term::App(elems) = t else { return None };
        if elems.len() != 3 || !matches!(elems[0], Term::Op(OpKind::Equal)) {
            return None;
        }
        let (l, r) = (&elems[1], &elems[2]);
        if !l.is_ground() || !r.is_ground() {
            return None;
        }
        // Numeric literals decide OUTRIGHT, both ways: equal canonical
        // values are entailed, different values are REFUTED (literal
        // semantics — there is no model where 1 = 2).  Symbols never
        // get the refuted arm (no unique-names assumption for them).
        if let (Term::Lit(Literal::Number(a)), Term::Lit(Literal::Number(b))) = (l, r) {
            if let (Some(x), Some(y)) =
                (crate::numeric::parse_num(a), crate::numeric::parse_num(b))
            {
                let d = if x == y { EqDecision::Entailed } else { EqDecision::Refuted };
                return Some((d, Vec::new(), Vec::new()));
            }
        }
        match (self.term_eq_key(l), self.term_eq_key(r)) {
            (Some(ka), Some(kb)) => {
                let mut why = Vec::new();
                if self.oracle.equal_holds(ka, kb, Some(&mut why)) {
                    // Deriving clauses from the equality proof forest
                    // (FD-congruence merges) become proof-DAG parents.
                    let clause_parents = self.oracle.eq_explain(ka, kb).1;
                    Some((EqDecision::Entailed, why, clause_parents))
                } else {
                    Some((EqDecision::Unknown, Vec::new(), Vec::new()))
                }
            }
            _ => {
                let d = if l == r { EqDecision::Entailed } else { EqDecision::Unknown };
                Some((d, Vec::new(), Vec::new()))
            }
        }
    }

    /// Decide a ground arithmetic comparison literal
    /// (`(greaterThan -100.0 0.0)` → `Some(false)`); `None` when the
    /// term isn't a comparison over two numeric literals.
    fn ground_compare(t: &Term) -> Option<bool> {
        let Term::App(elems) = t else { return None };
        if elems.len() != 3 { return None; }
        let (Term::Sym(p), Term::Lit(Literal::Number(a)), Term::Lit(Literal::Number(b))) =
            (&elems[0], &elems[1], &elems[2]) else { return None };
        let (x, y) = (crate::numeric::parse_num(a)?, crate::numeric::parse_num(b)?);
        crate::numeric::eval_compare(&p.name(), x, y)
    }

    /// `(a, b)` iff `atom` is a ground `(equal a b)` over two symbols.
    fn ground_equality(&self, atom: AtomId) -> Option<(Term, Term, u64, u64)> {
        let s = self.layer.atoms.resolve(atom, self.syn())?;
        if s.elements.len() != 3 {
            return None;
        }
        if !matches!(s.elements.first(), Some(Element::Op(OpKind::Equal))) {
            return None;
        }
        let lift = |el: &Element| -> Option<(Term, u64)> {
            match el {
                Element::Symbol(sym) => {
                    let t = Term::Sym(sym.0.clone());
                    let k = eq_key(&t)?;
                    Some((t, k))
                }
                Element::Literal(l @ Literal::Number(_)) => {
                    let t = Term::Lit(l.clone());
                    let k = eq_key(&t)?;
                    Some((t, k))
                }
                // A ground sub-sentence: its sid IS its content hash —
                // the compound's equality key.  Lift to a Term for the
                // registry (renderable representative).
                Element::Sub(sid) => {
                    let t = self.layer.atoms.term_of(*sid, self.syn())?;
                    t.is_ground().then_some((t, *sid))
                }
                _ => None,
            }
        };
        let (ta, ka) = lift(&s.elements[1])?;
        let (tb, kb) = lift(&s.elements[2])?;
        if ka == kb {
            return None;
        }
        Some((ta, tb, ka, kb))
    }

    // -- make: simplify + canonicalize + register ------------------------------

    /// Render a literal list as a clause for the interactive stepper: a
    /// disjunction with `¬` on negative literals (`⊥` for the empty clause).
    fn dbg_lits_kif(&self, lits: &[(bool, Term)]) -> String {
        if lits.is_empty() {
            return "⊥ (empty clause — refutation)".to_string();
        }
        lits.iter()
            .map(|(pos, t)| {
                let k = term_kif(t, self.syn());
                if *pos { k } else { format!("(not {k})") }
            })
            .collect::<Vec<_>>()
            .join("  ∨  ")
    }

    /// Render an existing clause (by id) for the interactive stepper.
    fn dbg_clause_kif(&self, id: u32) -> String {
        let c = &self.clauses[id as usize];
        let tier = match c.tier {
            CONJECTURE => "goal",
            SUPPORT => "supp",
            _ => "bg",
        };
        format!("({tier}) {}", self.dbg_lits_kif(&c.terms))
    }


    /// Build a clause from raw slot-form literals: arithmetic
    /// normalization, oracle discharge, depth cap, unit
    /// subsumption/simplification, learned-unit feedback, canonical
    /// dedup, tier weighting.  `None` = redundant/discarded.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn make(
        &mut self,
        lits:    Vec<(bool, Term)>,
        parents: Vec<u32>,
        rule:    &'static str,
        tier:    u8,
        source:  Option<SentenceId>,
        derived: bool,
    ) -> Option<u32> {
        // Interactive single-step: show the proposed derivation (the "match")
        // — rule, parent clauses, and the literals about to be built — and
        // block before we normalize/register it.  Only INFERENCES pause
        // (`derived`); the initial axiom/conjecture load (derived = false) is
        // not a match and would otherwise flood the prompt before the loop.
        if derived && stepdbg::enabled() {
            let body = format!(
                "rule = {rule}   tier = {tier}   derived = {derived}\n  \
                 conclusion: {}\n  from parents:\n{}",
                self.dbg_lits_kif(&lits),
                if parents.is_empty() {
                    "    (none — input/oracle unit)".to_string()
                } else {
                    parents
                        .iter()
                        .map(|p| format!("    [{p}] {}", self.dbg_clause_kif(*p)))
                        .collect::<Vec<_>>()
                        .join("\n")
                },
            );
            stepdbg::pause("MAKE", &body);
        }

        let mut fact_parents: Vec<SentenceId> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let mut parents = parents;

        // Arithmetic normalization, then ground-equality normalization:
        // every ground constant collapses to its equality-class
        // representative, so `equal` constants become one symbol.
        // Both are in-place: the (overwhelmingly common) untouched
        // literal costs a walk, not a rebuild.
        let mut lits = lits;
        let mut demod_used: Vec<u32> = Vec::new();
        for (_, t) in lits.iter_mut() {
            arith_norm(t);
            self.normalize_eq(t);
            // Forward demodulation: rewrite to KBO normal form with the
            // active oriented unit equations (a simplification — the
            // normalized literal replaces the original).
            let n = self.demodulate(t, &mut demod_used);
            self.stats.demod_rewrites += n;
        }
        if !demod_used.is_empty() {
            demod_used.sort_unstable();
            demod_used.dedup();
            if self.want_notes() {
                notes.push(format!("demodulated by {} unit equation(s)", demod_used.len()));
            }
            parents.extend(demod_used);
        }

        // Equality-presence signal for strict saturation verdicts: once
        // equality is in play, a saturation without a complete equality
        // calculus cannot honestly claim "no".  Sticky bit, only paid
        // for on the strict path.
        if self.opts.strategy.strict_saturation && !self.stats.saw_equality {
            self.stats.saw_equality = lits.iter().any(|(_, t)| is_equality_atom(t));
        }

        // Symmetric-argument orientation: a GROUND argument pair of a
        // symmetric relation sorts into one canonical order (the same
        // blank-key order `orient_equality` uses), so `(R b a)` and
        // `(R a b)` collapse to one literal — the metaschema's mirrored
        // resolvents die at dedup instead of multiplying.  Ground pairs
        // only: orienting OPEN literals is unstable under substitution
        // (the oriented pattern's instances may orient the other way);
        // open-literal completeness is covered by the symmetric
        // retrieval retry in `resolve` instead.  Top-level atoms only —
        // never inside argument subterms, where embedded formulas
        // (PropositionalAttitude contexts) are referentially opaque.
        if self.opts.strategy.schema {
            let mut oriented: SmallVec<[SymbolId; 2]> = SmallVec::new();
            for (_, t) in lits.iter_mut() {
                let Term::App(elems) = t else { continue };
                if elems.len() != 3 || elems[1] == elems[2] {
                    continue;
                }
                let Term::Sym(h) = &elems[0] else { continue };
                if !elems[1].is_ground() || !elems[2].is_ground() {
                    continue;
                }
                let rel = h.id();
                if !self.oracle.is_symmetric(rel) {
                    continue;
                }
                if blank_key(&elems[1]) > blank_key(&elems[2]) {
                    elems.swap(1, 2);
                    self.stats.sym_oriented += 1;
                    if !oriented.contains(&rel) {
                        oriented.push(rel);
                    }
                }
            }
            for rel in oriented {
                if let Some(sid) = self.oracle.symmetric_source(rel) {
                    fact_parents.push(sid);
                }
                if self.want_notes() {
                    let name = self
                        .syn()
                        .sym_name(rel)
                        .map(|s| s.name().to_string())
                        .unwrap_or_else(|| format!("{rel:#x}"));
                    notes.push(format!("oriented symmetric {name} arguments"));
                }
            }
        }

        // Sorted-relation filter: a ground relation atom whose argument
        // is provably disjoint from the position's declared domain is
        // ill-sorted (false in SUMO's typed reading).  An ill-sorted
        // positive literal drops (false ∨ C ≡ C); an ill-sorted negative
        // literal is vacuously true → the clause is a tautology; dropping
        // all positives leaves a vacuous clause.  All three → discard.
        // INPUT clauses (axioms / hypotheses / the conjecture) are
        // exempt: asserted facts are ground truth, and SUMO itself
        // violates its own domain declarations (Merge asserts
        // `component` over nuclei while declaring component's domains
        // CorpuscularObject ⊥ Substance).  Silently deleting an input
        // changes the problem; the filter exists to stop DERIVED
        // ill-sorted fabrications.
        if derived && self.oracle.has_disjointness() {
            let mut filtered = Vec::with_capacity(lits.len());
            let mut dropped_positive = false;
            for (pos, t) in &lits {
                if self.atom_ill_sorted(t) {
                    if !*pos {
                        return None; // (not ill-sorted) ≡ tautology
                    }
                    dropped_positive = true;
                    continue;
                }
                filtered.push((*pos, t.clone()));
            }
            if dropped_positive && filtered.is_empty() {
                return None; // vacuous: only ill-sorted positives
            }
            lits = filtered;
        }

        // Theory propagation: discharge ground binary literals against
        // the oracle.  An entailed-FALSE negative literal is deleted
        // (unit resolution with a virtual entailed unit); a clause with
        // an entailed-TRUE positive literal is redundant (oracle-
        // subsumed) — except unit facts, which stay as index content.
        let mut kept: Vec<(bool, Term)> = Vec::with_capacity(lits.len());
        for (pos, t) in &lits {
            // Ground arithmetic comparisons decide outright: a FALSE
            // literal drops (unit resolution against arithmetic), a
            // TRUE literal satisfies the clause (redundant).
            if let Some(truth) = Self::ground_compare(t) {
                if truth != *pos {
                    self.stats.oracle_discharges += 1;
                    if self.want_notes() {
                        notes.push(format!(
                            "{} -- arithmetic", lit_kif(*pos, t, self.syn())));
                    }
                    continue;
                }
                self.stats.oracle_subsumed += 1;
                return None;
            }
            if let Some((decision, why, eq_clauses)) = self.ground_equality_holds(t) {
                match decision {
                    EqDecision::Entailed => {
                        if !*pos {
                            self.stats.oracle_discharges += 1;
                            if self.want_notes() {
                                notes.push(format!(
                                    "(not {}) -- oracle: {}",
                                    term_kif(t, self.syn()),
                                    if why.is_empty() { "x = x".to_string() }
                                    else { witnesses_kif(&why, self.syn()) }));
                            }
                            for w in &why {
                                if let Some(sid) = w.sid { fact_parents.push(sid); }
                            }
                            parents.extend(eq_clauses);
                            continue;
                        }
                        if lits.len() > 1 {
                            self.stats.oracle_subsumed += 1;
                            return None;
                        }
                    }
                    EqDecision::Refuted => {
                        // Mirror image: a FALSE positive equality drops
                        // (1 ≠ 2 by literal semantics); a satisfied
                        // negative one makes the clause redundant.
                        if *pos {
                            self.stats.oracle_discharges += 1;
                            if self.want_notes() {
                                notes.push(format!(
                                    "{} -- numeric disequality",
                                    term_kif(t, self.syn())));
                            }
                            continue;
                        }
                        if lits.len() > 1 {
                            self.stats.oracle_subsumed += 1;
                            return None;
                        }
                    }
                    EqDecision::Unknown => {}
                }
                kept.push((*pos, t.clone()));
                continue;
            }
            if let Some((rel, x, y)) = term_binary_ids(t) {
                // Disjointness refutation: `(instance x C)` is provably
                // FALSE when a known class of x is provably disjoint
                // from C (partition / disjoint declarations).  A FALSE
                // positive drops; a satisfied negative literal makes
                // the clause redundant.
                let mut why_r: Vec<Witness> = Vec::new();
                if self.oracle.refutes_instance(rel, x, y, Some(&mut why_r)) {
                    if *pos {
                        self.stats.oracle_discharges += 1;
                        if self.want_notes() {
                            notes.push(format!(
                                "{} -- oracle refutes: {}",
                                term_kif(t, self.syn()),
                                witnesses_kif(&why_r, self.syn())));
                        }
                        for w in &why_r {
                            if let Some(sid) = w.sid { fact_parents.push(sid); }
                        }
                        continue;
                    }
                    // A satisfied NEGATIVE literal makes the clause
                    // redundant — but never discard a CONJECTURE
                    // clause this way: in an inconsistent KB the same
                    // atom can be both refuted (disjointness) and
                    // derivable (rules), and the goal literal must
                    // stay for the search to prove positively (the
                    // paraconsistent reading every other prover gives
                    // these tests).
                    if tier != CONJECTURE {
                        self.stats.oracle_subsumed += 1;
                        return None;
                    }
                }
                // Memoized witness-free check first; the witness walk
                // (uncached) runs only for entailed atoms.
                if self.oracle.holds(rel, x, y, None) {
                    let mut why: Vec<Witness> = Vec::new();
                    let _ = self.oracle.holds(rel, x, y, Some(&mut why));
                    if !*pos {
                        self.stats.oracle_discharges += 1;
                        if self.want_notes() {
                            notes.push(format!(
                                "(not {}) -- oracle: {}",
                                term_kif(t, self.syn()),
                                witnesses_kif(&why, self.syn())));
                        }
                        for w in &why {
                            // Stored facts cite their sid; learned units
                            // cite the deriving clause so the unit's own
                            // derivation chain stays in the proof DAG.
                            if let Some(sid) = w.sid {
                                fact_parents.push(sid);
                            } else if let Some(cid) =
                                self.oracle.learned_src(w.rel, w.x, w.y)
                            {
                                parents.push(cid);
                            }
                        }
                        continue;
                    }
                    if lits.len() > 1 {
                        self.stats.oracle_subsumed += 1;
                        return None;
                    }
                }
            }
            kept.push((*pos, t.clone()));
        }
        let lits = kept;

        // Ground-list theory: the first sighting of each ground
        // `(ListFn …)` term synthesizes its extension — membership and
        // positional facts — as pending units (drained outside make).
        // SUMO's list axioms quantify over these; saturation cannot
        // enumerate a ground list's members without them.
        for (_, t) in &lits {
            self.collect_ground_lists(t);
        }

        // Depth AND size caps for derived clauses.  Depth alone is not
        // enough: substitution duplicates subterms, and SUMO's
        // recursive list machinery can grow term WIDTH without bound
        // (a 52 GB / 7-hour intern_atom death spiral at full-config
        // scale found this the hard way).
        if derived && lits.iter().any(|(_, t)| {
            term_depth(t) > self.opts.strategy.max_depth
                || term_size(t) > self.opts.strategy.max_term_size
        }) {
            self.stats.discarded_deep += 1;
            return None;
        }

        // Unit subsumption / simplification against the active units.
        let mut kept: Vec<(bool, Term)> = Vec::with_capacity(lits.len());
        for (pos, t) in &lits {
            if t.is_ground() {
                let atom = self.layer.atoms.intern_atom(t);
                if self.units.ground_unit(*pos, atom).is_some() {
                    self.stats.unit_subsumed += 1;
                    return None; // subsumed by an active unit
                }
                if let Some(cid) = self.units.ground_unit(!*pos, atom) {
                    self.stats.unit_simplified += 1;
                    if self.want_notes() {
                        notes.push(format!(
                            "{} -- refuted by unit clause {}",
                            lit_kif(*pos, t, self.syn()), cid));
                    }
                    parents.push(cid);
                    continue;
                }
            }
            // Open units, reached through the mask/residue index — THE
            // KEY EQUATION routes the target to exactly the patterns
            // whose ground seats agree with its coins (the flat
            // same-head scan went superlinear on deep searches).
            if let Some((h, ar)) = term_head_key(t) {
                // The shifted match target is built LAZILY: most
                // literals have zero open-unit candidates, and the
                // shift is a full tree clone.
                let mut target: Option<Term> = None;
                let (n_elems, seats) = classify_seats(t);
                let mut dropped = false;
                'pol: for same_pol in [*pos, !*pos] {
                    for u in self.units.open_candidates(same_pol, h, ar, n_elems, &seats) {
                        self.stats.open_match_attempts += 1;
                        let tgt = target.get_or_insert_with(|| shift_slots(t, MATCH_TARGET_OFF));
                        let n = u.nvars as usize + 1;
                        let mut s = self.take_scratch(n);
                        let hit = match_one_way(&u.pattern, tgt, &mut s);
                        self.put_scratch(s, n);
                        if hit {
                            self.stats.open_match_hits += 1;
                            if same_pol == *pos {
                                self.stats.unit_subsumed += 1;
                                return None;
                            }
                            self.stats.unit_simplified += 1;
                            if self.want_notes() {
                                notes.push(format!(
                                    "{} -- refuted by unit clause {}",
                                    lit_kif(*pos, t, self.syn()), u.clause));
                            }
                            parents.push(u.clause);
                            dropped = true;
                            break 'pol;
                        }
                    }
                }
                if dropped { continue; }
            }
            kept.push((*pos, t.clone()));
        }
        let lits = kept;

        // Any resulting ground positive unit extends the oracle: a binary
        // relation edge feeds the closure; a ground `(equal a b)` feeds
        // the equality union-find (helps later derivations — already-made
        // clauses are normalized by the input pre-pass).  The edge is
        // registered AFTER the clause is pushed (below) so the learned
        // entry can carry this clause's id as its proof-DAG source.
        let unit_edge = if lits.len() == 1 && lits[0].0 {
            if let Some((rel, x, y)) = term_binary_ids(&lits[0].1) {
                // Remember the constants: FD-derived equalities over
                // prover-local skolems must be re-buildable as terms
                // (the store's symbol cache has never seen them).
                if let Term::App(elems) = &lits[0].1 {
                    for el in elems {
                        if let Some(k) = eq_key(el) {
                            self.eq_terms.entry(k).or_insert_with(|| el.clone());
                        }
                    }
                }
                Some((rel, x, y))
            } else {
                if let Some((l, r)) = term_ground_equality_sides(&lits[0].1) {
                    if let (Some(ka), Some(kb)) = (self.term_eq_key(l), self.term_eq_key(r)) {
                        if ka != kb {
                            let (l, r) = (l.clone(), r.clone());
                            self.register_equality(l, r, ka, kb);
                        }
                    }
                }
                None
            }
        } else {
            None
        };
        // Negative ground units feed the oracle's exclusion store
        // (exhaustiveness case-elimination).
        let neg_unit_edge = if lits.len() == 1 && !lits[0].0 {
            term_binary_ids(&lits[0].1)
        } else {
            None
        };

        let clause = canonical_clause(lits, &self.layer.atoms);

        // Tautology check on the canonical literals.
        let pos_atoms: Set64<AtomId> =
            clause.lits.iter().filter(|l| l.pos).map(|l| l.atom).collect();
        if clause.lits.iter().any(|l| !l.pos && pos_atoms.contains(&l.atom)) {
            return None;
        }

        // Schema channel: theory-rule shapes register with their
        // oracle registries, and the fully-replaced ones (symmetry
        // rule / symmetry metaschema) are absorbed outright.  This
        // catches DERIVED instances too — the metaschema resolving
        // against `(instance R SymmetricRelation)` births exactly the
        // per-R symmetry rule, which dies here at birth.  Never for
        // CONJECTURE-tier clauses: a negated existential conjecture
        // (`exists x y. R(x,y) ∧ ¬R(y,x)`) IS the symmetry shape, and
        // absorbing it would erase the goal (and break vacuity
        // detection, which requires a negated_conjecture proof step).
        if self.opts.strategy.schema
            && tier != CONJECTURE
            && clause.nvars >= 1
            && clause.lits.len() <= 4
        {
            if let Some(hit) =
                self.layer.schema.probe(&clause.lits, &self.layer.atoms, self.syn())
            {
                self.stats.schema_hits += 1;
                if self.apply_schema_hit(&hit, source) {
                    self.stats.schema_absorbed += 1;
                    return None;
                }
            }
        }

        // Slot-form terms, lifted once (canonical vars → dense slots).
        let terms: Vec<(bool, Term)> = clause
            .lits
            .iter()
            .filter_map(|l| {
                slot_atom(&self.layer.atoms, self.syn(), l.atom, 0).map(|t| (l.pos, t))
            })
            .collect();
        debug_assert_eq!(terms.len(), clause.lits.len());

        // Forward subsumption: an active clause already covers this one
        // ⇒ it is redundant, drop it (the flooding floor).  The new
        // clause is not yet in the arena, so no self-subsumption.
        if let Some(_by) = self.forward_subsumed(&clause.lits, &terms) {
            self.stats.subsumed += 1;
            return None;
        }

        let size: u64 = terms.iter().map(|(_, t)| term_size(t) as u64).sum();
        // Generative-existential throttle: every skolem-function
        // application multiplies the clause's weight, so self-feeding
        // chains (sk1(sk0(x))…) sink in the queue instead of flooding
        // it.  One skolem (an ordinary existential witness) costs a
        // factor of 2 — mild; nested chains grow superlinearly.
        let skolems: u64 = terms.iter().map(|(_, t)| term_skolem_apps(t)).sum();
        // Parameterized clause-weight function (the selection genome).
        // Defaults (cw_lits=1, cw_size=1, cw_vars=2, cw_skolem=1) reproduce
        // the historical `(#lits + size + 2·#vars)·(1 + skolems)`.
        let st = &self.opts.strategy;
        let base = (st.cw_lits * clause.lits.len() as u64
            + st.cw_size * size
            + st.cw_vars * u64::from(clause.nvars))
            .max(1)
            * (1 + st.cw_skolem * skolems);
        // Conjecture-distance factor: structurally goal-near clauses
        // keep their base weight; goal-far ones sink (×1..×1+W).
        let dist = self.goal_distance_factor(&clause.lits, tier);
        // KBO-maximal literals (ordered-inference eligibility).  Only
        // computed when an ordered rule needs it; otherwise all-maximal
        // (no restriction) so the unordered default pays nothing.
        let max_mask = self.maximal_literals(&clause.lits);
        let id = self.clauses.len() as u32;
        self.clauses.push(ClauseRec {
            id,
            lits: clause.lits,
            terms,
            nvars: clause.nvars,
            key: clause.key,
            parents,
            fact_parents,
            source,
            rule,
            tier,
            weight: base * self.opts.strategy.tier_weight[tier as usize] * dist,
            activated: false,
            max_mask,
            notes,
        });
        if let Some((rel, x, y)) = unit_edge {
            self.oracle.add_unit(rel, x, y, Some(id));
        }
        if let Some((rel, x, y)) = neg_unit_edge {
            self.oracle.add_neg_unit(rel, x, y, Some(id));
        }
        Some(id)
    }

    // -- queue ------------------------------------------------------------------

    /// Queue a made clause for given selection.  `None` (redundant) and
    /// already-seen/over-long clauses are dropped.
    pub(crate) fn push(&mut self, id: Option<u32>) -> Option<u32> {
        let id = id?;
        let c = &self.clauses[id as usize];
        if self.seen.contains(&c.key) { return None; }
        if c.lits.len() > self.opts.max_lits {
            self.stats.discarded_long += 1;
            return None;
        }
        self.seen.insert(c.key);
        let (w, n) = (c.weight, self.seq);
        self.seq += 1;
        self.h_weight.push(Reverse((w, n, id)));
        self.h_age.push(Reverse((n, id)));
        Some(id)
    }

    fn pop_given(&mut self) -> Option<u32> {
        self.tick += 1;
        let prefer_age = self.tick % self.opts.strategy.pick_ratio.max(1) == 0;
        for pass in 0..2 {
            let from_age = prefer_age == (pass == 0);
            if from_age {
                while let Some(Reverse((_, id))) = self.h_age.pop() {
                    if self.popped.insert(id) { return Some(id); }
                }
            } else {
                while let Some(Reverse((_, _, id))) = self.h_weight.pop() {
                    if self.popped.insert(id) { return Some(id); }
                }
            }
        }
        None
    }

    /// Index a clause's literals and register unit facts.
    pub(crate) fn activate(&mut self, id: u32) {
        if self.clauses[id as usize].activated { return; }
        self.clauses[id as usize].activated = true;
        let lits = self.clauses[id as usize].lits.clone();
        let layer = self.layer;
        let src = move |a| layer.atom_info(a);
        for (i, l) in lits.iter().enumerate() {
            self.idx.add(EntryRef { clause: id, lit: i as u8 }, l.pos, l.atom, &src);
        }
        if lits.len() == 1 {
            let layer = self.layer;
            let nv = self.clauses[id as usize].nvars;
            self.units.add_unit(
                id, lits[0].pos, lits[0].atom, nv,
                &layer.atom_infos, &layer.atoms, &layer.semantic.syntactic);
        }
        if self.opts.strategy.superposition {
            self.index_superposition(id);
        }
    }

    /// Whether ordered superposition's maximality machinery is needed.
    #[inline]
    fn needs_superposition(&self) -> bool {
        self.opts.strategy.superposition
    }

    /// Index one clause's superposition surfaces: every non-variable
    /// subterm position of its MAXIMAL literals (the "into" targets), and
    /// its maximal positive equality literals oriented `s ≻ t` (the
    /// "from" equations).  Idempotent per (clause, position) — content
    /// addressing dedups the subterm atoms.
    fn index_superposition(&mut self, id: u32) {
        let (lits, max_mask) = {
            let c = &self.clauses[id as usize];
            (c.lits.clone(), c.max_mask)
        };
        for (li, l) in lits.iter().enumerate() {
            if li >= 64 || (max_mask >> li) & 1 == 0 {
                continue; // non-maximal literals are not inference-eligible
            }
            let Some(t) = slot_atom(&self.layer.atoms, self.syn(), l.atom, 0) else { continue };
            // Subterm positions (non-var, non-top) → the "into" index.
            for (path, sub) in positions(&t) {
                let sub_atom = self.layer.atoms.intern_atom(&sub);
                let info = self.layer.atom_info(sub_atom);
                let path: smallvec::SmallVec<[u8; 4]> =
                    path.iter().map(|&p| p as u8).collect();
                self.term_idx.add(
                    super::index::TermPos { clause: id, lit: li as u8, path },
                    sub_atom, &info);
            }
            // Maximal positive equality oriented s ≻ t → the "from" set.
            if l.pos && is_equality_atom(&t) {
                if self.equality_oriented(&t).is_some() {
                    self.active_eqns.push((id, li as u8));
                } else {
                    // KBO can't orient it (e.g. `X = agatha`,
                    // commutativity): the "from" index skips it — a
                    // completeness loss strict saturation must count.
                    self.stats.unorientable_eqs += 1;
                }
            }
        }
    }

    /// If `t` is an equality atom `(equal s u)` whose sides are KBO-
    /// comparable with a strictly larger side, return `(s, u)` ORIENTED
    /// so the first is the larger — else `None` (unorientable / non-eq).
    fn equality_oriented(&self, t: &Term) -> Option<(Term, Term)> {
        let Term::App(elems) = t else { return None };
        if elems.len() != 3 || !matches!(elems[0], Term::Op(OpKind::Equal)) {
            return None;
        }
        let (a, b) = (&elems[1], &elems[2]);
        let ai = self.layer.atoms.intern_atom(a);
        let bi = self.layer.atoms.intern_atom(b);
        match self.kbo().compare(ai, bi, &self.layer.atoms, self.syn()) {
            super::kbo::KboCmp::Greater => Some((a.clone(), b.clone())),
            super::kbo::KboCmp::Less => Some((b.clone(), a.clone())),
            _ => None,
        }
    }

    /// Rebuild the superposition indexes from the activated arena — used
    /// after snapshot hydrate / background masking, where clauses were
    /// activated in another prover instance.
    fn rebuild_superposition_index(&mut self) {
        self.term_idx = super::index::TermIndex::default();
        self.active_eqns.clear();
        let n = self.clauses.len() as u32;
        for id in 0..n {
            if self.clauses[id as usize].activated {
                self.index_superposition(id);
            }
        }
    }

    /// One ordered-superposition inference: rewrite clause `t_cid`'s
    /// literal `t_li` at the non-variable subterm position `t_path`
    /// using equation clause `e_cid`'s maximal positive equality at
    /// literal `e_li`, oriented `s ≻ t`.  σ = mgu(s, u) where `u` is the
    /// target subterm; the resolvent is `(rest_E ∨ rest_T ∨ T[t])σ`
    /// (Bachmair–Ganzinger superposition, positive and negative variants
    /// unified — `t`'s literal polarity is preserved by `replace`).
    /// Routed through `make` (demodulation + subsumption + dedup).
    /// Returns the new clause id, or `None` when the inference is
    /// inapplicable (non-unifiable, into a variable, unorientable).
    fn superpose(
        &mut self,
        e_cid: u32, e_li: usize,
        t_cid: u32, t_li: usize,
        t_path: &[usize],
    ) -> Option<u32> {
        // The "from" equation, oriented so the first side is KBO-larger.
        let (e_terms, e_tier) = {
            let c = &self.clauses[e_cid as usize];
            (c.terms.clone(), c.tier)
        };
        let (s, t) = self.equality_oriented(&e_terms[e_li].1)?;

        // The "into" clause and its rewrite target `u` — never a variable
        // (superposition into variables is unsound for completeness and
        // explosive).
        let (t_terms, t_nvars, t_tier) = {
            let c = &self.clauses[t_cid as usize];
            (c.terms.clone(), c.nvars, c.tier)
        };
        let u = subterm_at(&t_terms[t_li].1, t_path)?.clone();
        if matches!(u, Term::Var(_)) { return None; }

        // Rename the equation apart from the target: shift its slots
        // above the target's variable range.
        let off = u64::from(t_nvars) + 1;
        let s2 = shift_slots(&s, off);
        let t2 = shift_slots(&t, off);

        // Size a substitution table covering both clauses' slot ranges.
        let mut slots = std::collections::BTreeSet::new();
        super::unify::term_slots(&s2, &mut slots);
        super::unify::term_slots(&t2, &mut slots);
        for (_, term) in &t_terms {
            super::unify::term_slots(term, &mut slots);
        }
        let max_slot = slots.iter().max().copied().unwrap_or(off);
        let mut subst: Subst = vec![None; (max_slot + 1) as usize];

        // σ = mgu(s, u).
        if !unify(&s2, &u, &mut subst) { return None; }

        // Resolvent: rest of E (renamed, σ) ∨ T with its `u` subterm
        // replaced by `t` (renamed, σ).
        let mut lits: Vec<(bool, Term)> =
            Vec::with_capacity(e_terms.len() + t_terms.len());
        for (k, (pos, term)) in e_terms.iter().enumerate() {
            if k == e_li { continue; }
            lits.push((*pos, apply(&shift_slots(term, off), &subst)));
        }
        for (k, (pos, term)) in t_terms.iter().enumerate() {
            let rewritten =
                if k == t_li { replace(term, t_path, &t2) } else { term.clone() };
            lits.push((*pos, apply(&rewritten, &subst)));
        }
        self.make(lits, vec![e_cid, t_cid], "superpos", e_tier.min(t_tier), None, true)
    }

    /// Ordered superposition for the given clause `given`, both
    /// directions against the active set — the equality-complete
    /// replacement for `paramodulants`.  Returns the empty-clause id on
    /// refutation.
    ///
    /// - **into** (`given` as target): each active oriented equation
    ///   rewrites a non-variable subterm of `given`'s maximal literals.
    /// - **from** (`given` as equation): each of `given`'s maximal
    ///   positive oriented equalities rewrites a subterm of an active
    ///   clause (probed from the `TermIndex`).
    ///
    /// `given` is not yet activated (it joins the indexes after this), so
    /// the two directions together cover every given×active pair without
    /// double-counting.
    fn superposition_inferences(&mut self, given: u32) -> Option<u32> {
        let (g_terms, g_max) = {
            let c = &self.clauses[given as usize];
            (c.terms.clone(), c.max_mask)
        };
        let mut n = 0usize;
        let cap = self.opts.strategy.para_cap;

        // -- into: active equations rewrite `given`'s maximal subterms.
        let eqns = self.active_eqns.clone();
        for (li, (_, atom)) in g_terms.iter().enumerate() {
            if li >= 64 || (g_max >> li) & 1 == 0 { continue; }
            for (path, _sub) in positions(atom) {
                for &(e_cid, e_li) in &eqns {
                    let made = self.superpose(e_cid, e_li as usize, given, li, &path);
                    if let Some(cid) = made {
                        if self.clauses[cid as usize].lits.is_empty() {
                            if let Some(e) = self.reportable_refutation(cid) {
                                return Some(e);
                            }
                            continue;
                        }
                    }
                    self.push(made);
                    n += 1;
                    if n >= cap { self.stats.gen_capped += 1; return None; }
                }
            }
        }

        // -- from: `given`'s maximal positive equalities rewrite active
        // subterms (probe the "into" TermIndex with the larger side `s`).
        for (li, (pos, atom)) in g_terms.iter().enumerate() {
            if li >= 64 || (g_max >> li) & 1 == 0 || !*pos { continue; }
            let Some((s, _t)) = self.equality_oriented(atom) else { continue };
            let s_atom = self.layer.atoms.intern_atom(&s);
            let qi = self.layer.atom_info(s_atom);
            let layer = self.layer;
            let src = move |a| layer.atom_info(a);
            let targets = self.term_idx.probe(&qi, &src);
            for tp in targets {
                let path: Vec<usize> = tp.path.iter().map(|&p| p as usize).collect();
                let made = self.superpose(given, li, tp.clause, tp.lit as usize, &path);
                if let Some(cid) = made {
                    if self.clauses[cid as usize].lits.is_empty() {
                        if let Some(e) = self.reportable_refutation(cid) {
                            return Some(e);
                        }
                        continue;
                    }
                }
                self.push(made);
                n += 1;
                if n >= cap { self.stats.gen_capped += 1; return None; }
            }
        }
        None
    }

    // -- problem loading ---------------------------------------------------------

    /// Add a stored root's clauses as BACKGROUND: straight into the
    /// active index (classic set-of-support background), never given.
    pub(crate) fn add_background_root(&mut self, root: SentenceId) {
        self.bg_roots.insert(root);
        for pc in self.layer.clauses_for(root).iter() {
            let Some(terms) = self.pclause_terms(pc) else { continue };
            if let Some(id) = self.make(terms, vec![], "axiom", BACKGROUND, Some(root), false) {
                let key = self.clauses[id as usize].key;
                if self.clauses[id as usize].lits.len() > self.opts.max_lits {
                    // An INPUT clause over the literal cap never enters
                    // the index: the loaded theory is incomplete, and a
                    // later saturation must not be read as a model.
                    self.stats.discarded_long += 1;
                    continue;
                }
                if self.seen.insert(key) {
                    // Full-saturation regime: background clauses also
                    // compete for given selection (axiom×axiom inference).
                    // Classic set-of-support only indexes them as passive
                    // partners — structurally unable to refute problems
                    // whose proof needs case analysis among the axioms.
                    if self.opts.strategy.full_saturation {
                        let (w, n) = (self.clauses[id as usize].weight, self.seq);
                        self.seq += 1;
                        self.h_weight.push(Reverse((w, n, id)));
                        self.h_age.push(Reverse((n, id)));
                    }
                    self.activate(id);
                }
            }
        }
    }

    /// Add a stored root's clauses as SUPPORT (problem hypotheses):
    /// rules into the passive queue, units activated + seeded.
    pub(crate) fn add_support_root(&mut self, root: SentenceId) {
        for pc in self.layer.clauses_for(root).iter() {
            let Some(terms) = self.pclause_terms(pc) else { continue };
            if let Some(id) = self.make(terms, vec![], "hypothesis", SUPPORT, Some(root), false) {
                self.add_support_clause(id);
            }
        }
    }

    fn add_support_clause(&mut self, id: u32) {
        let (n_lits, key) = {
            let c = &self.clauses[id as usize];
            (c.lits.len(), c.key)
        };
        if n_lits != 1 {
            // Multi-literal hypotheses queue as support; an EMPTY clause
            // (the inputs alone are contradictory — e.g. `p` and
            // `(not p)` both asserted) queues too, so `run` pops it and
            // reports the refutation instead of indexing nothing.
            self.push(Some(id));
        } else if self.seen.insert(key) {
            let l = self.clauses[id as usize].lits[0];
            if l.pos && self.layer.atom_info(l.atom).is_ground() {
                self.support_seeds.push((l.atom, id));
            }
        }
        self.activate(id);
    }

    /// Add the negated conjecture's clauses (already clausified by the
    /// caller, `negate=true`).  Ground positive binary units feed the
    /// oracle first — they are assumptions of this refutation.
    pub(crate) fn add_conjecture_clauses(&mut self, clauses: &[super::clause::PClause]) {
        self.has_conjecture = true;
        // No source clause id yet — `make` below re-registers each unit
        // with its clause id (add_unit upgrades None → Some).
        for pc in clauses {
            if pc.lits.len() == 1 && pc.lits[0].pos {
                if let Some(terms) = self.pclause_terms(pc) {
                    if let Some((rel, x, y)) = term_binary_ids(&terms[0].1) {
                        self.oracle.add_unit(rel, x, y, None);
                    }
                }
            }
        }
        for pc in clauses {
            let Some(terms) = self.pclause_terms(pc) else { continue };
            let id = self.make(terms, vec![], "negated_conjecture", CONJECTURE, None, false);
            self.push(id);
        }
    }

    pub(crate) fn pclause_terms(&self, pc: &super::clause::PClause) -> Option<Vec<(bool, Term)>> {
        pc.lits
            .iter()
            .map(|l| slot_atom(&self.layer.atoms, self.syn(), l.atom, 0).map(|t| (l.pos, t)))
            .collect()
    }

    /// Is clause `id` an activated, KBO-orientable positive unit equality
    /// — i.e. a demodulator that completion can superpose with?
    fn is_unit_equation(&self, id: u32) -> bool {
        let c = &self.clauses[id as usize];
        c.activated
            && c.terms.len() == 1
            && c.terms[0].0
            && self.equality_oriented(&c.terms[0].1).is_some()
    }

    /// The activated unit-equation clause ids, in arena (deterministic)
    /// order — completion's working set.
    fn unit_equation_ids(&self) -> Vec<u32> {
        (0..self.clauses.len() as u32)
            .filter(|&id| self.is_unit_equation(id))
            .collect()
    }

    /// Phase 6 — bounded background completion (Knuth–Bendix-style).  Run
    /// ONCE before the main loop: superpose the active unit equations
    /// against each other, keeping every *new* oriented unit equation as a
    /// demodulator, to a hard budget (completion can diverge; the budget
    /// is the terminator).  The payoff is that proof-time equational
    /// rewriting becomes cheap one-way demodulation against this richer,
    /// closer-to-confluent rule set instead of repeated live superposition.
    /// Sound: every product is `superpose`'d from two equational parents,
    /// so it is an equational consequence of the background.  Gated by
    /// `Strategy.bg_completion`; deterministic for a fixed input.
    pub(crate) fn complete_background(&mut self) {
        if !self.opts.strategy.bg_completion {
            return;
        }
        let budget = self.opts.strategy.bg_completion_budget.max(1);
        let mut produced = 0usize;
        let mut attempts = 0usize;
        let hard = budget.saturating_mul(16); // attempt backstop
        // LIFO frontier of equation ids still to superpose against the set;
        // newly derived equations join it (the closure's fixpoint engine).
        let mut frontier: Vec<u32> = self.unit_equation_ids();
        while let Some(eid) = frontier.pop() {
            if produced >= budget || attempts >= hard {
                break;
            }
            if !self.is_unit_equation(eid) {
                continue;
            }
            // `eid`'s oriented larger side rewrites the partners' subterms.
            let partners = self.unit_equation_ids();
            'partners: for p in partners {
                if p == eid {
                    continue;
                }
                let Some(p_atom) =
                    slot_atom(&self.layer.atoms, self.syn(), self.clauses[p as usize].lits[0].atom, 0)
                else { continue };
                for (path, _sub) in positions(&p_atom) {
                    attempts += 1;
                    if attempts >= hard {
                        break 'partners;
                    }
                    let Some(nid) = self.superpose(eid, 0, p, 0, &path) else { continue };
                    // Keep only genuinely new oriented unit equations.
                    let key = self.clauses[nid as usize].key;
                    if self.is_unit_equation_unactivated(nid) && self.seen.insert(key) {
                        self.activate(nid);
                        frontier.push(nid);
                        produced += 1;
                        if produced >= budget {
                            break 'partners;
                        }
                    }
                }
            }
        }
        self.stats.bg_completed = produced as u64;
    }

    /// Like `is_unit_equation` but for a freshly `make`'d (not-yet
    /// activated) clause — completion's acceptance test for a product.
    fn is_unit_equation_unactivated(&self, id: u32) -> bool {
        let c = &self.clauses[id as usize];
        c.terms.len() == 1
            && c.terms[0].0
            && self.equality_oriented(&c.terms[0].1).is_some()
    }

    // -- inference rules -----------------------------------------------------------

    /// Binary resolution on complementary eligible literals.
    fn resolve(&mut self, given: u32, gi: usize, partner: u32, pi: usize) -> Option<u32> {
        // Algebraic fast path: when the partner is a ground unit fact and
        // the given literal's open seats are simple variables, the
        // bindings decode straight out of the two atoms' power-sum
        // residual — no rename-apart, no unification walk.  Any anomaly
        // falls through to the general path below.
        if let Some(result) = self.resolve_decoded(given, gi, partner, pi) {
            return result;
        }
        let (g_nvars, p_nvars, p_is_ground_unit) = {
            let g = &self.clauses[given as usize];
            let p = &self.clauses[partner as usize];
            (g.nvars, p.nvars, p.lits.len() == 1 && p.nvars == 0)
        };
        self.stats.resolve_unify_attempts += 1;
        if p_is_ground_unit {
            self.stats.resolve_ground_partner += 1;
        }
        let off = g_nvars as u64 + 1;
        let n = (off + u64::from(p_nvars) + 1) as usize;
        let mut s = self.take_scratch(n);
        let mut via_symmetry: Option<SymbolId> = None;
        let resolvent: Option<Vec<(bool, Term)>> = {
            let g = &self.clauses[given as usize];
            let p = &self.clauses[partner as usize];
            // Rename-apart is VIRTUAL (slot offsets inside the
            // unifier): the partner literal is never materialized in
            // shifted form, so a failed attempt costs no allocation.
            let p_lit = &p.terms[pi].1;
            let mut matched = unify_off(&g.terms[gi].1, 0, p_lit, off, &mut s);
            if !matched {
                // Resolution modulo symmetry: when the given literal's
                // head is a known symmetric relation, retry with its
                // arguments swapped.  `R(s,t) ⊢ R(t,s)` is licensed by
                // the relation's symmetry source, cited on the
                // resolvent below.
                if let Some((rel, sw)) = self.symmetric_swap_term(&g.terms[gi].1) {
                    for slot in s.iter_mut().take(n) {
                        *slot = None; // discard partial bindings
                    }
                    if unify_off(&sw, 0, p_lit, off, &mut s) {
                        matched = true;
                        via_symmetry = Some(rel);
                    }
                }
            }
            if !matched {
                None // hash-collision reject
            } else {
                let mut new: Vec<(bool, Term)> =
                    Vec::with_capacity(g.terms.len() + p.terms.len() - 2);
                for (k, (pos, t)) in g.terms.iter().enumerate() {
                    if k != gi { new.push((*pos, apply(t, &s))); }
                }
                for (k, (pos, t)) in p.terms.iter().enumerate() {
                    if k != pi { new.push((*pos, apply_off(t, off, &s))); }
                }
                // Drop duplicate literals.
                let mut out: Vec<(bool, Term)> = Vec::with_capacity(new.len());
                for (pos, t) in new {
                    if !out.iter().any(|(p2, u)| *p2 == pos && *u == t) {
                        out.push((pos, t));
                    }
                }
                Some(out)
            }
        };
        self.put_scratch(s, n);
        let Some(out) = resolvent else { return None };
        self.stats.resolve_unify_hits += 1;
        self.stats.resolvents += 1;
        let tier = self.clauses[given as usize].tier.min(self.clauses[partner as usize].tier);
        let rule = if via_symmetry.is_some() { "resolve_sym" } else { "resolve" };
        let made = self.make(out, vec![given, partner], rule, tier, None, true);
        if let (Some(id), Some(rel)) = (made, via_symmetry) {
            self.stats.sym_resolutions += 1;
            if let Some(sid) = self.oracle.symmetric_source(rel) {
                self.clauses[id as usize].fact_parents.push(sid);
            }
        }
        made
    }

    /// Algebraic parameter extraction in the resolution hot loop.
    ///
    /// Applicability: partner is a ground UNIT clause; the given
    /// literal's atom has ≤ 2 open seats, each a simple top-level
    /// variable.  Then the two atoms' residual ⟨ΔS₁, ΔS₃⟩ is the sketch
    /// of exactly the fact-coins filling the open seats — decode it,
    /// look the coins up in the phone book, and the substitution falls
    /// out without `slot_atom` + `unify` ever touching the fact.
    ///
    /// Returns `None` ⇒ not applicable / anomaly ⇒ caller runs the
    /// general unification path (the universal fallback, so this can
    /// never make the prover *wrong*, only faster).
    /// The given literal's decode eligibility, computed ONCE per
    /// literal (it is partner-independent): ≤2 open seats, each a
    /// bare variable.  `None` ⇒ every partner takes the general path.
    fn decode_given_shape(&self, given: u32, gi: usize) -> Option<DecodeShape> {
        // A/B kill switch for benchmarking the algebraic fast path
        // (`SIGMA_NO_DECODE` via `Strategy::default`, or per lane).
        if !self.opts.strategy.decode {
            return None;
        }
        let g = &self.clauses[given as usize];
        let gi_info = self.layer.atom_info(g.lits[gi].atom);
        let m = gi_info.mask.count_ones();
        if m > 2 {
            return None;
        }
        // Every open seat must be a simple variable in the pattern term
        // (a compound-with-variable seat needs real unification).
        let Term::App(p_elems) = &g.terms[gi].1 else { return None };
        let mut open_slots: SmallVec<[(u8, u64); 2]> = SmallVec::new(); // (seat, slot)
        let mut bits = gi_info.mask;
        while bits != 0 {
            let seat = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            match p_elems.get(seat) {
                Some(Term::Var(slot)) => open_slots.push((seat as u8, *slot)),
                _ => return None,
            }
        }
        Some(DecodeShape {
            m,
            open_slots,
            base_residue: gi_info.base_residue,
            s3: gi_info.s3,
            arity: gi_info.arity,
            g_nvars: g.nvars,
            g_tier: g.tier,
        })
    }

    /// The partner-side gate + residual: `Some` iff `partner` is a
    /// ground unit of matching arity — then the residual sketch is the
    /// XOR of the two atoms' power sums (THE KEY EQUATION's decodable
    /// remainder).
    fn partner_residual(
        &self,
        shape: &DecodeShape,
        partner: u32,
        pi: usize,
    ) -> Option<crate::gf64::Sketch> {
        let p = &self.clauses[partner as usize];
        if p.lits.len() != 1 || p.nvars != 0 || pi != 0 {
            return None;
        }
        let pi_info = self.layer.atom_info(p.lits[0].atom);
        if !pi_info.is_ground() || shape.arity != pi_info.arity {
            return None;
        }
        Some(crate::gf64::Sketch {
            s1: shape.base_residue ^ pi_info.base_residue,
            s3: shape.s3 ^ pi_info.s3,
        })
    }

    /// The decode tail: phone-book the coins into a substitution and
    /// build the resolvent.  `None` ⇒ anomaly (collision, unknown
    /// coin, seat mismatch) — the caller falls back to general
    /// unification, which reaches the same verdict the slow way.
    fn resolve_from_decoded(
        &mut self,
        given: u32,
        gi: usize,
        partner: u32,
        shape: &DecodeShape,
        decoded: crate::gf64::Decoded,
    ) -> Option<Option<u32>> {
        use crate::gf64::Decoded;
        let coins: SmallVec<[u64; 2]> = match decoded {
            Decoded::None => SmallVec::new(),
            Decoded::One(c) => SmallVec::from_slice(&[c]),
            Decoded::Two(a, b) => SmallVec::from_slice(&[a, b]),
            Decoded::Fail => return None,
        };

        // Phone book: each coin must name exactly one expected open seat.
        let syn = &self.layer.semantic.syntactic;
        let mut s: Subst = vec![None; shape.g_nvars as usize + 1];
        let mut seen_seats: SmallVec<[u8; 2]> = SmallVec::new();
        for c in coins {
            let (seat, term) = self.layer.atom_infos.coin_term(c, &self.layer.atoms, syn)?;
            let Some(&(_, slot)) = shape.open_slots.iter().find(|(st, _)| *st == seat) else {
                return None; // decoded a seat the pattern didn't open
            };
            if seen_seats.contains(&seat) {
                return None;
            }
            seen_seats.push(seat);
            match &s[slot as usize] {
                None => s[slot as usize] = Some(term),
                // Repeated variable: both seats must decode equal fillers.
                Some(prev) if *prev == term => {}
                Some(_) => return None, // genuinely no resolvent — but let
                                        // unify reach the same verdict
            }
        }
        if seen_seats.len() != shape.open_slots.len() {
            return None; // every open seat must receive exactly one binding
        }

        // Build the resolvent: the given's other literals under σ; the
        // unit partner contributes nothing.
        let lits: Vec<(bool, Term)> = self.clauses[given as usize]
            .terms
            .iter()
            .enumerate()
            .filter(|(k, _)| *k != gi)
            .map(|(_, (pos, t))| (*pos, apply(t, &s)))
            .collect();
        self.stats.resolvents += 1;
        self.stats.decoded_resolutions += 1;
        let tier = shape.g_tier.min(self.clauses[partner as usize].tier);
        Some(self.make(lits, vec![given, partner], "resolve", tier, None, true))
    }

    /// Scalar composition of the three pieces — `resolve`'s fast path.
    fn resolve_decoded(
        &mut self,
        given: u32,
        gi: usize,
        partner: u32,
        pi: usize,
    ) -> Option<Option<u32>> {
        let shape = self.decode_given_shape(given, gi)?;
        let residual = self.partner_residual(&shape, partner, pi)?;
        let decoded = crate::gf64::decode(residual, shape.m);
        self.resolve_from_decoded(given, gi, partner, &shape, decoded)
    }

    /// Factor pairs of same-polarity unifiable literals.
    fn factors(&mut self, given: u32) -> Vec<Option<u32>> {
        let (terms, nvars, tier, lits) = {
            let c = &self.clauses[given as usize];
            (c.terms.clone(), c.nvars, c.tier, c.lits.clone())
        };
        // Memoized per-literal sketch info for the coin-level guard.
        let infos: Vec<_> = lits.iter()
            .map(|l| self.layer.atom_info(l.atom))
            .collect();
        let mut out = Vec::new();
        for i in 0..terms.len() {
            for j in (i + 1)..terms.len() {
                if terms[i].0 != terms[j].0 { continue; }
                self.stats.factor_attempts += 1;
                // Coin-level refutation: a seat ground in BOTH literals
                // with different coins (= different content) admits no
                // unifier; arity mismatch likewise.  Seats open on
                // either side are unconstrained.
                if infos[i].arity != infos[j].arity
                    || infos[i].seat_coins.iter().zip(infos[j].seat_coins.iter())
                        .any(|(&a, &b)| a != 0 && b != 0 && a != b)
                {
                    self.stats.factor_prefiltered += 1;
                    continue;
                }
                let n = nvars as usize + 1;
                let mut s = self.take_scratch(n);
                let lits: Option<Vec<(bool, Term)>> =
                    if unify(&terms[i].1, &terms[j].1, &mut s) {
                        Some(terms
                            .iter()
                            .enumerate()
                            .filter(|(k, _)| *k != j)
                            .map(|(_, (pos, t))| (*pos, apply(t, &s)))
                            .collect())
                    } else {
                        None
                    };
                self.put_scratch(s, n);
                if let Some(lits) = lits {
                    self.stats.factor_hits += 1;
                    out.push(self.make(lits, vec![given], "factor", tier, None, true));
                }
            }
        }
        out
    }

    /// Equality resolution: for each negative literal `(equal s t)` in
    /// `given`, if `s` and `t` unify (mgu σ), emit the clause with that
    /// literal removed and σ applied.  This is what discharges a
    /// negative equality constraint by binding — `C ∨ ?E≠Human` becomes
    /// `Cσ` with `?E↦Human`.  Reflexive `s≠s` is the degenerate case
    /// (σ empty), which simply drops the literal.
    fn equality_resolutions(&mut self, given: u32) -> Vec<Option<u32>> {
        let (terms, nvars, tier) = {
            let c = &self.clauses[given as usize];
            (c.terms.clone(), c.nvars, c.tier)
        };
        let mut out = Vec::new();
        for (i, (pos, t)) in terms.iter().enumerate() {
            if *pos { continue; } // negative literals only
            let Term::App(elems) = t else { continue };
            if elems.len() != 3 || !matches!(elems[0], Term::Op(OpKind::Equal)) {
                continue;
            }
            let mut s: Subst = vec![None; nvars as usize + 1];
            if unify(&elems[1], &elems[2], &mut s) {
                let lits: Vec<(bool, Term)> = terms
                    .iter()
                    .enumerate()
                    .filter(|(k, _)| *k != i)
                    .map(|(_, (p, lt))| (*p, apply(lt, &s)))
                    .collect();
                out.push(self.make(lits, vec![given], "eq_resolve", tier, None, true));
            }
        }
        out
    }

    /// Test-only: the rendered `(polarity, KIF)` literals of a clause.
    #[cfg(test)]
    pub(crate) fn dbg_lits(&self, id: u32) -> Vec<(bool, String)> {
        self.clauses[id as usize].terms.iter()
            .map(|(p, t)| (*p, term_kif(t, self.syn())))
            .collect()
    }

    /// KBO comparison of two slot-variable terms (intern + memoized
    /// compare) — the ordering check equality factoring's side conditions
    /// stand on.
    fn kbo_terms(&self, a: &Term, b: &Term) -> super::kbo::KboCmp {
        let ai = self.layer.atoms.intern_atom(a);
        let bi = self.layer.atoms.intern_atom(b);
        self.kbo().compare(ai, bi, &self.layer.atoms, self.syn())
    }

    /// Equality factoring — the completeness corner of the superposition
    /// calculus.  From a clause with two positive equality literals
    /// `s ≈ t` (literal `i`, eligible/maximal) and `u ≈ v` (literal `j`),
    /// with `σ = mgu(s, u)`, derive `(s ≈ v ∨ t ≉ v ∨ rest)σ` — the
    /// `u ≈ v` literal is merged into `s ≈ v` (since `sσ = uσ`) at the
    /// cost of the residue `t ≉ v`.  Required for refutational
    /// completeness with positive equality literals (Bachmair–Ganzinger).
    ///
    /// Soundness holds for ANY orientation of the two literals (equals-
    /// for-equals); the KBO side conditions (unify the larger sides, `i`
    /// maximal) only prune redundant inferences, so unorientable literals
    /// fall back to trying both sides.  `make` then demodulates / subsumes
    /// / dedups the result.
    pub(crate) fn equality_factors(&mut self, given: u32) -> Vec<Option<u32>> {
        let (terms, nvars, tier, max_mask) = {
            let c = &self.clauses[given as usize];
            (c.terms.clone(), c.nvars, c.tier, c.max_mask)
        };
        let mut out = Vec::new();
        for i in 0..terms.len() {
            // `s ≈ t` must be a positive, eligible (maximal) equality.
            if !terms[i].0 || (i < 64 && (max_mask >> i) & 1 == 0) { continue; }
            let Some((ai, bi)) = eq_sides(&terms[i].1) else { continue };
            for j in 0..terms.len() {
                if j == i || !terms[j].0 { continue; }
                let Some((aj, bj)) = eq_sides(&terms[j].1) else { continue };
                // Orient `i` so `s` is the larger side (skip the pairing
                // where `t ≻ s` — `s ≈ t` would not be the maximal side).
                for (s, t) in [(&ai, &bi), (&bi, &ai)] {
                    if matches!(self.kbo_terms(t, s), super::kbo::KboCmp::Greater) {
                        continue;
                    }
                    for (u, v) in [(&aj, &bj), (&bj, &aj)] {
                        let mut sub: Subst = vec![None; nvars as usize + 1];
                        if !unify(s, u, &mut sub) { continue; }
                        // Build (s ≈ v ∨ t ≉ v ∨ rest)σ.
                        let eq = |x: &Term, y: &Term| {
                            Term::App(vec![Term::Op(OpKind::Equal), x.clone(), y.clone()])
                        };
                        let mut lits: Vec<(bool, Term)> = Vec::with_capacity(terms.len() + 1);
                        for (k, (pos, lt)) in terms.iter().enumerate() {
                            if k == j { continue; }            // merged away
                            let lit = if k == i { eq(s, v) } else { lt.clone() };
                            lits.push((*pos, apply(&lit, &sub)));
                        }
                        lits.push((false, apply(&eq(t, v), &sub)));
                        out.push(self.make(lits, vec![given], "eq_factor", tier, None, true));
                    }
                }
            }
        }
        out
    }

    /// Rewrite `given` with active unit equalities (both orientations,
    /// with unification) — the stand-in for superposition.  Returns the
    /// empty clause's id if one falls out.
    fn paramodulants(&mut self, given: u32) -> Option<u32> {
        let (terms, nvars, tier) = {
            let c = &self.clauses[given as usize];
            (c.terms.clone(), c.nvars, c.tier)
        };
        let equals = self.units.equals.clone();
        let mut n = 0;
        for (eq_cid, l, r) in &equals {
            if matches!(l, Term::Var(_)) { continue; }
            let off = nvars as u64 + 1;
            let l2 = shift_slots(l, off);
            let r2 = shift_slots(r, off);
            let mut eq_slots = std::collections::BTreeSet::new();
            super::unify::term_slots(&l2, &mut eq_slots);
            super::unify::term_slots(&r2, &mut eq_slots);
            let max_slot = eq_slots.iter().max().copied().unwrap_or(off);
            for (li, (_, atom)) in terms.iter().enumerate() {
                for (path, sub) in positions(atom) {
                    let mut s: Subst = vec![None; (max_slot + 1) as usize];
                    if !unify(&l2, &sub, &mut s) { continue; }
                    let lits: Vec<(bool, Term)> = terms
                        .iter()
                        .enumerate()
                        .map(|(k, (pos, t))| {
                            let rewritten =
                                if k == li { replace(t, &path, &r2) } else { t.clone() };
                            (*pos, apply(&rewritten, &s))
                        })
                        .collect();
                    let made = self.make(lits, vec![given, *eq_cid], "para", tier, None, true);
                    if let Some(cid) = made {
                        if self.clauses[cid as usize].lits.is_empty() {
                            if let Some(e) = self.reportable_refutation(cid) {
                                return Some(e);
                            }
                            continue;
                        }
                    }
                    self.push(made);
                    n += 1;
                    if n >= self.opts.strategy.para_cap { return None; }
                }
            }
        }
        None
    }

    // -- forward closure -------------------------------------------------------------

    /// Bounded hyperresolution: support units × background clauses,
    /// joining all remaining negative literals against active positive
    /// units (or the oracle).  Only FLAT ground unit conclusions are
    /// kept — the problem-specific forward closure, without flooding.
    pub(crate) fn forward_close(&mut self) -> usize {
        let fc_start = Instant::now();
        // Copied out: the loop below borrows `self` mutably.
        let st = &self.opts.strategy;
        let (fc_rounds, fc_max_premise_lits, fc_flat_depth, fc_fanout, fc_cap, fc_branch, fc_max_pos) = (
            st.fc_rounds, st.fc_max_premise_lits, st.fc_flat_depth,
            st.fc_fanout, st.fc_cap, st.fc_branch, st.fc_max_pos.max(1),
        );
        let instance = self.oracle.roles().instance;
        let mut units: Vec<(AtomId, u32)> = self
            .support_seeds
            .clone()
            .into_iter()
            .filter(|(a, _)| {
                self.layer.atoms.resolve(*a, self.syn())
                    .and_then(|s| s.head_symbol()) != Some(instance)
            })
            .collect();
        let mut total = 0usize;
        for _ in 0..fc_rounds {
            let mut nxt: Vec<(AtomId, u32)> = Vec::new();
            'units: for (u_atom, u_cid) in &units {
                let u_info = self.layer.atom_info(*u_atom);
                let layer = self.layer;
                let src = move |a| layer.atom_info(a);
                let candidates = self.idx.complementary(true, &u_info, &src);
                let Some(u_term) = slot_atom(&self.layer.atoms, self.syn(), *u_atom, 0)
                else { continue };
                for at in candidates {
                    let (c_id, c_i) = (at.clause, at.lit as usize);
                    let (c_terms, c_nvars, c_npos) = {
                        let c = &self.clauses[c_id as usize];
                        if c.lits.len() > fc_max_premise_lits || c.lits[c_i].pos { continue; }
                        (c.terms.clone(), c.nvars,
                         c.terms.iter().filter(|(p, _)| *p).count())
                    };
                    if c_npos < 1 || c_npos > fc_max_pos { continue; }
                    let off = 1u64; // unit is ground: no slots of its own
                    let mut s: Subst = vec![None; (off + u64::from(c_nvars) + 1) as usize];
                    let p_lit = shift_slots(&c_terms[c_i].1, off);
                    self.stats.fc_unify_attempts += 1;
                    self.stats.fc_ground_candidate += 1; // seed unit is ground by construction
                    if !unify(&p_lit, &u_term, &mut s) { continue; }
                    self.stats.fc_unify_hits += 1;
                    let negs: Vec<Term> = c_terms.iter().enumerate()
                        .filter(|(k, (p, _))| *k != c_i && !*p)
                        .map(|(_, (_, t))| shift_slots(t, off))
                        .collect();
                    // ALL positive heads (a unit for a Horn rule; a short
                    // disjunction for a multi-conclusion rule) — the
                    // conclusion is their σ-applied disjunction.
                    let pos_terms: Vec<Term> = c_terms.iter()
                        .filter(|(p, _)| *p)
                        .map(|(_, t)| shift_slots(t, off))
                        .collect();

                    let mut got = 0usize;
                    let mut stack: Vec<(usize, Subst, Vec<SentenceId>, Vec<u32>, Vec<String>)> =
                        vec![(0, s, Vec::new(), Vec::new(), Vec::new())];
                    while let Some((k, s2, facts, used, jnotes)) = stack.pop() {
                        if k == negs.len() {
                            // The conclusion is the disjunction of all
                            // positive heads, σ-applied — every head must be
                            // a flat ground atom (the anti-flooding contract).
                            let atoms: Vec<Term> =
                                pos_terms.iter().map(|t| apply(t, &s2)).collect();
                            if atoms.iter().any(|a| {
                                !a.is_ground() || term_depth(a) > fc_flat_depth
                            }) {
                                continue;
                            }
                            let mut parents = vec![c_id, *u_cid];
                            parents.extend(used.iter().copied());
                            let lits: Vec<(bool, Term)> =
                                atoms.into_iter().map(|a| (true, a)).collect();
                            let made =
                                self.make(lits, parents, "hyper", SUPPORT, None, true);
                            let Some(cid) = made else { continue };
                            self.clauses[cid as usize].fact_parents.extend(facts.iter().copied());
                            self.clauses[cid as usize].notes.extend(jnotes.iter().cloned());
                            if self.clauses[cid as usize].lits.is_empty() {
                                // The joined conclusion was refuted
                                // outright (arithmetic / oracle
                                // discharge).  Queue it only if it is
                                // a reportable refutation.
                                if let Some(e) = self.reportable_refutation(cid) {
                                    self.push(Some(e));
                                }
                                continue;
                            }
                            let key = self.clauses[cid as usize].key;
                            if !self.seen.insert(key) { continue; }
                            self.activate(cid);
                            // Only UNIT conclusions re-seed the unit-driven
                            // next round; a derived disjunction can't.
                            if self.clauses[cid as usize].lits.len() == 1 {
                                let new_atom = self.clauses[cid as usize].lits[0].atom;
                                nxt.push((new_atom, cid));
                            }
                            total += 1;
                            got += 1;
                            if got >= fc_fanout || total >= fc_cap { break; }
                            continue;
                        }
                        let a = apply(&negs[k], &s2);
                        // Oracle discharge of a ground joined literal.
                        if a.is_ground() {
                            if let Some((rel, x, y)) = term_binary_ids(&a) {
                                if self.oracle.holds(rel, x, y, None) {
                                    let mut why: Vec<Witness> = Vec::new();
                                    let _ = self.oracle.holds(rel, x, y, Some(&mut why));
                                    let mut facts2 = facts.clone();
                                    let mut used2  = used.clone();
                                    for w in &why {
                                        if let Some(sid) = w.sid {
                                            facts2.push(sid);
                                        } else if let Some(cid) =
                                            self.oracle.learned_src(w.rel, w.x, w.y)
                                        {
                                            used2.push(cid);
                                        }
                                    }
                                    let mut jn = jnotes.clone();
                                    jn.push(format!(
                                        "(not {}) -- oracle: {}",
                                        term_kif(&a, self.syn()),
                                        witnesses_kif(&why, self.syn())));
                                    stack.push((k + 1, s2.clone(), facts2, used2, jn));
                                    continue;
                                }
                            }
                        }
                        // Join against active positive units via the index.
                        let qa = self.layer.atoms.intern_atom(&a);
                        let q_info = self.layer.atom_info(qa);
                        let cands = self.idx.probe(true, &q_info, &src);
                        let mut branch = 0usize;
                        for cand in cands {
                            let uc = &self.clauses[cand.clause as usize];
                            if uc.lits.len() != 1 { continue; }
                            // Two-way unification binds the unit's vars
                            // too, so (unlike the one-way matches) the
                            // substitution must cover its slot range.
                            let Some(u2) = slot_atom(
                                &self.layer.atoms, self.syn(), uc.lits[0].atom,
                                JOIN_UNIT_OFF as u32)
                            else { continue };
                            let mut s3 = s2.clone();
                            s3.resize((JOIN_UNIT_OFF + 257) as usize, None);
                            self.stats.fc_unify_attempts += 1;
                            if uc.nvars == 0 { self.stats.fc_ground_candidate += 1; }
                            if unify(&a, &u2, &mut s3) {
                                self.stats.fc_unify_hits += 1;
                                let mut used2 = used.clone();
                                used2.push(cand.clause);
                                stack.push((k + 1, s3, facts.clone(), used2, jnotes.clone()));
                                branch += 1;
                                if branch >= fc_branch { break; }
                            }
                        }
                    }
                    if total >= fc_cap { break 'units; }
                }
            }
            self.stats.forward_closed = total as u64;
            units = nxt;
            if units.is_empty() || total >= fc_cap { break; }
            // Wall-clock insurance: fc has its own caps, but theory
            // feedback (lists, FD, exhaustiveness) can make rounds
            // expensive at full-KB scale.
            if self.opts.cancelled()
                || (!self.opts.step
                    && self.opts.time_limit_secs > 0
                    && fc_start.elapsed().as_secs() >= self.opts.time_limit_secs.div_ceil(4))
            {
                break;
            }
        }
        total
    }

    // -- main loop ----------------------------------------------------------------

    /// Diagnostic histogram of the clause arena: count by (rule,
    /// first-literal head), top `n` — what is flooding, and from which
    /// inference.  Gated behind `SIGMA_FLOOD_DUMP` at the call site.
    pub(crate) fn flood_histogram(&self, n: usize) -> String {
        let mut counts: HashMap<(&'static str, String), usize> = HashMap::new();
        for c in &self.clauses {
            let head = c
                .terms
                .first()
                .and_then(|(_, t)| match t {
                    Term::App(elems) => elems.first().cloned(),
                    other => Some(other.clone()),
                })
                .map(|h| match h {
                    Term::Sym(s) => s.name().to_string(),
                    Term::Var(_) => "<var-head>".to_string(),
                    Term::Op(op) => format!("<{op:?}>"),
                    _ => "<other>".to_string(),
                })
                .unwrap_or_else(|| "<empty>".to_string());
            *counts.entry((c.rule, head)).or_insert(0) += 1;
        }
        let mut rows: Vec<((&'static str, String), usize)> = counts.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        rows.truncate(n);
        rows.iter()
            .map(|((rule, head), k)| format!("{k:>8}  {rule:<14} {head}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Does this clause's derivation touch the negated conjecture?
    /// (DFS over proof-DAG parents.)  The set-of-support refutation
    /// criterion: an empty clause that ISN'T rooted here only proves
    /// the inputs contradict each other.
    pub(crate) fn conjecture_rooted(&self, id: u32) -> bool {
        let mut stack = vec![id];
        let mut seen: Set64<u32> = Set64::default();
        while let Some(c) = stack.pop() {
            if !seen.insert(c) { continue; }
            let rec = &self.clauses[c as usize];
            if rec.rule == "negated_conjecture" {
                return true;
            }
            stack.extend(rec.parents.iter().copied());
        }
        false
    }

    /// Enable audit mode (see the `audit` field) with a collection cap.
    pub(crate) fn set_audit(&mut self, cap: usize) {
        self.audit = true;
        self.contradiction_cap = cap;
    }

    /// `Some(id)` iff this empty clause is a REPORTABLE refutation:
    /// always when running without a conjecture (consistency checks),
    /// otherwise only when conjecture-rooted.  Non-rooted empties are
    /// input contradictions — counted and suppressed.  In audit mode
    /// EVERY empty is collected and the run continues.
    fn reportable_refutation(&mut self, id: u32) -> Option<u32> {
        if !self.audit && (!self.has_conjecture || self.conjecture_rooted(id)) {
            return Some(id);
        }
        self.stats.input_contradictions += 1;
        if self.input_contradiction_ids.len() < self.contradiction_cap {
            self.input_contradiction_ids.push(id);
        }
        // Early stop once the cap is reached (audit `--limit`): the caller
        // only wants N contradictions, so terminate the search instead of
        // saturating on.  Returning this empty clause ends `run()`; the
        // harvest reads `input_contradiction_ids`, already populated.
        if self.audit && self.input_contradiction_ids.len() >= self.contradiction_cap {
            return Some(id);
        }
        None
    }

    /// `true` if a ground literal of `c` is now oracle- or
    /// unit-dischargeable (cheap precheck for re-simplification).
    fn stale(&self, id: u32) -> bool {
        let c = &self.clauses[id as usize];
        // A positive unit equality is the SOURCE of its own oracle
        // union-find entry (registered at make time): re-simplifying it
        // against that closure at pop time just collapses it to `x = x`,
        // destroying the one clause the superposition calculus needs to
        // superpose FROM and to index for future targets.  Keep it
        // intact while the superposition channel is on.
        let own_eq_source = self.opts.strategy.superposition
            && c.lits.len() == 1
            && c.lits[0].pos
            && self.ground_equality(c.lits[0].atom).is_some();
        for (i, l) in c.lits.iter().enumerate() {
            if self.layer.atom_info(l.atom).is_ground() {
                if self.units.ground_unit(l.pos, l.atom).is_some()
                    || self.units.ground_unit(!l.pos, l.atom).is_some()
                {
                    return true;
                }
                if let Some((rel, x, y)) = term_binary_ids(&c.terms[i].1) {
                    if self.oracle.holds(rel, x, y, None) { return true; }
                }
                if !own_eq_source {
                    if let Some((_, _, ka, kb)) = self.ground_equality(l.atom) {
                        if self.oracle.equal_holds(ka, kb, None) { return true; }
                    }
                }
            }
        }
        false
    }

    pub(crate) fn run(&mut self) -> (RunVerdict, usize) {
        if self.opts.step {
            stepdbg::force_on();
        }
        self.discharge_horn_joins();
        self.discharge_event_calculus();
        self.discharge_models();
        self.discharge_model_joins();
        self.discharge_backward();
        let t0 = Instant::now();
        let mut steps = 0usize;
        while steps < self.opts.max_steps {
            // In interactive single-step mode the wall clock is meaningless
            // (the user is paused at a prompt), so ignore the time limit —
            // only explicit cancellation (`q`) stops the run.
            if self.opts.cancelled()
                || (!self.opts.step
                    && self.opts.time_limit_secs > 0
                    && t0.elapsed().as_secs() >= self.opts.time_limit_secs)
            {
                return (RunVerdict::TimedOut, steps);
            }
            // FD congruence may have queued derived equalities (from
            // make's unit feedback / forward closure) — surface them
            // before selecting the next given.
            self.drain_fd_equalities();
            let Some(mut given) = self.pop_given() else {
                return (RunVerdict::Saturated, steps);
            };
            let prof = self.opts.profile;
            // Second-chance theory simplification: only when the oracle
            // or unit stores learned something touching this clause.
            let t_mech = prof.then(Instant::now);
            if !self.clauses[given as usize].lits.is_empty() && self.stale(given) {
                let (terms, tier, key) = {
                    let c = &self.clauses[given as usize];
                    (c.terms.clone(), c.tier, c.key)
                };
                match self.make(terms, vec![given], "oracle", tier, None, false) {
                    None => continue, // became redundant
                    Some(g2) => {
                        if self.clauses[g2 as usize].key != key {
                            given = g2;
                        }
                    }
                }
            }
            if let Some(t) = t_mech { self.stats.t_resimplify += t.elapsed(); }
            if self.clauses[given as usize].lits.is_empty() {
                match self.reportable_refutation(given) {
                    Some(e) => return (RunVerdict::Refutation(e), steps),
                    None => continue,
                }
            }
            steps += 1;

            // Interactive single-step: show the given clause being activated
            // and the queue state, before its inferences are generated.
            if stepdbg::enabled() {
                let body = format!(
                    "step {steps}   given [{given}]:  {}\n  \
                     passive queue: {} by-weight / {} by-age    total clauses: {}",
                    self.dbg_clause_kif(given),
                    self.h_weight.len(),
                    self.h_age.len(),
                    self.clauses.len(),
                );
                if !stepdbg::pause("GIVEN", &body) {
                    return (RunVerdict::TimedOut, steps);
                }
            }

            let t_mech = prof.then(Instant::now);
            for f in self.factors(given) {
                self.push(f);
            }
            if let Some(t) = t_mech { self.stats.t_factors += t.elapsed(); }

            // Equality resolution: from `C ∨ s≠t`, unify s and t and emit
            // `Cσ` — the rule that lets a negative equality literal bind a
            // variable to its required value (`(equal ?E Human)` ⇒
            // `?E := Human`, then the rest is discharged).
            let t_mech = prof.then(Instant::now);
            for e in self.equality_resolutions(given) {
                if let Some(eid) = e {
                    if self.clauses[eid as usize].lits.is_empty() {
                        if let Some(em) = self.reportable_refutation(eid) {
                            return (RunVerdict::Refutation(em), steps);
                        }
                        continue;
                    }
                }
                self.push(e);
            }
            if let Some(t) = t_mech { self.stats.t_eq_resolve += t.elapsed(); }

            let has_fn = self.clauses[given as usize].terms.iter().any(|(_, t)| {
                matches!(t, Term::App(elems)
                    if elems.iter().skip(1).any(|e| matches!(e, Term::App(_))))
            });
            let t_mech = prof.then(Instant::now);
            if self.opts.strategy.superposition {
                // Ordered superposition (the equality-complete calculus)
                // — replaces the unit-paramodulation stand-in for the ON
                // path.  Eligible at every tier: the active equation set
                // is the reduction ordering's domain.
                if let Some(empty) = self.superposition_inferences(given) {
                    return (RunVerdict::Refutation(empty), steps);
                }
            } else if !self.units.equals.is_empty()
                && self.clauses[given as usize].tier < BACKGROUND
                && has_fn
            {
                if let Some(empty) = self.paramodulants(given) {
                    return (RunVerdict::Refutation(empty), steps);
                }
            }
            if let Some(t) = t_mech { self.stats.t_paramod += t.elapsed(); }

            // Equality factoring: the completeness corner that lets two
            // positive equality literals merge (`s≈t ∨ s≈t'` ⇒ the
            // residue `t≉t'`).  Pairs with superposition — without the
            // rewrite engine it only adds sound-but-inert clauses.
            if self.opts.strategy.eq_factoring {
                for e in self.equality_factors(given) {
                    if let Some(eid) = e {
                        if self.clauses[eid as usize].lits.is_empty() {
                            if let Some(em) = self.reportable_refutation(eid) {
                                return (RunVerdict::Refutation(em), steps);
                            }
                            continue;
                        }
                    }
                    self.push(e);
                }
            }

            // Resolve only on the FEWEST-CANDIDATES literal (zero-count
            // literals are unviable *today*: skipping them keeps the
            // clause alive — they reappear in resolvents).
            let t_mech = prof.then(Instant::now);
            let lits = self.clauses[given as usize].lits.clone();
            // Ordered resolution: only KBO-maximal literals are eligible
            // (both sides — the partner side is filtered below).  `ordered`
            // off ⇒ `max_mask` is all-ones ⇒ no restriction.
            let ordered = self.opts.strategy.ordered_resolution;
            let given_max = self.clauses[given as usize].max_mask;
            let sel: Vec<usize> = if lits.len() == 1 {
                vec![0]
            } else if self.opts.strategy.full_saturation {
                // Full-saturation regime: EVERY (ordering-eligible) literal
                // resolves.  Single-literal selection below is a
                // goal-directed heuristic that is NOT refutation-complete —
                // it can permanently starve the one resolution a case-
                // analysis proof needs (PUZ001+1: the case-split clause's
                // `¬lives` literal loses the pick to an equality literal
                // whose only resolvent is a tautology).  Literals without
                // partners cost one empty index probe.
                (0..lits.len())
                    .filter(|&i| !ordered || (given_max >> i) & 1 == 1)
                    .collect()
            } else {
                // Literal selection (Strategy.lit_select): 0 = fewest index
                // candidates (default, most goal-directed), 1 = most, 2 =
                // first eligible (cheapest — no counting beyond viability).
                let lit_select = self.opts.strategy.lit_select;
                let mut best: Option<(usize, usize)> = None;
                let layer = self.layer;
                let src = move |a| layer.atom_info(a);
                for (i, l) in lits.iter().enumerate() {
                    if ordered && (given_max >> i) & 1 == 0 {
                        continue;
                    }
                    let info = self.layer.atom_info(l.atom);
                    let mut n = self.idx.count_complementary(l.pos, &info, &src);
                    if n == 0 {
                        // A symmetric-headed literal with no direct
                        // partners may still have swapped-form partners
                        // — don't let the zero skip it.
                        if let Some(sw) = self.symmetric_swapped_info(l.atom) {
                            n = self.idx.count_complementary(l.pos, &sw, &src);
                        }
                    }
                    if n == 0 {
                        continue;
                    }
                    let better = match (lit_select, best) {
                        (_, None) => true,
                        (1, Some((bn, _))) => n > bn,  // most candidates
                        (2, Some(_)) => false,         // first eligible wins
                        (_, Some((bn, _))) => n < bn,  // fewest (default)
                    };
                    if better {
                        best = Some((n, i));
                    }
                    if lit_select == 2 {
                        break; // first eligible literal — stop scanning
                    }
                }
                best.map(|(_, i)| vec![i]).unwrap_or_default()
            };
            for gi in sel {
                let l = lits[gi];
                let info = self.layer.atom_info(l.atom);
                let layer = self.layer;
                let src = move |a| layer.atom_info(a);
                let mut cands = self.idx.complementary(l.pos, &info, &src);
                // Symmetric dual retrieval: a symmetric-headed literal
                // also probes its argument-swapped form — indexed
                // partners may predate the relation's symmetry
                // registration, and open patterns orient independently
                // of their instances.  `resolve`'s swap retry makes the
                // extra candidates unify.
                if let Some(sw) = self.symmetric_swapped_info(l.atom) {
                    let extra = self.idx.complementary(l.pos, &sw, &src);
                    if !extra.is_empty() {
                        let seen_at: Set64<(u32, u8)> =
                            cands.iter().map(|e| (e.clause, e.lit)).collect();
                        cands.extend(
                            extra
                                .into_iter()
                                .filter(|e| !seen_at.contains(&(e.clause, e.lit))),
                        );
                    }
                }
                // Ordered resolution: the partner literal must be maximal
                // in its own clause too.
                let cands: Vec<EntryRef> = if ordered {
                    cands.into_iter()
                        .filter(|at| (self.clauses[at.clause as usize].max_mask >> at.lit) & 1 == 1)
                        .collect()
                } else {
                    cands
                };

                // Batched algebraic extraction: the given side of the
                // decode is partner-independent, so compute it once;
                // ground-unit partners' residuals decode as ONE batch
                // (Montgomery-shared inversion; bitsliced quadratic
                // solves at volume — see gf64::decode_batch).  Decode
                // anomalies and non-eligible partners take the general
                // unification path, exactly as the scalar fast path.
                if let Some(shape) = self.decode_given_shape(given, gi) {
                    let mut eligible: Vec<EntryRef> = Vec::new();
                    let mut residuals: Vec<crate::gf64::Sketch> = Vec::new();
                    let mut general: Vec<EntryRef> = Vec::new();
                    for at in cands {
                        match self.partner_residual(&shape, at.clause, at.lit as usize) {
                            Some(r) => { eligible.push(at); residuals.push(r); }
                            None => general.push(at),
                        }
                    }
                    let mut decoded = Vec::new();
                    crate::gf64::decode_batch(&residuals, shape.m, &mut decoded);
                    for (at, dec) in eligible.into_iter().zip(decoded) {
                        let r = match self.resolve_from_decoded(
                            given, gi, at.clause, &shape, dec)
                        {
                            Some(r) => r,
                            None => self.resolve(given, gi, at.clause, at.lit as usize),
                        };
                        if let Some(rid) = r {
                            if self.clauses[rid as usize].lits.is_empty() {
                                if let Some(e) = self.reportable_refutation(rid) {
                                    return (RunVerdict::Refutation(e), steps);
                                }
                                continue;
                            }
                        }
                        self.push(r);
                    }
                    for at in general {
                        let r = self.resolve(given, gi, at.clause, at.lit as usize);
                        if let Some(rid) = r {
                            if self.clauses[rid as usize].lits.is_empty() {
                                if let Some(e) = self.reportable_refutation(rid) {
                                    return (RunVerdict::Refutation(e), steps);
                                }
                                continue;
                            }
                        }
                        self.push(r);
                    }
                } else {
                    for at in cands {
                        let r = self.resolve(given, gi, at.clause, at.lit as usize);
                        if let Some(rid) = r {
                            if self.clauses[rid as usize].lits.is_empty() {
                                if let Some(e) = self.reportable_refutation(rid) {
                                    return (RunVerdict::Refutation(e), steps);
                                }
                                continue;
                            }
                        }
                        self.push(r);
                    }
                }
            }
            if let Some(t) = t_mech { self.stats.t_resolve += t.elapsed(); }
            self.activate(given);
        }
        (RunVerdict::StepsExhausted, steps)
    }
}

// -- term helpers -------------------------------------------------------------------

/// Classify a target literal's seats for the open-unit residue lookup
/// (see [`super::units::UnitStores::open_candidates`]).  Non-compound
/// targets conservatively force a scan.
fn classify_seats(t: &Term) -> (usize, SmallVec<[super::units::SeatK; 8]>) {
    use super::units::SeatK;
    let Term::App(elems) = t else {
        return (1, SmallVec::from_slice(&[SeatK::Compound(None)]));
    };
    let seats = elems.iter().enumerate().map(|(i, el)| match el {
        Term::Var(_) => SeatK::Var,
        Term::App(_) => SeatK::Compound(super::units::seat_shape_coin(i, el)),
        leaf => SeatK::Leaf(
            super::slot_term_seat_coin(i, leaf)
                .expect("leaf terms always coin")),
    }).collect();
    (elems.len(), seats)
}

/// Term depth: leaf = 0, compound = 1 + max child depth.
pub(crate) fn term_depth(t: &Term) -> u8 {
    match t {
        Term::App(elems) => {
            1 + elems.iter().map(term_depth).max().unwrap_or(0)
        }
        _ => 0,
    }
}

/// Number of skolem-function applications in `t` (clausify names them
/// `sk_<root>_<n>`), nested occurrences counted individually.
fn term_skolem_apps(t: &Term) -> u64 {
    match t {
        Term::App(elems) => {
            let own = matches!(elems.first(),
                Some(Term::Sym(s)) if s.name().starts_with("sk_")) as u64;
            own + elems.iter().map(term_skolem_apps).sum::<u64>()
        }
        Term::Sym(s) if s.name().starts_with("sk_") => 1,
        _ => 0,
    }
}

fn term_size(t: &Term) -> usize {
    match t {
        Term::App(elems) => elems.iter().map(term_size).sum(),
        _ => 1,
    }
}

/// Is `t` an equality atom `(equal s u)` (any polarity, any sides)?
fn is_equality_atom(t: &Term) -> bool {
    matches!(t, Term::App(elems)
        if elems.len() == 3 && matches!(elems[0], Term::Op(OpKind::Equal)))
}

/// `(rel, x, y)` ids for a symbol-triple ground binary atom term.
fn term_binary_ids(t: &Term) -> Option<(SymbolId, SymbolId, SymbolId)> {
    let Term::App(elems) = t else { return None };
    if elems.len() != 3 { return None; }
    let Term::Sym(rel) = &elems[0] else { return None };
    let Term::Sym(x) = &elems[1] else { return None };
    let Term::Sym(y) = &elems[2] else { return None };
    Some((rel.id(), x.id(), y.id()))
}

/// Lift a symbol-headed atom into `(relation id, argument terms)`.
/// `None` for variable / operator / non-`App` heads (the join only
/// dispatches on named relations).
fn lit_pattern(t: &Term) -> Option<(SymbolId, Vec<Term>)> {
    let Term::App(elems) = t else { return None };
    if elems.len() < 2 { return None; }
    let Term::Sym(h) = &elems[0] else { return None };
    Some((h.id(), elems[1..].to_vec()))
}

/// The symbol id of a bare-symbol term (`None` for variables, literals,
/// compounds).
/// Structural compatibility of two atoms' argument lists for backward
/// chaining: same arity, and no position where BOTH sides are distinct
/// ground leaves (symbols/literals).  This is a cheap, sound over-approximation
/// of unifiability — it rejects only pairs that provably cannot unify because
/// two constants clash in the same seat (the "match by structure, not variable
/// identity" prefilter).  A variable or compound on either side is always
/// compatible here; real unification (`resolve`) makes the final decision.
fn structurally_compatible(a: &[Term], b: &[Term]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).all(|(x, y)| match (x, y) {
        (Term::Sym(p), Term::Sym(q)) => p.id() == q.id(),
        (Term::Lit(p), Term::Lit(q)) => p == q,
        // a symbol vs a literal (both ground, different kinds) cannot unify
        (Term::Sym(_), Term::Lit(_)) | (Term::Lit(_), Term::Sym(_)) => false,
        _ => true, // a var or compound on either side: leave it to unification
    })
}

/// Interactive single-step debugger (gated `SIGMA_STEP`).  Pauses the prover at
/// each given-clause selection and each `make` (derived clause / match), prints
/// a human-readable view, and blocks on stdin so the search can be watched step
/// by step.  Intended for single-problem, single-threaded (`--jobs 1`) runs —
/// stdin from many sweep threads would interleave.  No-op (and zero overhead
/// beyond a cached env check) when unset.
mod stepdbg {
    use std::cell::Cell;
    thread_local! {
        static ENABLED: Cell<i8>  = const { Cell::new(-1) }; // -1 unknown, 0 off, 1 on
        static RUN:     Cell<bool> = const { Cell::new(false) }; // `c` pressed: stop pausing
        static SKIP:    Cell<u64>  = const { Cell::new(0) };     // `g N`: events to skip
    }

    pub(super) fn enabled() -> bool {
        ENABLED.with(|e| {
            if e.get() == -1 {
                e.set(i8::from(std::env::var_os("SIGMA_STEP").is_some()));
            }
            e.get() == 1
        })
    }

    /// Force the stepper on for this thread (the `--step` flag /
    /// `NativeOpts.step` path, independent of the `SIGMA_STEP` env var).
    pub(super) fn force_on() {
        ENABLED.with(|e| e.set(1));
        RUN.with(|r| r.set(false));
    }

    /// Show `header`/`body` and block for a command.  Returns `false` iff the
    /// user asked to quit (caller should abort the run).
    pub(super) fn pause(header: &str, body: &str) -> bool {
        if !enabled() || RUN.with(Cell::get) {
            return true;
        }
        if SKIP.with(|s| {
            let n = s.get();
            if n > 0 {
                s.set(n - 1);
                true
            } else {
                false
            }
        }) {
            return true;
        }
        use std::io::Write;
        eprintln!("\n\x1b[36m── {header} ──\x1b[0m\n{body}");
        loop {
            eprint!("\x1b[33m[step]\x1b[0m  ⏎=next  c=continue  g N=skip N  q=quit › ");
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
                RUN.with(|r| r.set(true)); // EOF (piped/no TTY): run to completion
                return true;
            }
            let cmd = line.trim();
            match cmd {
                "" | "s" | "n" => return true,
                "c" => {
                    RUN.with(|r| r.set(true));
                    return true;
                }
                "q" => return false,
                _ if cmd.starts_with("g ") => {
                    if let Ok(n) = cmd[2..].trim().parse::<u64>() {
                        SKIP.with(|s| s.set(n));
                        return true;
                    }
                    eprintln!("  (usage: g N)");
                }
                _ => eprintln!("  ?  ⏎=next  c=continue  g N=skip N  q=quit"),
            }
        }
    }
}

fn sym_of(t: &Term) -> Option<SymbolId> {
    match t {
        Term::Sym(s) => Some(s.id()),
        _ => None,
    }
}

/// Apply a ground binding (variable id → ground term) to a term.
fn subst(t: &Term, b: &HashMap<SymbolId, Term>) -> Term {
    match t {
        Term::Var(id) => b.get(id).cloned().unwrap_or_else(|| t.clone()),
        Term::App(es) => Term::App(es.iter().map(|e| subst(e, b)).collect()),
        other => other.clone(),
    }
}

/// One-way match of a (possibly open) pattern term against a ground fact
/// term, extending the binding in place.  Pattern variables bind to the
/// fact's subterm; ground pattern positions must be structurally equal.
fn match_term(p: &Term, f: &Term, b: &mut HashMap<SymbolId, Term>) -> bool {
    match p {
        Term::Var(id) => match b.get(id) {
            Some(existing) => existing == f,
            None => {
                b.insert(*id, f.clone());
                true
            }
        },
        Term::App(pe) => match f {
            Term::App(fe) if pe.len() == fe.len() => {
                for (pp, ff) in pe.iter().zip(fe) {
                    if !match_term(pp, ff, b) {
                        return false;
                    }
                }
                true
            }
            _ => false,
        },
        other => other == f,
    }
}

/// Match an argument vector against a ground fact tuple.
fn match_args(pat: &[Term], fact: &[Term], b: &mut HashMap<SymbolId, Term>) -> bool {
    if pat.len() != fact.len() {
        return false;
    }
    for (p, f) in pat.iter().zip(fact) {
        if !match_term(p, f, b) {
            return false;
        }
    }
    true
}

/// Where a ground fact used by the join came from — its proof
/// provenance.  Store facts cite their sentence (file:line); emitted
/// heads cite the prior `rule_join` clause that derived them (so chained
/// rules render as a connected DAG).
#[derive(Clone, Copy)]
enum FactSrc {
    Store(SentenceId),
    Emitted(u32),
    /// Derived by the inductive model (semi-naive evaluation), not a single
    /// stored atom — carries no citable sentence id (the per-atom model path
    /// likewise emits model units without fact parents).
    Model,
}

/// A ground fact in the join's generator map, with its provenance.
#[derive(Clone)]
struct JoinFact {
    args: Vec<Term>,
    src:  FactSrc,
}

/// Seat index over the join's fact map: `(relation, seat, value) →
/// indices into facts[relation]`.  Lets a generator with already-bound
/// seats retrieve only the matching facts (an index join) and rank
/// conjuncts by selectivity GIVEN the current binding, instead of
/// scanning every fact of the relation — collapses many-conjunct joins.
type SeatIndex = HashMap<(SymbolId, u8, u64), Vec<u32>>;

/// Hashable key for a ground leaf term (symbol id).  Only symbols are
/// indexed; literal-valued seats fall back to scan (rare in the
/// fact-query KBs, whose arguments are constants).
fn seat_key(t: &Term) -> Option<u64> {
    match t {
        Term::Sym(s) => Some(s.id()),
        _ => None,
    }
}

/// Build the seat index from the current fact map.
fn build_seat_index(facts: &HashMap<SymbolId, Vec<JoinFact>>) -> SeatIndex {
    let mut idx: SeatIndex = HashMap::new();
    for (rel, vec) in facts {
        for (fi, jf) in vec.iter().enumerate() {
            for (seat, a) in jf.args.iter().enumerate() {
                if let Some(k) = seat_key(a) {
                    idx.entry((*rel, seat as u8, k)).or_default().push(fi as u32);
                }
            }
        }
    }
    idx
}

/// Whether `rel` is a theory relation the oracle decides semantically
/// (taxonomy / shape-recognized roles / temporal point-network).  Such
/// relations are CHECKED through `holds` when a body literal is ground
/// but are never ENUMERATED as a join generator — the generative axioms
/// behind their facts are exactly what the join is starving.
fn is_theory_rel(
    rel: SymbolId,
    roles: &crate::semantics::roles::TaxonomyRoles,
    tids: &super::temporal::TemporalRelIds,
) -> bool {
    rel == roles.instance
        || rel == roles.subclass
        || rel == roles.subrelation
        || rel == roles.transitive
        || rel == roles.symmetric
        || rel == roles.domain
        || rel == roles.range
        || rel == roles.disjoint
        || rel == roles.partition
        || tids.is_temporal(rel)
}

/// `(a, b)` iff `t` is a ground `(equal a b)` over two distinct symbols.
/// Equality-class key of a ground leaf term: symbols by name hash,
/// numeric literals by canonical-value hash (`crate::numeric` — its
/// own namespace), so `(equal Value 8.0)` and a folded `(AdditionFn
/// 4 4)` land in the same class.
fn eq_key(t: &Term) -> Option<u64> {
    match t {
        Term::Sym(s) => Some(s.id()),
        Term::Lit(Literal::Number(v)) => crate::numeric::num_eq_key(v),
        _ => None,
    }
}

/// Whether the term is a numeric literal (preferred as equality-class
/// root so normalization keeps numbers literal — arithmetic
/// comparisons stay decidable after rewriting).
fn is_num_lit(t: &Term) -> bool {
    matches!(t, Term::Lit(Literal::Number(_)))
}

/// The two sides of a ground `(equal l r)` term, un-keyed (keys need
/// AtomTable access for compounds — see `NativeProver::term_eq_key`).
fn term_ground_equality_sides(t: &Term) -> Option<(&Term, &Term)> {
    let Term::App(elems) = t else { return None };
    if elems.len() != 3 || !matches!(elems[0], Term::Op(OpKind::Equal)) {
        return None;
    }
    if !elems[1].is_ground() || !elems[2].is_ground() {
        return None;
    }
    Some((&elems[1], &elems[2]))
}

/// The head bucket key of an atom term (open-unit lookup).
fn term_head_key(t: &Term) -> Option<(u64, u8)> {
    let Term::App(elems) = t else { return None };
    let arity = elems.len().min(255) as u8;
    match elems.first()? {
        Term::Sym(s) => Some((s.id(), arity)),
        Term::Op(op) => Some((u64::from(super::units::op_tag(op)), arity)),
        _ => None,
    }
}

/// Proper non-variable subterm positions of an atom (heads skipped),
/// paired with the subterm — paramodulation targets.
fn positions(atom: &Term) -> Vec<(Vec<usize>, Term)> {
    fn walk(t: &Term, path: Vec<usize>, out: &mut Vec<(Vec<usize>, Term)>) {
        if let Term::App(elems) = t {
            for (i, e) in elems.iter().enumerate().skip(1) {
                let mut p = path.clone();
                p.push(i);
                walk(e, p, out);
            }
        }
        if !path.is_empty() && !matches!(t, Term::Var(_)) {
            out.push((path, t.clone()));
        }
    }
    let mut out = Vec::new();
    walk(atom, Vec::new(), &mut out);
    out
}

/// Does clause `sub` subsume clause `sup`?  I.e. is there ONE
/// substitution σ mapping each `sub` literal (one-way match) onto a
/// distinct `sup` literal of the same polarity — multiset inclusion
/// `Subσ ⊆ Sup`?  Subsumer variables bind; `sup`'s variables are opaque
/// constants to the one-way matcher, so the substitution is indexed by
/// `sub`'s slots only (no rename-apart needed).  Backtracking over the
/// literal assignment; clauses are small, so the per-attempt subst clone
/// is cheap.
fn clause_subsumes(sub: &[(bool, Term)], sup: &[(bool, Term)]) -> bool {
    if sub.len() > sup.len() {
        return false;
    }
    let mut slots = std::collections::BTreeSet::new();
    for (_, t) in sub {
        super::unify::term_slots(t, &mut slots);
    }
    let nslots = slots.iter().max().map_or(0, |m| (*m + 1) as usize);
    let mut subst: Subst = vec![None; nslots];
    let mut used = vec![false; sup.len()];
    subsume_rec(sub, sup, 0, &mut subst, &mut used)
}

fn subsume_rec(
    sub: &[(bool, Term)],
    sup: &[(bool, Term)],
    i: usize,
    subst: &mut Subst,
    used: &mut [bool],
) -> bool {
    if i == sub.len() {
        return true;
    }
    let (sp, pat) = &sub[i];
    for (j, (tp, tgt)) in sup.iter().enumerate() {
        if used[j] || sp != tp {
            continue;
        }
        let saved = subst.clone();
        if match_one_way(pat, tgt, subst) {
            used[j] = true;
            if subsume_rec(sub, sup, i + 1, subst, used) {
                return true;
            }
            used[j] = false;
        }
        *subst = saved;
    }
    false
}

/// The largest slot-variable index occurring in `t`, or `None` when `t`
/// is ground — the offset basis for renaming a demodulator apart from
/// its target.
fn max_slot(t: &Term) -> Option<u64> {
    let mut slots = std::collections::BTreeSet::new();
    super::unify::term_slots(t, &mut slots);
    slots.iter().max().copied()
}

/// The two sides `(a, b)` of an equality atom `(equal a b)`, or `None`
/// when `t` is not a binary equality.
fn eq_sides(t: &Term) -> Option<(Term, Term)> {
    let Term::App(elems) = t else { return None };
    if elems.len() != 3 || !matches!(elems[0], Term::Op(OpKind::Equal)) {
        return None;
    }
    Some((elems[1].clone(), elems[2].clone()))
}

/// The subterm of `t` reached by following the argument-index `path`.
fn subterm_at<'t>(t: &'t Term, path: &[usize]) -> Option<&'t Term> {
    let mut cur = t;
    for &p in path {
        match cur {
            Term::App(elems) => cur = elems.get(p)?,
            _ => return None,
        }
    }
    Some(cur)
}

/// Replace the subterm at `path` with `new`.
fn replace(t: &Term, path: &[usize], new: &Term) -> Term {
    if path.is_empty() {
        return new.clone();
    }
    let Term::App(elems) = t else { return t.clone() };
    let mut out = elems.clone();
    out[path[0]] = replace(&elems[path[0]], &path[1..], new);
    Term::App(out)
}

/// Evaluate ground arithmetic function terms bottom-up
/// (`(AdditionFn 2 3)` → `5`) — the prototype's `arith_norm`.
/// In-place arithmetic folding.  The overwhelmingly common case is "no
/// arithmetic anywhere" — this walks without allocating and rewrites
/// only the folded node (the old by-value version rebuilt EVERY term
/// tree of EVERY literal entering `make`).
pub(crate) fn arith_norm(t: &mut Term) {
    let Term::App(elems) = t else { return };
    for e in elems.iter_mut() {
        arith_norm(e);
    }
    if elems.len() == 3 {
        if let (Term::Sym(f), Term::Lit(Literal::Number(a)), Term::Lit(Literal::Number(b))) =
            (&elems[0], &elems[1], &elems[2])
        {
            if let (Some(x), Some(y)) =
                (crate::numeric::parse_num(a), crate::numeric::parse_num(b))
            {
                if let Some(v) = crate::numeric::eval_binary_fn(&f.name(), x, y) {
                    *t = Term::Lit(Literal::Number(crate::numeric::format_num(v)));
                }
            }
        }
    }
}

// -- rendering helpers (notes / diagnostics) -----------------------------------------

pub(crate) fn term_kif(t: &Term, syn: &crate::syntactic::SyntacticLayer) -> String {
    match t {
        Term::Var(v) => format!("?V{}", v),
        Term::Sym(s) => s.name().to_string(),
        Term::Lit(Literal::Str(v)) | Term::Lit(Literal::Number(v)) => v.clone(),
        Term::Op(op) => op.name().to_string(),
        Term::App(elems) => {
            let inner: Vec<String> = elems.iter().map(|e| term_kif(e, syn)).collect();
            format!("({})", inner.join(" "))
        }
    }
}

fn lit_kif(pos: bool, t: &Term, syn: &crate::syntactic::SyntacticLayer) -> String {
    if pos { term_kif(t, syn) } else { format!("(not {})", term_kif(t, syn)) }
}

fn witnesses_kif(why: &[Witness], syn: &crate::syntactic::SyntacticLayer) -> String {
    if why.is_empty() {
        return "x = x".to_string();
    }
    let name = |id: SymbolId| {
        syn.sym_name(id).map(|s| s.name().to_string()).unwrap_or_else(|| format!("#{:x}", id))
    };
    why.iter()
        .map(|w| format!("({} {} {})", name(w.rel), name(w.x), name(w.y)))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod subsumption_tests {
    use super::{clause_subsumes, Term};
    use crate::types::Symbol;

    fn s(n: &str) -> Term { Term::Sym(Symbol::from(n)) }
    fn app(v: Vec<Term>) -> Term { Term::App(v) }
    fn v(n: u64) -> Term { Term::Var(n) }

    #[test]
    fn general_subsumes_specific_not_vice_versa() {
        let sub = vec![(true, app(vec![s("p"), v(0)]))];
        let sup = vec![(true, app(vec![s("p"), s("a")]))];
        assert!(clause_subsumes(&sub, &sup));
        assert!(!clause_subsumes(&sup, &sub));
    }

    #[test]
    fn multi_literal_subsumes_a_longer_clause() {
        // (¬q ?0 ∨ p ?0) ⊑ (¬q a ∨ p a ∨ r b)
        let sub = vec![
            (false, app(vec![s("q"), v(0)])),
            (true,  app(vec![s("p"), v(0)])),
        ];
        let sup = vec![
            (false, app(vec![s("q"), s("a")])),
            (true,  app(vec![s("p"), s("a")])),
            (true,  app(vec![s("r"), s("b")])),
        ];
        assert!(clause_subsumes(&sub, &sup));
    }

    #[test]
    fn shared_variable_must_bind_consistently() {
        // (p ?0 ?0) can't subsume (p a b) but does subsume (p a a).
        let sub = vec![(true, app(vec![s("p"), v(0), v(0)]))];
        assert!(!clause_subsumes(&sub, &vec![(true, app(vec![s("p"), s("a"), s("b")]))]));
        assert!(clause_subsumes(&sub, &vec![(true, app(vec![s("p"), s("a"), s("a")]))]));
    }

    #[test]
    fn polarity_must_match() {
        let sub = vec![(true, app(vec![s("p"), v(0)]))];
        let sup = vec![(false, app(vec![s("p"), s("a")]))];
        assert!(!clause_subsumes(&sub, &sup));
    }
}
