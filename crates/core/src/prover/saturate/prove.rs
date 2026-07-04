// crates/core/src/prover/saturate/prove.rs
//
// Proving driver

use std::collections::HashSet;
use std::time::Instant;

use crate::{SentenceId, SymbolId, profile_span};
use crate::prover::{ProverResult, ProverStatus, TerminationReason};
use crate::semantics::types::Scope;
use crate::syntactic::caches::session::session_id;
use crate::types::Element;

use super::{ProverLayer, Conjecture};
use super::clause::{AtomId, PClause};
use super::clausify::clausify_negated_conjunction;
use super::prover::{NativeOpts, NativeProver, RunVerdict};
use super::strategy::Strategy;
use super::theory::TheoryOracle;

impl ProverLayer {
    /// Intern the conjecture into the prover-local atom table (content-addressed,
    /// tag-free → sweep-safe; no shared-store churn/rollback, plan D5) and
    /// resolve the roots.  The `&self` core behind both the trait
    /// `intern_conjecture` and the `&self` `ask_native`.
    pub(super) fn intern_conjecture_native(&self, asts: &[crate::AstNode])
        -> Vec<(std::sync::Arc<crate::types::Sentence>, SentenceId)> {
        let mut sents = Vec::new();
        for n in asts {
            let Some((root, subs)) =
                crate::syntactic::sentence::build_detached(n) else { continue };
            for sub in subs {
                self.atoms.intern_sentence(sub);
            }
            let sid = self.atoms.intern_sentence(root);
            let Some(arc) = self.atoms
                .resolve(sid, &self.semantic.syntactic) else { continue };
            sents.push((arc, sid));
        }
        sents
    }

    /// The native `&self` prove entry — parse-detached conjecture → scaled
    /// prover-feedback loop over [`prove_one_driver`](Self::prove_one_driver).
    /// `&self` (interior-mut atoms/snapshots) so the sweep can prove many
    /// conjectures on one shared layer across threads.  Selection / session ride
    /// in on `opts` — the consolidated [`NativeOpts`].  `KnowledgeBase::ask_query`
    /// is a thin wrapper that parses KIF text and supplies the `ProveCtx`.
    pub(crate) fn prove_native(
        &self,
        asts:        Vec<crate::AstNode>,
        opts:        NativeOpts,
        ctx:         &crate::ProveCtx,
    ) -> ProverResult {
        // Prepare: normalize + seed + intern into the atom table (all `&self`).
        let normalized = Conjecture::normalize(asts);
        let seed_syms  = Conjecture::seed(&normalized);
        let sents      = self.intern_conjecture_native(&normalized);
        if sents.is_empty() {
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: "No query sentence parsed".into(),
                ..Default::default()
            };
        }
        let conj = Conjecture { sents, seed_syms };

        let selection     = opts.selection;
        let total_timeout = opts.time_limit_secs.min(u64::from(u32::MAX)) as u32;

        if !selection.autoscaling() {
            return self.prove_one_driver(&conj, selection, total_timeout, &opts, ctx).0;
        }

        // TPTP regime (standalone `.p`/`.tptp` problem — `set_tptp_problem`
        // swapped in `Strategy::tptp()`, the only source of
        // `full_saturation`): a CASC-style strategy schedule stands in for
        // the budget-widening retry, which is a no-op here (see
        // `run_portfolio_schedule`'s doc).  `SIGMA_NO_PORTFOLIO=1` forces the
        // single-lane path for A/B measurement.  The KIF/SUMO path
        // (`full_saturation` off) is completely unaffected — same `drive`
        // call as before.
        if opts.strategy.full_saturation && std::env::var_os("SIGMA_NO_PORTFOLIO").is_none() {
            return self.run_portfolio_schedule(&conj, total_timeout, &opts, ctx);
        }

