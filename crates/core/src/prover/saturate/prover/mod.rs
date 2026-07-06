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
use std::collections::{BinaryHeap, HashMap};
use std::time::Instant;

use smallvec::SmallVec;

use crate::parse::OpKind;
use crate::prover::CommonProverOpts;
use crate::semantics::types::Scope;
use crate::types::{Element, Literal, SentenceId, Symbol, SymbolId};
use crate::SineParams;

use super::ProverLayer;
use super::clause::{AtomId, ClauseKey, PClause, PLit, Term};
use super::AtomInfo;
use super::index::{EntryRef, LiteralIndex};
use super::oracle::{SemanticOracle, Witness};
use super::theory::TheoryOracle;
use super::hash64::{Map64, Set64};
use super::strategy::Strategy;
use super::unify::{apply, apply_off, match_one_way, shift_slots, slot_atom, unify, unify_off, Subst};
use super::units::UnitStores;

mod discharge;
mod forward;
mod fvi;
mod make;
mod schema_apply;
mod snapshot;
mod stats;

pub(crate) use fvi::{ClauseBlooms, ClauseFv, SubsRec};
pub(crate) use snapshot::ProverSnapshot;
pub(crate) use stats::ProverStats;

// Clause lineage tiers (queue priority: conjecture line first).
pub(crate) const CONJECTURE: u8 = 0;
pub(crate) const SUPPORT: u8 = 1;
pub(crate) const BACKGROUND: u8 = 2;
/// Backstop width for INPUT clauses under the TPTP full-saturation
/// regime (see [`NativeProver::input_width_cap`]): inputs are never
/// search-shaped by `max_lits` there, but a genuinely pathological
/// clause still has a ceiling.  Anything over it flows into
/// `discarded_long`, so the honesty gate keeps treating the load as
/// lossy.
const INPUT_WIDTH_BACKSTOP: usize = 512;
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
        // The step cap is a KIF-ask liveness guard, tuned for sub-second
        // interactive queries.  Under the TPTP regime the wall-clock budget
        // is already a hard ceiling (scale.rs deadlines), so a 4000-step cap
        // just surrenders paid-for time: the rescued ALG family exhausts it
        // in 2-4s of a 24s lane slice (measured, wide-lane experiment).
        // Effectively unbounded here; the clock governs.
        self.max_steps = 1_000_000;
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
// NOTE (SoA): the subsumption-scan fields (feature vector, bloom words,
// literal count) and the retirement flag deliberately do NOT live here —
// they moved to the id-indexed parallel arrays `NativeProver::subs` /
// `NativeProver::retired_bits` so the hot candidate scans read dense
// 32-byte records / bitmap words instead of pointer-chasing this large
// struct.  See `fvi::SubsRec` and `NativeProver::is_retired`.

/// A reusable one-way-match scratch: substitution buffer (all-`None`
/// between uses) plus binding trail (empty between uses).  Rollback via
/// the trail re-establishes both invariants — clear-don't-free.
#[derive(Debug, Default, Clone)]
pub(crate) struct MatchScratch {
    pub(super) s:     Subst,
    pub(super) trail: Vec<usize>,
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

/// Why the algebraic decode fast path was NOT applicable for a given
/// literal (stats-only — see `decode_given_shape_cause`; the partner-
/// side and decode-tail causes are counted directly at their sites).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeBail {
    /// `Strategy.decode` is off — not decode traffic at all (never
    /// counted; the `decode:` stats line stays all-zero).
    Off,
    /// More than 2 open seats (the quadratic sketch solver's limit).
    TooManyOpen,
    /// An open seat holds a compound containing a variable — decoding
    /// would need per-path (homomorphic) sketches, not seat coins.
    NestedVar,
    /// Non-`App` given literal / out-of-range seat (defensive).
    Other,
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


pub(crate) struct NativeProver<'a> {
    layer: &'a ProverLayer,
    scope: Scope,
    pub(crate) oracle: SemanticOracle<'a>,
    pub(crate) opts: NativeOpts,
    /// Per-prover KBO when `Strategy.prec_seed != 0` (a permuted symbol
    /// precedence); `None` ⇒ use the shared, warm `layer.kbo` unchanged.
    prec: Option<super::kbo::KboOrdering>,
    pub(crate) clauses: Vec<ClauseRec>,
    /// SoA twin of `clauses` (same indexing, same length — the arena
    /// lockstep invariant, debug-asserted at every write site): the
    /// packed 32-byte subsumption-scan record per clause.  These fields
    /// MOVED here from `ClauseRec` (single source of truth, no dual
    /// maintenance) so `forward_subsumed`'s candidate loop touches one
    /// dense array line per candidate instead of gathering four small
    /// fields from the large arena struct.  See `fvi::SubsRec`.
    pub(crate) subs: Vec<SubsRec>,
    /// Retirement bitmap (bit `id` of word `id / 64`): set by backward
    /// demodulation when a newer oriented unit equation rewrote clause
    /// `id` and its simplified replacement took over.  The arena record
    /// stays (proof-DAG references remain valid, stale index entries
    /// are tolerated by re-checking), but the clause is skipped on
    /// given selection and filtered from partner retrieval.  MOVED off
    /// `ClauseRec` (it was a bool there) — every candidate loop
    /// (subsumption, resolution partners, superposition, demod) now
    /// reads a dense, mostly-L1-resident bitmap via
    /// [`Self::is_retired`] instead of dereferencing the arena.  Sized
    /// in lockstep with `clauses` (`(len + 63) / 64` words).  All-zero
    /// unless `Strategy.bwd_demod` is on.
    retired_bits: Vec<u64>,
    /// Verified dedup map: `ClauseKey` → id of the FIRST clause accepted
    /// under that key.  A `ClauseKey` is a bare 64-bit content hash, so
    /// a key hit alone must never be trusted to DROP a clause — a
    /// collision between two genuinely different clauses would silently
    /// discard a non-duplicate (a completeness hole).  Every consumer
    /// goes through [`Self::seen_duplicate`] / [`Self::seen_insert`] /
    /// [`Self::seen_duplicate_lits`], which structurally verify the
    /// canonical literal lists on a key hit; a mismatch (true collision)
    /// is counted in `stats.dedup_collisions_detected` and the probing
    /// clause is ACCEPTED.  The map keeps the first id — subsequent
    /// collision-mates simply bypass dedup entirely, which is sound
    /// (dedup is an optimization; re-processing is never wrong).
    seen: Map64<ClauseKey, u32>,
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
    /// `(weight, guide_key, seq, id)` — `guide_key` is the semantic-guide
    /// tie-break (0 when `Strategy.semantic_guide` is off: an inert third
    /// column, so the ordering is byte-identical to the pre-guidance
    /// two-column key whenever the knob is off).  Ties within `weight`
    /// resolve by `guide_key` (lower = more model-false literals = picked
    /// sooner), then by `seq` (age) as before.
    h_weight: BinaryHeap<Reverse<(u64, u64, u64, u32)>>,
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
    /// Reusable binding trail for the open-unit match loop in `make`
    /// (pairs with `scratch`; rollback restores the all-`None`
    /// invariant without the per-attempt `Vec` the old internal trail
    /// allocated — LAT282+1 ran 3.84M attempts through that path).
    match_trail: Vec<usize>,
    /// Reusable substitution + trail for the demod walk's candidate
    /// matcher (`vec![None; off+nslots+1]` per candidate per node was
    /// most of the RNG-family rewrite churn).  `mem::take`n around the
    /// demod fixpoint so the walk can borrow it beside `&mut self`.
    demod_scratch: MatchScratch,
    /// Reusable buffers for the exact subsumption check
    /// (`clause_subsumes_in`) — see [`SubsScratch`].
    subs_scratch: SubsScratch,
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
    /// Forward-demodulation index: KBO-oriented positive unit equations,
    /// registered at ACTIVATION (given-clause processing / background
    /// load) and bucketed by their left side's head shape — see
    /// [`super::units::DemodIndex`].  Not snapshotted (orientation is
    /// KBO-dependent and `bg_fingerprint` excludes `prec_seed`);
    /// hydration rebuilds it from the arena like the superposition
    /// indexes.
    demods: super::units::DemodIndex,
    /// Backward-demodulation reverse index: subterm head key → ids of
    /// clauses that contain a node with that head shape (i.e. the only
    /// clauses that could hold an `l`-redex for a demodulator whose left
    /// side carries that key).  Maintained per made clause ONLY while
    /// `Strategy.bwd_demod` is on (empty and cost-free otherwise);
    /// entries are never pruned — stale ids (retired / superseded
    /// clauses) are tolerated and re-checked by the pass.  Rebuilt from
    /// the arena on snapshot hydrate, like the other derived indexes.
    bwd_index: Map64<u64, Vec<u32>>,
    /// Normal-form memo (the ground-term identity Part-4 core): for a
    /// GROUND maximal subtree keyed by content hash, the recorded demod
    /// outcome at a given demodulator generation — unchanged (skip the
    /// whole subtree) or a cached normal form (splice, replaying the
    /// recorded rewrite citations/count).  Shares rewrite work ACROSS
    /// clauses: the recurring normal forms that dominate equational
    /// traffic are found+built once per generation.  Per-run (validity
    /// is generation-scoped, and generations are per-`DemodIndex`);
    /// never populated unless `Strategy.demod` is on.
    nf_memo: Map64<super::terms::TermKey, super::terms::NfEntry>,
    seq: u64,
    tick: u64,
    /// Semantic clause-selection guidance (`Strategy.semantic_guide`) AND
    /// the `SIGMA_MODEL` in-loop simplification in `make` (`model_true_negative`):
    /// the KB's positive model + its evaluation `Provenance` (needed to
    /// `cite` a deletion's supporting KB sentences), built ONCE at `run()`
    /// start / first demand and shared by both consumers.  `None` covers
    /// two cases both treat identically (neutral / no-op) — the knob is
    /// off, or the one-shot build bailed (`ModelProgram::positive_model`
    /// hit its materialization budget or deadline).
    guide_model: Option<(super::model::Model, super::model::Provenance)>,
    /// Set once the guide-model build has been attempted (regardless of
    /// outcome) — guards against rebuilding on every `push` / `make`.
    guide_attempted: bool,
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
            subs: Vec::new(),
            retired_bits: Vec::new(),
            seen: Map64::default(),
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
            match_trail: Vec::new(),
            demod_scratch: MatchScratch::default(),
            subs_scratch: SubsScratch::default(),
            antisym_mined: Map64::default(),
            irrefl_mined: Map64::default(),
            inverse_mined: Vec::new(),
            bg_roots: std::collections::HashSet::new(),
            sym_swap_memo: Map64::default(),
            conj_sig: 0,
            demods: super::units::DemodIndex::default(),
            bwd_index: Map64::default(),
            nf_memo: Map64::default(),
            seq: 0,
            tick: 0,
            guide_model: None,
            guide_attempted: false,
            stats: ProverStats::default(),
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
        p.subs = snap.subs.clone();
        p.retired_bits = snap.retired_bits.clone();
        debug_assert_eq!(p.subs.len(), p.clauses.len(), "SoA/arena lockstep (hydrate)");
        debug_assert_eq!(
            p.retired_bits.len(), p.clauses.len().div_ceil(64),
            "retired bitmap/arena lockstep (hydrate)",
        );
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
        // Same for the demodulator index: orientation depends on THIS
        // run's KBO (`prec_seed`), which the snapshot key excludes.
        p.rebuild_demod_index();
        // And the backward-demodulation reverse index (maintained at
        // `make` time, which the hydrated clauses never went through).
        if p.opts.strategy.bwd_demod {
            p.rebuild_bwd_index();
        }
        p
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

