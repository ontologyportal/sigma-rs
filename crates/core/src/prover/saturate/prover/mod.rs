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
use super::parked;
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
mod term_arena;
mod ej;
mod forward;
mod fvi;
mod make;
mod postings;
mod rows;
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
/// High-bit tag distinguishing RECIPE queue entries from clause ids in
/// the shared passive heaps (`Strategy.deferred_passive`).  Sound
/// because a clause arena of 2^31 records is physically impossible
/// (each `ClauseRec` is >100 bytes); `push_recipe` debug-asserts the
/// recipe arena side too.
const RECIPE_TAG: u32 = 1 << 31;

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
    /// Collect per-mechanism timing inside the saturation loop (select /
    /// re-simplify / factor / eq-resolve / paramodulate / resolve /
    /// activate — the whole given-clause loop body).  Off by default —
    /// the timers cost a few clock reads per given-clause step.
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

    // -- discharge subsystems ---------------------------------------------
    // Separate reasoning engines that run once, as a prologue before the
    // given-clause loop even starts (they feed ground facts back in as
    // ordinary unit clauses) — a whole-attempt capability decision, not a
    // [`Strategy`] search-shaping knob.  Each pairs an enable flag with its
    // own budget so a caller can turn a subsystem on without inheriting an
    // unrelated caller's tuning.
    /// Enable the Datalog(&not;) model-discharge oracle (conjunctive-query
    /// entailment / Clark-completion certified absence).  Equivalent to
    /// setting `SIGMA_MODEL`.
    pub model: bool,
    /// Per-evaluation tuple budget for the model evaluator before it bails
    /// to ordinary resolution.  Equivalent to `SIGMA_MODEL_BUDGET`.
    pub model_budget: usize,
    /// Wall-clock cap (ms) on model materialization across all goal atoms.
    /// Equivalent to `SIGMA_MODEL_MS`.
    pub model_ms: u64,
    /// Enable the event-calculus discharge (kernel-based `holdsAt`
    /// decision over a parsed narrative).  Equivalent to setting `SIGMA_EC`.
    pub ec: bool,
    /// Enable SLD-style backward-chaining discharge.  Equivalent to
    /// setting `SIGMA_BACKWARD`.
    pub backward: bool,
    /// Wall-clock deadline (ms) for one backward-chaining DFS pass.
    /// Equivalent to `SIGMA_BACKWARD_MS`.
    pub backward_ms: u64,
    /// Node budget backstop for the backward-chaining DFS. Equivalent to
    /// `SIGMA_BACKWARD_NODES`.
    pub backward_nodes: u64,
    /// Worker-thread cap for [`super::prove::ProverLayer::run_portfolio_schedule`]'s
    /// lane race — how many TPTP strategy lanes run concurrently instead of
    /// the original sequential carry-forward schedule.  `1` (or a lane count
    /// of `1`) falls back to that unchanged sequential path.  Defaults to
    /// the hardware's available parallelism.  Equivalent to `SIGMA_CORES`.
    pub cores: usize,
    /// Enable the bounded existential chase feeding the model-join CQ
    /// answer path (TGD witnesses from `(=> body (exists …))` axioms).
    /// Only consulted when [`Self::model`] is also set.  Equivalent to
    /// setting `SIGMA_CHASE`.
    pub chase: bool,
    /// Wall-clock cap (ms) for the chase + join materialization window.
    /// Equivalent to `SIGMA_CHASE_MS`.
    pub chase_ms: u64,
    /// Race ONE dedicated chase+model lane in the TPTP portfolio schedule,
    /// in parallel with the standard strategy lanes — the in-prover
    /// equivalent of the external race wrapper's phase 2 (a composition
    /// switch; [`Self::chase`] instead enables the mechanism attempt-wide).
    /// Equivalent to setting `SIGMA_CHASE_LANE`.
    pub chase_lane: bool,
    /// Race ONE dedicated roles+disjointness lane in the TPTP portfolio
    /// schedule (Strategy `recognize_roles` + `disjoint_decomp`), in
    /// parallel with the standard lanes — the in-prover equivalent of the
    /// external race wrapper's phase 1.  NOTE: kept a SEPARATE lane from
    /// the chase lane by design — oracle ownership of `instance` under
    /// roles starves the chase's CQ join within a single attempt.
    /// Equivalent to setting `SIGMA_ROLES_LANE`.
    pub roles_lane: bool,
    /// Per-lane SInE start budgets for the TPTP portfolio schedule
    /// (index = lane; `0` or a missing entry keeps the shared start) —
    /// the portfolio's selection-diversity axis.  Equivalent to
    /// `SIGMA_LANE_BUDGETS` (comma-separated, e.g. `2000,500,8000`).
    pub lane_budgets: Vec<usize>,
}

