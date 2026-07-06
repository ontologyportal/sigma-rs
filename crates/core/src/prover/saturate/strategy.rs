// crates/core/src/saturate/strategy.rs
//
// Every search-shaping knob of the native prover in one serializable
// struct — the unit a portfolio mode enumerates.  Until now these
// tunables lived in three places that a parallel portfolio cannot use:
// hardcoded `const`s in prover.rs, process-global env vars (which every
// thread shares), and call-site magic numbers in the selection pipeline.
// A `Strategy` travels inside `NativeOpts`, so two provers running
// side by side can differ in ANY of these without touching the
// environment.
//
// Two construction paths, deliberately distinct:
//
// * [`Strategy::default`] — the shipping defaults WITH the historical
//   env-var overrides applied (`SIGMA_NO_SCHEMA`, `SIGMA_GOALDIST`,
//   `SIGMA_NO_LIU`, `SIGMA_HEADFILTER`, `SIGMA_NO_BG_SNAPSHOT`,
//   `SIGMA_NO_DECODE`, `SIGMA_DEMOD`).  Every existing caller goes through
//   this, so the A/B kill switches keep working exactly as before.
// * [`Strategy::base`] — the pure shipping defaults, no env reads.
//   Portfolio members derive from this so a stray env var can't skew
//   one lane of a benchmark.

use serde::{Deserialize, Serialize};