    /// Whether clause `id` was retired by backward demodulation — one
    /// bitmap-word read (the SoA home of the former `ClauseRec.retired`
    /// bool; see the `retired_bits` field docs).
    #[inline]
    pub(crate) fn is_retired(&self, id: u32) -> bool {
        (self.retired_bits[(id >> 6) as usize] >> (id & 63)) & 1 != 0
    }

    /// Retire clause `id` (backward demodulation only — retirement is
    /// permanent for the run; see `retired_bits`).
    #[inline]
    pub(super) fn set_retired(&mut self, id: u32) {
        self.retired_bits[(id >> 6) as usize] |= 1u64 << (id & 63);
    }

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
        self.oracle.set_roles(roles);
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

    // -- semantic clause-selection guidance (Strategy.semantic_guide) -------

    /// Build the run's guide model exactly once (called at [`Self::run`]
    /// start).  A no-op when the strategy knob is off; otherwise pulls the
    /// KB-lifetime [`super::model::ModelProgram`] registry entry and
    /// materializes its positive model, respecting that model's own
    /// materialization budget AND a wall-clock deadline (below) — a bail
    /// (`positive_model` → `None`, e.g. the tuple budget or the deadline was
    /// exceeded) disables guidance for the rest of THIS run — every clause
    /// then scores neutral, exactly as if the knob were off — and is
    /// counted once in `stats.guide_disabled_bail`.  Cheap to call when
    /// already attempted (`guide_attempted` guards the rebuild).
    pub(crate) fn ensure_guide_model(&mut self) {
        if self.guide_attempted || !self.opts.strategy.semantic_guide {
            return;
        }
        self.guide_attempted = true;
        let mp = self.layer.model_program();
        if std::env::var_os("SIGMA_MODEL_TRACE").is_some() {
            eprintln!(
                "[SIGMA_MODEL_TRACE] ensure_guide_model: monotone.rules={} monotone.edb_preds={} \
                 program.rules={} clusters={}",
                mp.monotone.rules.len(), mp.monotone.edb.len(),
                mp.program.rules.len(), mp.clusters.len()
            );
        }
        // Whole-KB monotone evaluation has no SInE-style scoping, so on a KB
        // the size of full SUMO it can take tens of seconds — guidance is a
        // heuristic tie-break, not required for soundness or completeness,
        // so it must never eat a large slice of the run's own time budget.
        // Cap it at a fixed few seconds (mirrors `discharge_model_joins`'s
        // 1500ms full-model cap), further capped by the run's own timeout
        // when that is smaller (and left unbounded only when the run itself
        // is unbounded, i.e. `time_limit_secs == 0`, e.g. interactive step
        // mode — `positive_model` is still bounded by its own tuple budget).
        const GUIDE_MODEL_BUDGET_SECS: u64 = 5;
        let cap_secs = if self.opts.time_limit_secs > 0 {
            self.opts.time_limit_secs.min(GUIDE_MODEL_BUDGET_SECS)
        } else {
            GUIDE_MODEL_BUDGET_SECS
        };
        let deadline = Instant::now() + std::time::Duration::from_secs(cap_secs);
        match mp.positive_model(Some(deadline)) {
            Some((model, prov)) => self.guide_model = Some((model, prov)),
            None => {
                self.guide_model = None;
                self.stats.guide_disabled_bail += 1;
            }
        }
    }