        // Prover-feedback autoscaling — the same shared planner the trait
        // `prove` uses, driven here directly so the path stays `&self`.
        use crate::prover::scale::{drive, ScaleConfig};
        use crate::syntactic::sine::{
            scale_factor, scale_max_disproofs, scale_max_time_runs, scale_min_budget,
        };
        let cfg = ScaleConfig {
            factor:        scale_factor(),
            max_disproofs: scale_max_disproofs(),
            max_time_runs: scale_max_time_runs(),
            min_budget:    scale_min_budget(),
            total_timeout,
        };
        drive(selection, cfg, Self::remap_native, |params, slice| {
            self.prove_one_driver(&conj, params, slice, &opts, ctx)
        })
    }

    /// Native step-exhaustion (`GaveUp`) narrows like a timeout, not widens —
    /// the search space was too big, the wrong gradient for the planner to
    /// read as prover-incompleteness. Shared by the plain autoscale loop
    /// above and each lane of [`run_portfolio_schedule`] (mirrors the trait
    /// `ProvingLayer::remap` override for `ProverLayer`).
    fn remap_native(
        _status: ProverStatus,
        term:    Option<TerminationReason>,
    ) -> Option<TerminationReason> {
        match term {
            Some(TerminationReason::GaveUp) => Some(TerminationReason::TimeLimit),
            other => other,
        }
    }

    /// CASC-style strategy schedule for a standalone TPTP problem: race
    /// [`Strategy::tptp_lanes`](super::strategy::Strategy::tptp_lanes) in
    /// order, each over its own slice of `total_timeout` (see
    /// [`crate::prover::scale::drive_portfolio`] for the split / carry-forward
    /// rule). Every lane still runs the ordinary budget-autoscaling `drive`
    /// loop internally — full saturation means that loop mostly just
    /// re-confirms the same search, but it costs nothing to leave it wired in
    /// (a lane that somehow doesn't reach the ceiling on its first shot still
    /// benefits). A verdict of Proved/Inconsistent, or a CONFIDENT
    /// Disproved/Consistent, from any lane ends the schedule immediately;
    /// otherwise the best-ranked (never worse-than-first) result across every
    /// lane is returned. The winning lane's name is prepended to
    /// `raw_output` so `SIGMA_STATS`/verbose output can show which lane
    /// solved it.
    pub(super) fn run_portfolio_schedule(
        &self,
        conj:          &Conjecture,
        total_timeout: u32,
        opts:          &NativeOpts,
        ctx:           &crate::ProveCtx,
    ) -> ProverResult {
        use crate::prover::scale::{adaptive_lane_count, drive, drive_portfolio, ScaleConfig};
        use crate::syntactic::sine::{
            scale_factor, scale_max_disproofs, scale_max_time_runs, scale_min_budget,
        };

        let all_lanes: Vec<Strategy> = Strategy::tptp_lanes();
        // Budget-adaptive lane count (task #33): racing all 5 lanes against a
        // tight total timeout starves every lane after the first — see
        // `adaptive_lane_count`'s doc for the measured trade-off. Truncating
        // (not reordering) `all_lanes` keeps the schedule's ordering promise
        // (`tptp_lanes_first_is_plain_tptp` etc.) intact for any prefix count.
        let lane_count = adaptive_lane_count(total_timeout, all_lanes.len());
        let lanes = &all_lanes[..lane_count];
        let selection = opts.selection;

        let (winner, mut result) = drive_portfolio(lanes.len(), total_timeout, |idx, slice| {
            let lane_opts = NativeOpts { strategy: lanes[idx].clone(), ..opts.clone() };
            let cfg = ScaleConfig {
                factor:        scale_factor(),
                max_disproofs: scale_max_disproofs(),
                max_time_runs: scale_max_time_runs(),
                min_budget:    scale_min_budget(),
                total_timeout: slice,
            };
            drive(selection, cfg, Self::remap_native, |params, per_run| {
                self.prove_one_driver(conj, params, per_run, &lane_opts, ctx)
            })
        });

        if crate::prover::scale::is_schedule_final(&result) {
            let lane_name = lanes[winner].name.as_str();
            ctx.debug(format!(
                "portfolio: lane {winner} ({lane_name}) reported the final verdict"
            ));
            result.raw_output =
                format!("portfolio: winning lane = {lane_name}\n{}", result.raw_output);
            if std::env::var_os("SIGMA_STATS").is_some() {
                eprintln!("PORTFOLIO winning lane: {lane_name}");
            }
        } else {
            ctx.debug(format!(
                "portfolio: no conclusive verdict across {} lanes", lanes.len()
            ));
            if std::env::var_os("SIGMA_STATS").is_some() {
                eprintln!("PORTFOLIO exhausted {} lanes", lanes.len());
            }
        }
        result
    }

    pub(super) fn prove_one_driver(
        &self,
        conj:        &Conjecture,
        sine_params: crate::SineParams,
        slice:       u32,
        opts:        &super::prover::NativeOpts,
        ctx:         &crate::ProveCtx,
    ) -> (ProverResult, usize) {
        let session = opts.session.as_deref();
        // Apply the autoscale slice as this attempt's wall-clock budget (the
        // scaled loop hands a per-run slice; a fixed shot passes the full
        // timeout).  `opts` is owned from here — it's moved into `NativeProver`.
        let opts = super::prover::NativeOpts {
            time_limit_secs: if slice > 0 { u64::from(slice) } else { opts.time_limit_secs },
            ..opts.clone()
        };
        let t0 = Instant::now();
        // Conjecture already parsed + interned ONCE by the caller (the
        // autoscale loop reuses it — no per-iteration re-parse, no KIF
        // text round-trip for the TPTP path).
        let conjecture_sents = &conj.sents;

        // Session hypotheses are force-included AND seed SInE alongside the
        // conjecture (mirrors the external path's seeding).
        let session_sids: Vec<SentenceId> = session
            .map(|s| self.semantic.syntactic.sessions.session_sentences(s))
            .unwrap_or_default();

        let mut seed = conj.seed_syms.clone();
        for sid in session_sids.iter() {
            seed.extend(self.semantic.syntactic.sentence_symbols(*sid));
        }

        // Shared relevance pass: SInE → head-filter → Liu rescue.  (`head_filter`
        // is OFF by default for the native prover — measured, it quadrupled
        // TQG52 — but kept as a portfolio axis.)
        let sel = crate::syntactic::SelectionParams {
            head_filter: opts.strategy.head_filter,
            liu_rescue:  opts.strategy.liu_rescue,
            liu_rounds:  opts.strategy.liu_rounds,
            liu_top_k:   opts.strategy.liu_top_k,
        };
        let (mut selected, goal_frontier) = {
            profile_span!(ctx, "ask.sine_select");
            self.semantic.syntactic.select_relevant(&seed, sine_params, &sel, ctx)
        };
        let raw_selected = selected.len();
        if !goal_frontier.is_empty() {
            ctx.debug(format!(
                "structural_include: +{} goal-near axioms SInE missed", goal_frontier.len()));
        }
        // SIGMA_SELECT_GREP=<substr>[,<substr>…]: print which selected roots'
        // KIF contains each substring — "is the proof axiom even in the
        // selection?" diagnostics.
        if let Ok(pats) = std::env::var("SIGMA_SELECT_GREP") {
            for pat in pats.split(',').filter(|p| !p.is_empty()) {
                let n = selected.iter().filter(|sid| {
                    crate::syntactic::display::sentence_to_plain_kif(
                        **sid, &self.semantic.syntactic)
                        .contains(pat)
                }).count();
                eprintln!("SELECT-GREP {pat:?}: {n} of {} selected roots", selected.len());
            }
        }

        // Clausify the negated conjecture.  Normalization may have
        // split it into several roots: they are ONE conjecture, so the
        // negation must wrap their conjunction (negating each root
        // separately would let one refuted conjunct "prove" the whole
        // conjunction).
        let conjecture_clauses: Vec<PClause> = {
            profile_span!(ctx, "ask.clausify_conjecture");
            clausify_negated_conjunction(
                &self.semantic.syntactic, &self.atoms, conjecture_sents)
        };

        // Definitional completion: polarity-aware selection repair.
        // A negative literal's predicate in any selected clause (or the
        // negated conjecture) is a proof OBLIGATION; if no selected
        // clause CONCLUDES that predicate (positive literal), its
        // definers are invisible to resolution and the problem may be
        // unprovable regardless of search effort.  TQG36's shape: the
        // goal's own definition is owned (SInE-wise) by `beforeOrEqual`,
        // which nothing selected provides — two completion rounds pull
        // the `overlapsTemporally` definition, then the `beforeOrEqual`
        // definition it depends on.
        if opts.strategy.def_completion && !sine_params.select_all {
            profile_span!(ctx, "ask.definitional_completion");
            self.definitional_completion(
                &conjecture_clauses, &goal_frontier, &mut selected, &opts.strategy, ctx);
        }
        // Deterministic clause-registration order: `selected` is a HashSet
        // whose RandomState iteration order otherwise seeds the given-clause
        // heap's `seq` tie-breaker — the documented source of run-to-run
        // nondeterminism.  SentenceIds are content hashes, so sorting gives
        // a stable, KB-content-determined order.
        let selected: Vec<SentenceId> = {
            let mut v: Vec<SentenceId> = selected.into_iter().collect();
            v.sort_unstable();
            v
        };

        // Goal-targeted disjointness activation.  The disjointness oracle
        // only discharges inequality / disjoint goals; activating it for a
        // conjecture that makes no use of disjointness (a pure
        // subclass/instance proof) reorders the given-clause search and
        // can lose proofs (CSR176/182).  So, under the SIGMA_DISJOINT_DECOMP
        // opt-in, engage it only when the conjecture's SHAPE needs it — an
        // `equal` literal (a negated `≠` goal, the antonym pattern) or a
        // `disjoint`-relation atom.  `SIGMA_DISJOINT_ALWAYS` forces it on
        // regardless (escape hatch for a proof that needs intermediate
        // disjointness despite its goal shape).  A reset guard keeps the
        // per-prove decision from leaking across proves.
        {
            let goal_needs_disjoint = {
                let syn = &self.semantic.syntactic;
                conjecture_clauses.iter().flat_map(|c| c.lits.iter()).any(|l| {
                    self.atoms.resolve(l.atom, syn).is_some_and(|s| {
                        match s.elements.first() {
                            Some(Element::Op(crate::parse::OpKind::Equal)) => true,
                            Some(Element::Symbol(h)) => {
                                h.name().to_ascii_lowercase().contains("disjoint")
                            }
                            _ => false,
                        }
                    })
                })
            };
            let active = std::env::var_os("SIGMA_DISJOINT_DECOMP").is_some()
                && (goal_needs_disjoint
                    || std::env::var_os("SIGMA_DISJOINT_ALWAYS").is_some());
            crate::semantics::roles::set_disjoint_decomp_override(Some(active));
        }
        struct DisjointOverrideGuard;
        impl Drop for DisjointOverrideGuard {
            fn drop(&mut self) {
                crate::semantics::roles::set_disjoint_decomp_override(None);
            }
        }
        let _disjoint_guard = DisjointOverrideGuard;

        // The oracle reasons in the asking session's scope (Base when
        // none): it must see the session's transient taxonomy/facts but
        // never the conjecture parse (a different tag-session).
        let scope = session
            .map(|s| Scope::Session(session_id(s)))
            .unwrap_or(Scope::Base);

        let input_gen = t0.elapsed();
        let t1 = Instant::now();
        let opts_profile = opts.profile;
        let strict_saturation = opts.strategy.strict_saturation;

        // Frozen-background key: a fingerprint of EVERYTHING that
        // shapes the pre-pass + background load.  The conjecture is
        // included because its ground equalities are registered into
        // the oracle BEFORE background indexing (load-bearing for
        // completeness), so the frozen base is per-(KB, session,
        // selection, conjecture) — the win is repeat queries (serve /
        // SDK loops / retried autoscale budgets), not cross-query
        // sharing.  The whole-KB root fold catches mutations that
        // change oracle-visible facts without changing the selection.
        // Snapshots are disabled alongside the opt-in goal-distance
        // factor: it bakes conjecture-dependent weights into background
        // clauses at FRESH-load time but not at delta-extension time,
        // and mixing the two within one base would be inconsistent.
        // Full saturation also disables snapshots: the frozen base excludes
        // passive-queue state, so a rehydrated background would silently
        // lose its given-candidate entries (and `freeze` asserts the queues
        // are empty).
        let snap_enabled = opts.strategy.bg_snapshot
            && !opts.strategy.goal_dist
            && !opts.strategy.full_saturation;
        let snap_key = {
            use xxhash_rust::xxh64::xxh64;
            let mix = |sid: u64| xxh64(&sid.to_be_bytes(), 0x5AFE_BA5E);
            let roots: u64 = self.semantic.syntactic.root_sids()
                .into_iter().map(mix).fold(0, |a, b| a ^ b);
            let sess: u64 = session_sids.iter().copied().map(mix).fold(0, |a, b| a ^ b);
            let conj: u64 = conjecture_sents.iter().map(|(_, sid)| mix(*sid))
                .fold(0, |a, b| a ^ b);
            let (stag, sid) = match scope {
                Scope::Base => (0u64, 0u64),
                Scope::Session(id) => (1u64, id),
            };
            // NOTE: the SELECTION is deliberately NOT in the key — one
            // snapshot per (KB, session, conjecture, scope, opts) base
            // serves every autoscale slice: narrower slices mask
            // (retain_background), wider ones delta-load.  The strategy
            // fingerprint covers every knob `make` consults while
            // loading background, so portfolio lanes with different
            // generation caps / channels never share a frozen base.
            let words = [roots, sess, conj, stag, sid,
                         opts.max_lits as u64, u64::from(opts.want_proof),
                         opts.strategy.bg_fingerprint()];
            let mut buf = [0u8; 64];
            for (i, w) in words.iter().enumerate() {
                buf[i * 8..(i + 1) * 8].copy_from_slice(&w.to_be_bytes());
            }
            xxh64(&buf, 0xF02E_BA5E)
        };
        // Slice plan against the cached base: identical/narrower
        // selections rehydrate (narrow ones re-derive the retrieval
        // surfaces for their subset); wider ones delta-load the missing
        // roots — UNLESS the delta carries a ground equality, which
        // would have to merge equivalence classes the frozen clauses
        // were normalized under (the same completeness constraint that
        // put the conjecture in the key) → full rebuild.
        enum BgPlan {
            Fresh,
            Hit(std::sync::Arc<crate::saturate::prover::ProverSnapshot>),
            Extend(std::sync::Arc<crate::saturate::prover::ProverSnapshot>, Vec<SentenceId>),
        }
        let plan = if !snap_enabled {
            BgPlan::Fresh
        } else {
            match self.bg_snapshots.get(&snap_key).map(|e| e.value().clone()) {
                None => BgPlan::Fresh,
                Some(snap) => {
                    let delta: Vec<SentenceId> = selected.iter().copied()
                        .filter(|s| !snap.loaded_roots.contains(s))
                        .collect();
                    if delta.is_empty() {
                        BgPlan::Hit(snap)
                    } else if self.delta_blocks_extension(&delta) {
                        BgPlan::Fresh
                    } else {
                        BgPlan::Extend(snap, delta)
                    }
                }
            }
        };

        let recognize_roles = opts.strategy.recognize_roles
            || std::env::var_os("SIGMA_RECOGNIZE_ROLES").is_some();
        let (verdict, steps, proof_kif, raw, phase_profile, contradiction_proofs, conjecture_used, complete_saturation) = {
            let mut prover = match &plan {
                BgPlan::Hit(snap) | BgPlan::Extend(snap, _) => {
                    profile_span!(ctx, "ask.bg_snapshot_hydrate");
                    let mut p = NativeProver::from_snapshot(self, scope, opts, snap);
                    // Narrower (or partially overlapping) slice: rebuild
                    // the retrieval surfaces over exactly the kept roots.
                    let exact = matches!(&plan, BgPlan::Hit(s)
                        if s.loaded_roots.len() == selected.len()
                            && selected.iter().all(|r| s.loaded_roots.contains(r)));
                    if !exact {
                        p.retain_background(&selected.iter().copied().collect());
                    }
                    p
                }
                BgPlan::Fresh => NativeProver::new(self, scope, opts),
            };

            if let BgPlan::Extend(_, delta) = &plan {
                // Delta pre-pass (guard guarantees no ground equalities;
                // FD / schema / subrel registration is additive) + load.
                profile_span!(ctx, "ask.bg_snapshot_extend");
                for sid in delta {
                    let cls = self.clauses_for(*sid);
                    prover.synthesize_subrelation_rules(&cls);
                    prover.mine_fd_relations(&cls, *sid);
                    prover.mine_schema(&cls, *sid);
                }
                for sid in delta {
                    prover.add_background_root(*sid);
                }
                if self.bg_snapshots.len() >= 8 {
                    self.bg_snapshots.clear();
                }
                self.bg_snapshots
                    .insert(snap_key, std::sync::Arc::new(prover.freeze()));
            }

            if !matches!(plan, BgPlan::Fresh) {
                // Per-run goal profile (not part of the frozen base).
                prover.set_goal(&conjecture_clauses);
                for sid in session_sids.iter() {
                    let cls = self.clauses_for(*sid);
                    prover.set_goal(&cls);
                }
            }

            if matches!(plan, BgPlan::Fresh) {
                // Shape-recognize the taxonomy vocabulary BEFORE the
                // pre-pass: the recovered instance/subclass/subrelation
                // ids must be in force before any exhaustive set,
                // disjointness, or `holds` decision reads them.
                if recognize_roles {
                    profile_span!(ctx, "ask.recognize_roles");
                    let roots: Vec<_> =
                        selected.iter().chain(session_sids.iter()).copied().collect();
                    prover.recognize_roles(&roots);
                }
                // Congruence-closure pre-pass: register every input ground
                // equality first, so the complete closure is in place before
                // any clause is made and normalized (order-independent —
                // `Class5-1 = … = Class5-10` collapse to one representative
                // regardless of file order).
                {
                    profile_span!(ctx, "ask.prepass_equalities_subrel");
                    for sid in selected.iter().chain(session_sids.iter()) {
                        let cls = self.clauses_for(*sid);
                        prover.register_equalities(&cls);
                        // Concrete subrelation rules for the relations in play.
                        prover.synthesize_subrelation_rules(&cls);
                        // Functional-dependency axioms feed the oracle's
                        // FD congruence (uniqueness without saturation).
                        prover.mine_fd_relations(&cls, *sid);
                        // Schema channel: rule-stated symmetry /
                        // transitivity / … register their relations before
                        // any clause is made, so orientation and the
                        // closures are active from the first input.
                        prover.mine_schema(&cls, *sid);
                    }
                    prover.register_equalities(&conjecture_clauses);
                    // Goal profile BEFORE any clause is made, so input
                    // axioms get conjecture-distance scores too.  The
                    // profile is conjecture + HYPOTHESES: under set of
                    // support the session facts are part of the question
                    // (TQG36's goal names two intervals; the third — which
                    // the proof's equalities run through — appears only in
                    // the `starts` hypotheses).
                    prover.set_goal(&conjecture_clauses);
                    for sid in session_sids.iter() {
                        let cls = self.clauses_for(*sid);
                        prover.set_goal(&cls);
                    }
                }

                {
                    profile_span!(ctx, "ask.load_background");
                    // Discharge-and-omit: skip the decomposition/`disjoint`
                    // meaning axioms whose semantics the oracle now supplies
                    // (recognized during the pre-pass above) — loading them
                    // would re-introduce the resolution flood the oracle is
                    // meant to replace.  The license comes from the oracle's
                    // `coverage()` claim (same contents as the old
                    // `decomposition_meaning_axioms` list).  Empty unless the
                    // decomposition opt-in is active, so the default path is
                    // unchanged.
                    let omit: HashSet<SentenceId> = prover
                        .oracle
                        .coverage()
                        .omitted_axioms
                        .into_iter()
                        .collect();
                    for sid in &selected {
                        if omit.contains(sid) {
                            continue;
                        }
                        prover.add_background_root(*sid);
                    }
                }
                if snap_enabled {
                    profile_span!(ctx, "ask.bg_snapshot_freeze");
                    // Tiny LRU-free cap: stale keys never hit (any KB /
                    // session / query change reshapes the key), so a
                    // periodic full clear is enough to bound memory.
                    if self.bg_snapshots.len() >= 8 {
                        self.bg_snapshots.clear();
                    }
                    self.bg_snapshots
                        .insert(snap_key, std::sync::Arc::new(prover.freeze()));
                }
            }
            {
                profile_span!(ctx, "ask.load_support_conjecture");
                for sid in &session_sids {
                    prover.add_support_root(*sid);
                }
                prover.add_conjecture_clauses(&conjecture_clauses);
            }

            if prover.opts.strategy.bg_completion {
                profile_span!(ctx, "ask.bg_completion");
                prover.complete_background();
            }
            if prover.opts.forward_close {
                profile_span!(ctx, "ask.forward_close");
                prover.forward_close();
            }
            let (verdict, steps) = {
                profile_span!(ctx, "ask.saturate");
                prover.run()
            };
            // Vacuity is decided on the RAW derivation DAG (a DFS over
            // clause parents), never on rendered steps — so rendering
            // can be skipped wholesale when no one will display it.
            let conjecture_used = match verdict {
                RunVerdict::Refutation(empty) => prover.conjecture_rooted(empty),
                _ => false,
            };
            // Proof-DAG discharge-rule reach (SIGMA_STATS instrumentation
            // only): at refutation, count how many clauses in the FOUND
            // proof actually came from a model/oracle discharge mechanism —
            // cheap (a DFS over already-built clause parents, no proof
            // rendering), so always computed at refutation regardless of
            // `want_proof`.
            if let RunVerdict::Refutation(empty) = verdict {
                let tags = crate::saturate::proof::count_proof_tags(&prover, empty);
                prover.stats.proof_tag_model += tags.model;
                prover.stats.proof_tag_model_join += tags.model_join;
                prover.stats.proof_tag_join += tags.join;
                prover.stats.proof_tag_event_calculus += tags.event_calculus;
                prover.stats.proof_tag_oracle += tags.oracle;
            }
            // A saturation is COMPLETE only if no capacity cap dropped
            // a clause along the way (input or derived).  Under strict
            // saturation (the TPTP problem path) the bar is refutation-
            // completeness itself: additionally require full saturation
            // (no set-of-support tiering — axiom×axiom inference ran)
            // over the WHOLE theory (SInE didn't drop axioms), no
            // generation cap hit, and a complete equality calculus
            // (superposition + eq_factoring, every indexed equation
            // orientable) whenever the problem contains equality.
            let complete_saturation = match verdict {
                RunVerdict::Saturated => {
                    let no_drops = prover.stats.discarded_long == 0
                        && prover.stats.discarded_deep == 0;
                    Some(no_drops && (!strict_saturation || {
                        let st = &prover.opts.strategy;
                        let eq_ok = !prover.stats.saw_equality
                            || (st.superposition
                                && st.eq_factoring
                                && prover.stats.unorientable_eqs == 0);
                        let whole_theory = selected.len()
                            >= self.semantic.syntactic.root_sids().len();
                        st.full_saturation
                            && eq_ok
                            && prover.stats.gen_capped == 0
                            && whole_theory
                    }))
                }
                _ => None,
            };
            let proof = match verdict {
                RunVerdict::Refutation(empty) if prover.opts.want_proof => {
                    profile_span!(ctx, "ask.extract_proof");
                    crate::saturate::proof::extract_proof(&prover, empty)
                }
                _ => Vec::new(),
            };
            if std::env::var_os("SIGMA_FLOOD_DUMP").is_some() {
                eprintln!("flood histogram (top 25):\n{}", prover.flood_histogram(25));
            }
            // Surface suppressed input contradictions as transcripts,
            // deduped by the set of source axioms they implicate.
            let mut contradiction_proofs: Vec<Vec<crate::prover::proof::KifProofStep>> = Vec::new();
            let mut seen_culprits: HashSet<Vec<crate::SentenceId>> = HashSet::new();
            for &cid in &prover.input_contradiction_ids {
                let steps = crate::saturate::proof::extract_proof(&prover, cid);
                let mut culprits: Vec<crate::SentenceId> =
                    steps.iter().filter_map(|s| s.source_sid).collect();
                culprits.sort_unstable();
                culprits.dedup();
                if seen_culprits.insert(culprits) {
                    contradiction_proofs.push(steps);
                }
            }
            let mut raw = format!(
                "native: {:?} after {} given-clause steps; {} clauses, {} resolvents \
                 ({} decoded), {} oracle discharges, {} unit subsumed, {} clause subsumed, \
                 {} demodulated, {} forward-closed, {} bg-completed{}\n\
                 verify-profile: resolve {}/{} ({} ground-unit partner), \
                 fc-join {}/{} ({} ground cand), open-match {}/{} ({} prefiltered), \
                 factor {}/{} ({} prefiltered)\n\
                 schema: {} hits, {} absorbed, {} sym-oriented, {} sym-resolutions, \
                 mined {} sym / {} trans / {} other\n\
                 model-discharge: {} atoms seen, {} rejected (lit_pattern), \
                 {} arg collapsed (compound), {} arg collapsed (repeated-var), \
                 {} answered, {} unanswered, bails: {} unsafe / {} unstratifiable / \
                 {} budget-or-deadline-overflow / {} undefined-relation, \
                 {} model_literals_deleted\n\
                 model-complete: {} certified relations, {} negatives emitted, \
                 blocked: {} skipped-head / {} unstratifiable / {} body-chain / {} role\n\
                 demod-probe: {} rewrite attempts, {} rewrites applied, {} dup hits, \
                 {} scans_performed, {} scans_skipped_by_prefilter\n\
                 proof-DAG reach: {} model, {} model_join, {} rule_join, \
                 {} event_calculus, {} oracle\n\
                 guide: {} guided_clauses_scored, {} guide_disabled_bail",
                verdict, steps, prover.clauses.len(), prover.stats.resolvents,
                prover.stats.decoded_resolutions,
                prover.stats.oracle_discharges, prover.stats.unit_subsumed,
                prover.stats.subsumed,
                prover.stats.demod_rewrites,
                prover.stats.forward_closed,
                prover.stats.bg_completed,
                if prover.stats.input_contradictions > 0 {
                    format!(
                        "; WARNING: {} input contradiction(s) suppressed (the \
                         axioms/hypotheses are mutually inconsistent — verdicts \
                         are relative to the conjecture-relevant fragment)",
                        prover.stats.input_contradictions)
                } else {
                    String::new()
                },
                prover.stats.resolve_unify_hits, prover.stats.resolve_unify_attempts,
                prover.stats.resolve_ground_partner,
                prover.stats.fc_unify_hits, prover.stats.fc_unify_attempts,
                prover.stats.fc_ground_candidate,
                prover.stats.open_match_hits, prover.stats.open_match_attempts,
                prover.stats.open_match_prefiltered,
                prover.stats.factor_hits, prover.stats.factor_attempts,
                prover.stats.factor_prefiltered,
                prover.stats.schema_hits, prover.stats.schema_absorbed,
                prover.stats.sym_oriented, prover.stats.sym_resolutions,
                prover.stats.mined_symmetric, prover.stats.mined_transitive,
                prover.stats.mined_other,
                prover.stats.model_atoms_seen, prover.stats.model_atoms_rejected,
                prover.stats.model_arg_collapsed_compound,
                prover.stats.model_arg_collapsed_repeated_var,
                prover.stats.model_atoms_answered, prover.stats.model_atoms_unanswered,
                prover.stats.model_unsafe_bails, prover.stats.model_unstratifiable_bails,
                prover.stats.model_budget_or_deadline_overflows,
                prover.stats.model_undefined_relation,
                prover.stats.model_literals_deleted,
                prover.stats.model_certified_relations,
                prover.stats.model_complete_negatives_emitted,
                prover.stats.model_cert_blocked_skipped_head,
                prover.stats.model_cert_blocked_unstratifiable,
                prover.stats.model_cert_blocked_body_chain,
                prover.stats.model_cert_blocked_role,
                prover.stats.demod_rewrite_attempts, prover.stats.demod_rewrites_applied,
                prover.stats.demod_dup_hits,
                prover.stats.demod_scans_performed,
                prover.stats.demod_scans_skipped_by_prefilter,
                prover.stats.proof_tag_model, prover.stats.proof_tag_model_join,
                prover.stats.proof_tag_join, prover.stats.proof_tag_event_calculus,
                prover.stats.proof_tag_oracle,
                prover.stats.guided_clauses_scored, prover.stats.guide_disabled_bail);
            // Backward-demodulation line only when the knob is on: the
            // default-path SIGMA_STATS output stays byte-identical.
            if prover.opts.strategy.bwd_demod {
                raw.push_str(&format!(
                    "\nbwd-demod: {} triggered, {} clauses_rewritten, {} retired, \
                     {} cap_hits",
                    prover.stats.bwd_demod_triggered,
                    prover.stats.bwd_demod_clauses_rewritten,
                    prover.stats.bwd_demod_retired,
                    prover.stats.bwd_demod_cap_hits));
            }
            // Rigid-conflict (EGD inconsistency) line only when one occurred:
            // default-path SIGMA_STATS output stays byte-identical.
            if prover.stats.model_rigid_conflicts > 0 {
                raw.push_str(&format!(
                    "\nmodel-egd: {} rigid_conflicts (evaluation aborted Inconsistent)",
                    prover.stats.model_rigid_conflicts));
            }
            // Subsumption feature-vector prefilter line only when
            // subsumption is on (KIF default has it off): default-path
            // SIGMA_STATS output stays byte-identical.
            if prover.opts.strategy.subsumption {
                raw.push_str(&format!(
                    "\nsubs-fvi: {} checks_attempted, {} rejected_by_fv, {} full_checks",
                    prover.stats.subs_checks_attempted,
                    prover.stats.subs_rejected_by_fv,
                    prover.stats.subs_full_checks));
            }
            if std::env::var_os("SIGMA_STATS").is_some() {
                eprintln!("{raw}");
            }
            let mechanisms: Vec<(String, std::time::Duration)> = if opts_profile {
                [
                    ("saturate.resimplify", prover.stats.t_resimplify),
                    ("saturate.factor", prover.stats.t_factors),
                    ("saturate.eq_resolve", prover.stats.t_eq_resolve),
                    ("saturate.paramodulate", prover.stats.t_paramod),
                    ("saturate.resolve", prover.stats.t_resolve),
                ]
                .into_iter()
                .map(|(n, d)| (n.to_string(), d))
                .collect()
            } else {
                Vec::new()
            };
            (verdict, steps, proof, raw, mechanisms, contradiction_proofs, conjecture_used, complete_saturation)
        };
        let prover_run = t1.elapsed();


        // Status mapping.  A refutation whose proof never touches the
        // negated conjecture means the selected axioms alone derive ⊥ —
        // vacuous → Inconsistent (kb/prove.rs's rule).  Saturation:
        // under the legacy KIF path (strict off), no refutation from
        // this support set → report Disproved with the Saturation
        // marker (same shape as Vampire's CounterSatisfiable mapping;
        // the strategy's caps make this a strong signal, not a
        // certificate).  Under strict saturation (the TPTP path),
        // Disproved is a CERTIFICATE: it requires the run to have been
        // genuinely refutation-complete (`complete_saturation`), else
        // the honest verdict is Unknown — still `Saturation`-flagged so
        // the autoscale loop widens rather than giving up.
        let (status, termination) = match verdict {
            RunVerdict::Refutation(_) => {
                if conjecture_used {
                    (ProverStatus::Proved, None)
                } else {
                    (ProverStatus::Inconsistent, None)
                }
            }
            RunVerdict::Saturated if strict_saturation && complete_saturation != Some(true) =>
                (ProverStatus::Unknown, Some(TerminationReason::Saturation)),
            RunVerdict::Saturated =>
                (ProverStatus::Disproved, Some(TerminationReason::Saturation)),
            RunVerdict::StepsExhausted =>
                (ProverStatus::Unknown, Some(TerminationReason::GaveUp)),
            RunVerdict::TimedOut =>
                (ProverStatus::Timeout, Some(TerminationReason::TimeLimit)),
        };

        let mut result = ProverResult {
            status,
            termination,
            complete_saturation,
            given_steps: Some(steps),
            raw_output: raw,
            proof_kif,
            phase_profile,
            contradiction_proofs,
            ..Default::default()
        };
        result.timings.input_gen = input_gen;
        result.timings.prover_run = prover_run;
        (result, raw_selected)
    }

    /// `true` when extending a frozen background with `delta` roots is UNSAFE:
    /// a delta clause asserts a ground equality whose class merge the
    /// already-frozen clauses were not normalized under.
    fn delta_blocks_extension(&self, delta: &[SentenceId]) -> bool {
        use crate::parse::OpKind;
        delta.iter().any(|sid| {
            self.clauses_for(*sid).iter().any(|pc| {
                pc.nvars == 0 && pc.lits.len() == 1 && pc.lits[0].pos
                    && self.atoms
                        .resolve(pc.lits[0].atom, &self.semantic.syntactic)
                        .is_some_and(|sent| matches!(
                            sent.elements.first(), Some(Element::Op(OpKind::Equal))))
            })
        })
    }

    /// Polarity-aware definitional completion of a selection.  A predicate in a
    /// NEGATIVE literal of the GOAL LINE (negated conjecture + `frontier`) is a
    /// proof obligation; if nothing selected CONCLUDES it (positive literal),
    /// its providers are pulled in (rarest first), to a fixpoint or the caps.
    /// `provided` is computed over the WHOLE selection — an obligation counts as
    /// missing only if nothing anywhere concludes it.
    fn definitional_completion(
        &self,
        conjecture: &[PClause],
        frontier:   &[SentenceId],
        selected:   &mut HashSet<SentenceId>,
        strategy:   &Strategy,
        ctx:        &crate::ProveCtx,
    ) {
        let max_rounds = strategy.defcomp_rounds;
        let max_adds = strategy.defcomp_max_adds;
        /// Predicates more general than this are hubs — always provided.
        const OCC_CAP: usize = 1500;
        let per_sym = strategy.defcomp_per_sym;

        let syn = &self.semantic.syntactic;
        let trace = std::env::var_os("SIGMA_LIU_TRACE").is_some();
        let mut adds = 0usize;

        // Predicate head of one canonical literal's atom.
        let lit_head = |atom: AtomId| -> Option<SymbolId> {
            let sent = self.atoms.resolve(atom, syn)?;
            match sent.elements.first()? {
                Element::Symbol(h) => Some(h.id()),
                _ => None,
            }
        };

        let mut frontier: Vec<SentenceId> = frontier.to_vec();
        for _ in 0..max_rounds {
            // Obligations: negative-literal predicates of the GOAL LINE.
            let mut required: HashSet<SymbolId> = HashSet::new();
            for pc in conjecture {
                for l in &pc.lits {
                    if !l.pos {
                        if let Some(h) = lit_head(l.atom) { required.insert(h); }
                    }
                }
            }
            for sid in &frontier {
                for pc in self.clauses_for(*sid).iter() {
                    for l in &pc.lits {
                        if !l.pos {
                            if let Some(h) = lit_head(l.atom) { required.insert(h); }
                        }
                    }
                }
            }
            // Providers: positive-literal predicates ANYWHERE selected.
            let mut provided: HashSet<SymbolId> = HashSet::new();
            for pc in conjecture {
                for l in &pc.lits {
                    if l.pos {
                        if let Some(h) = lit_head(l.atom) { provided.insert(h); }
                    }
                }
            }
            for sid in selected.iter() {
                for pc in self.clauses_for(*sid).iter() {
                    for l in &pc.lits {
                        if l.pos {
                            if let Some(h) = lit_head(l.atom) { provided.insert(h); }
                        }
                    }
                }
            }

            // Missing obligations, rarest first (rare = definitional), with
            // each symbol's provider candidates — one index read for the whole
            // round instead of two lock round-trips per symbol.
            let (missing, candidates_by_sym): (Vec<(usize, SymbolId)>, Vec<Vec<SentenceId>>) =
                self.semantic.syntactic.sine_current(|idx| {
                    let mut missing: Vec<(usize, SymbolId)> = required
                        .difference(&provided)
                        .filter_map(|&s| {
                            let occ = idx.generality(s);
                            (occ > 0 && occ <= OCC_CAP).then_some((occ, s))
                        })
                        .collect();
                    missing.sort_unstable();
                    let candidates = missing
                        .iter()
                        .map(|&(_, p)| {
                            idx.axioms_of_symbol(p).iter().map(|&(_, aid)| aid).collect()
                        })
                        .collect();
                    (missing, candidates)
                });
            if missing.is_empty() || adds >= max_adds { break; }

            let mut round_adds = 0usize;
            let mut next_frontier: Vec<SentenceId> = Vec::new();
            for ((_, p), candidates) in missing.into_iter().zip(candidates_by_sym) {
                if adds >= max_adds { break; }
                let mut pulled = 0usize;
                for aid in candidates {
                    if pulled >= per_sym || adds >= max_adds { break; }
                    if selected.contains(&aid) { continue; }
                    // A provider must CONCLUDE p: a clause with a positive
                    // p-headed literal.
                    let provides = self.clauses_for(aid).iter().any(|pc| {
                        pc.lits.iter().any(|l| l.pos && lit_head(l.atom) == Some(p))
                    });
                    if !provides { continue; }
                    if trace {
                        eprintln!(
                            "COMPLETION: {} <- {}",
                            syn.sym_name(p).map(|s| s.name().to_string()).unwrap_or_default(),
                            crate::syntactic::display::sentence_to_plain_kif(aid, syn));
                    }
                    selected.insert(aid);
                    next_frontier.push(aid);
                    adds += 1;
                    round_adds += 1;
                    pulled += 1;
                }
            }
            if round_adds == 0 {
                break; // obligations remain but nothing in the KB provides them
            }
            frontier = next_frontier;
        }
        if adds > 0 {
            ctx.debug(format!("definitional_completion: +{adds} provider axioms"));
        }
    }
}