/// One complete configuration of the native prover's search behavior.
///
/// `Serialize`/`Deserialize` so portfolio specs can live in JSON (CLI
/// flags, config files, sweep harnesses); `#[serde(default)]` makes
/// every field optional — a spec names only what it changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Strategy {
    /// Label for reporting (portfolio lanes, stats lines).  Not
    /// compared by the snapshot fingerprint.
    pub name: String,

    // -- given-clause queue -------------------------------------------------

    /// Per-tier weight multiplier, indexed by tier
    /// (CONJECTURE=0 / SUPPORT=1 / BACKGROUND=2).  Lower = picked
    /// sooner; the default mildly prefers conjecture descendants.
    pub tier_weight: [u64; 3],
    /// Every `pick_ratio`-th given clause is chosen by AGE instead of
    /// weight (the fairness valve; 0 is treated as 1 = always-age).
    pub pick_ratio: u64,
    /// Liu & Xu-style conjecture-distance queue factor: clauses sharing
    /// no ground leaves with the conjecture weigh up to
    /// `1 + goal_dist_w` times their base.  OFF by default — measured
    /// no benefit once selection was repaired — but a classic
    /// portfolio axis.  Enabling it also disables background-snapshot
    /// reuse (the factor bakes conjecture-dependent weights into
    /// background clauses).
    pub goal_dist: bool,
    /// Max extra weight factor for goal-far clauses (only read when
    /// `goal_dist` is on).
    pub goal_dist_w: u64,

    // -- clause-selection weight FUNCTION (the E-style "genome") -------------
    // base = (cw_lits·#lits + cw_size·term-size + cw_vars·#vars)
    //        · (1 + cw_skolem·#skolem-applications),  floored at 1.
    // The defaults (1,1,2,1) reproduce the historical hardcoded formula.
    /// Per-literal coefficient in the clause weight.
    pub cw_lits: u64,
    /// Per-term-size (leaf count) coefficient in the clause weight.
    pub cw_size: u64,
    /// Per-variable coefficient in the clause weight.
    pub cw_vars: u64,
    /// Skolem-application penalty multiplier (throttles generative
    /// existentials: weight ×(1 + cw_skolem·skolems)).
    pub cw_skolem: u64,
    /// Literal-selection rule for resolution (which literal of the given
    /// clause to resolve on): 0 = fewest index candidates (the historical
    /// default — most goal-directed), 1 = most candidates, 2 = first
    /// eligible (cheapest).  A flat int for GA mutation.
    pub lit_select: u8,

    // -- derived-clause caps ------------------------------------------------

    /// Term-depth cap for derived clauses (deeper → discarded_deep).
    pub max_depth: u8,
    /// Term-width cap (leaf count) for derived clauses.  Depth alone
    /// does not bound SUMO's recursive list machinery.
    pub max_term_size: usize,
    /// Override for the derived-clause LITERAL-count cap
    /// (`NativeOpts::max_lits`, which lives outside `Strategy` because it
    /// is also the historical KIF/SUMO-path default rather than a pure
    /// search-shaping knob). `None` (the default, every existing lane)
    /// leaves `NativeOpts::max_lits` exactly as the caller set it —
    /// byte-identical to today. `Some(n)` is consumed ONLY at
    /// portfolio-lane-build time (`run_portfolio_schedule`), which
    /// overwrites the lane's `NativeOpts.max_lits` with `n` — so a lane
    /// can widen the literal-count ceiling on DERIVED resolvents without
    /// touching `push_input`'s separate (already-generous,
    /// `full_saturation`-only) input-width backstop. Measured motivation
    /// (task: TPTP `tptp-wide` lane): under the TPTP regime derived
    /// clauses over 8 literals are dropped into `discarded_long` even
    /// though whole clauses now load at input time (the definitional-CNF
    /// rescue + input-width-backstop work) — some ALG-family problems
    /// (ALG027+1, ALG106+1) run genuine searches that lose wide
    /// resolvents this way and finish GaveUp/Timeout rather than
    /// StepsExhausted-honest.
    pub derived_width_cap: Option<u8>,
    /// Paramodulants generated per given clause.
    pub para_cap: usize,
    /// Max demodulation rewrites applied to a single term in `make`
    /// (a fan-out guard; KBO already guarantees termination).
    pub demod_cap: u64,
    /// Max candidate-clause checks per backward-demodulation pass (one
    /// pass per newly activated oriented unit equation).  Hitting the
    /// cap leaves the remaining candidates unsimplified — sound:
    /// interreduction is optional redundancy elimination.
    pub bwd_demod_cap: usize,
    /// KBO symbol-PRECEDENCE seed — permutes the total order on symbols
    /// (orientation of every equation + literal maximality, the highest-
    /// impact ordering lever).  `0` = the historical id-order (shares the
    /// layer KBO, byte-identical).  Non-zero builds a per-prover KBO whose
    /// precedence ranks symbols by `hash(id, seed)` — a different but still
    /// admissible total order, i.e. a different search shape (cf. Vampire's
    /// random `--symbol_precedence`).  A flat int for GA mutation.
    pub prec_seed: u64,

    // -- forward closure (bounded hyperresolution) ----------------------------

    /// Premise clauses longer than this never join.
    pub fc_max_premise_lits: usize,
    /// New units derived per (unit, premise) pair.
    pub fc_fanout: usize,
    /// Conclusions deeper than this are not kept (flat ground units
    /// only — the anti-flooding contract).
    pub fc_flat_depth: u8,
    /// Closure rounds (each round's conclusions seed the next).
    pub fc_rounds: usize,
    /// Total conclusions across the whole closure.
    pub fc_cap: usize,
    /// Branching per joined negative literal.
    pub fc_branch: usize,
    /// Max positive-head literals a forward-closure premise may have.
    /// `1` (default) keeps only Horn single-head rules → flat ground unit
    /// conclusions.  `>1` lets a multi-conclusion rule
    /// (`¬p ∨ q ∨ r`) be pre-resolved against known facts into the SHORT
    /// ground DISJUNCTION of its heads (`q ∨ r`), committed as a support
    /// clause.  This "reduce the problem before the question" step bypasses
    /// the ordered-resolution restriction on multi-head rules (TQG16-class)
    /// — the heavy 3-literal background axiom is consumed at closure time,
    /// so the restricted backward search only meets the easy residue.
    /// Bounded by `fc_flat_depth` (ground depth) and this cap (width).
    pub fc_max_pos: usize,

    // -- inference channels ---------------------------------------------------

    /// The schema channel: algebraic pattern mining (symmetry /
    /// transitivity / metaschemas / Leibniz equality), ground
    /// symmetric orientation, rule absorption, symmetric dual
    /// retrieval.  `SIGMA_NO_SCHEMA` turns it off globally.
    pub schema: bool,
    /// The algebraic decode fast path in `resolve` (power-sum binding
    /// extraction instead of general unification where eligible).
    /// `SIGMA_NO_DECODE` turns it off globally.
    pub decode: bool,
    /// Forward demodulation: rewrite every new clause to KBO normal form
    /// with the active oriented unit equations (`l → r` where
    /// `l >_kbo r`).  A simplification — the rewritten clause replaces
    /// the original — so it shrinks the search space rather than growing
    /// it.  Consumes the reduction ordering ([`super::kbo`]).
    pub demod: bool,
    /// Backward demodulation (interreduction): when a NEW oriented unit
    /// equation `l → r` activates, rewrite the EXISTING clauses that
    /// contain an `l`-redex — the replacement goes back through `make`
    /// (rule tag `bwd_demod`) and the original is RETIRED (skipped on
    /// pop, filtered from partner retrieval), so the clause sets stay
    /// interreduced instead of drowning in unreduced copies.  Candidates
    /// come from the subterm-occurrence postings (exact ground keys +
    /// (head, len) buckets, maintained per made clause while this is
    /// on); the pass is bounded by [`Self::bwd_demod_cap`].  Default ON
    /// under the TPTP regime ([`Self::tptp`]); `SIGMA_NO_BWD_DEMOD=1`
    /// forces it off and `SIGMA_BWD_DEMOD=1` forces it on (the off
    /// switch wins — same A/B convention as `SIGMA_DEMOD`, extended
    /// with an off direction now that a default is true).  Pair with
    /// `demod` — on its own the new clauses are not kept in normal form.
    pub bwd_demod: bool,
    /// Ordered resolution: restrict binary resolution to KBO-maximal
    /// literals.  The ordered refinement (still complete) and the
    /// prerequisite for ordered superposition.  OFF by default — it
    /// reshapes the search and is only sound-and-useful paired with the
    /// superposition calculus; it also gates the per-clause maximality
    /// computation, so off ⇒ zero cost.
    pub ordered_resolution: bool,
    /// Full (multi-literal) clause subsumption: drop a new clause when an
    /// active clause subsumes it (forward).  The general redundancy
    /// eliminator that complements unit subsumption — the flooding floor
    /// the superposition calculus stands on.
    pub subsumption: bool,
    /// Cross-literal equality-join subsumption prefilter (phase 2b of
    /// the subterm-index milestone; see `prover/ej.rs`): after the keq
    /// counting filter passes a candidate subsumer, decode its planned
    /// literals against their keq-feasible partner literals via the
    /// phase-2a channel rows, rejecting candidates with a
    /// zero-survivor literal or an empty per-variable decoded-key
    /// intersection across literals sharing a variable.  A pure
    /// necessary-condition prefilter in front of `clause_subsumes_in`
    /// — derivations are identical with it on or off; only the
    /// full-check count and wall time move.  Only meaningful with
    /// [`Self::subsumption`] on.  `SIGMA_NO_SUBS_JOIN=1` forces it
    /// off, `SIGMA_SUBS_JOIN=1` forces it on (off wins) — the same
    /// two-directional A/B convention as `SIGMA_BWD_DEMOD`.
    pub subs_join: bool,
    /// The phase-2a k-channel Vandermonde ROWS (see `prover/rows.rs`):
    /// compute and STORE a 4-word GF(2^64) presence/decode row for every
    /// walked subterm at clause-accept time (the content-keyed row table
    /// + the per-bucket lockstep row column in `SubtermPostings`), and
    /// run the decode-chain prefilter it backs in front of the open-lhs
    /// backward-demodulation verify.  OFF by default: measured negative
    /// on BOTH consumers — the backward-demod decode chain (2a) charged a
    /// ~2% per-accept row-registration tax for a <1ms filter payoff, and
    /// the subsumption equality-join (2b, [`Self::subs_join`]) ended up
    /// using TRANSIENT rows only (it never reads the stored table).  With
    /// this off the postings themselves (exact ground keys + (head, len)
    /// head buckets — phase 1) are UNAFFECTED and backward demodulation
    /// runs identically via the seat prefilter + `match_one_way_off`;
    /// only the row computation/storage and the decode chain are skipped,
    /// so a default build pays zero row tax.  `SIGMA_SUBTERM_ROWS=1`
    /// forces it ON, `SIGMA_NO_SUBTERM_ROWS=1` forces it OFF (off wins) —
    /// the same two-directional A/B convention as `SIGMA_BWD_DEMOD` /
    /// [`Self::subs_join`].  Derivation-neutral: the decode chain is a
    /// NECESSARY-condition prefilter in front of the unchanged structural
    /// verify, so the backward-demod derivations are identical either way.
    pub subterm_rows: bool,
    /// Ordered superposition: the complete equality calculus (replaces the
    /// SOS-gated unit-paramodulation stand-in).  Implies the maximality
    /// machinery and the subterm-index population.  OFF by default until
    /// the equational corpus + Vampire benchmark validate it.
    pub superposition: bool,
    /// Equality factoring: from `s≈t ∨ s'≈t' ∨ C` with `mgu(s,s')=σ`
    /// derive `(s≈t' ∨ t≉t' ∨ C)σ`.  The completeness corner of the
    /// superposition calculus — required for refutational completeness
    /// with positive equality literals.  Pair with `superposition`; on
    /// its own it only adds (sound) inferences without the rewrite engine.
    pub eq_factoring: bool,
    /// Background completion (Phase 6): before the main loop, run bounded
    /// Knuth–Bendix completion among the active unit equations — superpose
    /// them against each other, orient + demodulate the results, and add
    /// the new oriented equations as demodulators.  So proof-time
    /// equational rewriting is cheap one-way demodulation rather than
    /// repeated live superposition.  Sound (every derived equation is an
    /// equational consequence); default OFF.
    pub bg_completion: bool,
    /// Hard cap on equations produced by `bg_completion` (the freeze-cost
    /// terminator — completion can diverge, so the budget is the bound).
    pub bg_completion_budget: usize,
    /// Shape-recognized taxonomy roles: re-derive the `instance` /
    /// `subclass` / `subrelation` operators and the `TransitiveRelation`
    /// / `SymmetricRelation` meta-classes from their DEFINING axioms
    /// instead of hard-coded English names (`saturate::roles`).  Lets
    /// the oracle engage on renamed / non-English dialects.  Default
    /// OFF — and a provable no-op on SUMO (the names resolve to the
    /// same ids the recognizer recovers).
    pub recognize_roles: bool,
    /// Modal K-distribution schemata over quote constructors (native
    /// HO-parity, part A): at problem assembly, inject conjunction
    /// distribution for the attitude relations `knows`/`believes` —
    /// `(=> (rel ?A (and_q ?P ?Q)) (and (rel ?A ?P) (rel ?A ?Q)))` —
    /// when the KB's taxonomy declares the relation's argument-2 domain
    /// as `Formula` (or a descendant) AND the relation occurs in the
    /// problem (`prove.rs`'s `modal_k_qualifying`).  Parity with the THF
    /// lane's K-axiom fragment; the schemata only REARRANGE quoted
    /// structure, never unquote.  `SIGMA_NO_MODAL_K=1` turns it off
    /// globally.
    pub modal_k: bool,

    // -- saturation regime ----------------------------------------------------

    /// Full-saturation regime: background (KB axiom) clauses enter the
    /// passive queue as given-clause candidates too, so axiom×axiom
    /// inference happens.  OFF by default = classic set-of-support
    /// (background is indexed but never given) — the right search shape
    /// against a huge satisfiable KB (SUMO), but structurally unable to
    /// prove problems whose refutation needs inference among the axioms
    /// themselves (PUZ001+1's killer-identity case analysis: the negated
    /// conjecture unifies with nothing until the axioms have reasoned
    /// among themselves).  Standalone TPTP problems run with this ON.
    /// Incompatible with background-snapshot reuse (the frozen base
    /// excludes queue state), which is force-disabled while this is on.
    pub full_saturation: bool,
    /// Honest saturation verdicts: report a `Saturated` run as
    /// "disproved" ONLY when the run was genuinely refutation-complete —
    /// full saturation (no set-of-support tiering) over the WHOLE
    /// theory, no clause lost to capacity caps, no generation cap hit,
    /// and a complete equality calculus (superposition + eq_factoring,
    /// every indexed equation orientable) whenever the problem contains
    /// equality literals.  Anything less surfaces as Unknown ("no proof
    /// found"), never as a confident "no".  OFF by default: the
    /// KIF/SUMO path keeps its historical "saturation under SOS ⇒
    /// strong no" reading (its expected-no tests depend on it).
    pub strict_saturation: bool,

    // -- selection pipeline (read by `ask_native`, not the loop) -------------

    /// Liu & Xu structural rescue: after SInE, pull goal-near axioms
    /// by IDF-weighted shared content that the trigger relation
    /// missed.  `SIGMA_NO_LIU` turns it off (with `def_completion`).
    pub liu_rescue: bool,
    /// Structural-rescue iteration rounds (1 = no drift, by design).
    pub liu_rounds: usize,
    /// Top-k axioms admitted per structural-rescue round.
    pub liu_top_k: usize,
    /// Polarity-aware definitional completion: pull providers for
    /// goal-line proof obligations nothing selected concludes.
    pub def_completion: bool,
    /// Completion rounds (chains of definitions need several).
    pub defcomp_rounds: usize,
    /// Total axioms completion may add.
    pub defcomp_max_adds: usize,
    /// Providers pulled per missing predicate per round.
    pub defcomp_per_sym: usize,
    /// Drop bookkeeping sentences (documentation / termFormat /
    /// domain…) from the selection, as the subprocess path always
    /// has.  OFF by default: measured, it quadrupled TQG52.
    /// `SIGMA_HEADFILTER=1` turns it on globally.
    pub head_filter: bool,
    /// Frozen-background snapshot reuse across runs over the same
    /// problem base.  `SIGMA_NO_BG_SNAPSHOT` turns it off globally.
    /// Forced off while `goal_dist` is on (see there).
    pub bg_snapshot: bool,

    // -- semantic guidance (E/Vampire-style) ---------------------------------

    /// Semantic clause selection: score each passive clause by the
    /// fraction of its ground, model-checkable literals that are FALSE
    /// in the KB's positive model (a clause whose literals are false in
    /// a model of the background theory is closer to a conflict — the
    /// classic saturation-prover "avoid true clauses" guidance).  Used
    /// ONLY as a secondary tie-break within the existing weight/age
    /// queue discipline — it can reorder the search but never widen or
    /// narrow it, so it cannot make the prover unsound.  OFF by default;
    /// `SIGMA_GUIDE=1` opts in globally (see [`Self::from_env`]).  The
    /// model is built once per run, at [`super::prover::NativeProver::run`]
    /// start, from [`crate::saturate::caches::model_registry`]'s
    /// KB-lifetime [`crate::saturate::model::ModelProgram`]; a budget bail
    /// (`positive_model` returns `None`) disables guidance for the run
    /// (counted in `ProverStats::guide_disabled_bail`), never treated as
    /// a hard error.
    pub semantic_guide: bool,
    /// Deferred-passive discipline (lazy clause materialization): binary
    /// resolution and superposition products are queued as compact
    /// RECIPES (parent ids + unifier fragment + composed queue facts)
    /// instead of being built + run through `make` at generation time;
    /// construction and the full `make` pipeline run only when a recipe
    /// is SELECTED from the passive queue.  Attacks the measured
    /// manufacture-to-activation waste (GRP618+1 @60s: 251,929
    /// resolvents built, 1,932 activated — `make` was 61% of CPU).
    /// A STRUCTURAL strategy: the search it produces is deliberately
    /// different (approximate queue ordering from composed facts;
    /// passive recipes are invisible to subsumption/postings until
    /// materialized; simplification happens at selection time, against
    /// a strictly FRESHER demodulator/unit set than eager generation
    /// saw).  Default OFF everywhere.  `SIGMA_DEFERRED_PASSIVE=1`
    /// forces it on, `SIGMA_NO_DEFERRED_PASSIVE=1` forces it off (off
    /// wins) — the same two-directional A/B convention as
    /// `SIGMA_BWD_DEMOD`.  Deliberately NOT in `bg_fingerprint`:
    /// background loading is always eager (recipes exist only inside
    /// `run()`'s given-clause loop), so the frozen background is
    /// byte-identical either way.
    pub deferred_passive: bool,
    /// Recipe budget for the deferred-passive discipline: the maximum
    /// number of LIVE recipes (queued, not yet materialized) the
    /// passive queue may hold.  While the queue is at the cap, new
    /// resolution/superposition products fall back to the EAGER path
    /// (materialize + `make` at generation time, exactly as knob-off)
    /// — the discipline degrades gracefully; no inference is ever
    /// dropped, so completeness and soundness are untouched (fallbacks
    /// counted in `ProverStats::deferred_cap_fallbacks`).  Slots free
    /// as recipes materialize, so deferral resumes once the queue
    /// drains below the cap.  Only read when [`Self::deferred_passive`]
    /// is on.  Default sized from the MEASURED live-recipe memory
    /// footprint on the heaviest known generator (RNG044+1 @60s, the
    /// phase-1 ~20x-RSS case): see `base()`'s note for the arithmetic.
    pub deferred_cap: u32,
}

