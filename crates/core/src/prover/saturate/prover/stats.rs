// crates/core/src/prover/saturate/prover/stats.rs
//
// Per-run instrumentation counters for the given-clause loop
// (`NativeProver::stats`).  Populated throughout `mod.rs`/`discharge.rs`/
// `make.rs`/`forward.rs`; the SIGMA_STATS end-of-run report formatting
// lives in `prove.rs`, which reads these fields directly.

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
    /// `forward_subsumed` candidate probes attempted: one per active,
    /// non-retired, length-compatible candidate the literal index
    /// returned as a possible subsumer of the new clause (retired
    /// clauses and outright-too-long subsumers are filtered before this
    /// count, same as `clause_subsumes` would reject them for free) —
    /// the prefilter chain's denominator.  Invariant:
    /// `subs_checks_attempted == subs_rejected_by_bloom_leaf +
    /// subs_rejected_by_bloom_glit + subs_rejected_by_fv +
    /// subs_rejected_by_keq + subs_full_checks` (the channels run in
    /// that order; each rejection is attributed to the FIRST channel
    /// that fired).
    pub(crate) subs_checks_attempted: u64,
    /// Of those, how many were REJECTED by the leaf-bloom channel
    /// (`fvi::ClauseBlooms::leaf` subset test — one AND per candidate,
    /// the cheapest channel, so it runs first).
    pub(crate) subs_rejected_by_bloom_leaf: u64,
    /// Of those, how many were REJECTED by the ground-literal-bloom
    /// channel (`fvi::ClauseBlooms::glit` subset test), having passed
    /// the leaf bloom.
    pub(crate) subs_rejected_by_bloom_glit: u64,
    /// Attempted checks where the ground-literal channel could act at
    /// all: the candidate subsumer has at least one FULLY GROUND
    /// literal (`glit != 0`) and the check reached that channel — the
    /// channel's applicability denominator (a clause with no ground
    /// literals passes it vacuously).
    pub(crate) subs_glit_applicable: u64,
    /// Of those, how many were REJECTED by the feature-vector prefilter
    /// (`fvi::ClauseFv::le`) before the expensive `clause_subsumes` call
    /// — the prefilter's payoff.
    pub(crate) subs_rejected_by_fv: u64,
    /// Of those, how many were REJECTED by the per-literal Key-Equation
    /// counting filter (`keq_unpartnered`): some literal of the
    /// candidate subsumer has NO Key-Equation-compatible literal in the
    /// new clause (same polarity + same arity + residue match), having
    /// passed both blooms and the FV channels.
    pub(crate) subs_rejected_by_keq: u64,
    /// (candidate-subsumer literal, new-clause literal) compatibility
    /// tests evaluated inside the Key-Equation counting filter — every
    /// inner-scan step counts, including the cheap polarity/arity
    /// gates; each partner scan stops at its first compatible literal.
    /// The channel's workload numerator (each test is
    /// O(popcount(mask)) at worst).
    pub(crate) keq_pair_tests: u64,
    /// Of those, how many passed every prefilter channel and were handed
    /// to the exact `clause_subsumes` check (see the sum invariant on
    /// `subs_checks_attempted`).
    pub(crate) subs_full_checks: u64,
    /// TRUE `ClauseKey` collisions detected by the verified dedup
    /// (`NativeProver::seen_duplicate*` / `seen_insert`): a `seen` key
    /// hit whose first-accepted clause has DIFFERENT canonical literals
    /// than the probing clause.  The probing clause is ACCEPTED (never
    /// dropped — dropping a non-duplicate on a bare 64-bit key match
    /// would be a completeness hole); the map keeps the FIRST id, so
    /// later collision-mates simply bypass dedup, which is sound (dedup
    /// is an optimization, re-processing is never wrong).  Expected
    /// ~never; a nonzero count prints its own SIGMA_STATS line.
    pub(crate) dedup_collisions_detected: u64,
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

    // -- decode fast-path cause profile (Step-2 instrumentation only;
    //    zero behavior change).  Counted EXCLUSIVELY in the batch
    //    section of the resolve loop — each (given-literal, partner)
    //    pair exactly once: per pair for partner/tail causes, bulk
    //    (candidate-set size) for given-shape causes.  Scalar decode
    //    reruns (batch-anomaly fallbacks through `resolve`, and the
    //    goal-directed discharge paths) never count.  Invariant:
    //    `decode_attempts == decode_bindings_extracted +
    //    decode_bail_nested_var + decode_bail_too_many_open +
    //    decode_bail_partner_shape + decode_bail_phonebook_or_collision
    //    + decode_bail_other`.  All zero when `Strategy.decode` is off.
    /// Pairs that entered the decode machinery — the cause denominator.
    pub(crate) decode_attempts: u64,
    /// Pairs whose bindings were fully extracted algebraically (the
    /// resolvent was built with no unification walk) — the batch-scoped
    /// slice of `decoded_resolutions`.
    pub(crate) decode_bindings_extracted: u64,
    /// Given-shape bail: an open seat holds a compound containing a
    /// variable — THE decision counter for the homomorphic
    /// path-weighted sketch extension (bulk-attributed).
    pub(crate) decode_bail_nested_var: u64,
    /// Given-shape bail: more than 2 open seats (the quadratic sketch
    /// solver's limit; bulk-attributed).
    pub(crate) decode_bail_too_many_open: u64,
    /// Partner-side bail: not a ground unit of matching arity.
    pub(crate) decode_bail_partner_shape: u64,
    /// Decode-tail bail: the residual sketch failed to decode
    /// (`Decoded::Fail`) or a decoded coin was missing from the phone
    /// book — both collision-flavored.
    pub(crate) decode_bail_phonebook_or_collision: u64,
    /// Everything else: non-`App` given literal / out-of-range seat
    /// (bulk-attributed) plus decode-tail seat/binding mismatches.
    pub(crate) decode_bail_other: u64,

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
    /// Relations COMPLETION-CERTIFIED by the model registry (a KB-level
    /// property recorded once per run when SIGMA_MODEL is on, not a
    /// counter — see `ModelProgram::certified`).
    pub(crate) model_certified_relations: u64,
    /// Negative ground units emitted by the Clark-completion discharge
    /// (rule tag `model_complete`).
    pub(crate) model_complete_negatives_emitted: u64,
    /// Certification refusals by reason, copied from the registry's
    /// build-time `CertBlocked` breakdown when SIGMA_MODEL is on.
    pub(crate) model_cert_blocked_skipped_head: u64,
    pub(crate) model_cert_blocked_unstratifiable: u64,
    pub(crate) model_cert_blocked_body_chain: u64,
    pub(crate) model_cert_blocked_role: u64,
    /// Model evaluations aborted by `ModelError::Inconsistent`: an EGD
    /// forced a union of two distinct rigid (numeric-literal) symbols.
    /// Printed in SIGMA_STATS only when nonzero (default output unchanged).
    pub(crate) model_rigid_conflicts: u64,
    /// Ground flat NEGATIVE literals `¬R(args)` deleted in `make` because
    /// the shared positive model (`SIGMA_MODEL`) already contains `R(args)`
    /// — the model-sourced mirror of the oracle's `oracle.holds` deletion
    /// just above it (see `model_true_negative`).  Zero unless `SIGMA_MODEL`
    /// is set.
    pub(crate) model_literals_deleted: u64,

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
    /// Subterm visits during `demodulate`'s traversal that the
    /// SYMBOL-SIGNATURE prefilter (`DemodIndex::possibly_matches`) ruled
    /// out before any match probe was built — no active demodulator
    /// shares the subterm's head shape, so the clone/shift/match walk
    /// was skipped outright.
    pub(crate) demod_scans_skipped_by_prefilter: u64,
    /// Subterm visits during `demodulate`'s traversal that passed the
    /// prefilter and were actually handed to the candidate match loop
    /// (paired with `demod_scans_skipped_by_prefilter` for the
    /// reduction ratio).
    pub(crate) demod_scans_performed: u64,

    // -- ground-term identity: whole-subtree bloom pruning + NF memo
    //    (Parts 3.2/4 of the two-tier design; all zero unless
    //    Strategy.demod is on — the machinery never runs otherwise).
    /// Ground maximal subtrees whose entire descent was skipped because
    /// their symbol bloom shares no bit with the registered demodulator
    /// head-bit mask (`DemodIndex::head_bits`) — a proof of redex
    /// absence, twin-checked in debug builds.
    pub(crate) bloom_subtrees_pruned: u64,
    /// NF-memo probes: one per ground maximal subtree the demod walk
    /// enters that survives the bloom prune.
    pub(crate) nf_probes: u64,
    /// Probes answered "already in normal form" — the whole subtree is
    /// skipped, no redex search.
    pub(crate) nf_hits_unchanged: u64,
    /// Probes answered with a cached normal form — spliced in (one
    /// clone), no redex search; rewrite/citation accounting replayed
    /// from the entry.
    pub(crate) nf_hits_rewritten: u64,
    /// Probes with no usable entry (absent, stale generation, or too
    /// little demod-cap budget left to splice the whole normal form).
    pub(crate) nf_misses: u64,
    /// Entries discarded on probe because their generation predates the
    /// current demodulator set (a new registration can enable further
    /// rewrites, so an older recorded NF is no longer known-normal).
    pub(crate) nf_stale_discards: u64,

    // -- backward demodulation (Strategy.bwd_demod; see
    //    NativeProver::backward_demodulate).  All zero unless the knob
    //    (or SIGMA_BWD_DEMOD=1) is on.
    /// Backward passes run — one per newly activated, KBO-oriented unit
    /// equation the demodulator index accepted.
    pub(crate) bwd_demod_triggered: u64,
    /// Existing (active/passive) clauses a backward pass rewrote; each
    /// spawns a replacement through `make` (rule tag `bwd_demod`).
    pub(crate) bwd_demod_clauses_rewritten: u64,
    /// Originals retired after a backward rewrite (tracks
    /// `bwd_demod_clauses_rewritten` 1:1 — split out so an invariant
    /// break is visible in the stats line).
    pub(crate) bwd_demod_retired: u64,
    /// Backward passes truncated by `Strategy.bwd_demod_cap` — the
    /// remaining candidates were left unsimplified (sound; just less
    /// interreduced).
    pub(crate) bwd_demod_cap_hits: u64,

    // -- proof-DAG discharge-rule reach (counted once per completed proof
    //    extraction, at refutation time).
    pub(crate) proof_tag_model: u64,
    pub(crate) proof_tag_model_join: u64,
    pub(crate) proof_tag_join: u64,
    pub(crate) proof_tag_event_calculus: u64,
    pub(crate) proof_tag_oracle: u64,

    // -- semantic clause-selection guidance (Strategy.semantic_guide;
    //    see `NativeProver::guide_score` / `push`).  Zero unless the
    //    strategy knob (or `SIGMA_GUIDE=1`) is on.
    /// Passive clauses whose guide score was computed (a non-neutral
    /// literal was found and scored against the positive model).
    pub(crate) guided_clauses_scored: u64,
    /// Guidance requested but the one-time model build bailed
    /// (`ModelProgram::positive_model` returned `None`, e.g. the
    /// materialization budget was exceeded) — guidance silently
    /// disabled for the rest of the run.
    pub(crate) guide_disabled_bail: u64,
}
