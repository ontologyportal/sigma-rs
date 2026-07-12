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
use super::clausify::clausify_negated_conjunction_lossy;
use super::prover::{NativeOpts, NativeProver, RunVerdict};
use super::strategy::Strategy;
use super::theory::TheoryOracle;

/// The one `NativeOpts` field a portfolio lane's `Strategy` can override:
/// `max_lits` (the derived-clause literal-count ceiling) lives on
/// `NativeOpts`, not `Strategy`, because it doubles as the historical
/// KIF/SUMO-path default rather than a pure search-shaping knob (see
/// `NativeOpts::max_lits`'s doc). `Strategy::derived_width_cap` is the
/// portfolio-only escape hatch: `None` (every lane except `tptp-wide`)
/// returns `base_max_lits` unchanged — byte-identical to before this knob
/// existed; `Some(n)` overrides it for that lane only. Consumed exactly
/// once, here, at lane-build time in `run_portfolio_schedule`.
pub(super) fn lane_max_lits(lane: &Strategy, base_max_lits: usize) -> usize {
    lane.derived_width_cap.map(usize::from).unwrap_or(base_max_lits)
}

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
        let (normalized, norm_dropped) = Conjecture::normalize(asts);
        let seed_syms  = Conjecture::seed(&normalized);
        let sents      = self.intern_conjecture_native(&normalized);
        if sents.is_empty() {
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: "No query sentence parsed".into(),
                ..Default::default()
            };
        }
        // Intern/build failures surface as a shortfall (`intern_conjecture_native`
        // yields at most one entry per normalized ast, skipping failures).
        let dropped = norm_dropped + normalized.len().saturating_sub(sents.len());
        let conj = Conjecture { sents, seed_syms, dropped };

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
        use crate::prover::scale::{adaptive_start_budget, drive, effective_max_time_runs, ScaleConfig};
        use crate::syntactic::sine::{
            default_budget, scale_factor, scale_max_disproofs, scale_max_time_runs,
            scale_min_budget,
        };
        // Don't climb from a budget the indexed universe could never fill —
        // see `adaptive_start_budget`'s doc.
        let total_axioms = self.semantic.syntactic.sine_current(|idx| idx.axiom_count());
        let min_budget = scale_min_budget();
        let cfg = ScaleConfig {
            factor:        scale_factor(),
            max_disproofs: scale_max_disproofs(),
            // Don't starve the one attempt that matters of most of its
            // slice when there's no budget-search left to do — see
            // `effective_max_time_runs`'s doc.
            max_time_runs: effective_max_time_runs(
                scale_max_time_runs(), total_axioms, min_budget),
            min_budget,
            total_timeout,
        };
        let requested     = selection.auto_budget.unwrap_or_else(default_budget);
        let selection = crate::SineParams {
            auto_budget: Some(adaptive_start_budget(requested, total_axioms, &cfg)),
            ..selection
        };
        drive(selection, cfg, Self::remap_native, |params, slice| {
            self.prove_one_driver(&conj, params, slice, &opts, ctx)
        })
    }

    /// Native step-exhaustion (`GaveUp`) narrows like a timeout, not widens —
    /// the search space was too big, the wrong gradient for the planner to
    /// read as prover-incompleteness. Remapped to `ResourceOut`, NOT
    /// `TimeLimit`: `classify` narrows on `(Unknown, ResourceOut)`, while
    /// `(Unknown, TimeLimit)` falls to its catch-all and would stop the loop
    /// without the Narrow retry. Shared by the plain autoscale loop
    /// above and each lane of [`run_portfolio_schedule`] (mirrors the trait
    /// `ProvingLayer::remap` override for `ProverLayer`).
    fn remap_native(
        _status: ProverStatus,
        term:    Option<TerminationReason>,
    ) -> Option<TerminationReason> {
        match term {
            Some(TerminationReason::GaveUp) => Some(TerminationReason::ResourceOut),
            other => other,
        }
    }

    /// CASC-style strategy schedule for a standalone TPTP problem: race
    /// [`Strategy::tptp_lanes`](super::strategy::Strategy::tptp_lanes)
    /// against each other. Every lane still runs the ordinary
    /// budget-autoscaling `drive` loop internally — full saturation means
    /// that loop mostly just re-confirms the same search, but it costs
    /// nothing to leave it wired in (a lane that somehow doesn't reach the
    /// ceiling on its first shot still benefits). A verdict of
    /// Proved/Inconsistent, or a CONFIDENT Disproved/Consistent, from any
    /// lane ends the schedule immediately; otherwise the best-ranked
    /// (never worse-than-first) result across every lane is returned. The
    /// winning lane's name is prepended to `raw_output` so
    /// `SIGMA_STATS`/verbose output can show which lane solved it.
    ///
    /// Two dispatch paths, chosen by `opts.cores`:
    ///
    /// - **`cores > 1` and more than one lane** (the common case):
    ///   [`crate::prover::scale::drive_portfolio_parallel`] races every
    ///   lane concurrently, each given the FULL `total_timeout` (they
    ///   aren't sharing one wall-clock window, so there's no reason to
    ///   shrink the lane count for a tight budget the way the sequential
    ///   path has to) — the first lane to return a schedule-final verdict
    ///   raises a shared cancel flag every other in-flight lane's
    ///   saturation loop polls, so losers stop promptly instead of running
    ///   to their own deadline.
    /// - **`cores <= 1`, or only one lane configured**: the original
    ///   sequential [`crate::prover::scale::drive_portfolio`] carry-forward
    ///   schedule, unchanged — the fallback for a single-core caller (or
    ///   any future lane that can't safely run concurrently and should be
    ///   raced this way instead).
    pub(super) fn run_portfolio_schedule(
        &self,
        conj:          &Conjecture,
        total_timeout: u32,
        opts:          &NativeOpts,
        ctx:           &crate::ProveCtx,
    ) -> ProverResult {
        use crate::prover::scale::{
            adaptive_lane_count, adaptive_start_budget, drive, drive_portfolio,
            drive_portfolio_parallel, effective_max_time_runs, ScaleConfig,
        };
        use crate::syntactic::sine::{
            default_budget, scale_factor, scale_max_disproofs, scale_max_time_runs,
            scale_min_budget,
        };
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        let all_lanes: Vec<Strategy> = Strategy::tptp_lanes();
        let cores = opts.cores.max(1);
        // Parallel lanes don't divide `total_timeout` (each gets the whole
        // thing), so the "racing more lanes starves each one" concern
        // `adaptive_lane_count` exists for (task #33) only applies to the
        // sequential fallback — race every configured lane when running
        // concurrently.
        let use_parallel = cores > 1 && all_lanes.len() > 1;
        let lane_count = if use_parallel {
            all_lanes.len()
        } else {
            adaptive_lane_count(total_timeout, all_lanes.len())
        };
        // Truncating (not reordering) `all_lanes` keeps the schedule's
        // ordering promise (`tptp_lanes_first_is_plain_tptp` etc.) intact.
        let lanes = &all_lanes[..lane_count];

        // Same "don't climb from a budget the indexed universe could never
        // fill" adjustment as the plain (non-portfolio) path above — every
        // lane shares one selection start point, so it's computed once here
        // rather than per lane.
        let total_axioms = self.semantic.syntactic.sine_current(|idx| idx.axiom_count());
        let probe_cfg = ScaleConfig {
            factor: scale_factor(), max_disproofs: 0, max_time_runs: 0,
            min_budget: scale_min_budget(), total_timeout: 0,
        };
        let requested = opts.selection.auto_budget.unwrap_or_else(default_budget);
        let selection = crate::SineParams {
            auto_budget: Some(adaptive_start_budget(requested, total_axioms, &probe_cfg)),
            ..opts.selection
        };
        let min_budget = probe_cfg.min_budget;
        // Same reasoning as `effective_max_time_runs`'s doc: when there's no
        // budget-search left to do, a lane's one attempt that matters should
        // get its FULL slice, not `slice / max_time_runs` of it — otherwise
        // a hard problem needing real search time (not axiom tuning) times
        // out on a fraction of its budget for no reason. Bites hardest in
        // the parallel path, where a lane that gives up early has no
        // sequential carry-forward to fall back on.
        let max_time_runs = effective_max_time_runs(
            scale_max_time_runs(), total_axioms, min_budget);

        // Shared by both dispatch paths: run lane `idx` for `slice` seconds
        // (`0` = unbounded), with `lane_cancel` wired into that lane's own
        // `NativeOpts` (only the parallel path hands this a live flag).
        let build_lane_result = |idx: usize, slice: u32, lane_cancel: Option<Arc<AtomicBool>>| {
            let lane_opts = NativeOpts {
                strategy: lanes[idx].clone(),
                max_lits: lane_max_lits(&lanes[idx], opts.max_lits),
                cancel:   lane_cancel,
                ..opts.clone()
            };
            let cfg = ScaleConfig {
                factor: scale_factor(), max_disproofs: scale_max_disproofs(),
                max_time_runs, min_budget,
                total_timeout: slice,
            };
            drive(selection, cfg, Self::remap_native, |params, per_run| {
                self.prove_one_driver(conj, params, per_run, &lane_opts, ctx)
            })
        };

        let (winner, mut result) = if use_parallel {
            let cancel = Arc::new(AtomicBool::new(false));
            let workers = cores.min(lane_count);
            drive_portfolio_parallel(lane_count, workers, &cancel, |idx| {
                build_lane_result(idx, total_timeout, Some(cancel.clone()))
            })
        } else {
            drive_portfolio(lane_count, total_timeout, |idx, slice| {
                build_lane_result(idx, slice, None)
            })
        };

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
        // SIGMA_SELECT_DUMP=<path>: write the canonical KIF of every
        // selected root (one per line) — the "did SInE keep the proof
        // axioms?" diagnostic.  Conjecture roots are tagged `# conj`.
        if let Ok(path) = std::env::var("SIGMA_SELECT_DUMP") {
            use std::io::Write;
            if let Ok(f) = std::fs::File::create(&path) {
                let mut w = std::io::BufWriter::new(f);
                for sid in &selected {
                    let kif = crate::syntactic::display::sentence_to_plain_kif(
                        *sid, &self.semantic.syntactic);
                    let _ = writeln!(w, "{kif}");
                }
                for (_, sid) in conjecture_sents {
                    let kif = crate::syntactic::display::sentence_to_plain_kif(
                        *sid, &self.semantic.syntactic);
                    let _ = writeln!(w, "# conj\t{kif}");
                }
            }
        }
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
        // conjunction).  `conj_lossy` records goal clauses dropped for
        // shape/capacity reasons — input loss the completeness gate must see.
        let (conjecture_clauses, conj_lossy): (Vec<PClause>, bool) = {
            profile_span!(ctx, "ask.clausify_conjecture");
            clausify_negated_conjunction_lossy(
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

        // Input-completeness gate (defense in depth): count input formulas
        // that FAILED to make it into the clause set — conjecture roots
        // dropped at normalize/intern (`conj.dropped`), goal clauses lost in
        // clausification (`conj_lossy`), and selected/support roots that
        // clausified to nothing for a shape/capacity reason
        // (`root_load_failed`).  Any such loss makes a later "Saturated"
        // verdict meaningless as a countermodel certificate: the missing
        // formula could be the very one that closes the refutation.  The
        // count feeds `complete_saturation` below, which under strict
        // saturation (the TPTP path) demotes Disproved/Satisfiable to
        // Unknown/GaveUp — silent drops become verdict-poisoning by
        // construction.
        let failed_roots: usize = selected.iter()
            .chain(session_sids.iter())
            .filter(|sid| self.root_load_failed(**sid))
            .count();
        let input_load_failures =
            conj.dropped + usize::from(conj_lossy) + failed_roots;

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

        // Modal K-distribution injection (native HO-parity, part A): which
        // attitude relations qualify for THIS problem.  Decided here (needs
        // the final sorted selection + the scope); the clauses themselves
        // load after snapshot freeze/rehydrate below, so frozen background
        // bases never contain them.
        let modal_k_rels: Vec<&'static str> = if opts.strategy.modal_k {
            self.modal_k_qualifying(scope, &selected, &seed)
        } else {
            Vec::new()
        };

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

        let recognize_roles = opts.strategy.recognize_roles;
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
            // SIGMA_HINTS=<file>: Veroff-style watchlist replay.  The
            // file holds reference-proof clauses each wrapped `~(...)`,
            // so the standard negated-conjunction clausifier returns
            // the POSITIVE clause (double negation) — same parser, same
            // canonicalization, hence keys that match derived clauses
            // exactly.  Interning is content-addressed and tag-free, so
            // hint sentences leave no semantic residue in the store.
            if let Some(hp) = std::env::var_os("SIGMA_HINTS") {
                if let Ok(text) = std::fs::read_to_string(&hp) {
                    // Parse directly (not parse_document: its per-root
                    // fingerprinting rejects Annotated statements, which
                    // TPTP items are until stripped below).  Same options
                    // as the ".p" problem loader so hint clauses take the
                    // identical parse path as problem clauses.
                    let parser = crate::parse::Parser::Tptp {
                        options: Some(crate::parse::TptpParseOptions {
                            formulas_only: false,
                            keep_conjectures: false,
                            ..Default::default()
                        }),
                    };
                    let (items, _errs) = parser.parse(&text, "hints");
                    for item in items {
                        let Some(ast) = item.as_stmt().cloned() else { continue };
                        // TPTP items arrive annotation-wrapped; the
                        // downstream pipeline expects bare formulas.
                        let ast = ast.strip_annotation();
                        let (normalized, _) = Conjecture::normalize(vec![ast]);
                        let sents = self.intern_conjecture_native(&normalized);
                        if sents.is_empty() { continue; }
                        let (cls, _) = clausify_negated_conjunction_lossy(
                            &self.semantic.syntactic, &self.atoms, &sents);
                        for c in cls {
                            if std::env::var_os("SIGMA_HINTS_DEBUG").is_some() {
                                eprintln!("HINT {:016x} {:?}", c.key.0, c.lits);
                            }
                            prover.hints.insert(c.key);
                        }
                    }
                    eprintln!("hints: {} watchlist keys loaded", prover.hints.len());
                }
            }

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
                // Part-A injection: K-distribution schemata for the
                // qualifying attitude relations.  GUARDRAIL (Montague):
                // these axioms only REARRANGE quoted structure — ?P/?Q
                // stay in argument position under the same attitude
                // relation; nothing is ever unquoted, so no general
                // truth/unquote bridge over quotes is introduced here.
                // See `clausify::modal_k_clauses`.
                for rel in &modal_k_rels {
                    let cls = super::clausify::modal_k_clauses(rel, &self.atoms);
                    prover.add_injected_clauses(&cls, "modal_k");
                }
                for sid in &session_sids {
                    prover.add_support_root(*sid);
                }
                prover.add_conjecture_clauses(
                    &conjecture_clauses,
                    conjecture_sents.first().map(|(_, sid)| *sid),
                );
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
                    // `slot_lift_failures`: a stored clause that never made
                    // it into the run (root still reads as loaded).
                    // `input_contradictions`: a suppressed axiom-only empty
                    // clause — the theory is UNSAT, so a saturation over
                    // the SOS remainder is meaningless as a countermodel.
                    let no_drops = prover.stats.discarded_long == 0
                        && prover.stats.discarded_deep == 0
                        && prover.stats.slot_lift_failures == 0
                        && prover.stats.input_contradictions == 0
                        && input_load_failures == 0;
                    Some(no_drops && (!strict_saturation || {
                        let st = &prover.opts.strategy;
                        let eq_ok = !prover.stats.saw_equality
                            || (st.superposition
                                && st.eq_factoring
                                && prover.stats.unorientable_eqs == 0);
                        let whole_theory = selected.len()
                            >= self.semantic.syntactic.root_sids().len();
                        // Schema absorption replaces the absorbed axiom at
                        // inference level ONLY for binary resolution (the
                        // swap retry in `resolve`); factoring and the unit
                        // open-match channel have no symmetric handling, so
                        // a saturation after any absorption is not
                        // refutation-complete and must not certify.
                        st.full_saturation
                            && eq_ok
                            && prover.stats.gen_capped == 0
                            && prover.stats.schema_absorbed == 0
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
            if std::env::var_os("SIGMA_WIDTH_DUMP").is_some() {
                eprintln!("width histogram:\n{}", prover.width_histogram());
            }
            if std::env::var_os("SIGMA_FLOOD_DUMP").is_some() {
                eprintln!("flood histogram (top 25):\n{}", prover.flood_histogram(25));
            }
            if let Some(path) = std::env::var_os("SIGMA_GATE0_DUMP") {
                if let Err(e) = prover.gate0_dump(std::path::Path::new(&path)) {
                    eprintln!("gate0 dump failed: {e}");
                }
            }
            if std::env::var_os("SIGMA_STATS").is_some() {
                if let Some(sa) = &prover.arena {
                    eprintln!("{}", sa.report());
                }
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
            // Input-completeness gate: say LOUDLY when input formulas never
            // made it into the clause set — the line every consumer of a
            // withheld Satisfiable/Disproved verdict needs to see.
            if input_load_failures > 0 {
                // Truthful per mode: strict withholds the verdict outright;
                // the legacy KIF path still REPORTS Disproved (a heuristic
                // signal, not a certificate — see the verdict mapping), so
                // the warning must not claim it was withheld there.
                raw.push_str(&format!(
                    "\nWARNING: {input_load_failures} input formula(s) failed to load \
                     (conjecture roots dropped: {}, goal clausification lossy: {}, \
                     background/support roots failed: {}) — {}",
                    conj.dropped, conj_lossy, failed_roots,
                    if strict_saturation {
                        "Satisfiable/countermodel verdicts withheld (GaveUp)"
                    } else {
                        "Saturated verdicts are heuristic (loaded theory incomplete)"
                    }));
            }
            // Ground-term identity line (NF memo + subtree bloom prune)
            // only when demod is on: the default-path SIGMA_STATS output
            // stays byte-identical with demod off.
            if prover.opts.strategy.demod {
                raw.push_str(&format!(
                    "\nnf-memo: {} probes, {} hits_unchanged, {} hits_rewritten, \
                     {} misses, {} stale_discards; bloom: {} subtrees_pruned",
                    prover.stats.nf_probes,
                    prover.stats.nf_hits_unchanged,
                    prover.stats.nf_hits_rewritten,
                    prover.stats.nf_misses,
                    prover.stats.nf_stale_discards,
                    prover.stats.bloom_subtrees_pruned));
            }
            // Backward-demodulation line only when the knob is on: the
            // default-path SIGMA_STATS output stays byte-identical.
            if prover.opts.strategy.bwd_demod {
                raw.push_str(&format!(
                    "\nbwd-demod: {} triggered, {} clauses_rewritten, {} retired, \
                     {} cap_hits, {} term_rewrites; postings: {} queries, \
                     {} hits, {} bucket_scanned, {} compactions",
                    prover.stats.bwd_demod_triggered,
                    prover.stats.bwd_demod_clauses_rewritten,
                    prover.stats.bwd_demod_retired,
                    prover.stats.bwd_demod_cap_hits,
                    prover.stats.bwd_demod_term_rewrites,
                    prover.stats.bwd_postings_queries,
                    prover.stats.bwd_postings_hits,
                    prover.stats.bwd_bucket_scanned,
                    prover.stats.bwd_postings_compactions));
                // Phase-2 decode chain, its OWN line, printed ONLY when
                // `Strategy.subterm_rows` is on (the same convention as
                // the subs-ej sub-line under subs_join): off (the
                // default) the chain never runs, so the line is
                // suppressed and the default-path SIGMA_STATS output
                // stays byte-identical to a frozen build modulo this
                // dropped line.  A pure counter line — byte-identity
                // diffs drop it; the bwd-demod line above stays
                // byte-identical across the phase.
                if prover.opts.strategy.subterm_rows {
                    raw.push_str(&format!(
                        "\nbwd-decode: {} swept, {} rej_surplus, {} rej_probe, \
                         {} rej_binding, {} fallbacks, {} trivial, {} verify_calls",
                        prover.stats.bwd_decode_swept,
                        prover.stats.bwd_decode_rej_surplus,
                        prover.stats.bwd_decode_rej_probe,
                        prover.stats.bwd_decode_rej_binding,
                        prover.stats.bwd_decode_fallbacks,
                        prover.stats.bwd_decode_trivial,
                        prover.stats.bwd_verify_calls));
                }
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
                    "\nsubs-fvi: {} checks_attempted, {} rejected_by_bloom_leaf, \
                     {} rejected_by_bloom_glit ({} glit_applicable), \
                     {} rejected_by_fv, {} rejected_by_keq ({} keq_pair_tests), \
                     {} full_checks",
                    prover.stats.subs_checks_attempted,
                    prover.stats.subs_rejected_by_bloom_leaf,
                    prover.stats.subs_rejected_by_bloom_glit,
                    prover.stats.subs_glit_applicable,
                    prover.stats.subs_rejected_by_fv,
                    prover.stats.subs_rejected_by_keq,
                    prover.stats.keq_pair_tests,
                    prover.stats.subs_full_checks));
                // Phase-2b equality-join channel, its OWN line (a pure
                // counter line — byte-identity diffs drop it; the
                // subs-fvi line above keeps its exact format, with only
                // `full_checks` legitimately moving when the channel
                // diverts rejections ahead of the exact check).
                if prover.opts.strategy.subs_join {
                    raw.push_str(&format!(
                        "\nsubs-ej: {} candidates, {} pairs_decoded, \
                         {} rej_no_partner, {} rej_join, {} skipped_unusable, \
                         {} full_checks_saved",
                        prover.stats.ej_candidates,
                        prover.stats.ej_pairs_decoded,
                        prover.stats.ej_rej_no_partner,
                        prover.stats.ej_rej_join,
                        prover.stats.ej_skipped_unusable,
                        prover.stats.ej_full_checks_saved));
                }
            }
            // Deferred-passive discipline line only when the knob is
            // on: the default-path SIGMA_STATS output stays
            // byte-identical.
            if !prover.hints.is_empty() {
                raw.push_str(&format!(
                    "\nhints: {}/{} covered, {} boosts",
                    prover.hint_matched.len(),
                    prover.hints.len(),
                    prover.stats.hint_boosts,
                ));
            }
            if prover.opts.strategy.split_naming {
                raw.push_str(&format!(
                    "\nsplit: {} rescued, {} pieces, bails {} connected / {} fat / {} selector, {} unit guards",
                    prover.stats.split_rescued,
                    prover.stats.split_pieces,
                    prover.stats.split_bail_connected,
                    prover.stats.split_bail_fat,
                    prover.stats.split_bail_selector,
                    prover.stats.split_guard_units,
                ));
            }
            if prover.opts.strategy.deferred_passive {
                let s = &prover.stats;
                raw.push_str(&format!(
                    "\ndeferred: {} recipes_queued, {} prequeue_deduped, \
                     {} materialized, {} act_dedup_hits, {} act_subsumed, \
                     {} act_rejected_other, {} act_over_cap, \
                     {} cap_fallbacks; weight-drift: \
                     {}/{} exact, avg |drift| {}",
                    s.recipes_queued,
                    s.recipes_prequeue_deduped,
                    s.recipes_materialized,
                    s.act_dedup_hits,
                    s.act_subsumed,
                    s.act_rejected_other,
                    s.act_over_cap,
                    s.deferred_cap_fallbacks,
                    s.composed_weight_exact,
                    s.composed_weight_samples,
                    s.composed_weight_drift_sum / s.composed_weight_samples.max(1)));
            }
            // Decode fast-path cause profile (Step-2; always printed —
            // the ONE new line in a SIGMA_STATS capture diff.  All-zero
            // when `Strategy.decode` is off).
            raw.push_str(&format!(
                "\ndecode: {} attempts, {} bindings_extracted, bails: {} nested_var / \
                 {} too_many_open / {} partner_shape / {} phonebook_or_collision / {} other",
                prover.stats.decode_attempts,
                prover.stats.decode_bindings_extracted,
                prover.stats.decode_bail_nested_var,
                prover.stats.decode_bail_too_many_open,
                prover.stats.decode_bail_partner_shape,
                prover.stats.decode_bail_phonebook_or_collision,
                prover.stats.decode_bail_other));
            // Definitional-CNF rescue line only when a rescue actually
            // ran (process-cumulative — clausification lives inside
            // cache generation, which has no per-run stats handle):
            // default-path SIGMA_STATS output stays byte-identical on
            // problems that clausify losslessly.
            {
                let (defcnf_defs, defcnf_roots) = super::clausify::defcnf_counters();
                if defcnf_roots > 0 {
                    raw.push_str(&format!(
                        "\ndefcnf: {defcnf_defs} definitions_introduced, \
                         {defcnf_roots} roots_rescued"));
                }
            }
            // KappaFn comprehension line only when a kappa term was
            // actually met (process-cumulative, like defcnf above):
            // kappa-free problems keep byte-identical SIGMA_STATS output.
            {
                let (kappa_comp, kappa_bails) = super::clausify::kappa_counters();
                if kappa_comp > 0 || kappa_bails > 0 {
                    raw.push_str(&format!(
                        "\nkappa: {kappa_comp} comprehensions_emitted, \
                         {kappa_bails} malformed_bails"));
                }
            }
            // Verified-dedup collision line only when one actually
            // occurred (expected ~never): default-path SIGMA_STATS
            // output stays byte-identical.
            if prover.stats.dedup_collisions_detected > 0 {
                raw.push_str(&format!(
                    "\ndedup: {} true ClauseKey collision(s) detected — colliding \
                     clauses accepted, never dropped",
                    prover.stats.dedup_collisions_detected));
            }
            if std::env::var_os("SIGMA_STATS").is_some() {
                eprintln!("{raw}");
            }
            let mechanisms: Vec<(String, std::time::Duration)> = if opts_profile {
                [
                    ("saturate.select", prover.stats.t_select),
                    ("saturate.resimplify", prover.stats.t_resimplify),
                    ("saturate.factor", prover.stats.t_factors),
                    ("saturate.eq_resolve", prover.stats.t_eq_resolve),
                    ("saturate.paramodulate", prover.stats.t_paramod),
                    ("saturate.resolve", prover.stats.t_resolve),
                    ("saturate.activate", prover.stats.t_activate),
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


        // Status mapping — the shared ladder (`map_verdict`).  A
        // refutation whose proof never touches the negated conjecture
        // means the selected axioms alone derive ⊥ — vacuous →
        // Inconsistent (kb/prove.rs's rule).  Saturation: under the
        // legacy KIF path (strict off), no refutation from this support
        // set → report Disproved with the Saturation marker (same shape
        // as Vampire's CounterSatisfiable mapping; the strategy's caps
        // make this a strong signal, not a certificate).  Under strict
        // saturation (the TPTP path), Disproved is a CERTIFICATE: it
        // requires the run to have been genuinely refutation-complete
        // (`complete_saturation`), else the honest verdict is Unknown —
        // still `Saturation`-flagged so the autoscale loop widens rather
        // than giving up.
        let (status, termination) = super::prover::map_verdict(
            verdict, conjecture_used, strict_saturation, complete_saturation,
            super::prover::VerdictMode::Ask);

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

    /// Part-A injection guard: the attitude relations qualifying for
    /// modal K-distribution on THIS problem — exactly `knows`/`believes`
    /// (THF-lane parity scope), and only when
    ///
    ///   (a) the KB's taxonomy DECLARES the relation's argument-2 domain
    ///       as `Formula` (or a Formula descendant) — the computed-
    ///       declaration guard the THF chip's `ho_signatures` cache
    ///       encodes but the THF assembler's injection skips (a filed
    ///       bug: it keys on the declared NAME only); here the domain
    ///       check is enforced from day one — and
    ///   (b) the relation actually occurs in the selected axioms /
    ///       conjecture / session seed, so unrelated problems carry no
    ///       dead weight.
    ///
    /// `selected` must be sorted (binary search).
    fn modal_k_qualifying(
        &self,
        scope:    Scope,
        selected: &[SentenceId],
        seed:     &HashSet<SymbolId>,
    ) -> Vec<&'static str> {
        use crate::semantics::types::RelationDomain;
        let syn = &self.semantic.syntactic;
        let Some(formula) = syn.sym_id("Formula") else { return Vec::new() };
        let mut out = Vec::new();
        for rel in ["knows", "believes"] {
            let Some(sym) = syn.sym_id(rel) else { continue };
            // (a) declared Formula domain at argument 2.
            let arg2_is_formula = matches!(
                self.semantic.domain_scoped(sym, scope).get(1),
                Some(RelationDomain::Domain(cls))
                    if *cls == formula
                        || self.semantic.has_ancestor_scoped(*cls, formula, scope));
            if !arg2_is_formula {
                continue;
            }
            // (b) occurrence in the problem.
            let occurs = seed.contains(&sym)
                || syn.sine_current(|idx| {
                    idx.axioms_of_symbol(sym)
                        .iter()
                        .any(|&(_, aid)| selected.binary_search(&aid).is_ok())
                });
            if occurs {
                out.push(rel);
            }
        }
        out
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