impl Strategy {
    /// The pure shipping defaults — no environment reads.  Portfolio
    /// members start here.
    pub fn base() -> Self {
        Self {
            name: "default".into(),
            tier_weight: [1, 2, 2],
            pick_ratio: 5,
            goal_dist: false,
            goal_dist_w: 2,
            cw_lits: 1,
            cw_size: 1,
            cw_vars: 2,
            cw_skolem: 1,
            lit_select: 0,
            max_depth: 5,
            max_term_size: 64,
            derived_width_cap: None,
            para_cap: 200,
            demod_cap: 64,
            bwd_demod_cap: 10_000,
            prec_seed: 0,
            fc_max_premise_lits: 6,
            fc_fanout: 16,
            fc_flat_depth: 2,
            fc_rounds: 6,
            fc_cap: 4000,
            fc_branch: 8,
            fc_max_pos: 1,
            schema: true,
            decode: true,
            ordered_resolution: false,
            subsumption: false,
            // OFF by default: measured 2026-07-05 at 60s (TPTP regime,
            // portfolio off) the channel rejects 97-99% of keq-passing
            // candidates before the exact check (GRP618+1: 5.15M -> 127k
            // full checks; LAT282+1: 11.97M -> 39k) yet given-clause
            // throughput consistently LOSES ~1% (2411->2388, 3843->3798,
            // 2398->2369) — the exact checks it removes were already
            // cheap fast-fail matches (~2% of CPU), and the decode +
            // transient-row machinery costs slightly more than it saves.
            // `SIGMA_SUBS_JOIN=1` is the on-switch for re-measurement on
            // corpora where `clause_subsumes_in` backtracking actually
            // bites (wide clauses, many equal-atom literals).
            subs_join: false,
            // OFF by default: the k-channel row machinery (phase 2a) was
            // measured negative on both consumers (see the field doc) —
            // the ~2% per-accept registration tax bought a <1ms
            // backward-demod filter, and the subsumption equality-join
            // uses transient rows only.  Off ⇒ zero row tax; phase 1's
            // postings + backward demodulation are unaffected.
            // `SIGMA_SUBTERM_ROWS=1` re-enables it for measurement.
            subterm_rows: false,
            superposition: false,
            eq_factoring: false,
            bg_completion: false,
            bg_completion_budget: 256,
            recognize_roles: false,
            modal_k: true,
            full_saturation: false,
            strict_saturation: false,
            // OFF by default: a controlled A/B over a 1285-problem TPTP
            // cross-section (2026-06-13) showed the first-cut forward
            // demodulator costs ~50 problems (7%) at an 8s timeout — it
            // prunes well (fewer steps on solves) but its per-`make`
            // overhead (a linear re-interning scan of `units.equals`
            // with a KBO compare per literal, no demodulator index)
            // pushes timeout-boundary problems over the line.  SUMO plain
            // is indifferent.  Re-default once the demodulator set is
            // indexed; kept as a portfolio knob meanwhile.
            demod: false,
            // ON by default since the posting-indexed retrieval landed
            // (exact ground keys + (head, len) buckets + seat
            // prefilter): a backward pass now touches only verified
            // redex holders, and the KIF/SUMO regression gate stayed
            // green with it on.  `SIGMA_NO_BWD_DEMOD=1` is the off
            // switch; pairs with `demod` for the full interreduction
            // discipline (see the field docs).
            bwd_demod: true,
            liu_rescue: true,
            liu_rounds: 1,
            liu_top_k: 32,
            def_completion: true,
            defcomp_rounds: 4,
            defcomp_max_adds: 64,
            defcomp_per_sym: 8,
            head_filter: false,
            bg_snapshot: true,
            semantic_guide: false,
            // OFF by default (a structural strategy variant, measured
            // via its env A/B / the tptp-deferred portfolio lane; see
            // the field doc).
            deferred_passive: false,
            // Cap arithmetic (measured on RNG044+1 @60s, the heaviest
            // known recipe generator): the uncapped run peaks at
            // 23.4 GB max RSS (18.1 GB peak footprint) vs 0.73 GB
            // knob-off while holding ~27.5M live recipes — ~820 B per
            // live recipe on the max-RSS basis (~630 B on the footprint
            // basis): the 240 B arena slot (`size_of::<Option<Recipe>>()`,
            // pinned by `recipe_footprint_is_within_the_cap_arithmetic`)
            // + the cloned binding-fragment term trees' heap + two
            // passive-heap entries + the pre-queue dedup key.  2M live
            // recipes × ~820 B ≈ 1.6 GB worst-case recipe memory
            // (≈ 2.4 GB total RSS on RNG044+1) — inside the ~1-2 GB
            // budget a 6-way portfolio lane can afford, and >20x the
            // live-queue depth any non-pathological problem in the
            // phase-1 slices reached.
            deferred_cap: 2_000_000,
        }
    }

