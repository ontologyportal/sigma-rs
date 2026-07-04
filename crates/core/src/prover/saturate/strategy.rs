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
    /// Paramodulants generated per given clause.
    pub para_cap: usize,
    /// Max demodulation rewrites applied to a single term in `make`
    /// (a fan-out guard; KBO already guarantees termination).
    pub demod_cap: u64,
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
            para_cap: 200,
            demod_cap: 64,
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
            superposition: false,
            eq_factoring: false,
            bg_completion: false,
            bg_completion_budget: 256,
            recognize_roles: false,
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
            liu_rescue: true,
            liu_rounds: 1,
            liu_top_k: 32,
            def_completion: true,
            defcomp_rounds: 4,
            defcomp_max_adds: 64,
            defcomp_per_sym: 8,
            head_filter: false,
            bg_snapshot: true,
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
        s.demod = Self::demod_env_override(s.demod);
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
    /// engine, conjecture-directed weighting, an alternate literal-selection
    /// rule, and a different KBO symbol precedence (sweep memory: precedence
    /// flips are one of the highest-impact single-axis levers).  Each lane
    /// keeps `full_saturation` / `strict_saturation` / `superposition` /
    /// `eq_factoring` / `subsumption` on — only ONE knob moves per lane, so a
    /// win or loss is attributable.
    pub fn tptp_lanes() -> Vec<Strategy> {
        let base = Strategy::tptp();
        vec![
            base.clone().named("tptp-complete"),
            Strategy {
                demod: true,
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
                ..base
            }
            .named("tptp-precseed"),
        ]
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
            para_cap: r.pick(&[50, 100, 200, 400, 800]) as usize,
            demod_cap: r.pick(&[32, 64, 128, 256]),
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
            // Experimental (superposition prerequisites); rarely sampled
            // until the full ordered calculus lands.
            ordered_resolution: r.chance(15),
            subsumption: r.chance(20),
            superposition: r.chance(10),
            eq_factoring: r.chance(10),
            bg_completion: r.chance(10),
            bg_completion_budget: r.pick(&[128, 256, 512]) as usize,
            // Correctness/portability feature, not a search lever — kept
            // out of the sweep genome (and a no-op on SUMO anyway).
            recognize_roles: false,
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

    #[test]
    fn tptp_lanes_are_distinct_and_named() {
        let lanes = Strategy::tptp_lanes();
        assert_eq!(lanes.len(), 5);
        for (i, a) in lanes.iter().enumerate() {
            assert!(!a.name.is_empty());
            for b in &lanes[i + 1..] {
                assert_ne!(a, b, "{} duplicates {}", a.name, b.name);
            }
        }
    }

    #[test]
    fn tptp_lanes_keep_the_complete_calculus_on() {
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
        assert_eq!(Strategy::tptp_lanes()[0], Strategy::tptp());
    }
}