    /// Ensure the shared positive model is materialized for THIS run,
    /// regardless of which consumer demands it first (`Strategy.semantic_guide`
    /// scoring, or `SIGMA_MODEL`'s in-loop `make` simplification) — same
    /// one-shot budget/deadline discipline as [`Self::ensure_guide_model`],
    /// just without that method's knob gate.  Idempotent: `guide_attempted`
    /// guards the rebuild, so whichever consumer runs first pays the build
    /// cost and the other reuses it for free.
    pub(crate) fn ensure_model_for_simplification(&mut self) {
        if self.guide_attempted {
            return;
        }
        self.guide_attempted = true;
        let mp = self.layer.model_program();
        const MODEL_BUDGET_SECS: u64 = 5;
        let cap_secs = if self.opts.time_limit_secs > 0 {
            self.opts.time_limit_secs.min(MODEL_BUDGET_SECS)
        } else {
            MODEL_BUDGET_SECS
        };
        let deadline = Instant::now() + std::time::Duration::from_secs(cap_secs);
        match mp.positive_model(Some(deadline)) {
            Some((model, prov)) => self.guide_model = Some((model, prov)),
            None => {
                self.guide_model = None;
                self.stats.guide_disabled_bail += 1;
            }
        }
    }

    /// Decode a ground literal's atom into `(relation, args)` when it is a
    /// FLAT application (`(rel a1 a2 …)` with every argument a bare ground
    /// symbol) — the only shape the guide model's tuples can be compared
    /// against.  `None` for non-ground atoms, compound arguments, and
    /// non-`App`/propositional atoms: those are exactly the "unmodeled"
    /// literals the scoring treats as neutral.
    fn guide_lit_pattern(t: &Term) -> Option<(SymbolId, Vec<SymbolId>)> {
        let Term::App(elems) = t else { return None };
        let Term::Sym(rel) = elems.first()? else { return None };
        let mut args = Vec::with_capacity(elems.len() - 1);
        for e in &elems[1..] {
            match e {
                Term::Sym(s) => args.push(s.id()),
                _ => return None, // variable or compound: unmodeled
            }
        }
        Some((rel.id(), args))
    }

    /// Whether one literal is FALSE in the guide model: the positive
    /// literal's tuple is ABSENT from the relation, or the negative
    /// literal's tuple is PRESENT.  `None` when the literal is neutral
    /// (non-ground, non-flat, or the relation is not in the model at all —
    /// an unmodeled predicate is neither confirmed nor refuted, so it must
    /// not count toward either side of the fraction).
    fn guide_lit_false(&self, pos: bool, atom: AtomId) -> Option<bool> {
        let (model, _prov) = self.guide_model.as_ref()?;
        if !self.layer.atom_info(atom).is_ground() {
            return None;
        }
        let t = slot_atom(&self.layer.atoms, self.syn(), atom, 0)?;
        let (rel, args) = Self::guide_lit_pattern(&t)?;
        let tuples = model.get(&rel)?; // relation absent from the model: neutral
        let present = tuples.contains(&args);
        Some(pos != present)
    }