    /// `base()` with the historical env-var kill switches applied —
    /// what `Strategy::default()` (and therefore every legacy call
    /// site) uses.
    pub fn from_env() -> Self {
        let mut s = Self::base();
        let on = |k: &str| std::env::var_os(k).is_some();
        if on("SIGMA_NO_SCHEMA") { s.schema = false; }
        if on("SIGMA_NO_DECODE") { s.decode = false; }
        if on("SIGMA_GOALDIST")  { s.goal_dist = true; }
        if on("SIGMA_NO_LIU")    { s.liu_rescue = false; s.def_completion = false; }
        if on("SIGMA_HEADFILTER") { s.head_filter = true; }
        if on("SIGMA_NO_BG_SNAPSHOT") { s.bg_snapshot = false; }
        if on("SIGMA_GUIDE")     { s.semantic_guide = true; }
        if on("SIGMA_NO_MODAL_K") { s.modal_k = false; }
        s.demod = Self::demod_env_override(s.demod);
        s.bwd_demod = Self::bwd_demod_env_override(s.bwd_demod);
        s.subs_join = Self::subs_join_env_override(s.subs_join);
        s.subterm_rows = Self::subterm_rows_env_override(s.subterm_rows);
        s.deferred_passive = Self::deferred_passive_env_override(s.deferred_passive);
        s.deferred_cap = Self::deferred_cap_env_override(s.deferred_cap);
        s
    }

    /// `SIGMA_DEMOD=1` forces demod on regardless of a strategy's own
    /// default — not a "historical" kill switch like the others in
    /// `from_env()` (demod shipped OFF from the start; see `base()`'s note
    /// on the measured TPTP regression), but added in the same style: an
    /// opt-in override for A/B measurement, shared by both `from_env()` and
    /// `tptp()` so the override works on the TPTP path too (`tptp()` builds
    /// from `base()` directly and does not otherwise read the environment).
    fn demod_env_override(default: bool) -> bool {
        if std::env::var_os("SIGMA_DEMOD").is_some() { true } else { default }
    }