impl Default for NativeOpts {
    fn default() -> Self {
        Self {
            selection: SineParams::default(), session: None,
            max_steps: 4000, max_lits: 8, time_limit_secs: 30,
            forward_close: true, profile: false, want_proof: false,
            strategy: Strategy::default(), cancel: None, step: false,
            model: std::env::var_os("SIGMA_MODEL").is_some(),
            model_budget: std::env::var("SIGMA_MODEL_BUDGET").ok()
                .and_then(|v| v.parse().ok()).unwrap_or(250_000),
            model_ms: std::env::var("SIGMA_MODEL_MS").ok()
                .and_then(|v| v.parse().ok()).unwrap_or(800),
            ec: std::env::var_os("SIGMA_EC").is_some(),
            backward: std::env::var_os("SIGMA_BACKWARD").is_some(),
            backward_ms: std::env::var("SIGMA_BACKWARD_MS").ok()
                .and_then(|v| v.parse().ok()).unwrap_or(800),
            backward_nodes: std::env::var("SIGMA_BACKWARD_NODES").ok()
                .and_then(|v| v.parse().ok()).unwrap_or(200_000),
            cores: std::env::var("SIGMA_CORES").ok().and_then(|v| v.parse().ok())
                .unwrap_or_else(|| std::thread::available_parallelism()
                    .map(std::num::NonZeroUsize::get).unwrap_or(1)),
            chase: std::env::var_os("SIGMA_CHASE").is_some(),
            chase_ms: std::env::var("SIGMA_CHASE_MS").ok()
                .and_then(|v| v.parse().ok()).unwrap_or(10_000),
            chase_lane: std::env::var_os("SIGMA_CHASE_LANE").is_some(),
            roles_lane: std::env::var_os("SIGMA_ROLES_LANE").is_some(),
            lane_budgets: std::env::var("SIGMA_LANE_BUDGETS").ok()
                .map(|s| s.split(',')
                    .map(|t| t.trim().parse().unwrap_or(0))
                    .collect())
                .unwrap_or_default(),
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

/// A deferred inference (`Strategy.deferred_passive`): everything
/// needed to re-run the SAME construction the eager path would have
/// run, at selection time instead of generation time.  Parents are
/// safe to reference forever — the clause arena is append-only and
/// retirement is a bitmap; records never move or free.
#[derive(Debug, Clone)]
struct Recipe {
    /// `[given, partner]` for resolution; `[e_cid, t_cid]` for
    /// superposition.
    parents: [u32; 2],
    rule: RecipeRule,
    /// The unifier, snapshotted as its bound (absolute-slot → fragment)
    /// entries — fragments are stored exactly as `unify_off` left them
    /// (bound-side fragments already shifted to absolute slot space),
    /// so rebuilding a `Subst` from this and re-running `apply` /
    /// `apply_off` reproduces the eager conclusion byte-for-byte.
    binding: SmallVec<[(u32, Term); 4]>,
    /// `min(parent tiers)` — frozen at creation (tiers never change).
    tier: u8,
    /// COMPOSED queue weight: the same clause-weight formula `make`
    /// uses, computed on the RAW conclusion's scalars without building
    /// a single term (see [`compose_term`]).  Exact whenever `make`
    /// would not simplify the conclusion; an upper-ish bound otherwise
    /// (duplicate-literal merges and `make`'s literal deletions are
    /// unknowable without materializing — documented approximation,
    /// drift measured in `stats.composed_weight_drift_sum`).
    weight: u64,
}

#[derive(Debug, Clone)]
enum RecipeRule {
    /// Binary resolution `given[gi] × partner[pi]`.  `sym` carries the
    /// symmetric-swap relation when the unifier came from the
    /// resolution-modulo-symmetry retry (cited at materialization
    /// exactly as the eager path cites it).  `decoded` distinguishes
    /// the algebraic fast path's construction (given literals only, no
    /// duplicate-literal pass — the partner is a ground unit) from the
    /// general path's, so replay is bit-faithful to whichever path
    /// deferred it.
    Resolve { gi: u16, pi: u16, sym: Option<SymbolId>, decoded: bool },
    /// Ordered superposition: equation clause's literal `e_li` rewrites
    /// target clause's literal `t_li` at `path`.  The equation's KBO
    /// orientation is recomputed at materialization (deterministic:
    /// memoized content-keyed compares, per-run precedence).
    Superpose { e_li: u16, t_li: u16, path: SmallVec<[u16; 8]> },
}

/// The pre-queue dedup key for a recipe: (rule tag, parents, packed
/// aux) folded through a splitmix64-style avalanche so the result is
/// UNIFORM — the contract `Set64`'s pass-through hasher (and
/// hashbrown's low-bits bucketing) requires.  See `recipe_seen`.
#[inline]
fn recipe_key(tag: u8, a: u32, b: u32, aux: u64) -> u64 {
    #[inline]
    fn mix64(mut z: u64) -> u64 {
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    mix64(mix64(u64::from(tag) << 62 ^ (u64::from(a) << 32) ^ u64::from(b)) ^ aux)
}

/// Accumulator for [`compose_term`]: the composed clause scalars,
/// gathered WITHOUT building any term.
#[derive(Default)]
struct ComposeAcc {
    /// `term_size` of the substituted conclusion (leaf count) —
    /// EXACTLY additive under substitution.
    size: u64,
    /// `term_skolem_apps` of the substituted conclusion — additive the
    /// same way.
    skolems: u64,
    /// Distinct UNBOUND absolute slots seen — the conclusion's `nvars`.
    vars: SmallVec<[u64; 16]>,
}

/// Walk `t` (viewed at slot offset `off`) under substitution `s`,
/// accumulating the scalars `apply_off(t, off, s)` WOULD have — same
/// walk-off semantics (bound fragments are absolute, so recursing into
/// one resets the offset to 0), no allocation.  This is where the
/// deferred-passive discipline earns: the eager path pays a full tree
/// construction + `make` per conclusion just to learn these numbers
/// for queue ordering.
fn compose_term(t: &Term, off: u64, s: &Subst, acc: &mut ComposeAcc) {
    match t {
        Term::Var(v) => {
            let slot = *v + off;
            match s.get(slot as usize).and_then(Option::as_ref) {
                Some(bound) => compose_term(bound, 0, s, acc),
                None => {
                    acc.size += 1;
                    if !acc.vars.contains(&slot) {
                        acc.vars.push(slot);
                    }
                }
            }
        }
        Term::App(elems) => {
            // Mirrors `term_skolem_apps`: the App's own head-skolem
            // count PLUS every element (the head `Sym` recursion adds
            // its own 1 again — the established double count).
            if matches!(elems.first(),
                Some(Term::Sym(sy)) if sy.as_str().starts_with("sk_"))
            {
                acc.skolems += 1;
            }
            for e in elems {
                compose_term(e, off, s, acc);
            }
        }
        Term::Sym(sy) => {
            acc.size += 1;
            if sy.as_str().starts_with("sk_") {
                acc.skolems += 1;
            }
        }
        _ => acc.size += 1,
    }
}

/// [`compose_term`] for the superposition target literal: compose `t`
/// (at offset `off`) with its subterm at `path` REPLACED by `repl`
/// (viewed at `repl_off`) — the scalars of
/// `apply(&replace(t, path, &shift_slots(repl, repl_off)), s)` without
/// building either tree.
fn compose_term_at(
    t: &Term, off: u64,
    path: &[u16],
    repl: &Term, repl_off: u64,
    s: &Subst,
    acc: &mut ComposeAcc,
) {
    let Some((&step, rest)) = path.split_first() else {
        compose_term(repl, repl_off, s, acc);
        return;
    };
    let Term::App(elems) = t else {
        // Path into a non-App can't happen for a path produced by
        // `positions` on this literal; degrade to the unreplaced walk.
        compose_term(t, off, s, acc);
        return;
    };
    if matches!(elems.first(), Some(Term::Sym(sy)) if sy.as_str().starts_with("sk_")) {
        acc.skolems += 1;
    }
    for (i, e) in elems.iter().enumerate() {
        if i == step as usize {
            compose_term_at(e, off, rest, repl, repl_off, s, acc);
        } else {
            compose_term(e, off, s, acc);
        }
    }
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

/// How a driver consumes a [`RunVerdict`]: `Ask` maps saturation onto the
/// Disproved family, `Consistency` onto Consistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerdictMode { Ask, Consistency }

/// The one `RunVerdict` → `(ProverStatus, TerminationReason)` ladder,
/// shared by `prove_one_driver`, `check_consistency_driver`, and
/// `doxastic_project`.  The three used to carry hand-copied ladders that
/// drifted: consistency certified Consistent with no completeness gate at
/// all, and the doxastic ask arm ignored the `complete_saturation` it had
/// itself computed.
///
/// - Refutation: conjecture-rooted → Proved; otherwise the inputs alone
///   derive ⊥ → Inconsistent (both modes).
/// - Saturated + `Ask`: under strict saturation Disproved is a CERTIFICATE
///   and requires `complete_saturation`; the legacy KIF path reports
///   Disproved as a heuristic signal (unchanged behavior).
/// - Saturated + `Consistency`: Consistent is inherently a certificate —
///   ALWAYS gated on `complete_saturation`, strict or not.
pub(crate) fn map_verdict(
    verdict:             RunVerdict,
    conjecture_used:     bool,
    strict_saturation:   bool,
    complete_saturation: Option<bool>,
    mode:                VerdictMode,
) -> (crate::prover::ProverStatus, Option<crate::prover::TerminationReason>) {
    use crate::prover::{ProverStatus as S, TerminationReason as TR};
    match verdict {
        RunVerdict::Refutation(_) if conjecture_used => (S::Proved, None),
        RunVerdict::Refutation(_) => (S::Inconsistent, None),
        RunVerdict::Saturated => match mode {
            VerdictMode::Ask if strict_saturation && complete_saturation != Some(true) =>
                (S::Unknown, Some(TR::Saturation)),
            VerdictMode::Ask =>
                (S::Disproved, Some(TR::Saturation)),
            VerdictMode::Consistency if complete_saturation != Some(true) =>
                (S::Unknown, Some(TR::GaveUp)),
            VerdictMode::Consistency =>
                (S::Consistent, Some(TR::Saturation)),
        },
        RunVerdict::StepsExhausted => (S::Unknown, Some(TR::GaveUp)),
        RunVerdict::TimedOut => (S::Timeout, Some(TR::TimeLimit)),
    }
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
    /// Deferred-inference arena (`Strategy.deferred_passive`): recipes
    /// share the passive heaps with made clauses via `RECIPE_TAG`-bit
    /// ids indexing here.  `take()`n at materialization (entries are
    /// popped at most once — both heap copies dedup through `popped`
    /// like clause ids).  Always empty when the knob is off.
    recipes: Vec<Option<Recipe>>,
    /// Approximate pre-queue dedup for recipes: an avalanche-mixed
    /// 64-bit key over (rule tag, parents, packed aux) — drops exact
    /// re-derivations (e.g. the same superposition reached from both
    /// the "into" and "from" directions) before they queue.  Exact
    /// dedup still happens at materialization, so a false miss is
    /// never unsound; a 64-bit collision falsely dropping a DISTINCT
    /// derivation is ~2^-64 per pair — negligible against the caps
    /// that already shape the search.  Keys MUST be pre-mixed
    /// ([`recipe_key`]): `Set64`'s pass-through hasher is only sound
    /// for uniform keys, and the raw fields here are small sequential
    /// integers (measured: the unmixed tuple key clustered ~every
    /// entry into a handful of buckets — `insert` was 88% of a
    /// RNG044+1 run's CPU).
    recipe_seen: Set64<u64>,
    /// Recipes may be created ONLY inside `run()`'s given-clause loop:
    /// the load-time paths (background/support/conjecture), the forward
    /// closure, and the goal-directed discharge passes all run through
    /// `resolve`/`superpose` too and need materialized results.  Set
    /// by `run()` after its discharge prologue; false everywhere else.
    defer_active: bool,
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
    /// Lazily compiled per-clause literal decode plans for the
    /// subsumption equality-join channel (`Strategy.subs_join`; see
    /// `ej.rs`).  Indexed by clause id in arena order; `None` until the
    /// clause FIRST survives to the ej stage of a `forward_subsumed`
    /// probe — most clauses never do, so compiling lazily (instead of
    /// at accept, as 2a registered rows) avoids the flagged 2a
    /// registration tax.  Compiled once, reused forever (clause terms
    /// are immutable; retirement never invalidates content).
    ej_plans: Vec<Option<Box<ej::ClausePlans>>>,
    /// Reusable scratch for the equality-join channel: the new
    /// clause's transient subterm row table (built at most once per
    /// `forward_subsumed` call, by the first candidate reaching the ej
    /// stage), the decode binding table + trail, and the per-variable
    /// possibility sets.
    ej_scratch: ej::EjScratch,
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
    /// Backward-demodulation subterm-occurrence postings
    /// ([`postings::SubtermPostings`]): exact ground-key postings plus
    /// (head, len) buckets over every rewritable subterm occurrence of
    /// every arena clause.  Maintained per made clause ONLY while
    /// `Strategy.bwd_demod` is on (empty and cost-free otherwise);
    /// retirement is lazy (queries re-check `ClauseRec.retired`) with
    /// counter-driven compaction.  Rebuilt from the arena on snapshot
    /// hydrate, like the other derived indexes.  The phase-2a k-channel
    /// ROWS inside it (content-keyed row table + per-bucket row column)
    /// are populated only when `Strategy.subterm_rows` is ALSO on — off
    /// (the default) the postings are identical but the ~2%
    /// row-registration tax is skipped and the decode chain that would
    /// read the rows never runs.
    bwd_postings: postings::SubtermPostings,
    /// Reusable candidate-clause buffer for the backward-demod pass
    /// (zero per-query allocation once warm).
    bwd_cand_scratch: Vec<u32>,
    /// Reusable compiled decode plan for the backward-demod pass (the
    /// phase-2 k-channel chain, [`rows::PatternPlan`]) — compiled once
    /// per open-lhs pass; node storage amortizes across queries.
    bwd_plan: rows::PatternPlan,
    /// Reusable decode binding table (pattern slot → decoded key) plus
    /// its rollback trail.  Cleared per CANDIDATE via the trail —
    /// decoded blank keys are never joined across candidate clauses.
    bwd_bind_scratch: Vec<Option<u64>>,
    bwd_bind_trail: Vec<u32>,
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
    /// This run's wall-clock deadline, set at the top of [`Self::run`]
    /// (same anchor as the loop-top check).  Generation stages poll it
    /// via [`Self::out_of_time`] so ONE given clause's inference burst
    /// cannot overrun the budget between loop-top checks — a mega-term
    /// clause (SWV536-1.010: a single 354K-position ground literal)
    /// otherwise spends minutes inside a single `superposition_inferences`
    /// call, every `superpose` attempt deep-cloning the whole clause.
    /// `None` = unbounded (no time limit, or interactive step mode).
    run_deadline: Option<Instant>,
    /// B′ term arena: dense hash-consed store of every accepted
    /// clause's terms; readers migrate onto it stage by stage.  Default
    /// ON; `None` under SIGMA_NO_ARENA=1 (every reader falls back to
    /// the owned-tree paths).
    pub(crate) arena: Option<Box<term_arena::TermArena>>,
    /// Per-clause literal-root ids into the arena, lockstep with
    /// `clauses`; an empty entry means owned-tree fallback.
    pub(crate) arena_roots: Vec<SmallVec<[u32; 4]>>,
    /// Atom ids of naming-split guard symbols minted this run — the
    /// unit-guard diagnostic reads it (Strategy.split_naming only).
    pub(crate) split_guard_atoms: Set64<AtomId>,
    /// SIGMA_HINTS watchlist (Veroff-style): canonical keys of
    /// reference-proof clauses; a derived clause matching one is queued
    /// at weight 0.  Empty unless the env lever is set (prove.rs).
    pub(crate) hints: Set64<ClauseKey>,
    /// Distinct hint keys actually matched — reference-proof COVERAGE:
    /// how much of the oracle's proof our calculus ever derived.
    pub(crate) hint_matched: Set64<ClauseKey>,
    pub(crate) stats: ProverStats,
}

impl<'a> NativeProver<'a> {
    pub(crate) fn new(layer: &'a ProverLayer, scope: Scope, opts: NativeOpts) -> Self {
        // Measurement lever: SIGMA_MAX_LITS overrides the derived-clause
        // width cap (single-lane A/B of width headroom; the portfolio
        // path has Strategy.derived_width_cap for the same job).
        let mut opts = opts;
        if let Some(v) = std::env::var("SIGMA_MAX_LITS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
        {
            opts.max_lits = v;
        }
        let prec = (opts.strategy.prec_seed != 0)
            .then(|| super::kbo::KboOrdering::with_prec_seed(opts.strategy.prec_seed));
        // Anchor the wall deadline at CONSTRUCTION: the attempt's budget
        // covers loading too — mega-CNF postings registration can dwarf
        // the search budget (SWV536-1.010: minutes of registration before
        // `run()` ever anchored).  `run()` and `add_conjecture_clauses`
        // respect an existing anchor (set-if-None).
        let run_deadline = (!opts.step && opts.time_limit_secs > 0)
            .then(|| Instant::now() + std::time::Duration::from_secs(opts.time_limit_secs));
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
            recipes: Vec::new(),
            recipe_seen: Set64::default(),
            defer_active: false,
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
            ej_plans: Vec::new(),
            ej_scratch: ej::EjScratch::default(),
            antisym_mined: Map64::default(),
            irrefl_mined: Map64::default(),
            inverse_mined: Vec::new(),
            bg_roots: std::collections::HashSet::new(),
            sym_swap_memo: Map64::default(),
            conj_sig: 0,
            demods: super::units::DemodIndex::default(),
            bwd_postings: postings::SubtermPostings::default(),
            bwd_cand_scratch: Vec::new(),
            bwd_plan: rows::PatternPlan::default(),
            bwd_bind_scratch: Vec::new(),
            bwd_bind_trail: Vec::new(),
            nf_memo: Map64::default(),
            seq: 0,
            tick: 0,
            guide_model: None,
            guide_attempted: false,
            run_deadline,
            arena: term_arena::TermArena::from_env(),
            arena_roots: Vec::new(),
            split_guard_atoms: Set64::default(),
            hints: Set64::default(),
            hint_matched: Set64::default(),
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
        // B′: rebuild the term arena from the hydrated clause list in
        // order — TermId assignment is a pure function of intern order,
        // so the rebuilt arena is identical to the snapshot run's.
        if let Some(ar) = p.arena.as_mut() {
            for c in &p.clauses {
                p.arena_roots
                    .push(ar.intern_clause(&c.terms, &c.lits).unwrap_or_default());
            }
        } else {
            p.arena_roots = vec![Default::default(); p.clauses.len()];
        }
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
        // And the backward-demodulation postings (maintained at `make`
        // time, which the hydrated clauses never went through).
        if p.opts.strategy.bwd_demod {
            p.rebuild_bwd_postings();
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
        // Tombstone the literal index too: `count_complementary` feeds the
        // fewest-candidates literal selection, and counting retired
        // partners lets a literal whose partners are ALL retired win the
        // selection and then produce zero resolvents (the retrieval side
        // filters retired ids after selection — the count must agree).
        self.idx.retire(id);
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
        self.goal_distance_factor_sig(sig, tier)
    }

    /// [`Self::goal_distance_factor`] on a pre-ORed leaf signature —
    /// the recipe path's entry (a recipe has no canonical literals to
    /// scan; it passes its parents' combined sigs, a conservative
    /// superset of the conclusion's).
    fn goal_distance_factor_sig(&self, sig: u64, tier: u8) -> u64 {
        if !self.opts.strategy.goal_dist || self.conj_sig == 0 || tier == CONJECTURE {
            return 1;
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
        if self.opts.strategy.subs_join {
            // The equality-join channel's transient row table (the new
            // clause's subterm rows) is call-scoped: mark it stale
            // here; the FIRST candidate reaching the ej stage rebuilds
            // it, later candidates of this probe reuse it (see ej.rs).
            self.ej_scratch.mark_stale();
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
            // MatchStored: the stored literal is the (candidate
            // subsumer's) PATTERN; if C subsumes D, every C-literal
            // one-way matches some D-literal, so C still surfaces on
            // that literal's probe — the direction-strict seat filter
            // only drops candidates the exact matcher would refuse.
            for at in self.idx.probe_rel(l.pos, info, &src, super::index::SeatRel::MatchStored) {
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
            // Channel: cross-literal equality join on the phase-2a
            // channel rows (`Strategy.subs_join`; see `ej.rs`) —
            // strictly between keq and the exact check.  A `true` here
            // is a sound rejection (necessary-condition machinery,
            // twin-verified against the reference matcher in
            // debug/test builds); `false` says nothing.
            if self.opts.strategy.subs_join
                && self.ej_reject(cid, lits, &tinfos, terms, &c_infos)
            {
                continue;
            }
            self.stats.subs_full_checks += 1;
            let c = &self.clauses[cid as usize];
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

    /// The equality-join prefilter for ONE candidate subsumer (see
    /// `ej.rs`): lazily compiles + memoizes the candidate's per-literal
    /// decode plans, lazily builds the new clause's transient row
    /// table (at most once per `forward_subsumed` call), and runs the
    /// zero-partner + per-variable semi-join rules.  `true` = REJECT —
    /// the exact check would have failed (twin-verified in debug/test
    /// builds at every rejection site).
    fn ej_reject(
        &mut self,
        cid: u32,
        d_lits: &[PLit],
        d_infos: &[AtomInfo],
        d_terms: &[(bool, Term)],
        c_infos: &[std::sync::Arc<AtomInfo>],
    ) -> bool {
        if self.ej_plans.len() < self.clauses.len() {
            self.ej_plans.resize_with(self.clauses.len(), || None);
        }
        if self.ej_plans[cid as usize].is_none() {
            let plans = ej::compile_clause_plans(
                &self.clauses[cid as usize].terms,
                &self.layer.term_facts,
                self.kbo(),
            );
            self.ej_plans[cid as usize] = Some(Box::new(plans));
        }
        let plans = self.ej_plans[cid as usize].as_deref().expect("compiled above");
        let c = &self.clauses[cid as usize];
        ej::filter(
            plans,
            &c.lits,
            c_infos,
            &c.terms,
            d_lits,
            d_infos,
            d_terms,
            &mut self.ej_scratch,
            &mut self.stats,
        )
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
            // Naming-split rescue: instead of the silent discard, split
            // a variable-disjoint over-wide clause into guarded pieces
            // that fit under the cap (docs/plans/splitting-lane.md).
            if self.opts.strategy.split_naming {
                if let Some(sel) = self.try_split(id, max_lits) {
                    return Some(sel);
                }
            }
            self.stats.discarded_long += 1;
            return None;
        }
        // Combined wide-lane discipline: under a raised cap, still split
        // DECOMPOSABLE clauses above the split_width threshold so width
        // headroom is spent on connected clauses only.
        let sw = self.opts.strategy.split_width as usize;
        if self.opts.strategy.split_naming
            && sw > 0
            && self.clauses[id as usize].lits.len() > sw
        {
            if let Some(sel) = self.try_split(id, max_lits.min(sw.max(2))) {
                return Some(sel);
            }
        }
        // Unit-guard diagnostic: a derived unit q / ¬q resolves a split
        // case globally — the split-does-logical-work signal.
        if self.opts.strategy.split_naming
            && self.clauses[id as usize].lits.len() == 1
            && self.split_guard_atoms.contains(&self.clauses[id as usize].lits[0].atom)
        {
            self.stats.split_guard_units += 1;
        }
        self.seen_record(key, id);
        if std::env::var_os("SIGMA_HINTS_DEBUG").is_some() && !self.hints.is_empty() {
            let c = &self.clauses[id as usize];
            eprintln!("LOAD {:016x} {:?} rule={}", key.0, c.lits, c.rule);
        }
        let c = &self.clauses[id as usize];
        let (mut w, n) = (c.weight, self.seq);
        // Watchlist boost: a clause on the reference proof's path goes
        // to the front of the weight queue.
        if !self.hints.is_empty() && self.hints.contains(&key) {
            w = 0;
            self.hint_matched.insert(key);
            self.stats.hint_boosts += 1;
        }
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

    /// Naming-split rescue (docs/plans/splitting-lane.md step 2): split
    /// the over-wide accepted-but-unqueued clause `id` into variable-
    /// disjoint components through fresh propositional guards, selector
    /// encoding — pieces `¬q_i ∨ K_i` plus the selector `q_1 ∨ … ∨ q_n`
    /// — every emitted clause under the cap.  Guard names derive from
    /// each component's canonical content key, so identical components
    /// ANYWHERE in the run share one guard symbol and their definition
    /// pieces dedup through the ordinary `ClauseKey` gate (the
    /// Riazanov–Voronkov component index, by content addressing).
    /// Pieces cite `id` as parent (rule "split"), so proofs walk
    /// through the original wide clause to its real derivation.
    /// Returns the queued selector id, or None when the clause does
    /// not decompose into cap-fitting pieces — the caller then
    /// discards exactly as before.
    fn try_split(&mut self, id: u32, max_lits: usize) -> Option<u32> {
        let (terms, tier) = {
            let c = &self.clauses[id as usize];
            (c.terms.clone(), c.tier)
        };
        let comps = var_disjoint_components(&terms);
        // Rescue conditions: genuinely decomposable, selector fits the
        // cap, every piece (component + its guard) fits the cap.
        if comps.len() < 2 {
            self.stats.split_bail_connected += 1;
            return None;
        }
        if comps.len() > max_lits {
            self.stats.split_bail_selector += 1;
            return None;
        }
        if comps.iter().any(|k| k.len() + 1 > max_lits) {
            self.stats.split_bail_fat += 1;
            return None;
        }
        let mut selector: Vec<(bool, Term)> = Vec::with_capacity(comps.len());
        for comp in &comps {
            let lits: Vec<(bool, Term)> =
                comp.iter().map(|&i| terms[i].clone()).collect();
            let (pc, _) = super::canon::canonical_clause_hashed(lits.clone());
            let guard = Term::Sym(Symbol::from(
                format!("sp_{:016x}", pc.key.0).as_str(),
            ));
            let mut piece: Vec<(bool, Term)> = Vec::with_capacity(lits.len() + 1);
            piece.push((false, guard.clone()));
            piece.extend(lits);
            let made = self.make(piece, vec![id], "split", tier, None, true);
            if made.is_some() {
                self.stats.split_pieces += 1;
            }
            self.push(made);
            selector.push((true, guard));
        }
        let sel = self.make(selector, vec![id], "split", tier, None, true);
        if let Some(sid) = sel {
            for l in self.clauses[sid as usize].lits.clone() {
                self.split_guard_atoms.insert(l.atom);
            }
            self.stats.split_pieces += 1;
        }
        let queued = self.push(sel);
        self.stats.split_rescued += 1;
        queued.or(Some(id))
    }

    /// Whether the deferred-passive discipline applies to the current
    /// inference: the knob is on AND we are inside `run()`'s
    /// given-clause loop (see `defer_active`).
    #[inline]
    fn defer_recipes(&self) -> bool {
        self.opts.strategy.deferred_passive && self.defer_active
    }

    /// Whether the recipe budget (`Strategy::deferred_cap`) has a free
    /// slot.  Live recipes = queued − materialized (the pre-queue dedup
    /// rejects BEFORE `recipes_queued` counts, so deduped pushes never
    /// occupy a slot; materialization is the only decrement — `take()`n
    /// arena entries free their binding fragments).  At the cap, new
    /// products fall back to the EAGER path (see the deferral sites) —
    /// counted in `deferred_cap_fallbacks`, never dropped.
    #[inline]
    fn recipe_slot_available(&self) -> bool {
        self.stats.recipes_queued - self.stats.recipes_materialized
            < u64::from(self.opts.strategy.deferred_cap)
    }

    /// The semantic-guide tie-break column for a recipe queue entry.
    /// A recipe has no canonical literals to score, so it takes the
    /// all-neutral half-point `guide_score` gives unmodeled clauses
    /// (`GUIDE_SCALE / 2`); 0 (the inert value) when the knob is off,
    /// so knob-off heap keys stay byte-identical.
    #[inline]
    fn recipe_guide_key(&self) -> u64 {
        if self.opts.strategy.semantic_guide { 500 } else { 0 }
    }

    /// OR of both parents' literal leaf signatures — the conservative
    /// goal-distance profile for a recipe (a conclusion's leaves are a
    /// subset of its parents': bindings come from unifying two parent
    /// literals).  Only called when `goal_dist` is live.
    fn parents_leaf_sig(&self, a: u32, b: u32) -> u64 {
        let mut sig = 0u64;
        for &cid in &[a, b] {
            for l in &self.clauses[cid as usize].lits {
                sig |= self.layer.atom_info(l.atom).leaf_sig;
            }
        }
        sig
    }

    /// The composed queue weight for a recipe — the SAME clause-weight
    /// formula `make` applies to a materialized clause (`cw_*` genome ×
    /// tier weight × goal-distance factor), fed the composed scalars.
    fn recipe_weight(&self, acc: &ComposeAcc, nlits: u64, tier: u8, sig: u64) -> u64 {
        let st = &self.opts.strategy;
        let base = (st.cw_lits * nlits
            + st.cw_size * acc.size
            + st.cw_vars * acc.vars.len() as u64)
            .max(1)
            * (1 + st.cw_skolem * acc.skolems);
        base * st.tier_weight[tier as usize] * self.goal_distance_factor_sig(sig, tier)
    }

    /// Snapshot the bound entries of a unifier — the replayable binding
    /// fragment a [`Recipe`] stores.
    fn snapshot_binding(s: &Subst, n: usize) -> SmallVec<[(u32, Term); 4]> {
        let mut out: SmallVec<[(u32, Term); 4]> = SmallVec::new();
        for (slot, b) in s.iter().enumerate().take(n) {
            if let Some(t) = b {
                out.push((slot as u32, t.clone()));
            }
        }
        out
    }

    /// Queue a recipe (deferred inference), unless the approximate
    /// pre-queue dedup has seen the identical (rule, parents, aux)
    /// derivation.  Shares the passive heaps with made clauses — the
    /// composed weight competes under exactly the ordering `push` uses.
    fn push_recipe(&mut self, recipe: Recipe, key: u64) {
        if !self.recipe_seen.insert(key) {
            self.stats.recipes_prequeue_deduped += 1;
            return;
        }
        let idx = self.recipes.len() as u32;
        debug_assert_eq!(idx & RECIPE_TAG, 0, "recipe arena overflowed the tag bit");
        let tagged = RECIPE_TAG | idx;
        let (w, g, n) = (recipe.weight, self.recipe_guide_key(), self.seq);
        self.seq += 1;
        self.recipes.push(Some(recipe));
        self.h_weight.push(Reverse((w, g, n, tagged)));
        self.h_age.push(Reverse((n, tagged)));
        self.stats.recipes_queued += 1;
    }

    /// Materialize a selected recipe: re-run the SAME construction the
    /// eager path would have run (identical inputs ⇒ identical clause),
    /// then the FULL `make` pipeline, then the exact dedup + width
    /// gates `push` would have applied at generation time.  `Some(id)`
    /// ⇒ the clause becomes the given; `None` ⇒ rejected (counted), the
    /// caller pops the next passive entry.  Rejection costs exactly
    /// what the eager path pays for EVERY conclusion, so the worst case
    /// per entry is today's cost.
    fn materialize_recipe(&mut self, ridx: u32) -> Option<u32> {
        let Some(recipe) = self.recipes[ridx as usize].take() else {
            debug_assert!(false, "recipe {ridx} materialized twice");
            return None;
        };
        self.stats.recipes_materialized += 1;
        let subs_before = self.stats.subsumed;
        let made = match recipe.rule {
            RecipeRule::Resolve { .. } => self.materialize_resolve(&recipe),
            RecipeRule::Superpose { .. } => self.materialize_superpose(&recipe),
        };
        let Some(id) = made else {
            // `make` rejected it — attribute forward subsumption via
            // the counter delta; everything else (tautology, oracle /
            // unit subsumption, caps) folds into `other`.
            if self.stats.subsumed > subs_before {
                self.stats.act_subsumed += 1;
            } else {
                self.stats.act_rejected_other += 1;
            }
            return None;
        };
        if self.clauses[id as usize].lits.is_empty() {
            // A refutation candidate: hand it to `run()`'s empty-given
            // handling (the eager path checks emptiness before dedup
            // too — an earlier suppressed empty clause must never
            // swallow this one as a "duplicate").
            return Some(id);
        }
        // The dedup + width gates `push_capped` runs at generation time.
        let key = self.clauses[id as usize].key;
        if self.seen_duplicate(key, id) {
            self.stats.act_dedup_hits += 1;
            return None;
        }
        if self.clauses[id as usize].lits.len() > self.opts.max_lits {
            self.stats.discarded_long += 1;
            self.stats.act_over_cap += 1;
            return None;
        }
        self.seen_record(key, id);
        // Composed-vs-exact weight drift sample (accepted entries only:
        // rejects have no meaningful exact weight).
        let exact = self.clauses[id as usize].weight;
        self.stats.composed_weight_samples += 1;
        self.stats.composed_weight_drift_sum += exact.abs_diff(recipe.weight);
        if exact == recipe.weight {
            self.stats.composed_weight_exact += 1;
        }
        Some(id)
    }

    /// Replay a deferred binary resolution — the general path's
    /// construction, or the decoded fast path's (`decoded`), exactly as
    /// the eager code would have built it.
    fn materialize_resolve(&mut self, rp: &Recipe) -> Option<u32> {
        let [given, partner] = rp.parents;
        let RecipeRule::Resolve { gi, pi, sym, decoded } = rp.rule else {
            unreachable!("materialize_resolve on a non-resolve recipe")
        };
        let (g_nvars, p_nvars) = {
            let g = &self.clauses[given as usize];
            let p = &self.clauses[partner as usize];
            (g.nvars, p.nvars)
        };
        let off = u64::from(g_nvars) + 1;
        let n = (off + u64::from(p_nvars) + 1) as usize;
        let mut s = self.take_scratch(n);
        for (slot, t) in &rp.binding {
            s[*slot as usize] = Some(t.clone());
        }
        let out: Vec<(bool, Term)> = {
            let g = &self.clauses[given as usize];
            let p = &self.clauses[partner as usize];
            if decoded {
                // The decoded path's construction: the given's other
                // literals under σ; the ground unit partner contributes
                // nothing (and no duplicate-literal pass ran there).
                g.terms
                    .iter()
                    .enumerate()
                    .filter(|(k, _)| *k != gi as usize)
                    .map(|(_, (pos, t))| (*pos, apply(t, &s)))
                    .collect()
            } else {
                let mut new: Vec<(bool, Term)> =
                    Vec::with_capacity(g.terms.len() + p.terms.len() - 2);
                for (k, (pos, t)) in g.terms.iter().enumerate() {
                    if k != gi as usize { new.push((*pos, apply(t, &s))); }
                }
                for (k, (pos, t)) in p.terms.iter().enumerate() {
                    if k != pi as usize { new.push((*pos, apply_off(t, off, &s))); }
                }
                // Drop duplicate literals (the eager path's pass).
                let mut out: Vec<(bool, Term)> = Vec::with_capacity(new.len());
                for (pos, t) in new {
                    if !out.iter().any(|(p2, u)| *p2 == pos && *u == t) {
                        out.push((pos, t));
                    }
                }
                out
            }
        };
        self.put_scratch(s, n);
        let rule = if sym.is_some() { "resolve_sym" } else { "resolve" };
        let made = self.make(out, vec![given, partner], rule, rp.tier, None, true);
        if let (Some(id), Some(rel)) = (made, sym) {
            self.stats.sym_resolutions += 1;
            if let Some(sid) = self.oracle.symmetric_source(rel) {
                self.clauses[id as usize].fact_parents.push(sid);
            }
        }
        made
    }

    /// Replay a deferred superposition.  The equation's orientation is
    /// recomputed (deterministic — memoized content-keyed KBO under
    /// this run's fixed precedence), the stored unifier is rebuilt, and
    /// the conclusion is constructed exactly as `superpose` builds it.
    fn materialize_superpose(&mut self, rp: &Recipe) -> Option<u32> {
        let [e_cid, t_cid] = rp.parents;
        let RecipeRule::Superpose { e_li, t_li, ref path } = rp.rule else {
            unreachable!("materialize_superpose on a non-superpose recipe")
        };
        let (e_terms, e_tier) = {
            let c = &self.clauses[e_cid as usize];
            (c.terms.clone(), c.tier)
        };
        let (_s, t) = self.equality_oriented(&e_terms[e_li as usize].1)?;
        let (t_terms, t_nvars, t_tier) = {
            let c = &self.clauses[t_cid as usize];
            (c.terms.clone(), c.nvars, c.tier)
        };
        let off = u64::from(t_nvars) + 1;
        let t2 = shift_slots(&t, off);
        let max_slot = rp.binding.iter().map(|(sl, _)| *sl).max().unwrap_or(0);
        let mut subst: Subst = vec![None; max_slot as usize + 1];
        for (slot, b) in &rp.binding {
            subst[*slot as usize] = Some(b.clone());
        }
        let path: Vec<usize> = path.iter().map(|&p| p as usize).collect();
        let mut lits: Vec<(bool, Term)> =
            Vec::with_capacity(e_terms.len() + t_terms.len());
        for (k, (pos, term)) in e_terms.iter().enumerate() {
            if k == e_li as usize { continue; }
            lits.push((*pos, apply(&shift_slots(term, off), &subst)));
        }
        for (k, (pos, term)) in t_terms.iter().enumerate() {
            let rewritten =
                if k == t_li as usize { replace(term, &path, &t2) } else { term.clone() };
            lits.push((*pos, apply(&rewritten, &subst)));
        }
        debug_assert_eq!(e_tier.min(t_tier), rp.tier, "recipe tier drifted");
        self.make(lits, vec![e_cid, t_cid], "superpos", rp.tier, None, true)
    }

    fn pop_given(&mut self) -> Option<u32> {
        loop {
            let id = self.pop_queue_entry()?;
            if id & RECIPE_TAG == 0 {
                return Some(id);
            }
            // A recipe entry: materialize it into the given clause; a
            // rejection (duplicate / subsumed / over-cap — counted in
            // `materialize_recipe`) pops the next passive entry.  Each
            // rejection replays a full make pipeline, so a deep backlog
            // of rejecting recipes can grind for minutes inside ONE
            // `run()` iteration — poll the anchored deadline between
            // materializations.  The caller re-checks `out_of_time` on a
            // `None`, grading this bail TimedOut rather than Saturated.
            if self.out_of_time() {
                return None;
            }
            if let Some(mid) = self.materialize_recipe(id & !RECIPE_TAG) {
                return Some(mid);
            }
        }
    }

    /// One raw pop from the passive heaps (clause id or tagged recipe
    /// id) — the pre-deferred `pop_given` body.
    fn pop_queue_entry(&mut self) -> Option<u32> {
        self.tick += 1;
        let prefer_age = self.tick % self.opts.strategy.pick_ratio.max(1) == 0;
        for pass in 0..2 {
            let from_age = prefer_age == (pass == 0);
            // Retired (backward-demodulated) clauses are lazily skipped
            // here — cheaper than deleting heap entries, and marking
            // them popped keeps the other heap's copy dead too.
            // (Recipe entries skip the retirement probe: the bitmap is
            // indexed by CLAUSE id, and recipes retire nothing.)
            if from_age {
                while let Some(Reverse((_, id))) = self.h_age.pop() {
                    if self.popped.insert(id) {
                        if id & RECIPE_TAG == 0 && self.is_retired(id) { continue; }
                        return Some(id);
                    }
                }
            } else {
                while let Some(Reverse((_, _, _, id))) = self.h_weight.pop() {
                    if self.popped.insert(id) {
                        if id & RECIPE_TAG == 0 && self.is_retired(id) { continue; }
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

    parked! {
        /// Whether ordered superposition's maximality machinery is needed.
        #[inline]
        fn needs_superposition(&self) -> bool {
            self.opts.strategy.superposition
        }
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

    /// `true` once this run's wall-clock budget is spent or the caller's
    /// cancel flag is raised (see `run_deadline`).  Polled inside the
    /// generation stages: the loop-top deadline check alone cannot bound
    /// a single iteration, and one mega-clause iteration can otherwise
    /// run minutes past the budget.
    #[inline]
    pub(super) fn out_of_time(&self) -> bool {
        self.opts.cancelled()
            || self.run_deadline.is_some_and(|d| Instant::now() >= d)
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
        // Orient the "from" equation and locate the rewrite target `u`
        // BEFORE cloning anything clause-sized: most attempts die at the
        // mgu below, and the old path paid two full term-vector clones
        // plus two shifted trees per failure.  Up front only the two
        // oriented equation sides and the target subterm are cloned.
        let e_tier = self.clauses[e_cid as usize].tier;
        let (t_nvars, t_tier) = {
            let c = &self.clauses[t_cid as usize];
            (c.nvars, c.tier)
        };
        let (s, t) = self.equality_oriented(&self.clauses[e_cid as usize].terms[e_li].1)?;

        // The rewrite target `u` — never a variable (superposition into
        // variables is unsound for completeness and explosive).
        let u = subterm_at(&self.clauses[t_cid as usize].terms[t_li].1, t_path)?.clone();
        if matches!(u, Term::Var(_)) { return None; }

        // Rename-apart is VIRTUAL (`unify_off`): the equation's slots
        // ride at `off`, nothing is shifted before the mgu succeeds.
        // Slots are DENSE per clause (canonical renumbering: target
        // 0..=t_nvars, equation off..=off+e_nvars), so the table size is
        // arithmetic — same convention as `resolve` — with no
        // per-attempt slot-collection walk over every literal.
        let off = u64::from(t_nvars) + 1;
        let e_nvars = self.clauses[e_cid as usize].nvars;
        let mut subst: Subst = vec![None; (off + u64::from(e_nvars) + 1) as usize];

        // σ = mgu(s, u).
        if !unify_off(&s, off, &u, 0, &mut subst) { return None; }

        // The mgu holds — NOW materialize the copies the construction
        // below consumes (bindings from `unify_off` are absolute, byte-
        // identical to unifying against a pre-shifted equation side).
        let e_terms = self.clauses[e_cid as usize].terms.clone();
        let t_terms = self.clauses[t_cid as usize].terms.clone();
        let t2 = shift_slots(&t, off);

        // Deferred-passive discipline: queue a recipe instead of
        // building the conclusion + running `make`.  Conjecture-tier
        // products stay eager (the goal line's progress — including
        // empty-clause detection — must not sit deferred in the queue).
        // At the recipe budget (`Strategy::deferred_cap`) the product
        // falls through to the EAGER construction below instead —
        // counted, never dropped.
        let tier = e_tier.min(t_tier);
        if self.defer_recipes() && tier != CONJECTURE && !self.recipe_slot_available() {
            self.stats.deferred_cap_fallbacks += 1;
        } else if self.defer_recipes() && tier != CONJECTURE {
            // Composed scalars of the raw conclusion, no terms built.
            let mut acc = ComposeAcc::default();
            for (k, (_, term)) in e_terms.iter().enumerate() {
                if k == e_li { continue; }
                compose_term(term, off, &subst, &mut acc);
            }
            let path16: SmallVec<[u16; 8]> =
                t_path.iter().map(|&p| p as u16).collect();
            for (k, (_, term)) in t_terms.iter().enumerate() {
                if k == t_li {
                    compose_term_at(term, 0, &path16, &t, off, &subst, &mut acc);
                } else {
                    compose_term(term, 0, &subst, &mut acc);
                }
            }
            let nlits = (e_terms.len() - 1 + t_terms.len()) as u64;
            let sig = if self.opts.strategy.goal_dist && self.conj_sig != 0 {
                self.parents_leaf_sig(e_cid, t_cid)
            } else {
                0
            };
            let weight = self.recipe_weight(&acc, nlits, tier, sig);
            let binding = Self::snapshot_binding(&subst, subst.len());
            // Aux key: literal indexes + a fold of the path (the same
            // (e,t,path) inference is reachable from both superposition
            // directions — this is exactly the re-derivation the
            // pre-queue dedup exists to drop).
            let aux = ((e_li as u64) << 56)
                | ((t_li as u64) << 48)
                | (path16.iter().fold(0u64, |h, &p| {
                    h.wrapping_mul(0x0000_0100_0000_01B3) ^ u64::from(p)
                }) & 0x0000_FFFF_FFFF_FFFF);
            self.push_recipe(
                Recipe {
                    parents: [e_cid, t_cid],
                    rule: RecipeRule::Superpose { e_li: e_li as u16, t_li: t_li as u16, path: path16 },
                    binding,
                    tier,
                    weight,
                },
                recipe_key(1, e_cid, t_cid, aux),
            );
            return None;
        }

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
            for path in positions_paths(atom) {
                for &(e_cid, e_li) in &eqns {
                    // Wall-clock poll per attempt: a mega-term clause
                    // makes this loop the budget's blind spot.
                    // Truncation is counted as `gen_capped` (same
                    // completeness semantics: inferences were never made).
                    if self.out_of_time() {
                        self.stats.gen_capped += 1;
                        return None;
                    }
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
                // Same wall-clock poll as the "into" direction: a mega-term
                // ACTIVE clause makes every rewrite of it just as unbounded.
                if self.out_of_time() {
                    self.stats.gen_capped += 1;
                    return None;
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
            let Some(terms) = self.pclause_terms(pc) else {
                // Slot-lift failure (>MAX_CANON_SLOTS distinct variables):
                // the clause is stored, so `root_load_failed` still reads
                // the root as loaded — count the loss or
                // `complete_saturation` certifies a weakened theory.
                self.stats.slot_lift_failures += 1;
                continue;
            };
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

    /// Add SYNTHESIZED axiom clauses (problem-assembly injection — the
    /// modal K-distribution schemata) as BACKGROUND inputs: exactly
    /// [`Self::add_background_root`]'s indexing discipline, minus the
    /// root bookkeeping — there is no stored root, so the clauses cite
    /// `rule` in proofs (like `subrel_schema`) instead of a source
    /// formula.  Called AFTER any frozen-background snapshot is taken /
    /// rehydrated (see prove.rs), so snapshots never contain injected
    /// clauses and rehydration can never silently drop them.
    pub(crate) fn add_injected_clauses(
        &mut self,
        clauses: &[super::clause::PClause],
        rule:    &'static str,
    ) {
        for pc in clauses {
            let Some(terms) = self.pclause_terms(pc) else {
                // Slot-lift failure (>MAX_CANON_SLOTS distinct variables):
                // the clause is stored, so `root_load_failed` still reads
                // the root as loaded — count the loss or
                // `complete_saturation` certifies a weakened theory.
                self.stats.slot_lift_failures += 1;
                continue;
            };
            if let Some(id) = self.make(terms, vec![], rule, BACKGROUND, None, false) {
                let key = self.clauses[id as usize].key;
                if self.clauses[id as usize].lits.len() > self.input_width_cap() {
                    self.stats.discarded_long += 1;
                    continue;
                }
                if self.seen_insert(key, id) {
                    // Same regime split as `add_background_root`: under
                    // full saturation the injected axioms compete for
                    // given selection too; classic set-of-support only
                    // indexes them as passive partners.
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
            let Some(terms) = self.pclause_terms(pc) else {
                // Slot-lift failure (>MAX_CANON_SLOTS distinct variables):
                // the clause is stored, so `root_load_failed` still reads
                // the root as loaded — count the loss or
                // `complete_saturation` certifies a weakened theory.
                self.stats.slot_lift_failures += 1;
                continue;
            };
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
    ///
    /// `conjecture_root` is the (first) stored `SentenceId` the caller
    /// negated+clausified — threaded through as each clause's `source` so
    /// `extract_proof` can cite the original conjecture and link every
    /// resulting unit clause back to ONE shared "negated conjecture" step,
    /// instead of rendering each as an unrelated, parentless fact.
    pub(crate) fn add_conjecture_clauses(
        &mut self,
        clauses: &[super::clause::PClause],
        conjecture_root: Option<SentenceId>,
    ) {
        self.has_conjecture = true;
        // Loading itself can exhaust the wall budget on mega-CNF inputs
        // (postings registration walks every subterm of every accepted
        // clause — SWV536-1.010 spent minutes here, past every generation
        // -stage poll).  Anchor the deadline at load start so the
        // registration poll in `bwd_index_clause` bites during loading;
        // `run()` keeps this anchor when set, bounding the WHOLE attempt
        // by one budget instead of load+run each getting a fresh one.
        if self.run_deadline.is_none() && !self.opts.step && self.opts.time_limit_secs > 0 {
            self.run_deadline = Some(
                Instant::now() + std::time::Duration::from_secs(self.opts.time_limit_secs),
            );
        }
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
            let Some(terms) = self.pclause_terms(pc) else {
                // Slot-lift failure (>MAX_CANON_SLOTS distinct variables):
                // the clause is stored, so `root_load_failed` still reads
                // the root as loaded — count the loss or
                // `complete_saturation` certifies a weakened theory.
                self.stats.slot_lift_failures += 1;
                continue;
            };
            let id = self.make(terms, vec![], "negated_conjecture", CONJECTURE, conjecture_root, false);
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
        // Wall-clock poll: the candidate loops in `run` call this once per
        // partner, and a single attempt against a mega-term clause is a
        // whole-clause unify walk — past the deadline each remaining
        // candidate must degrade to this cheap bail (the loop-top check
        // then reports TimedOut before anything is popped).
        if self.out_of_time() {
            return None;
        }
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
        // The recipe-budget probe is hoisted out of the borrow block
        // below (`stats` needs `&mut self` there); nothing between here
        // and `push_recipe` can change the live-recipe count.
        let defer_armed = self.defer_recipes();
        let defer_slot = defer_armed && self.recipe_slot_available();
        let mut cap_fallback = false;
        let mut s = self.take_scratch(n);
        let mut via_symmetry: Option<SymbolId> = None;
        let mut deferred: Option<(Recipe, u64)> = None;
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
            let want_defer = defer_armed
                && g.terms.len() + p.terms.len() > 2
                && g.tier.min(p.tier) != CONJECTURE;
            if !matched {
                None // hash-collision reject
            } else if want_defer && defer_slot {
                // Deferred-passive discipline: snapshot the unifier and
                // the composed scalars; construction + `make` run at
                // selection.  Unit×unit resolvents (the raw empty
                // clause) and conjecture-tier products stay eager —
                // refutation detection must not sit deferred in the
                // queue.
                let tier = g.tier.min(p.tier);
                let mut acc = ComposeAcc::default();
                for (k, (_, t)) in g.terms.iter().enumerate() {
                    if k != gi { compose_term(t, 0, &s, &mut acc); }
                }
                for (k, (_, t)) in p.terms.iter().enumerate() {
                    if k != pi { compose_term(t, off, &s, &mut acc); }
                }
                let nlits = (g.terms.len() + p.terms.len() - 2) as u64;
                let sig = if self.opts.strategy.goal_dist && self.conj_sig != 0 {
                    self.parents_leaf_sig(given, partner)
                } else {
                    0
                };
                let weight = self.recipe_weight(&acc, nlits, tier, sig);
                deferred = Some((
                    Recipe {
                        parents: [given, partner],
                        rule: RecipeRule::Resolve {
                            gi: gi as u16,
                            pi: pi as u16,
                            sym: via_symmetry,
                            decoded: false,
                        },
                        binding: Self::snapshot_binding(&s, n),
                        tier,
                        weight,
                    },
                    recipe_key(0, given, partner, ((gi as u64) << 32) | pi as u64),
                ));
                None
            } else {
                // Reaching here with `want_defer` set means the recipe
                // budget (`Strategy::deferred_cap`) was exhausted: fall
                // back to the EAGER path — build + `make` now, exactly
                // as knob-off.  Never drop the inference (dropping
                // loses derivations); counted after the borrow block.
                cap_fallback = want_defer;
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
        if cap_fallback {
            self.stats.deferred_cap_fallbacks += 1;
        }
        if let Some((recipe, key)) = deferred {
            // The unification succeeded and the inference exists — the
            // hit/volume counters advance at creation (so ON-vs-OFF
            // `resolvents` counts stay comparable); manufacture is
            // counted separately in `recipes_materialized`.
            self.stats.resolve_unify_hits += 1;
            self.stats.resolvents += 1;
            self.push_recipe(recipe, key);
            return None;
        }
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

        // Deferred-passive discipline: same gates as the general path —
        // the decoded bindings ARE the unifier fragment, so the recipe
        // replays without any decode machinery.  At the recipe budget
        // (`Strategy::deferred_cap`) the product falls through to the
        // EAGER construction below instead — counted, never dropped.
        let tier = shape.g_tier.min(self.clauses[partner as usize].tier);
        if self.defer_recipes()
            && self.clauses[given as usize].terms.len() > 1
            && tier != CONJECTURE
            && !self.recipe_slot_available()
        {
            self.stats.deferred_cap_fallbacks += 1;
        } else if self.defer_recipes()
            && self.clauses[given as usize].terms.len() > 1
            && tier != CONJECTURE
        {
            let g = &self.clauses[given as usize];
            let mut acc = ComposeAcc::default();
            for (k, (_, t)) in g.terms.iter().enumerate() {
                if k != gi { compose_term(t, 0, &s, &mut acc); }
            }
            let nlits = (g.terms.len() - 1) as u64;
            let sig = if self.opts.strategy.goal_dist && self.conj_sig != 0 {
                self.parents_leaf_sig(given, partner)
            } else {
                0
            };
            let weight = self.recipe_weight(&acc, nlits, tier, sig);
            let binding = Self::snapshot_binding(&s, s.len());
            self.stats.resolvents += 1;
            self.stats.decoded_resolutions += 1;
            if count { self.stats.decode_bindings_extracted += 1; }
            self.push_recipe(
                Recipe {
                    parents: [given, partner],
                    rule: RecipeRule::Resolve {
                        gi: gi as u16,
                        pi: 0,
                        sym: None,
                        decoded: true,
                    },
                    binding,
                    tier,
                    weight,
                },
                // Same key space as the general path: the decoded fast
                // path and general unification of the same (given, gi,
                // partner, pi=0) pair are the SAME inference.
                recipe_key(0, given, partner, (gi as u64) << 32),
            );
            return Some(None);
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
                // lits² unify/make attempts with no output cap — poll the
                // wall clock like `paramodulants`; the truncation counts
                // into `gen_capped` so strict saturation never certifies
                // over the skipped inferences.
                if self.stats.factor_attempts & 63 == 0 && self.out_of_time() {
                    self.stats.gen_capped += 1;
                    return out;
                }
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
        let mut polls = 0u32;
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
                        // Up to 4·lits² eager `make`s per given with no
                        // output cap (unlike para_cap'd superposition) —
                        // poll the wall clock like `paramodulants`, and
                        // count the truncation so strict saturation never
                        // certifies over the skipped inferences.
                        polls += 1;
                        if polls & 63 == 0 && self.out_of_time() {
                            self.stats.gen_capped += 1;
                            return out;
                        }
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
        let mut polls = 0u32;
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
                    // Failing unifies never tick `n`, so `para_cap` alone
                    // does not bound this walk over a mega-term clause —
                    // poll the wall clock every 64 positions.
                    polls += 1;
                    if polls & 63 == 0 && self.out_of_time() {
                        self.stats.gen_capped += 1;
                        return None;
                    }
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

    /// Serialize the active clause set's slot-form literal terms, one
    /// literal per line, for the GATE 0 representation bench.  Gated
    /// behind `SIGMA_GATE0_DUMP=<path>` at the call site.
    pub(crate) fn gate0_dump(&self, path: &std::path::Path) -> std::io::Result<()> {
        use std::io::Write;
        fn esc(out: &mut impl Write, s: &str) -> std::io::Result<()> {
            for b in s.bytes() {
                match b {
                    b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.' => {
                        out.write_all(&[b])?
                    }
                    _ => write!(out, "%{b:02x}")?,
                }
            }
            Ok(())
        }
        fn wt(out: &mut impl Write, t: &Term) -> std::io::Result<()> {
            match t {
                Term::Var(v) => write!(out, "v:{v}"),
                Term::Sym(s) => {
                    write!(out, "s:")?;
                    esc(out, &s.name())
                }
                Term::Lit(crate::types::Literal::Str(x)) => {
                    write!(out, "l:")?;
                    esc(out, x)
                }
                Term::Lit(crate::types::Literal::Number(x)) => {
                    write!(out, "n:")?;
                    esc(out, x)
                }
                Term::Op(op) => write!(out, "o:{op:?}"),
                Term::App(elems) => {
                    write!(out, "(")?;
                    for (i, e) in elems.iter().enumerate() {
                        if i > 0 {
                            write!(out, " ")?;
                        }
                        wt(out, e)?;
                    }
                    write!(out, ")")
                }
            }
        }
        let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
        for c in &self.clauses {
            if !c.activated || self.is_retired(c.id) {
                continue;
            }
            writeln!(out, "# clause {} nvars {} rule {}", c.id, c.nvars, c.rule)?;
            for (pos, t) in &c.terms {
                write!(out, "T {} ", u8::from(*pos))?;
                wt(&mut out, t)?;
                writeln!(out)?;
            }
        }
        out.flush()
    }

    /// Splitting-plan step-1 diagnostic (docs/plans/splitting-lane.md):
    /// clause-width histogram split by provenance (input vs generated),
    /// plus the variable-disjoint-component count per width band — the
    /// naming-split opportunity is a clause that is BOTH wide AND
    /// decomposable.  Gated behind `SIGMA_WIDTH_DUMP` at the call site.
    pub(crate) fn width_histogram(&self) -> String {
        // width -> (input count, generated count, decomposable count)
        let mut bands: std::collections::BTreeMap<usize, (u64, u64, u64)> =
            std::collections::BTreeMap::new();
        for c in &self.clauses {
            let w = c.lits.len();
            let e = bands.entry(w.min(32)).or_insert((0, 0, 0));
            if c.rule == "axiom" || c.rule == "conjecture" {
                e.0 += 1;
            } else {
                e.1 += 1;
            }
            // variable-disjoint decomposability: do the literals split
            // into >=2 groups sharing no slots?  Union-find over
            // literals via slot sets (cheap: widths are small).
            if w >= 2 && var_disjoint_components(&c.terms).len() >= 2 {
                e.2 += 1;
            }
        }
        let mut out = String::from("width	input	generated	decomposable
");
        for (w, (i, g, d)) in bands {
            out.push_str(&format!("{w}	{i}	{g}	{d}
"));
        }
        out.push_str(&format!(
            "discarded_long {} discarded_deep {} max_lits {}
",
            self.stats.discarded_long, self.stats.discarded_deep, self.opts.max_lits,
        ));
        out
    }

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
        // Anchor the wall deadline BEFORE the discharge prologue: the
        // horn-join / model / event-calculus passes below can be the
        // expensive part of an attempt, and their internal polls read
        // `run_deadline` — anchoring after them would leave the prologue
        // unbounded.  Keep a load-time anchor when one exists (see
        // `add_conjecture_clauses`): the attempt's budget covers load AND
        // search.  Fast-load paths reach here with `None` and anchor now,
        // exactly as before.
        let t0 = Instant::now();
        if self.run_deadline.is_none() {
            self.run_deadline = (!self.opts.step && self.opts.time_limit_secs > 0)
                .then(|| t0 + std::time::Duration::from_secs(self.opts.time_limit_secs));
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
        self.defer_active = false;
        let dbg_prologue = std::env::var_os("SIGMA_MODEL_TRACE").is_some();
        let mut t_pass = std::time::Instant::now();
        let mark = |name: &str, t: &mut std::time::Instant| {
            if dbg_prologue {
                eprintln!("[SIGMA_MODEL_TRACE] prologue {name}: {:?}", t.elapsed());
            }
            *t = std::time::Instant::now();
        };
        self.discharge_horn_joins();
        mark("horn_joins", &mut t_pass);
        self.discharge_event_calculus();
        mark("event_calculus", &mut t_pass);
        self.discharge_models();
        mark("models", &mut t_pass);
        self.discharge_model_joins();
        mark("model_joins", &mut t_pass);
        self.discharge_backward();
        mark("backward", &mut t_pass);
        // Deferred-passive discipline: recipes may be created only from
        // here on — the discharge prologue above (and every load-time /
        // forward-closure path, which runs before `run()`) drives
        // `resolve`/`superpose` too and needs materialized results.
        self.defer_active = self.opts.strategy.deferred_passive;
        let mut steps = 0usize;
        while steps < self.opts.max_steps {
            // In interactive single-step mode the wall clock is meaningless
            // (the user is paused at a prompt), so ignore the time limit —
            // only explicit cancellation (`q`) stops the run.  Polls the
            // ANCHORED `run_deadline` (construction / load start), not
            // `t0`: measuring from `run()` start would grant a slow load
            // a second full budget on every `continue` path that skips
            // the stage polls (redundant given, suppressed empty).
            if self.out_of_time() {
                return (RunVerdict::TimedOut, steps);
            }
            // FD congruence may have queued derived equalities (from
            // make's unit feedback / forward closure) — surface them
            // before selecting the next given.
            self.drain_fd_equalities();
            let prof = self.opts.profile;
            let t_select = prof.then(Instant::now);
            let popped = self.pop_given();
            if let Some(t) = t_select { self.stats.t_select += t.elapsed(); }
            let Some(mut given) = popped else {
                // `pop_given` also returns None on a mid-materialization
                // deadline bail — that must grade as TimedOut, never as
                // the Saturated certificate.
                if self.out_of_time() {
                    return (RunVerdict::TimedOut, steps);
                }
                return (RunVerdict::Saturated, steps);
            };
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
            if stepdbg::enabled() || self.opts.step {
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

            // A mega-clause iteration can spend the whole remaining budget
            // inside one generation stage; the loop-top check alone would
            // let the rest of this iteration (resolution, activation) run
            // long past the deadline.  Re-check at the stage boundary so
            // the budget stays a hard ceiling.
            if self.out_of_time() {
                return (RunVerdict::TimedOut, steps);
            }

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
                // in its own clause too.  Literals ≥ 64 have no mask bit
                // and are always eligible (the same convention as every
                // other `max_mask` consumer) — without the guard the
                // shift overflows on 65+-literal partners.
                let cands: Vec<EntryRef> = if ordered {
                    cands.into_iter()
                        .filter(|at| at.lit as usize >= 64
                            || (self.clauses[at.clause as usize].max_mask >> at.lit) & 1 == 1)
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
            // Activation indexes every subterm position of the given
            // clause (superposition targets) — seconds of work for a
            // mega-term clause, and pointless once the budget is spent:
            // the arena is abandoned on TimedOut.
            if self.out_of_time() {
                return (RunVerdict::TimedOut, steps);
            }
            let t_activate = prof.then(Instant::now);
            self.activate(given);
            if let Some(t) = t_activate { self.stats.t_activate += t.elapsed(); }
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
                Some(Term::Sym(s)) if s.as_str().starts_with("sk_")) as u64;
            own + elems.iter().map(term_skolem_apps).sum::<u64>()
        }
        Term::Sym(s) if s.as_str().starts_with("sk_") => 1,
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

parked! {
    /// Whether the term is a numeric literal (preferred as equality-class
    /// root so normalization keeps numbers literal — arithmetic
    /// comparisons stay decidable after rewriting).
    fn is_num_lit(t: &Term) -> bool {
        matches!(t, Term::Lit(Literal::Number(_)))
    }
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

/// [`positions`] without the subterm clones, for callers that only need
/// the paths (`superpose` re-derives the subterm itself): one shared
/// path buffer, snapshotted per emitted position — no tree clone per
/// node of every maximal literal of every given clause.
fn positions_paths(atom: &Term) -> Vec<Vec<usize>> {
    fn walk(t: &Term, path: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if let Term::App(elems) = t {
            for (i, e) in elems.iter().enumerate().skip(1) {
                path.push(i);
                walk(e, path, out);
                path.pop();
            }
        }
        if !path.is_empty() && !matches!(t, Term::Var(_)) {
            out.push(path.clone());
        }
    }
    let mut out = Vec::new();
    walk(atom, &mut Vec::new(), &mut out);
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
            if keq_lit_compatible(cl, cinfo, dl, dinfo) {
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

/// One (C-literal, D-literal) Key-Equation compatibility test — the
/// per-pair NECESSARY condition for `cσ = d` that [`keq_unpartnered`]
/// counts partners with (see its docs for the full soundness
/// argument).  The seat-shape conjunct is the phase-0
/// matching-direction strengthening: `cσ = d` also forces every rigid
/// seat CLASS of `c` onto `d` (a masked-but-concrete-headed seat keeps
/// its head and length under σ; a bare-variable `d` seat can never be
/// a rigid `c` seat's image) — see `AtomInfo::seats_match_onto`.
/// Shared verbatim by the equality-join channel's partner enumeration
/// (`ej::filter`) so the two chains can never drift.
#[inline]
fn keq_lit_compatible(cl: &PLit, cinfo: &AtomInfo, dl: &PLit, dinfo: &AtomInfo) -> bool {
    dl.pos == cl.pos
        && dinfo.arity == cinfo.arity
        && dinfo.residue_under(cinfo.mask) == cinfo.base_residue
        && cinfo.seats_match_onto(dinfo)
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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
/// Partition a clause's literals into maximal variable-disjoint groups
/// (ground literals are singleton groups): the decomposition both the
/// width diagnostic and the naming-split rescue use.  Merge-to-fixpoint
/// so a bridging literal fuses every group it touches.  Returns groups
/// of literal INDICES, in first-touch order (deterministic).
pub(super) fn var_disjoint_components(terms: &[(bool, Term)]) -> Vec<Vec<usize>> {
    let mut groups: Vec<(std::collections::BTreeSet<u64>, Vec<usize>)> = Vec::new();
    for (li, (_, t)) in terms.iter().enumerate() {
        let mut slots = std::collections::BTreeSet::new();
        super::unify::term_slots(t, &mut slots);
        let mut merged_lits = vec![li];
        let mut merged_slots = slots;
        loop {
            let mut fused = false;
            let mut i = 0;
            while i < groups.len() {
                let overlap = !groups[i].0.is_disjoint(&merged_slots)
                    && !(groups[i].0.is_empty() || merged_slots.is_empty());
                if overlap {
                    let (gs, gl) = groups.swap_remove(i);
                    merged_slots.extend(gs);
                    merged_lits.extend(gl);
                    fused = true;
                } else {
                    i += 1;
                }
            }
            if !fused {
                break;
            }
        }
        groups.push((merged_slots, merged_lits));
    }
    groups
        .into_iter()
        .map(|(_, mut lits)| {
            lits.sort_unstable();
            lits
        })
        .collect()
}

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

#[cfg(test)]
mod deferred_tests {
    use super::*;
    use crate::semantics::caches::test_support::kif_layer;
    use crate::types::Symbol;

    fn s(n: &str) -> Term { Term::Sym(Symbol::from(n)) }
    fn app(v: Vec<Term>) -> Term { Term::App(v) }
    fn v(n: u64) -> Term { Term::Var(n) }
    fn eq(l: Term, r: Term) -> Term { app(vec![Term::Op(OpKind::Equal), l, r]) }

    fn prover(layer: &ProverLayer, deferred: bool, superposition: bool) -> NativeProver<'_> {
        let mut strategy = Strategy::base();
        strategy.deferred_passive = deferred;
        strategy.superposition = superposition;
        let opts = NativeOpts { strategy, ..Default::default() };
        let mut p = NativeProver::new(layer, Scope::Base, opts);
        // Tests drive `resolve`/`superpose` directly (no `run()` loop),
        // so arm the in-loop gate by hand.
        p.defer_active = deferred;
        p
    }

    fn add(p: &mut NativeProver<'_>, lits: Vec<(bool, Term)>) -> u32 {
        p.make(lits, vec![], "hypothesis", SUPPORT, None, false)
            .expect("input clause made")
    }

    /// Index of the literal whose atom is an `App` headed by `head`
    /// with the given polarity (canonicalization may reorder literals).
    fn lit_idx(p: &NativeProver<'_>, id: u32, pos: bool, head: &str) -> usize {
        p.clauses[id as usize]
            .terms
            .iter()
            .position(|(lp, t)| {
                *lp == pos
                    && matches!(t, Term::App(elems)
                        if matches!(&elems[0], Term::Sym(sy) if &*sy.name() == head))
            })
            .expect("literal present")
    }

    /// The full roundtrip contract: a deferred recipe, once selected,
    /// materializes into EXACTLY the clause the eager path builds —
    /// same canonical literals, terms, key, nvars, and queue weight —
    /// and the composed (recipe) weight matches the exact weight when
    /// `make` does not simplify the conclusion.
    #[test]
    fn resolve_recipe_materializes_eager_identical_clause() {
        // Fixture pairs: (given lits, partner lits, gi head, pi head).
        // Covers the general-unification path (2-lit partner blocks the
        // decode fast path), a skolem-bearing conclusion (the skolem
        // weight factor), and a ground-unit partner (the decoded path).
        let fixtures: Vec<(Vec<(bool, Term)>, Vec<(bool, Term)>)> = vec![
            (
                vec![(false, app(vec![s("p"), v(0), s("b")])), (true, app(vec![s("q"), v(0)]))],
                vec![(true, app(vec![s("p"), s("a"), s("b")])), (true, app(vec![s("r"), s("c")]))],
            ),
            (
                vec![
                    (false, app(vec![s("p"), v(0), s("b")])),
                    (true, app(vec![s("q"), app(vec![s("sk_w"), v(0)])])),
                ],
                vec![(true, app(vec![s("p"), s("a"), s("b")])), (true, app(vec![s("r"), s("c")]))],
            ),
            (
                vec![(false, app(vec![s("p"), v(0), s("b")])), (true, app(vec![s("q"), v(0)]))],
                vec![(true, app(vec![s("p"), s("a"), s("b")]))],
            ),
        ];
        for (g_lits, p_lits) in fixtures {
            let layer = ProverLayer::new(kif_layer(""));

            let mut eager = prover(&layer, false, false);
            let ge = add(&mut eager, g_lits.clone());
            let pe = add(&mut eager, p_lits.clone());
            let gi = lit_idx(&eager, ge, false, "p");
            let pi = lit_idx(&eager, pe, true, "p");
            let eid = eager.resolve(ge, gi, pe, pi).expect("eager resolvent");

            let mut defer = prover(&layer, true, false);
            let gd = add(&mut defer, g_lits.clone());
            let pd = add(&mut defer, p_lits.clone());
            let gi_d = lit_idx(&defer, gd, false, "p");
            let pi_d = lit_idx(&defer, pd, true, "p");
            assert_eq!(
                defer.resolve(gd, gi_d, pd, pi_d), None,
                "knob on: resolve defers instead of materializing",
            );
            assert_eq!(defer.stats.recipes_queued, 1);
            let composed = defer.recipes[0].as_ref().expect("recipe queued").weight;
            let mid = defer.pop_given().expect("recipe materializes on selection");
            assert_eq!(defer.stats.recipes_materialized, 1);

            let e = &eager.clauses[eid as usize];
            let m = &defer.clauses[mid as usize];
            assert_eq!(e.lits, m.lits, "canonical literals diverged");
            assert_eq!(e.terms, m.terms, "slot terms diverged");
            assert_eq!(e.key, m.key, "clause key diverged");
            assert_eq!(e.nvars, m.nvars, "nvars diverged");
            assert_eq!(e.weight, m.weight, "queue weight diverged");
            assert_eq!(e.rule, m.rule, "rule tag diverged");
            assert_eq!(
                composed, e.weight,
                "composed weight must be exact when make does not simplify",
            );
            assert_eq!(defer.stats.composed_weight_exact, 1);
            assert_eq!(defer.stats.composed_weight_drift_sum, 0);
        }
    }

    #[test]
    fn superpose_recipe_materializes_eager_identical_clause() {
        let layer = ProverLayer::new(kif_layer(""));
        let e_lits = vec![(true, eq(app(vec![s("f"), v(0)]), v(0)))];
        let t_lits = vec![(true, app(vec![s("h"), app(vec![s("f"), s("a")])]))];

        let mut eager = prover(&layer, false, true);
        let ee = add(&mut eager, e_lits.clone());
        let te = add(&mut eager, t_lits.clone());
        let eid = eager.superpose(ee, 0, te, 0, &[1]).expect("eager superposition");

        let mut defer = prover(&layer, true, true);
        let ed = add(&mut defer, e_lits);
        let td = add(&mut defer, t_lits);
        assert_eq!(defer.superpose(ed, 0, td, 0, &[1]), None, "knob on: superpose defers");
        assert_eq!(defer.stats.recipes_queued, 1);
        let composed = defer.recipes[0].as_ref().expect("recipe queued").weight;
        let mid = defer.pop_given().expect("recipe materializes on selection");

        let e = &eager.clauses[eid as usize];
        let m = &defer.clauses[mid as usize];
        assert_eq!(e.lits, m.lits, "canonical literals diverged");
        assert_eq!(e.terms, m.terms, "slot terms diverged");
        assert_eq!(e.key, m.key, "clause key diverged");
        assert_eq!(e.weight, m.weight, "queue weight diverged");
        assert_eq!(composed, e.weight, "composed weight exact on unsimplified conclusion");
        assert_eq!(defer.stats.composed_weight_exact, 1);
    }

    /// Composed facts are computed on the RAW conclusion: a duplicate-
    /// literal merge (the eager path's pre-`make` pass) makes the
    /// composed weight a strict over-estimate — the drift is measured,
    /// and the materialized clause still matches the eager one exactly.
    #[test]
    fn composed_weight_overestimates_on_literal_merge() {
        let layer = ProverLayer::new(kif_layer(""));
        let g_lits = vec![(false, app(vec![s("p"), v(0)])), (true, app(vec![s("q"), s("b")]))];
        let p_lits = vec![(true, app(vec![s("p"), s("b")])), (true, app(vec![s("q"), s("b")]))];

        let mut eager = prover(&layer, false, false);
        let ge = add(&mut eager, g_lits.clone());
        let pe = add(&mut eager, p_lits.clone());
        let eid = eager
            .resolve(ge, lit_idx(&eager, ge, false, "p"), pe, lit_idx(&eager, pe, true, "p"))
            .expect("eager resolvent");
        assert_eq!(eager.clauses[eid as usize].lits.len(), 1, "merged to a unit");

        let mut defer = prover(&layer, true, false);
        let gd = add(&mut defer, g_lits);
        let pd = add(&mut defer, p_lits);
        assert_eq!(
            defer.resolve(gd, lit_idx(&defer, gd, false, "p"), pd, lit_idx(&defer, pd, true, "p")),
            None,
        );
        let composed = defer.recipes[0].as_ref().expect("recipe").weight;
        let mid = defer.pop_given().expect("materialized");
        let m = &defer.clauses[mid as usize];
        assert_eq!(eager.clauses[eid as usize].lits, m.lits);
        assert_eq!(eager.clauses[eid as usize].key, m.key);
        assert!(
            composed > m.weight,
            "raw-conclusion composed weight ({composed}) over-estimates the merged \
             clause's exact weight ({})",
            m.weight,
        );
        assert_eq!(defer.stats.composed_weight_samples, 1);
        assert_eq!(defer.stats.composed_weight_exact, 0);
        assert_eq!(defer.stats.composed_weight_drift_sum, composed - m.weight);
    }

    /// Activation rejects: an exact duplicate is dropped at
    /// materialization (counted), and the queue pops the next entry.
    #[test]
    fn duplicate_recipe_rejected_at_materialization() {
        let layer = ProverLayer::new(kif_layer(""));
        let mut p = prover(&layer, true, false);
        let g = add(&mut p, vec![
            (false, app(vec![s("p"), v(0), s("b")])),
            (true, app(vec![s("q"), v(0)])),
        ]);
        let pt = add(&mut p, vec![
            (true, app(vec![s("p"), s("a"), s("b")])),
            (true, app(vec![s("r"), s("c")])),
        ]);
        // Pre-accept the exact resolvent through the eager queue path,
        // so `seen` holds its key (what `push` records at generation).
        let dup = p
            .make(
                vec![(true, app(vec![s("q"), s("a")])), (true, app(vec![s("r"), s("c")]))],
                vec![], "hypothesis", SUPPORT, None, false,
            )
            .expect("made");
        p.push(Some(dup));
        let gi = lit_idx(&p, g, false, "p");
        let pi = lit_idx(&p, pt, true, "p");
        assert_eq!(p.resolve(g, gi, pt, pi), None, "deferred");
        // First pop: the pre-accepted clause (earlier seq wins the tie).
        assert_eq!(p.pop_given(), Some(dup));
        // Second pop: the recipe materializes into an exact duplicate —
        // rejected, counted, queue exhausted.
        assert_eq!(p.pop_given(), None);
        assert_eq!(p.stats.recipes_materialized, 1);
        assert_eq!(p.stats.act_dedup_hits, 1);
        assert_eq!(p.stats.composed_weight_samples, 0, "rejects are not weight-sampled");
    }

    /// `make`-level rejects (tautology here) are counted as
    /// `act_rejected_other` and the pop loop moves on.
    #[test]
    fn tautology_recipe_rejected_by_make_at_materialization() {
        let layer = ProverLayer::new(kif_layer(""));
        let mut p = prover(&layer, true, false);
        let g = add(&mut p, vec![
            (false, app(vec![s("p"), s("a")])),
            (true, app(vec![s("q"), s("c")])),
        ]);
        let pt = add(&mut p, vec![
            (true, app(vec![s("p"), s("a")])),
            (false, app(vec![s("q"), s("c")])),
        ]);
        let gi = lit_idx(&p, g, false, "p");
        let pi = lit_idx(&p, pt, true, "p");
        assert_eq!(p.resolve(g, gi, pt, pi), None, "deferred");
        assert_eq!(p.stats.recipes_queued, 1);
        assert_eq!(p.pop_given(), None, "tautology rejected; queue exhausted");
        assert_eq!(p.stats.recipes_materialized, 1);
        assert_eq!(p.stats.act_rejected_other, 1);
        assert_eq!(p.stats.act_dedup_hits, 0);
    }

    /// The approximate pre-queue dedup drops an exact re-derivation
    /// (same rule, parents, aux) before it queues.
    #[test]
    fn prequeue_dedup_drops_rederivation() {
        let layer = ProverLayer::new(kif_layer(""));
        let mut p = prover(&layer, true, false);
        let g = add(&mut p, vec![
            (false, app(vec![s("p"), v(0), s("b")])),
            (true, app(vec![s("q"), v(0)])),
        ]);
        let pt = add(&mut p, vec![
            (true, app(vec![s("p"), s("a"), s("b")])),
            (true, app(vec![s("r"), s("c")])),
        ]);
        let gi = lit_idx(&p, g, false, "p");
        let pi = lit_idx(&p, pt, true, "p");
        assert_eq!(p.resolve(g, gi, pt, pi), None);
        assert_eq!(p.resolve(g, gi, pt, pi), None);
        assert_eq!(p.stats.recipes_queued, 1, "second derivation dropped pre-queue");
        assert_eq!(p.stats.recipes_prequeue_deduped, 1);
    }

    /// Unit×unit resolution (the raw empty clause) stays EAGER even
    /// with the knob on — refutation detection never sits deferred.
    #[test]
    fn unit_unit_refutation_stays_eager() {
        let layer = ProverLayer::new(kif_layer(""));
        let mut p = prover(&layer, true, false);
        let g = add(&mut p, vec![(false, app(vec![s("p"), s("a")]))]);
        let pt = add(&mut p, vec![(true, app(vec![s("p"), s("a")]))]);
        let empty = p.resolve(g, 0, pt, 0).expect("eager empty clause");
        assert!(p.clauses[empty as usize].lits.is_empty());
        assert_eq!(p.stats.recipes_queued, 0);
    }

    /// Like [`prover`] but with an explicit recipe budget
    /// (`Strategy::deferred_cap`).
    fn prover_capped(layer: &ProverLayer, cap: u32) -> NativeProver<'_> {
        let mut p = prover(layer, true, false);
        p.opts.strategy.deferred_cap = cap;
        p
    }

    /// The Part-A cap contract: with the recipe budget exhausted
    /// (`deferred_cap = 0`), every product falls back to the EAGER path
    /// — built + `make`d at generation time — and the clause set
    /// produced is IDENTICAL to the uncapped run's (which defers, then
    /// materializes on selection) and to the knob-off eager run's.
    /// Nothing is dropped; the fallbacks are counted.
    #[test]
    fn cap_forced_eager_fallback_produces_identical_clause_set_to_uncapped() {
        let g_lits = vec![
            (false, app(vec![s("p"), v(0), s("b")])),
            (true, app(vec![s("q"), v(0)])),
        ];
        let p1_lits = vec![
            (true, app(vec![s("p"), s("a"), s("b")])),
            (true, app(vec![s("r"), s("c")])),
        ];
        let p2_lits = vec![
            (true, app(vec![s("p"), s("d"), s("b")])),
            (true, app(vec![s("w"), s("d")])),
        ];

        // Drive the same two resolutions on each prover; return the
        // derived clause ids.
        let drive = |p: &mut NativeProver<'_>| -> Vec<Option<u32>> {
            let g = add(p, g_lits.clone());
            let p1 = add(p, p1_lits.clone());
            let p2 = add(p, p2_lits.clone());
            let gi = lit_idx(p, g, false, "p");
            let r1 = p.resolve(g, gi, p1, lit_idx(p, p1, true, "p"));
            let r2 = p.resolve(g, gi, p2, lit_idx(p, p2, true, "p"));
            vec![r1, r2]
        };

        let layer = ProverLayer::new(kif_layer(""));
        let mut eager = prover(&layer, false, false);
        let e_ids: Vec<u32> = drive(&mut eager).into_iter().map(|r| r.unwrap()).collect();

        let mut capped = prover_capped(&layer, 0);
        let c_ids: Vec<u32> = drive(&mut capped)
            .into_iter()
            .map(|r| r.expect("cap-forced fallback materializes eagerly"))
            .collect();
        assert_eq!(capped.stats.recipes_queued, 0, "no slot ⇒ nothing defers");
        assert_eq!(capped.stats.deferred_cap_fallbacks, 2);

        let mut uncapped = prover(&layer, true, false);
        assert_eq!(drive(&mut uncapped), vec![None, None], "uncapped defers");
        assert_eq!(uncapped.stats.recipes_queued, 2);
        assert_eq!(uncapped.stats.deferred_cap_fallbacks, 0);
        let u_ids: Vec<u32> = (0..2).map(|_| uncapped.pop_given().unwrap()).collect();

        // Same arena shape eager-vs-capped (same construction order)…
        assert_eq!(eager.clauses.len(), capped.clauses.len());
        for (e, c) in e_ids.iter().zip(&c_ids) {
            let (e, c) = (&eager.clauses[*e as usize], &capped.clauses[*c as usize]);
            assert_eq!(e.lits, c.lits, "capped fallback diverged from eager");
            assert_eq!(e.terms, c.terms);
            assert_eq!(e.key, c.key);
            assert_eq!(e.weight, c.weight);
        }
        // …and the SET of derived clauses matches the uncapped run
        // (selection order may permute materializations).
        let keys = |p: &NativeProver<'_>, ids: &[u32]| {
            let mut ks: Vec<_> = ids.iter().map(|i| p.clauses[*i as usize].key).collect();
            ks.sort_unstable_by_key(|k| format!("{k:?}"));
            ks
        };
        assert_eq!(keys(&eager, &e_ids), keys(&uncapped, &u_ids));
    }

    /// Live-recipe accounting: materialization frees its slot, so
    /// deferral RESUMES once the queue drains below the cap.
    #[test]
    fn cap_slot_frees_on_materialization_and_deferral_resumes() {
        let layer = ProverLayer::new(kif_layer(""));
        let mut p = prover_capped(&layer, 1);
        let g = add(&mut p, vec![
            (false, app(vec![s("p"), v(0), s("b")])),
            (true, app(vec![s("q"), v(0)])),
        ]);
        let partners: Vec<u32> = ["a", "d", "e"]
            .iter()
            .map(|c| {
                add(&mut p, vec![
                    (true, app(vec![s("p"), s(c), s("b")])),
                    (true, app(vec![s("r"), s(c)])),
                ])
            })
            .collect();
        let gi = lit_idx(&p, g, false, "p");

        // Slot free ⇒ defers.
        assert_eq!(p.resolve(g, gi, partners[0], lit_idx(&p, partners[0], true, "p")), None);
        assert_eq!(p.stats.recipes_queued, 1);
        // At the cap ⇒ eager fallback.
        assert!(p.resolve(g, gi, partners[1], lit_idx(&p, partners[1], true, "p")).is_some());
        assert_eq!(p.stats.deferred_cap_fallbacks, 1);
        assert_eq!(p.stats.recipes_queued, 1);
        // Materializing the queued recipe frees its slot…
        assert!(p.pop_given().is_some());
        assert_eq!(p.stats.recipes_materialized, 1);
        // …so the next product defers again.
        assert_eq!(p.resolve(g, gi, partners[2], lit_idx(&p, partners[2], true, "p")), None);
        assert_eq!(p.stats.recipes_queued, 2);
        assert_eq!(p.stats.deferred_cap_fallbacks, 1);
    }

    /// The default cap's arithmetic stands on the measured per-recipe
    /// footprint; the arena-slot term of that arithmetic is pinned here
    /// so a `Recipe` growing a field re-opens the sizing discussion.
    #[test]
    fn recipe_footprint_is_within_the_cap_arithmetic() {
        assert!(
            std::mem::size_of::<Option<Recipe>>() <= 240,
            "Option<Recipe> grew past the documented arena-slot budget: {} B \
             (the deferred_cap default in Strategy::base() was sized against \
             240 B slots + measured binding-heap spill — re-measure before \
             raising this)",
            std::mem::size_of::<Option<Recipe>>(),
        );
    }

    /// Knob off: the discipline is entirely absent — resolve builds and
    /// `make`s immediately, no recipe state is touched.
    #[test]
    fn knob_off_is_inert() {
        let layer = ProverLayer::new(kif_layer(""));
        let mut p = prover(&layer, false, false);
        p.defer_active = true; // even with the loop gate armed
        let g = add(&mut p, vec![
            (false, app(vec![s("p"), v(0), s("b")])),
            (true, app(vec![s("q"), v(0)])),
        ]);
        let pt = add(&mut p, vec![
            (true, app(vec![s("p"), s("a"), s("b")])),
            (true, app(vec![s("r"), s("c")])),
        ]);
        let gi = lit_idx(&p, g, false, "p");
        let pi = lit_idx(&p, pt, true, "p");
        assert!(p.resolve(g, gi, pt, pi).is_some(), "eager resolvent");
        assert!(p.recipes.is_empty());
        assert_eq!(p.stats.recipes_queued, 0);
        assert_eq!(p.stats.recipes_materialized, 0);
    }
}