    /// Semantic guidance score for a clause's canonical literals: the
    /// fraction (scaled to `0..=GUIDE_SCALE`) of its non-neutral literals
    /// that are false in the guide model — 0 = every checkable literal is
    /// TRUE in the model (far from a conflict), `GUIDE_SCALE` = every
    /// checkable literal is FALSE (every literal simultaneously
    /// contradicts the model, the classic "closest to empty" heuristic).
    /// All-neutral clauses (nothing modeled) score `GUIDE_SCALE / 2` —
    /// exactly in the middle, so they neither win nor lose the tie-break
    /// against a clause the model does speak to.  `0` (the "off" value,
    /// also the all-neutral floor half-point folds to under integer
    /// division) when the knob is off or the model bailed.
    pub(crate) fn guide_score(&mut self, lits: &[PLit]) -> u64 {
        const GUIDE_SCALE: u64 = 1000;
        if !self.opts.strategy.semantic_guide {
            return 0;
        }
        self.ensure_guide_model();
        if self.guide_model.is_none() {
            return 0;
        }
        let (mut false_n, mut total) = (0u64, 0u64);
        for l in lits {
            if let Some(is_false) = self.guide_lit_false(l.pos, l.atom) {
                total += 1;
                if is_false { false_n += 1; }
            }
        }
        if total == 0 {
            return GUIDE_SCALE / 2;
        }
        self.stats.guided_clauses_scored += 1;
        // Score HIGH = more model-false literals; the pick queue wants
        // LOW keys first (BinaryHeap<Reverse<..>>), so invert here once
        // rather than at every call site.
        GUIDE_SCALE - (GUIDE_SCALE * false_n / total)
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
    /// returns for ours — then refuted by the feature-vector prefilter
    /// (`fvi::ClauseFv::le`, O(1): #lits/#pos/#neg/size/KBO-weight all
    /// monotone under matching, so a channel violation soundly rules out
    /// subsumption) and the per-literal Key-Equation counting filter
    /// ([`keq_unpartnered`]) before falling back to the expensive exact
    /// check, [`clause_subsumes`].  Gated by `Strategy.subsumption`.
    fn forward_subsumed(&mut self, lits: &[PLit], terms: &[(bool, Term)]) -> Option<u32> {
        if !self.opts.strategy.subsumption || lits.is_empty() {
            return None;
        }
        let layer = self.layer;
        let src = move |a| layer.atom_info(a);
        // TRANSIENT query-side atom infos, computed from the slot terms
        // (hash-before-intern): the candidate clause's atoms are not
        // (yet) resident in the AtomTable — most candidates die right
        // here, and the accepted ones intern immediately after this
        // check.  Field-for-field equal to the memoized infos the old
        // path read (`AtomInfos::info`), minus the memo/phone-book side
        // effects; the index side (`src`) still resolves through the
        // layer memo — only the query side is transient.
        let tinfos: smallvec::SmallVec<[super::AtomInfo; 4]> =
            terms.iter().map(|(_, t)| super::term_atom_info(t)).collect();
        #[cfg(any(test, debug_assertions))]
        for (l, (_, t)) in lits.iter().zip(terms) {
            // Twin (debug builds only): the transient info must be
            // byte-equal to the memoized one for the interned atom, and
            // the hash-only id must be the intern id.  (This twin DOES
            // intern — acceptable in debug, like the KBO fast-path twins.)
            let id = self.layer.atoms.intern_slot_atom(t);
            debug_assert_eq!(id, l.atom, "hash-only atom id diverged from intern for {t:?}");
            debug_assert_eq!(
                super::term_atom_info(t),
                *self.layer.atom_info(l.atom),
                "transient AtomInfo diverged from AtomInfos::compute for {t:?}",
            );
        }
        let mut cand: Set64<u32> = Set64::default();
        for (l, info) in lits.iter().zip(&tinfos) {
            for at in self.idx.probe(l.pos, info, &src) {
                cand.insert(at.clause);
            }
        }
        let d_fv = ClauseFv::compute_from_terms(
            lits, terms, &tinfos, self.kbo(),
            self.opts.strategy.demod.then_some(&self.layer.term_facts),
        );
        #[cfg(any(test, debug_assertions))]
        debug_assert_eq!(
            d_fv,
            ClauseFv::compute(
                lits, self.kbo(), &src, &self.layer.atoms, self.syn(),
                self.opts.strategy.demod.then_some(&self.layer.term_facts),
            ),
            "transient feature vector diverged from the memoized compute",
        );
        // Bloom prefilter words for the candidate (the D side), computed
        // once per probe from the same per-atom data (transiently) the
        // stored C-side words used at clause birth — identical bit
        // derivation on both sides is what licenses the subset tests
        // (see `fvi::ClauseBlooms` for the per-channel soundness
        // arguments).
        let d_blooms = ClauseBlooms::compute_from_infos(lits, terms, &tinfos);
        #[cfg(any(test, debug_assertions))]
        debug_assert_eq!(
            d_blooms,
            ClauseBlooms::compute(lits, terms, &src),
            "transient bloom words diverged from the memoized compute",
        );
        // Candidate scan: every per-candidate filter below (retired bit,
        // length, both blooms, FV) reads ONLY the dense SoA twins
        // (`retired_bits` + `subs` — one bitmap word + one packed
        // 32-byte record per candidate); the big arena `ClauseRec` is
        // first touched by the rare survivors that reach the keq / exact
        // channels.  Filter ORDER and semantics are byte-identical to
        // the pre-SoA loop.
        debug_assert_eq!(self.subs.len(), self.clauses.len(), "SoA/arena lockstep");
        for cid in cand {
            if self.is_retired(cid) {
                continue; // a retired clause must not delete its own replacement
            }
            let rec = self.subs[cid as usize];
            if rec.nlits as usize > terms.len() {
                continue;
            }
            // Every candidate reaching here is a genuine subsumption
            // ATTEMPT (retired/length-mismatched candidates are filtered
            // above without ever being "attempted" — `clause_subsumes`
            // itself would reject a longer subsumer just as cheaply, so
            // counting them would inflate the denominator without
            // reflecting the prefilter's actual workload).
            self.stats.subs_checks_attempted += 1;
            // Channel: leaf bloom (one AND + compare — cheapest, runs
            // first).  C's ground leaves survive any substitution and
            // Cσ's literals sit among D's, so every leaf bit of C must
            // appear in D; a missing bit soundly refutes subsumption.
            if rec.blooms.leaf & !d_blooms.leaf != 0 {
                self.stats.subs_rejected_by_bloom_leaf += 1;
                // Debug twin (house discipline): a bloom rejection must
                // agree with the exact check.
                #[cfg(any(test, debug_assertions))]
                debug_assert!(
                    !clause_subsumes(&self.clauses[cid as usize].terms, terms),
                    "leaf bloom rejected {:?} but clause_subsumes({:?}, {:?}) \
                     would have accepted it (blooms {:#x} vs {:#x})",
                    cid, self.clauses[cid as usize].terms, terms,
                    rec.blooms.leaf, d_blooms.leaf,
                );
                continue;
            }
            // Channel: ground-literal bloom.  Every FULLY GROUND literal
            // of C is its own σ-image and must appear verbatim in D, so
            // its polarity-mixed atom bit must be set in D's word.
            // `glit == 0` (no ground literals) passes vacuously — the
            // applicability counter tracks how often the channel can
            // act at all.
            if rec.blooms.glit != 0 {
                self.stats.subs_glit_applicable += 1;
                if rec.blooms.glit & !d_blooms.glit != 0 {
                    self.stats.subs_rejected_by_bloom_glit += 1;
                    #[cfg(any(test, debug_assertions))]
                    debug_assert!(
                        !clause_subsumes(&self.clauses[cid as usize].terms, terms),
                        "ground-literal bloom rejected {:?} but \
                         clause_subsumes({:?}, {:?}) would have accepted it \
                         (blooms {:#x} vs {:#x})",
                        cid, self.clauses[cid as usize].terms, terms,
                        rec.blooms.glit, d_blooms.glit,
                    );
                    continue;
                }
            }
            if !rec.fv.le(&d_fv) {
                self.stats.subs_rejected_by_fv += 1;
                // Soundness cross-check (debug builds / tests only, zero
                // cost in release): the prefilter must never reject a
                // pair `clause_subsumes` would have accepted.
                #[cfg(any(test, debug_assertions))]
                debug_assert!(
                    !clause_subsumes(&self.clauses[cid as usize].terms, terms),
                    "FV prefilter rejected {:?} but clause_subsumes({:?}, {:?}) \
                     would have accepted it (fv {:?} vs {:?})",
                    cid, self.clauses[cid as usize].terms, terms, rec.fv, d_fv,
                );
                continue;
            }
            // Blooms + FV passed — NOW touch the arena record (the rare
            // path: most candidates died on the packed record above).
            let c = &self.clauses[cid as usize];
            debug_assert_eq!(c.lits.len(), rec.nlits as usize, "SoA nlits lockstep");
            // Channel: per-literal Key-Equation counting filter — every
            // literal of C must have at least one Key-Equation-compatible
            // literal in D, or C cannot subsume D (see `keq_unpartnered`
            // for the soundness argument).  C-side per-literal infos are
            // resident layer memos (C is an accepted, active clause);
            // D-side infos are the SAME transient `tinfos` the bloom/FV
            // channels above already used — nothing is recomputed.
            let c_infos: SmallVec<[std::sync::Arc<AtomInfo>; 4]> =
                c.lits.iter().map(|l| layer.atom_info(l.atom)).collect();
            let mut pair_tests = 0u64;
            let keq_rejected =
                keq_unpartnered(&c.lits, &c_infos, lits, &tinfos, &mut pair_tests);
            self.stats.keq_pair_tests += pair_tests;
            if keq_rejected {
                self.stats.subs_rejected_by_keq += 1;
                // MANDATORY debug twin: a Key-Equation rejection must
                // agree with the reference matcher.
                #[cfg(any(test, debug_assertions))]
                debug_assert!(
                    !clause_subsumes(&c.terms, terms),
                    "Key-Equation counting filter rejected {:?} but \
                     clause_subsumes({:?}, {:?}) would have accepted it",
                    cid, c.terms, terms,
                );
                continue;
            }
            self.stats.subs_full_checks += 1;
            let hit = clause_subsumes_in(&c.terms, terms, &mut self.subs_scratch);
            #[cfg(any(test, debug_assertions))]
            debug_assert_eq!(
                hit,
                clause_subsumes(&c.terms, terms),
                "scratch subsumption diverged from the reference for {cid}",
            );
            if hit {
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


    // -- verified dedup -----------------------------------------------------------
    //
    // `seen` maps a 64-bit `ClauseKey` to the FIRST clause id accepted
    // under it.  All three accessors verify a key hit STRUCTURALLY
    // (canonical literal lists — `ClauseKey` is a hash of exactly that
    // sequence, so equal lits ⇔ genuinely α-equivalent clauses) before
    // reporting "duplicate".  A key hit with different literals is a
    // TRUE collision: counted, and the probing clause is ACCEPTED —
    // dropping it on a naked 64-bit match would silently lose a
    // non-duplicate clause (a completeness risk).  The map keeps the
    // first id, so collision-mates bypass dedup from then on (sound:
    // dedup only saves work, duplicates are never wrong to re-process).

    /// Probe only (no insert): is the arena clause `id` a structurally
    /// verified duplicate of the first clause accepted under `key`?
    fn seen_duplicate(&mut self, key: ClauseKey, id: u32) -> bool {
        let Some(&first) = self.seen.get(&key) else { return false };
        if self.clauses[first as usize].lits == self.clauses[id as usize].lits {
            true
        } else {
            self.stats.dedup_collisions_detected += 1;
            false
        }
    }

    /// Probe only, literal-list form — for candidates that are not (or
    /// not yet) in the arena, e.g. the demod duplicate-hit stats probe
    /// in `make`.
    pub(super) fn seen_duplicate_lits(&mut self, key: ClauseKey, lits: &[PLit]) -> bool {
        let Some(&first) = self.seen.get(&key) else { return false };
        if self.clauses[first as usize].lits.as_slice() == lits {
            true
        } else {
            self.stats.dedup_collisions_detected += 1;
            false
        }
    }

    /// Record `key → id`, keeping the FIRST id on an occupied entry.
    /// No verification and no collision counting: the caller has
    /// already probed via [`Self::seen_duplicate`], where a collision
    /// was counted — this split keeps probe-then-record sites (like
    /// `push`) from double-counting one event.
    fn seen_record(&mut self, key: ClauseKey, id: u32) {
        self.seen.entry(key).or_insert(id);
    }

    /// Insert-guard form (the old `if self.seen.insert(key)` idiom):
    /// `true` when the clause counts as NEW — first sighting of the key
    /// (recorded), or a verified TRUE collision (counted; accepted; the
    /// first id stays).  `false` = structurally verified duplicate.
    pub(super) fn seen_insert(&mut self, key: ClauseKey, id: u32) -> bool {
        use std::collections::hash_map::Entry;
        match self.seen.entry(key) {
            Entry::Vacant(e) => {
                e.insert(id);
                true
            }
            Entry::Occupied(e) => {
                let first = *e.get();
                if self.clauses[first as usize].lits == self.clauses[id as usize].lits {
                    false
                } else {
                    self.stats.dedup_collisions_detected += 1;
                    true
                }
            }
        }
    }

    // -- queue ------------------------------------------------------------------

    /// Queue a made clause for given selection.  `None` (redundant) and
    /// already-seen/over-long clauses are dropped.
    pub(crate) fn push(&mut self, id: Option<u32>) -> Option<u32> {
        self.push_capped(id, self.opts.max_lits)
    }

    /// Load-time `push` for INPUT clauses (SUPPORT hypotheses and the
    /// negated CONJECTURE, including their definitional-CNF products):
    /// identical dedup + queueing, but the width discard uses
    /// [`Self::input_width_cap`], so under the TPTP full-saturation
    /// regime inputs load whole instead of being shaped like derived
    /// clauses.
    pub(crate) fn push_input(&mut self, id: Option<u32>) -> Option<u32> {
        let cap = self.input_width_cap();
        self.push_capped(id, cap)
    }

    /// Width cap for INPUT clauses (BACKGROUND / SUPPORT / CONJECTURE
    /// roots as loaded).  `max_lits` is a SEARCH-SHAPING cap: on the
    /// KIF/SUMO path dropping an over-wide clause is an acceptable
    /// trade (the honesty gate withholds countermodel claims).  Under
    /// `Strategy.full_saturation` (the TPTP problem regime) it is
    /// strictly a loss: the discard silently makes the loaded theory
    /// incomplete, which both forfeits inferences a proof may need AND
    /// forces `complete_saturation` to withhold verdicts from every
    /// later saturation.  So there inputs load whole, with only the
    /// generous [`INPUT_WIDTH_BACKSTOP`] guarding pathological widths
    /// (an over-backstop discard still counts into `discarded_long`,
    /// keeping the honesty accounting truthful).  Derived-clause caps
    /// are untouched — that is search shaping, not input fidelity.
    fn input_width_cap(&self) -> usize {
        if self.opts.strategy.full_saturation {
            INPUT_WIDTH_BACKSTOP.max(self.opts.max_lits)
        } else {
            self.opts.max_lits
        }
    }

    fn push_capped(&mut self, id: Option<u32>, max_lits: usize) -> Option<u32> {
        let id = id?;
        let key = self.clauses[id as usize].key;
        if self.seen_duplicate(key, id) { return None; }
        if self.clauses[id as usize].lits.len() > max_lits {
            self.stats.discarded_long += 1;
            return None;
        }
        self.seen_record(key, id);
        let c = &self.clauses[id as usize];
        let (w, n) = (c.weight, self.seq);
        self.seq += 1;
        // Semantic-guide tie-break: 0 (inert, first-in-key-order) when the
        // knob is off, so the heap ordering is byte-identical to the
        // pre-guidance behavior — this can only be observed as a
        // REORDERING among clauses that already tie on `weight`, never a
        // change to which clause wins on weight alone.
        let lits = c.lits.clone();
        let g = self.guide_score(&lits);
        self.h_weight.push(Reverse((w, g, n, id)));
        self.h_age.push(Reverse((n, id)));
        Some(id)
    }

    fn pop_given(&mut self) -> Option<u32> {
        self.tick += 1;
        let prefer_age = self.tick % self.opts.strategy.pick_ratio.max(1) == 0;
        for pass in 0..2 {
            let from_age = prefer_age == (pass == 0);
            // Retired (backward-demodulated) clauses are lazily skipped
            // here — cheaper than deleting heap entries, and marking
            // them popped keeps the other heap's copy dead too.
            if from_age {
                while let Some(Reverse((_, id))) = self.h_age.pop() {
                    if self.popped.insert(id) {
                        if self.is_retired(id) { continue; }
                        return Some(id);
                    }
                }
            } else {
                while let Some(Reverse((_, _, _, id))) = self.h_weight.pop() {
                    if self.popped.insert(id) {
                        if self.is_retired(id) { continue; }
                        return Some(id);
                    }
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
            let demod = self.index_demodulator(id);
            // Backward demodulation: the NEWLY oriented equation
            // re-normalizes the EXISTING clause sets (interreduction).
            // Trigger only here — the hydrate/mask rebuild paths call
            // `index_demodulator` for equations that were already
            // active, whose backward pass already ran (or predates the
            // snapshot).
            if self.opts.strategy.bwd_demod {
                if let Some(d) = demod {
                    self.backward_demodulate(id, &d);
                }
            }
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
    ///
    /// GROUND fast path (Part 3.3, active only under `Strategy.demod`):
    /// two ground sides with different KBO weights orient on the weight
    /// alone (variable condition vacuous) — read from the layer's
    /// ground-term facts memo, skipping both interns.  This sits on the
    /// superposition hot path (`superpose` re-orients the "from"
    /// equation per inference).  Debug twin asserts agreement with the
    /// full compare.
    fn equality_oriented(&self, t: &Term) -> Option<(Term, Term)> {
        let Term::App(elems) = t else { return None };
        if elems.len() != 3 || !matches!(elems[0], Term::Op(OpKind::Equal)) {
            return None;
        }
        let (a, b) = (&elems[1], &elems[2]);
        if self.opts.strategy.demod {
            if let (Some(fa), Some(fb)) = (
                self.layer.term_facts.ground_facts(a, &self.layer.kbo),
                self.layer.term_facts.ground_facts(b, &self.layer.kbo),
            ) {
                if fa.kbo_weight != fb.kbo_weight {
                    let fast = if fa.kbo_weight > fb.kbo_weight {
                        Some((a.clone(), b.clone()))
                    } else {
                        Some((b.clone(), a.clone()))
                    };
                    #[cfg(any(test, debug_assertions))]
                    {
                        let ai = self.layer.atoms.intern_atom(a);
                        let bi = self.layer.atoms.intern_atom(b);
                        let full = match self.kbo().compare(ai, bi, &self.layer.atoms, self.syn()) {
                            super::kbo::KboCmp::Greater => Some((a.clone(), b.clone())),
                            super::kbo::KboCmp::Less => Some((b.clone(), a.clone())),
                            _ => None,
                        };
                        debug_assert_eq!(
                            fast, full,
                            "ground weight fast path diverged from KBO compare \
                             ({} vs {}) for {t:?}",
                            fa.kbo_weight, fb.kbo_weight,
                        );
                    }
                    return fast;
                }
            }
        }
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
        // (Retired equations dropped: their normalized replacements are
        // re-indexed on their own activation.)
        let eqns: Vec<(u32, u8)> = self
            .active_eqns
            .iter()
            .copied()
            .filter(|&(c, _)| !self.is_retired(c))
            .collect();
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
                if self.is_retired(tp.clause) {
                    continue; // superseded by its backward-demod replacement
                }
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
                if self.clauses[id as usize].lits.len() > self.input_width_cap() {
                    // An INPUT clause over the width cap never enters
                    // the index: the loaded theory is incomplete, and a
                    // later saturation must not be read as a model.
                    // Under `full_saturation` the cap is the generous
                    // backstop (inputs load whole — see
                    // `input_width_cap`), so this only fires on
                    // pathological widths there.
                    self.stats.discarded_long += 1;
                    continue;
                }
                if self.seen_insert(key, id) {
                    // Full-saturation regime: background clauses also
                    // compete for given selection (axiom×axiom inference).
                    // Classic set-of-support only indexes them as passive
                    // partners — structurally unable to refute problems
                    // whose proof needs case analysis among the axioms.
                    if self.opts.strategy.full_saturation {
                        let (w, n) = (self.clauses[id as usize].weight, self.seq);
                        self.seq += 1;
                        let lits = self.clauses[id as usize].lits.clone();
                        let g = self.guide_score(&lits);
                        self.h_weight.push(Reverse((w, g, n, id)));
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
            self.push_input(Some(id));
        } else if self.seen_insert(key, id) {
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
            self.push_input(id);
        }
    }

    pub(crate) fn pclause_terms(&self, pc: &super::clause::PClause) -> Option<Vec<(bool, Term)>> {
        pc.lits
            .iter()
            .map(|l| slot_atom(&self.layer.atoms, self.syn(), l.atom, 0).map(|t| (l.pos, t)))
            .collect()
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
        self.decode_given_shape_cause(given, gi).ok()
    }

    /// [`Self::decode_given_shape`] with the BAIL CAUSE surfaced —
    /// stats-only instrumentation (Step-2 decode profile): the batch
    /// section of the resolve loop bulk-attributes an ineligible
    /// literal's whole candidate set to the one shape cause.  Identical
    /// checks in identical order; `Err` maps exactly onto the old
    /// `None`s.
    fn decode_given_shape_cause(
        &self,
        given: u32,
        gi: usize,
    ) -> Result<DecodeShape, DecodeBail> {
        // A/B kill switch for benchmarking the algebraic fast path
        // (`SIGMA_NO_DECODE` via `Strategy::default`, or per lane).
        if !self.opts.strategy.decode {
            return Err(DecodeBail::Off);
        }
        let g = &self.clauses[given as usize];
        let gi_info = self.layer.atom_info(g.lits[gi].atom);
        let m = gi_info.mask.count_ones();
        if m > 2 {
            return Err(DecodeBail::TooManyOpen);
        }
        // Every open seat must be a simple variable in the pattern term
        // (a compound-with-variable seat needs real unification).
        let Term::App(p_elems) = &g.terms[gi].1 else { return Err(DecodeBail::Other) };
        let mut open_slots: SmallVec<[(u8, u64); 2]> = SmallVec::new(); // (seat, slot)
        let mut bits = gi_info.mask;
        while bits != 0 {
            let seat = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            match p_elems.get(seat) {
                Some(Term::Var(slot)) => open_slots.push((seat as u8, *slot)),
                // An open (= non-ground) seat holding a COMPOUND: the
                // subterm contains a variable somewhere below the seat
                // surface — THE decision counter for a homomorphic
                // (path-weighted) sketch extension.
                Some(Term::App(_)) => return Err(DecodeBail::NestedVar),
                _ => return Err(DecodeBail::Other),
            }
        }
        Ok(DecodeShape {
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
    ///
    /// `count`: attribute this pair's outcome to the Step-2 decode
    /// cause counters.  `true` ONLY from the batch section of the
    /// resolve loop (which counts each pair exactly once); the scalar
    /// rerun a batch anomaly triggers through `resolve` →
    /// `resolve_decoded` passes `false`, so a fallback pair is never
    /// double-counted (it deterministically re-reaches the same bail).
    fn resolve_from_decoded(
        &mut self,
        given: u32,
        gi: usize,
        partner: u32,
        shape: &DecodeShape,
        decoded: crate::gf64::Decoded,
        count: bool,
    ) -> Option<Option<u32>> {
        use crate::gf64::Decoded;
        let coins: SmallVec<[u64; 2]> = match decoded {
            Decoded::None => SmallVec::new(),
            Decoded::One(c) => SmallVec::from_slice(&[c]),
            Decoded::Two(a, b) => SmallVec::from_slice(&[a, b]),
            Decoded::Fail => {
                // The residual sketch itself failed to decode.
                if count { self.stats.decode_bail_phonebook_or_collision += 1; }
                return None;
            }
        };

        // Phone book: each coin must name exactly one expected open seat.
        let syn = &self.layer.semantic.syntactic;
        let mut s: Subst = vec![None; shape.g_nvars as usize + 1];
        let mut seen_seats: SmallVec<[u8; 2]> = SmallVec::new();
        for c in coins {
            let Some((seat, term)) =
                self.layer.atom_infos.coin_term(c, &self.layer.atoms, syn)
            else {
                // Unknown coin — not in the phone book (collision).
                if count { self.stats.decode_bail_phonebook_or_collision += 1; }
                return None;
            };
            let Some(&(_, slot)) = shape.open_slots.iter().find(|(st, _)| *st == seat) else {
                if count { self.stats.decode_bail_other += 1; }
                return None; // decoded a seat the pattern didn't open
            };
            if seen_seats.contains(&seat) {
                if count { self.stats.decode_bail_other += 1; }
                return None;
            }
            seen_seats.push(seat);
            match &s[slot as usize] {
                None => s[slot as usize] = Some(term),
                // Repeated variable: both seats must decode equal fillers.
                Some(prev) if *prev == term => {}
                Some(_) => {
                    if count { self.stats.decode_bail_other += 1; }
                    return None; // genuinely no resolvent — but let
                                 // unify reach the same verdict
                }
            }
        }
        if seen_seats.len() != shape.open_slots.len() {
            if count { self.stats.decode_bail_other += 1; }
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
        if count { self.stats.decode_bindings_extracted += 1; }
        let tier = shape.g_tier.min(self.clauses[partner as usize].tier);
        Some(self.make(lits, vec![given, partner], "resolve", tier, None, true))
    }

    /// Scalar composition of the three pieces — `resolve`'s fast path.
    /// Never counts decode causes (`count = false`): every pair the
    /// saturation loop routes here was already counted by the batch
    /// section (anomaly fallbacks re-reach the same bail), and the
    /// goal-directed discharge paths (`discharge.rs`) are outside the
    /// resolve-loop traffic the decode profile measures.
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
        self.resolve_from_decoded(given, gi, partner, &shape, decoded, false)
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
            if self.is_retired(*eq_cid) { continue; }
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
        // Semantic clause-selection guidance: build the run's positive
        // model exactly once, up front (before any given-clause is
        // scored).  A no-op when `Strategy.semantic_guide` is off; a
        // budget bail disables guidance for the rest of this run (see
        // `ensure_guide_model`).  Clauses already queued by background
        // loading (`add_background_root`'s full-saturation path, which
        // runs before `run()`) were scored via `guide_score`'s own lazy
        // call to this same method, so they are not left stale.
        self.ensure_guide_model();
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
                // Retired (backward-demodulated) clauses no longer
                // partner — their simplified replacements do.
                cands.retain(|at| !self.is_retired(at.clause));
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
                //
                // Decode cause counters (Step-2 profile, stats-only):
                // this section is the ONE place each (literal, partner)
                // pair is counted — per pair for partner/tail causes,
                // bulk (`cands.len()`) for given-shape causes, since an
                // ineligible literal falls EVERY candidate through to
                // ordinary unification.  The scalar reruns inside
                // `resolve` never count (see `resolve_decoded`).
                match self.decode_given_shape_cause(given, gi) {
                    Ok(shape) => {
                        let mut eligible: Vec<EntryRef> = Vec::new();
                        let mut residuals: Vec<crate::gf64::Sketch> = Vec::new();
                        let mut general: Vec<EntryRef> = Vec::new();
                        for at in cands {
                            self.stats.decode_attempts += 1;
                            match self.partner_residual(&shape, at.clause, at.lit as usize) {
                                Some(r) => { eligible.push(at); residuals.push(r); }
                                None => {
                                    // Non-ground / non-unit / arity-
                                    // mismatched partner.
                                    self.stats.decode_bail_partner_shape += 1;
                                    general.push(at);
                                }
                            }
                        }
                        let mut decoded = Vec::new();
                        crate::gf64::decode_batch(&residuals, shape.m, &mut decoded);
                        for (at, dec) in eligible.into_iter().zip(decoded) {
                            let r = match self.resolve_from_decoded(
                                given, gi, at.clause, &shape, dec, true)
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
                    }
                    Err(cause) => {
                        // Bulk attribution: every candidate pair of this
                        // literal falls through to ordinary unification
                        // because of the GIVEN side's shape.
                        let n = cands.len() as u64;
                        match cause {
                            DecodeBail::Off => {} // knob off: no decode traffic
                            DecodeBail::NestedVar => {
                                self.stats.decode_attempts += n;
                                self.stats.decode_bail_nested_var += n;
                            }
                            DecodeBail::TooManyOpen => {
                                self.stats.decode_attempts += n;
                                self.stats.decode_bail_too_many_open += n;
                            }
                            DecodeBail::Other => {
                                self.stats.decode_attempts += n;
                                self.stats.decode_bail_other += n;
                            }
                        }
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
/// Per-literal Key-Equation counting filter for forward subsumption:
/// `true` when some literal of the candidate subsumer `C` has NO
/// Key-Equation-compatible partner literal in the new clause `D` — a
/// SOUND rejection of "C subsumes D" (the exact matcher would have said
/// no); `false` says nothing (the full check still verifies).
///
/// SOUNDNESS (necessary condition): if C subsumes D, each literal `c`
/// of C maps under the (single, clause-wide) matching substitution σ to
/// an IDENTICAL literal `d ∈ D` of the same polarity (`cσ = d`
/// syntactically — that is what the one-way matcher establishes,
/// literal by literal).  σ only ever replaces variables, so it does not
/// alter `c`'s ground seats: `d` agrees with `c` on every ground seat
/// of `c`, coin for coin (coin keys are pure content functions, and a
/// ground compound seat's key is its content hash — identical content,
/// identical coin).  Whatever σ wrote into `c`'s masked seats is
/// removed from `d`'s fingerprint by `residue_under`:
/// `AtomInfo::mask` masks every non-ground seat WHOLE — a bare variable
/// or a compound containing one sets the whole seat's bit and
/// contributes a zero coin (see `fingerprint.rs::{compute, seat_meta}`;
/// the same semantics on the transient D side via `term_atom_info`,
/// property-tested in `make.rs` and debug-twinned at the
/// `forward_subsumed` call site) — so no partial-seat content survives
/// under a masked bit on either side.  Seats ≥ `MAX_SEATS` carry no
/// coin and no mask bit on either side (skipped alike; selectivity
/// loss only).  Hence NECESSARILY, for `d = cσ`:
///
///   polarity(c) == polarity(d),  arity(c) == arity(d),  and
///   d.info.residue_under(c.info.mask) == c.info.base_residue
///
/// — both sides reduce to `arity_tag(n) ⊕ XOR{coins of c's ground
/// seats}`: `d`'s ground coins at `c`'s ground seats are identical, its
/// content at `c`'s masked seats is XOR-ed off (if ground in `d`) or
/// was never coined (if still open in `d` — zero coin, skipped by the
/// `!self.mask` guard).  This is exactly the unit-store KEY EQUATION
/// applied per pair, O(popcount(mask)) per test via `seat_coins`.  So a
/// literal of C with NO compatible partner among D's literals refutes
/// subsumption outright.  Equal-arity literals always have equal REAL
/// arity when identical, so the saturated-`u8` arity gate never falsely
/// rejects, and a differing real arity behind an equal saturated one is
/// caught by the `arity_tag` mixed into the residues.
///
/// The converse is NOT checked (per-literal partners need not be
/// simultaneously realizable by one σ, and two C literals may claim the
/// same D literal) — that is the full matcher's job; false passes cost
/// one redundant `clause_subsumes_in` call, never a wrong answer.
///
/// The C-literal scan starts at the most-ground literal (fewest masked
/// seats → most coins pinned → highest chance of having no partner) and
/// takes the rest in clause order; each partner scan over D stops at
/// the first compatible literal.  `pair_tests` counts every inner-scan
/// step (the `keq_pair_tests` stat).
fn keq_unpartnered(
    c_lits: &[PLit],
    c_infos: &[std::sync::Arc<AtomInfo>],
    d_lits: &[PLit],
    d_infos: &[AtomInfo],
    pair_tests: &mut u64,
) -> bool {
    debug_assert_eq!(c_lits.len(), c_infos.len());
    debug_assert_eq!(d_lits.len(), d_infos.len());
    // Most-selective-first: argmin popcount(mask), ties to the first
    // occurrence (`min_by_key` keeps the earliest minimum) — cheap and
    // deterministic; the remaining literals follow in clause order.
    let first = c_infos
        .iter()
        .enumerate()
        .min_by_key(|(_, info)| info.mask.count_ones())
        .map_or(0, |(i, _)| i);
    let order = std::iter::once(first).chain((0..c_lits.len()).filter(|&i| i != first));
    for ci in order {
        let (cl, cinfo) = (&c_lits[ci], &*c_infos[ci]);
        let mut partnered = false;
        for (dl, dinfo) in d_lits.iter().zip(d_infos) {
            *pair_tests += 1;
            if dl.pos == cl.pos
                && dinfo.arity == cinfo.arity
                && dinfo.residue_under(cinfo.mask) == cinfo.base_residue
            {
                partnered = true;
                break;
            }
        }
        if !partnered {
            return true;
        }
    }
    false
}

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

/// Reusable buffers for [`clause_subsumes_in`] — prover-owned, shared
/// across every candidate of every `forward_subsumed` probe.  The
/// profile showed the per-candidate setup (BTreeSet slot scan, fresh
/// `Subst`, fresh `used`, and a `subst.clone()` per literal-assignment
/// attempt) as roughly half the subsumption bucket's allocator traffic.
#[derive(Debug, Default, Clone)]
pub(crate) struct SubsScratch {
    subst: Subst,
    used:  Vec<bool>,
    trail: Vec<usize>,
}

/// [`clause_subsumes`] on reusable scratch: identical verdicts (debug
/// twin at the `forward_subsumed` call site + twin test below), zero
/// per-candidate allocation once the buffers are warm.  Backtracking is
/// trail-based — a failed branch rolls back exactly its own bindings —
/// instead of the reference's clone/restore of the whole substitution.
fn clause_subsumes_in(
    sub: &[(bool, Term)],
    sup: &[(bool, Term)],
    scr: &mut SubsScratch,
) -> bool {
    if sub.len() > sup.len() {
        return false;
    }
    let nslots = sub.iter().map(|(_, t)| term_slots_end(t)).max().unwrap_or(0);
    if scr.subst.len() < nslots {
        scr.subst.resize(nslots, None);
    }
    scr.used.clear();
    scr.used.resize(sup.len(), false);
    debug_assert!(scr.trail.is_empty());
    debug_assert!(scr.subst.iter().all(Option::is_none));
    let hit = subsume_rec_in(sub, sup, 0, scr);
    // Restore the all-`None` invariant (on failure the recursion already
    // rolled everything back and the trail is empty).
    for &slot in &scr.trail {
        scr.subst[slot] = None;
    }
    scr.trail.clear();
    hit
}

fn subsume_rec_in(
    sub: &[(bool, Term)],
    sup: &[(bool, Term)],
    i: usize,
    scr: &mut SubsScratch,
) -> bool {
    if i == sub.len() {
        return true;
    }
    let (sp, pat) = &sub[i];
    for j in 0..sup.len() {
        let (tp, tgt) = &sup[j];
        if scr.used[j] || sp != tp {
            continue;
        }
        let mark = scr.trail.len();
        if super::unify::match_one_way_off(pat, 0, tgt, &mut scr.subst, &mut scr.trail) {
            scr.used[j] = true;
            if subsume_rec_in(sub, sup, i + 1, scr) {
                return true;
            }
            scr.used[j] = false;
            for &slot in &scr.trail[mark..] {
                scr.subst[slot] = None;
            }
            scr.trail.truncate(mark);
        }
    }
    false
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

/// [`replace`] for OWNED trees, in place: navigate to `path` and drop
/// `new` in — no sibling/ancestor cloning, no rebuild, `new` is moved.
/// Same resulting tree as `replace` (twin test below); a path that
/// dead-ends in a non-`App` leaves `t` unchanged, mirroring `replace`'s
/// `t.clone()` arm.  The demod fixpoint's per-step full-tree
/// clone+drop churn (RNG044+1: 58% of CPU in rewrite machinery) was
/// exactly `*t = replace(t, ..)`.
fn replace_in_place(t: &mut Term, path: &[usize], new: Term) {
    let mut cur = t;
    for &p in path {
        let Term::App(elems) = cur else { return };
        cur = &mut elems[p];
    }
    *cur = new;
}

/// Largest slot index in `t` plus one — allocation-free `nslots` for
/// the subsumption scratch (the old path built a `BTreeSet` per call).
fn term_slots_end(t: &Term) -> usize {
    match t {
        Term::Var(v) => *v as usize + 1,
        Term::App(elems) => elems.iter().map(term_slots_end).max().unwrap_or(0),
        _ => 0,
    }
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
    use super::{clause_subsumes, clause_subsumes_in, replace, replace_in_place, SubsScratch, Term};
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

    // The scratch-based exact check must agree with the reference on a
    // matrix of pairs — including backtracking cases where an early
    // literal assignment must be undone (the trail-rollback path) —
    // and must leave the scratch invariants intact between calls.
    #[test]
    fn clause_subsumes_in_agrees_with_reference_and_keeps_scratch_clean() {
        let pairs: Vec<(Vec<(bool, Term)>, Vec<(bool, Term)>)> = vec![
            (vec![(true, app(vec![s("p"), v(0)]))],
             vec![(true, app(vec![s("p"), s("a")]))]),
            (vec![(true, app(vec![s("p"), s("a")]))],
             vec![(true, app(vec![s("p"), v(0)]))]),
            (vec![(true, app(vec![s("p"), v(0), v(0)]))],
             vec![(true, app(vec![s("p"), s("a"), s("b")]))]),
            (vec![(true, app(vec![s("p"), v(0), v(0)]))],
             vec![(true, app(vec![s("p"), s("a"), s("a")]))]),
            // Backtracking: (p ?0) must first try (p a), fail the SECOND
            // literal under ?0=a, back off, and succeed with ?0=b.
            (vec![(true, app(vec![s("p"), v(0)])), (true, app(vec![s("q"), v(0)]))],
             vec![(true, app(vec![s("p"), s("a")])), (true, app(vec![s("p"), s("b")])),
                  (true, app(vec![s("q"), s("b")]))]),
            (vec![(false, app(vec![s("q"), v(1)])), (true, app(vec![s("p"), v(1)]))],
             vec![(false, app(vec![s("q"), s("a")])), (true, app(vec![s("p"), s("a")])),
                  (true, app(vec![s("r"), s("b")]))]),
            (vec![(true, app(vec![s("p"), v(0)])), (true, app(vec![s("q"), v(0)]))],
             vec![(true, app(vec![s("p"), s("a")]))]),
        ];
        let mut scr = SubsScratch::default();
        for (sub, sup) in pairs {
            let reference = clause_subsumes(&sub, &sup);
            let scratch = clause_subsumes_in(&sub, &sup, &mut scr);
            assert_eq!(reference, scratch, "verdict diverged for {sub:?} vs {sup:?}");
            assert!(scr.trail.is_empty(), "trail must drain between calls");
            assert!(scr.subst.iter().all(Option::is_none), "subst must reset between calls");
        }
    }

    // In-place replace is the allocating `replace`'s twin, path by path.
    #[test]
    fn replace_in_place_matches_replace() {
        let tree = app(vec![
            s("f"),
            app(vec![s("g"), s("a"), app(vec![s("h"), v(3)])]),
            s("c"),
        ]);
        let new = app(vec![s("k"), s("z")]);
        for path in [vec![], vec![1], vec![2], vec![1, 2], vec![1, 2, 1]] {
            let reference = replace(&tree, &path, &new);
            let mut in_place = tree.clone();
            replace_in_place(&mut in_place, &path, new.clone());
            assert_eq!(in_place, reference, "diverged at path {path:?}");
        }
    }
}