    /// `SIGMA_NO_BWD_DEMOD=1` forces backward demodulation OFF,
    /// `SIGMA_BWD_DEMOD=1` forces it ON (off wins when both are set) —
    /// the two-directional peer of [`Self::demod_env_override`], shared
    /// by `from_env()` and `tptp()` so the A/B override works on both
    /// paths.  The off direction exists because `tptp()` now defaults
    /// the knob TRUE (posting-indexed retrieval landed).
    fn bwd_demod_env_override(default: bool) -> bool {
        if std::env::var_os("SIGMA_NO_BWD_DEMOD").is_some() {
            false
        } else if std::env::var_os("SIGMA_BWD_DEMOD").is_some() {
            true
        } else {
            default
        }
    }

    /// `SIGMA_NO_SUBS_JOIN=1` forces the subsumption equality-join
    /// channel OFF, `SIGMA_SUBS_JOIN=1` forces it ON (off wins when
    /// both are set) — the two-directional A/B peer of
    /// [`Self::bwd_demod_env_override`], shared by `from_env()` and
    /// `tptp()` so the override works on both paths.
    fn subs_join_env_override(default: bool) -> bool {
        if std::env::var_os("SIGMA_NO_SUBS_JOIN").is_some() {
            false
        } else if std::env::var_os("SIGMA_SUBS_JOIN").is_some() {
            true
        } else {
            default
        }
    }

    /// `SIGMA_NO_SUBTERM_ROWS=1` forces the k-channel row machinery
    /// (phase-2a rows + the decode-chain prefilter) OFF,
    /// `SIGMA_SUBTERM_ROWS=1` forces it ON (off wins when both are set) —
    /// the two-directional A/B peer of [`Self::subs_join_env_override`],
    /// shared by `from_env()` and `tptp()` so the override works on both
    /// paths (the TPTP regime is where backward demodulation — and so the
    /// decode chain — actually runs).
    fn subterm_rows_env_override(default: bool) -> bool {
        if std::env::var_os("SIGMA_NO_SUBTERM_ROWS").is_some() {
            false
        } else if std::env::var_os("SIGMA_SUBTERM_ROWS").is_some() {
            true
        } else {
            default
        }
    }

    /// `SIGMA_NO_DEFERRED_PASSIVE=1` forces the deferred-passive
    /// discipline OFF, `SIGMA_DEFERRED_PASSIVE=1` forces it ON (off
    /// wins when both are set) — the two-directional A/B peer of
    /// [`Self::subterm_rows_env_override`], shared by `from_env()` and
    /// `tptp()` so the override works on both paths (the TPTP regime
    /// is where the generation volume the discipline attacks actually
    /// exists).
    fn deferred_passive_env_override(default: bool) -> bool {
        if std::env::var_os("SIGMA_NO_DEFERRED_PASSIVE").is_some() {
            false
        } else if std::env::var_os("SIGMA_DEFERRED_PASSIVE").is_some() {
            true
        } else {
            default
        }
    }

    /// `SIGMA_DEFERRED_CAP=N` overrides the recipe budget
    /// ([`Self::deferred_cap`]) — the measurement/A-B lever for sizing
    /// the default (unparsable values are ignored).  Shared by
    /// `from_env()` and `tptp()` like the other deferred-passive
    /// overrides.
    fn deferred_cap_env_override(default: u32) -> u32 {
        std::env::var("SIGMA_DEFERRED_CAP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    /// The complete-calculus configuration for standalone TPTP problems
    /// (`.p` / `.tptp`): full saturation (no set-of-support tiering —
    /// axiom×axiom inference is on), ordered superposition + equality
    /// factoring (the complete equality calculus), forward subsumption
    /// (the flooding floor full saturation needs), and strict (honest)
    /// saturation verdicts.  `demod` stays OFF by default — measured
    /// regression on the TPTP cross-section (see `base()`) — but still
    /// honors `SIGMA_DEMOD=1` (see [`Self::demod_env_override`]) so it can
    /// be A/B'd on this path too.  Otherwise the KIF/SUMO path's `base()`
    /// is untouched.
    pub fn tptp() -> Self {
        Self {
            full_saturation:   true,
            strict_saturation: true,
            superposition:     true,
            eq_factoring:      true,
            subsumption:       true,
            demod:             Self::demod_env_override(false),
            // ON by default under the TPTP regime: retrieval is now
            // posting-indexed (exact ground keys + (head, len) buckets
            // + seat prefilter), so a backward pass touches only
            // verified redex holders instead of scanning bucket
            // clauses.  `SIGMA_NO_BWD_DEMOD=1` is the off switch.
            bwd_demod:         Self::bwd_demod_env_override(true),
            // The equality-join channel's A/B override must reach the
            // TPTP path too (its regime is where subsumption — and so
            // the channel — actually runs).
            subs_join:         Self::subs_join_env_override(Self::base().subs_join),
            // The k-channel row machinery's A/B override must reach the
            // TPTP path too (its regime is where backward demodulation —
            // and so the decode chain — actually runs).
            subterm_rows:      Self::subterm_rows_env_override(Self::base().subterm_rows),
            // Same A/B convention as demod: `SIGMA_GUIDE=1` lets the
            // semantic-guide tie-break be measured on the TPTP path too
            // (`from_env()` is not consulted here, so without this the
            // knob was unreachable for standalone `.p` runs).
            semantic_guide:    std::env::var_os("SIGMA_GUIDE").is_some(),
            // The deferred-passive discipline's A/B override must reach
            // the TPTP path too (its regime is where the generation
            // volume the discipline attacks actually exists).
            deferred_passive:  Self::deferred_passive_env_override(false),
            deferred_cap:      Self::deferred_cap_env_override(Self::base().deferred_cap),
            ..Self::base()
        }
        .named("tptp-complete")
    }

    /// Builder-style rename, for portfolio lane labels.
    pub fn named(mut self, name: &str) -> Self {
        self.name = name.into();
        self
    }

    /// The TPTP-regime strategy schedule: a small, named set of lanes, each a
    /// single-axis delta on [`Self::tptp`], for [`crate::prover::scale`]'s
    /// portfolio driver to race in sequence over slices of the total
    /// wall-clock budget.  Single-strategy native runs solve a fraction of
    /// what Vampire's `--mode casc` schedule finds on the same TPTP
    /// cross-section; retrying the identical strategy with a widened axiom
    /// budget is a no-op here (TPTP problems already run `full_saturation` —
    /// every axiom is in from the first attempt, there is nothing left to
    /// widen into), so the only lever standing in for "try a different
    /// search shape" is the strategy itself.
    ///
    /// Ordered by (measured) standalone hit rate: the shipping complete
    /// calculus first, then the classic complementary axes — a rewrite
    /// engine, conjecture-directed weighting, the deferred-passive
    /// structural discipline, an alternate literal-selection rule, and a
    /// different KBO symbol precedence (sweep memory: precedence flips are
    /// one of the highest-impact single-axis levers).  Each lane keeps
    /// `full_saturation` / `strict_saturation` / `superposition` /
    /// `eq_factoring` / `subsumption` on — only ONE knob moves per lane, so a
    /// win or loss is attributable.
    ///
    /// `SIGMA_NO_DEFERRED_PASSIVE=1` removes the `tptp-deferred` lane,
    /// reproducing the pre-phase-2 five-lane schedule byte-identically —
    /// the documented old-lane-set reproduction switch (it also forces the
    /// knob off inside every lane, so the whole binary behaves as if the
    /// discipline never shipped).  `SIGMA_DEFERRED_PASSIVE=1` (force-on)
    /// drops the lane too: every lane is already deferred then, and a
    /// duplicate-but-for-the-name lane would only dilute the schedule.
    pub fn tptp_lanes() -> Vec<Strategy> {
        let base = Strategy::tptp();
        let mut lanes = vec![
            base.clone().named("tptp-complete"),
            Strategy {
                demod: true,
                // The rewrite-engine lane carries the full interreduction
                // pair: forward demod keeps NEW clauses in normal form,
                // backward demod re-normalizes the EXISTING sets when a
                // new oriented equation lands.
                bwd_demod: true,
                ..base.clone()
            }
            .named("tptp-demod"),
            Strategy {
                goal_dist: true,
                // `goal_dist` forces bg_snapshot off on the KIF path (the
                // factor bakes conjecture-dependent weights into background
                // clauses); TPTP problems don't share snapshots across
                // conjectures anyway, but keep the invariant explicit here
                // too so this lane can't accidentally violate it.
                bg_snapshot: false,
                ..base.clone()
            }
            .named("tptp-goaldist"),
            Strategy {
                // 1 = most index candidates (vs. the default 0 = fewest) —
                // the complementary literal-selection rule.
                lit_select: 1,
                ..base.clone()
            }
            .named("tptp-litselect"),
            Strategy {
                // A different (non-identity) KBO symbol precedence — the
                // highest-impact single-axis lever per the sweep notes.
                prec_seed: 0xA5A5_1234,
                ..base.clone()
            }
            .named("tptp-precseed"),
            // `derived_width_cap` (the "tptp-wide" lane) deliberately has NO
            // slot: measured twice on the rescued ALG family at 60s — first
            // masked by the 4000-step surrender (every lane exhausted
            // `max_steps` in 2-4s of its slice), then again AFTER
            // `set_tptp_problem` lifted the step cap — it converted zero
            // verdicts either time; wide derived clauses alone are not the
            // binding constraint (searches now run their full budget and
            // time out).  The `derived_width_cap` mechanism stays (the clean
            // seam for cap experiments and future tuned schedules); a lane
            // earns its slot back through the planned sweep, not by hand.
            // `semantic_guide` deliberately has NO lane: measured on the
            // 100-problem TPTP slice it scored zero clauses (the Horn
            // extractor sees no `(=> …)` roots in CNF/disjunctive TPTP
            // input, so the model is empty there) and won zero verdicts —
            // a slot would only dilute the working lanes' budget.  Revisit
            // once extraction mines Horn structure from CNF clauses.
        ];
        // The `tptp-deferred` STRUCTURAL lane — the one lane whose delta is a
        // different search DISCIPLINE rather than a numeric knob.  Its slot is
        // measurement-earned (slice300v2 @60s single-lane on-vs-off): +2 net
        // solves at equal budget, and the union case eager 77 + deferred 79 ⇒
        // 81 — four ON-only solves {LAT293+4, LAT307+4, LAT310+4, SWW180+1}
        // vs two OFF-only, exactly the lane diversity the numeric-knob sweep
        // failed to find.  Placement (index 3, after the tuned trio, before
        // litselect/precseed): the ON-only solves are FAST under the
        // discipline (measured single-lane: 3.1s / 3.4s / 5.9s / 10.8s), so
        // the ~7-8s mid-schedule slice a 60s budget yields captures most of
        // them; the tight-budget buckets (`adaptive_lane_count`: 1 lane
        // <15s, 3 lanes 15..=40s) keep their measured composition because
        // the prefix ahead of index 3 is unchanged; and the two lanes the
        // overnight sweep measured weakest (litselect / precseed) sit
        // behind it, so any cumulative-overrun tail-skip hits them first.
        //
        // The lane carries its own recipe budget: 2.75M covers SWW180+1's
        // measured 2.66M-live-recipe solve (3.4s; at the shipping 2M cap the
        // eager fallbacks perturb the search and the solve is lost) at
        // ~2.3 GB worst-case recipe memory — affordable HERE because a
        // portfolio lane's exposure is bounded by its slice (~8s of queue
        // growth), unlike the knob-on default which must survive a full
        // 60s run on the heaviest generator under ~3 GB (the 2M default;
        // see `base()`'s arithmetic).
        if std::env::var_os("SIGMA_NO_DEFERRED_PASSIVE").is_none() && !base.deferred_passive {
            lanes.insert(
                3,
                Strategy {
                    deferred_passive: true,
                    deferred_cap: 2_750_000,
                    ..base
                }
                .named("tptp-deferred"),
            );
        }
        lanes
    }

    /// Fingerprint of the fields that shape the FROZEN BACKGROUND —
    /// anything `make` consults while loading background clauses
    /// (caps that drop clauses, the schema channel's absorption /
    /// orientation, stored weights).  Folded into the snapshot key so
    /// two strategies never share a base they'd have built
    /// differently.  Search-time knobs (pick ratio, paramodulation
    /// cap, forward closure, decode, selection) are deliberately
    /// excluded — they don't change what the frozen state contains.
    pub fn bg_fingerprint(&self) -> u64 {
        use xxhash_rust::xxh64::xxh64;
        let words: [u64; 8] = [
            self.tier_weight[0],
            self.tier_weight[1],
            self.tier_weight[2],
            u64::from(self.max_depth),
            self.max_term_size as u64,
            u64::from(self.schema),
            u64::from(self.goal_dist),
            self.goal_dist_w,
        ];
        let mut buf = [0u8; 64];
        for (i, w) in words.iter().enumerate() {
            buf[i * 8..(i + 1) * 8].copy_from_slice(&w.to_be_bytes());
        }
        xxh64(&buf, 0x57A7_E6F0)
    }

    /// A deterministic random strategy for sweep harnesses: `seed`
    /// fully determines the result (splitmix64 stream), so a sweep is
    /// reproducible from its seed and a found lane can be regenerated
    /// from its name.  Sampling ranges are the plausible-search
    /// envelope around the shipping defaults — discrete choice lists
    /// (log-spaced for the caps), with the measured-good switches
    /// biased ON (schema/decode/Liu) and the measured-risky ones
    /// biased OFF (head filter, goal-dist).  `bg_snapshot` stays true:
    /// it is a caching knob, invisible to the search itself.
    pub fn sample(seed: u64) -> Strategy {
        // splitmix64 (Vigna) — tiny, no dependency, full-period.
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = self.0;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                z ^ (z >> 31)
            }
            fn pick(&mut self, xs: &[u64]) -> u64 {
                xs[(self.next() % xs.len() as u64) as usize]
            }
            fn chance(&mut self, percent: u64) -> bool {
                self.next() % 100 < percent
            }
        }
        let mut r = Rng(seed);

        Strategy {
            name: format!("rnd-{seed:08x}"),
            tier_weight: [
                r.pick(&[1, 1, 1, 2]),
                r.pick(&[1, 2, 2, 3, 4]),
                r.pick(&[1, 2, 2, 3, 4, 6, 8]),
            ],
            pick_ratio: r.pick(&[2, 3, 4, 5, 6, 8, 10]),
            goal_dist: r.chance(25),
            goal_dist_w: r.pick(&[1, 2, 3, 4]),
            // Clause-weight coefficients — the search's core gradient; keep
            // them near the viable region (the IJCAI "within an order of
            // magnitude" rule) so the GA/sweep doesn't wander.
            cw_lits: r.pick(&[0, 1, 1, 2, 3]),
            cw_size: r.pick(&[1, 1, 2, 3]),
            cw_vars: r.pick(&[0, 1, 2, 2, 4]),
            cw_skolem: r.pick(&[0, 1, 1, 2, 4]),
            lit_select: r.pick(&[0, 0, 0, 1, 2]) as u8,
            max_depth: r.pick(&[4, 5, 6, 7]) as u8,
            max_term_size: r.pick(&[48, 64, 96, 128]) as usize,
            // Not in the sweep genome yet (no RNG draw): a fresh knob,
            // measured via the dedicated `tptp-wide` lane first.
            derived_width_cap: None,
            para_cap: r.pick(&[50, 100, 200, 400, 800]) as usize,
            demod_cap: r.pick(&[32, 64, 128, 256]),
            // Not in the sweep genome yet (a literal, no RNG draw — the
            // sample streams of existing seeds shift only by the fields
            // that DO draw): measure it via the tptp-demod lane first.
            bwd_demod_cap: 10_000,
            prec_seed: r.pick(&[0, 0, 0, 0xA5A5_1234, 0x1357_9BDF, 0xF00D_CAFE]),
            fc_max_premise_lits: r.pick(&[4, 6, 8]) as usize,
            fc_fanout: r.pick(&[8, 16, 32, 64]) as usize,
            fc_flat_depth: r.pick(&[1, 2, 3]) as u8,
            fc_rounds: r.pick(&[2, 4, 6, 8, 10]) as usize,
            fc_cap: r.pick(&[2000, 4000, 8000, 16000]) as usize,
            fc_branch: r.pick(&[4, 8, 16]) as usize,
            fc_max_pos: r.pick(&[1, 1, 2, 3]) as usize,
            schema: r.chance(85),
            decode: r.chance(90),
            // Biased OFF — measured net-negative on TPTP pre-indexing
            // (see base()); still sampled so the sweep can re-find a
            // win once the demodulator is indexed.
            demod: r.chance(35),
            // Not in the sweep genome yet (no RNG draw): measure it via
            // the tptp-demod lane first.
            bwd_demod: false,
            // Experimental (superposition prerequisites); rarely sampled
            // until the full ordered calculus lands.
            ordered_resolution: r.chance(15),
            subsumption: r.chance(20),
            // Not in the sweep genome (no RNG draw — existing seeds'
            // sample streams shift only via fields that DO draw): a
            // pure prefilter, measured via its env A/B instead.
            subs_join: Strategy::base().subs_join,
            // Not in the sweep genome (no RNG draw): the row machinery
            // was measured net-negative, kept switchable via its env A/B.
            subterm_rows: Strategy::base().subterm_rows,
            superposition: r.chance(10),
            eq_factoring: r.chance(10),
            bg_completion: r.chance(10),
            bg_completion_budget: r.pick(&[128, 256, 512]) as usize,
            // Correctness/portability feature, not a search lever — kept
            // out of the sweep genome (and a no-op on SUMO anyway).
            recognize_roles: false,
            // Correctness/parity feature (attitude-relation K schemata),
            // not a search lever — kept out of the sweep genome.
            modal_k: true,
            // Problem-regime knobs (SOS vs full saturation, verdict
            // honesty), not search levers — the caller's path picks them.
            full_saturation: false,
            strict_saturation: false,
            liu_rescue: r.chance(85),
            liu_rounds: r.pick(&[1, 1, 1, 2]) as usize,
            liu_top_k: r.pick(&[16, 32, 64, 128]) as usize,
            def_completion: r.chance(85),
            defcomp_rounds: r.pick(&[2, 4, 6, 8]) as usize,
            defcomp_max_adds: r.pick(&[32, 64, 128, 256]) as usize,
            defcomp_per_sym: r.pick(&[4, 8, 16]) as usize,
            head_filter: r.chance(15),
            bg_snapshot: true,
            // Cheap, reorder-only guidance; sample it like the other
            // search-shaping switches once the tie-break has proven out.
            semantic_guide: r.chance(20),
            // Not in the sweep genome (no RNG draw — existing seeds'
            // sample streams shift only via fields that DO draw): a
            // structural variant, measured via its env A/B first.
            deferred_passive: false,
            // Not in the sweep genome (no RNG draw): a safety budget,
            // not a search lever — the measured default applies.
            deferred_cap: Strategy::base().deferred_cap,
        }
    }

    /// A small, diverse default portfolio.  Lanes are ordered by how
    /// often they win alone: the shipping default first, then the
    /// classic complementary axes (goal-directed weighting, wider /
    /// narrower generation, selection-shape variants).  A portfolio
    /// runner races these and takes the first conclusive verdict.
    pub fn default_portfolio() -> Vec<Strategy> {
        let base = Strategy::base();
        vec![
            base.clone(),
            Strategy {
                goal_dist: true,
                bg_snapshot: false,
                ..base.clone()
            }
            .named("goal-directed"),
            Strategy {
                tier_weight: [1, 1, 4],
                pick_ratio: 3,
                ..base.clone()
            }
            .named("conjecture-heavy"),
            Strategy {
                max_depth: 7,
                max_term_size: 96,
                para_cap: 400,
                fc_cap: 8000,
                ..base.clone()
            }
            .named("deep-generation"),
            Strategy {
                head_filter: true,
                liu_top_k: 64,
                defcomp_max_adds: 128,
                ..base.clone()
            }
            .named("lean-selection"),
            Strategy {
                schema: false,
                decode: false,
                ..base
            }
            .named("plain-resolution"),
        ]
    }
}

impl Default for Strategy {
    /// Env-honoring, matching the prover's historical behavior: the
    /// `SIGMA_*` kill switches keep working for every caller that
    /// doesn't set a strategy explicitly.
    fn default() -> Self {
        Self::from_env()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_json_spec_overrides_only_named_fields() {
        let s: Strategy =
            serde_json::from_str(r#"{"name":"narrow","para_cap":50,"schema":false}"#).unwrap();
        assert_eq!(s.name, "narrow");
        assert_eq!(s.para_cap, 50);
        assert!(!s.schema);
        // Unnamed fields fall back to the (env-honoring) defaults.
        assert_eq!(s.max_depth, Strategy::default().max_depth);
    }

    #[test]
    fn bg_fingerprint_separates_make_shaping_knobs_only() {
        let base = Strategy::base();
        let mut search_only = base.clone();
        search_only.pick_ratio = 1;
        search_only.para_cap = 9999;
        search_only.liu_top_k = 128;
        assert_eq!(base.bg_fingerprint(), search_only.bg_fingerprint());

        let mut deeper = base.clone();
        deeper.max_depth = 7;
        assert_ne!(base.bg_fingerprint(), deeper.bg_fingerprint());

        let mut no_schema = base;
        no_schema.schema = false;
        assert_ne!(no_schema.bg_fingerprint(), Strategy::base().bg_fingerprint());
    }

    #[test]
    fn sample_is_deterministic_and_seed_sensitive() {
        assert_eq!(Strategy::sample(42), Strategy::sample(42));
        // Adjacent seeds must diverge somewhere (different streams).
        let (a, b) = (Strategy::sample(42), Strategy::sample(43));
        assert_ne!(a, b);
    }

    #[test]
    fn default_portfolio_lanes_are_distinct() {
        let lanes = Strategy::default_portfolio();
        for (i, a) in lanes.iter().enumerate() {
            for b in &lanes[i + 1..] {
                assert_ne!(a, b, "{} duplicates {}", a.name, b.name);
            }
        }
    }

    /// `tptp_lanes()` reads `SIGMA_NO_DEFERRED_PASSIVE` (the old-lane-set
    /// reproduction switch), and one test below mutates that process-global
    /// env var — every test whose assertions depend on the DEFAULT lane set
    /// must therefore serialize against it (unlike scale.rs's
    /// `SIGMA_ALL_LANES` test, where no concurrent test's expectations read
    /// the same var).  `unwrap_or_else(into_inner)` keeps one failing test
    /// from poisoning the rest.
    static LANE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn lane_env_guard() -> std::sync::MutexGuard<'static, ()> {
        LANE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn tptp_lanes_are_distinct_and_named() {
        let _env = lane_env_guard();
        let lanes = Strategy::tptp_lanes();
        assert_eq!(lanes.len(), 6);
        for (i, a) in lanes.iter().enumerate() {
            assert!(!a.name.is_empty());
            for b in &lanes[i + 1..] {
                assert_ne!(a, b, "{} duplicates {}", a.name, b.name);
            }
        }
    }

    #[test]
    fn tptp_lanes_keep_the_complete_calculus_on() {
        let _env = lane_env_guard();
        // Every lane must stay within the TPTP-regime contract (full
        // saturation + strict/honest verdicts + the complete equality
        // calculus) — only the search-shaping knob named by the lane moves.
        for s in Strategy::tptp_lanes() {
            assert!(s.full_saturation, "{}: full_saturation dropped", s.name);
            assert!(s.strict_saturation, "{}: strict_saturation dropped", s.name);
            assert!(s.superposition, "{}: superposition dropped", s.name);
            assert!(s.eq_factoring, "{}: eq_factoring dropped", s.name);
            assert!(s.subsumption, "{}: subsumption dropped", s.name);
        }
    }

    #[test]
    fn tptp_lanes_first_is_plain_tptp() {
        let _env = lane_env_guard();
        assert_eq!(Strategy::tptp_lanes()[0], Strategy::tptp());
    }

    #[test]
    fn tptp_lanes_ordering_prefix_is_stable() {
        let _env = lane_env_guard();
        // Lane names/order must stay exactly as before under any
        // lane-count truncation (`run_portfolio_schedule`'s
        // `adaptive_lane_count` slices this Vec directly).  In
        // particular the FIRST THREE lanes — the `15..=40s` bucket's
        // measured composition — must not be perturbed by the
        // `tptp-deferred` insertion at index 3.
        let lanes = Strategy::tptp_lanes();
        let names: Vec<&str> = lanes.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "tptp-complete",
                "tptp-demod",
                "tptp-goaldist",
                "tptp-deferred",
                "tptp-litselect",
                "tptp-precseed",
            ]
        );
    }

    #[test]
    fn tptp_deferred_lane_is_a_single_axis_delta_with_its_measured_cap() {
        let _env = lane_env_guard();
        // The structural lane differs from `tptp()` in exactly the
        // deferred-passive discipline (plus its lane-specific recipe
        // budget — see the lane comment for the SWW180+1 sizing) and
        // its name; every other knob is byte-identical.
        let lane = Strategy::tptp_lanes()
            .into_iter()
            .find(|l| l.name == "tptp-deferred")
            .expect("tptp-deferred lane present by default");
        assert!(lane.deferred_passive);
        assert_eq!(lane.deferred_cap, 2_750_000);
        let neutralized = Strategy {
            deferred_passive: false,
            deferred_cap: Strategy::tptp().deferred_cap,
            ..lane
        }
        .named("tptp-complete");
        assert_eq!(neutralized, Strategy::tptp());
    }

    #[test]
    fn tptp_lanes_no_deferred_env_reproduces_the_old_five_lane_schedule() {
        let _env = lane_env_guard();
        // SIGMA_NO_DEFERRED_PASSIVE=1 is the documented
        // old-lane-set reproduction switch: the schedule must be
        // byte-identical to the pre-phase-2 five lanes (and, with the
        // knob forced off everywhere, the whole binary behaves as if
        // the discipline never shipped).  Mutates a process-global env
        // var — scoped narrowly and cleaned up immediately, same
        // convention as scale.rs's SIGMA_ALL_LANES test.
        std::env::set_var("SIGMA_NO_DEFERRED_PASSIVE", "1");
        let result = std::panic::catch_unwind(|| {
            let lanes = Strategy::tptp_lanes();
            let names: Vec<&str> = lanes.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(
                names,
                vec![
                    "tptp-complete",
                    "tptp-demod",
                    "tptp-goaldist",
                    "tptp-litselect",
                    "tptp-precseed",
                ]
            );
            assert!(lanes.iter().all(|l| !l.deferred_passive));
        });
        std::env::remove_var("SIGMA_NO_DEFERRED_PASSIVE");
        result.unwrap();
    }

    #[test]
    fn derived_width_cap_mechanism_survives_without_a_lane() {
        let _env = lane_env_guard();
        // The tptp-wide LANE was measured out (zero conversions even after
        // the step-cap lift), but the mechanism is the seam future tuned
        // schedules use: a Strategy carrying the cap must round-trip and
        // differ from tptp() in exactly that field.
        let complete = Strategy::tptp();
        let wide = Strategy { derived_width_cap: Some(32), ..complete.clone() };
        assert_eq!(wide.derived_width_cap, Some(32));
        let reset = Strategy { derived_width_cap: None, ..wide };
        assert_eq!(reset, complete);
        assert!(Strategy::tptp_lanes().iter().all(|l| l.derived_width_cap.is_none()));
    }
}
