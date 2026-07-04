// crates/core/src/saturate/model/mod.rs
//
// The ontology model-builder.
//
// This is the generic engine the bespoke oracles (taxonomy closure, Horn
// rule-join, inertial event calculus) are special cases of: a runtime,
// semi-naive evaluator for **stratified Datalog with negation** over tuples of
// `SymbolId`.  Given a logic program (rules + EDB ground facts) extracted from
// the axioms, it computes the program's perfect model — the materialized
// relations — which the prover consults (`discharge_models`/
// `discharge_model_joins`) to decide ground literals and retrieve entailed
// background units.
//
// See docs/model-builder-implementation.md for the full plan.  The event
// calculus is no longer a parity cross-check: `narrative_to_program` +
// `Program::evaluate` is the SOLE evaluation path `discharge_event_calculus`
// (in `prover/discharge.rs`) uses — the bespoke `eventcalc::simulate`
// forward-simulator it once validated against has been retired.
// `ec_kernel_holds_grid` (below) is the golden-grid regression that replaced
// that parity test.

use std::collections::{HashMap, HashSet};

use smallvec::SmallVec;

use crate::types::{SentenceId, SymbolId};

pub(crate) mod cluster;
pub(crate) mod extract;
pub(crate) mod magic;
pub(crate) mod recognize;
pub(crate) mod seminaive;

use crate::syntactic::SyntacticLayer;

/// The Level-1 model program (Phase 5): the cheap, stable structure derived
/// from the whole KB — extracted Horn rules + instantiated role schemas,
/// partitioned into stratifiable clusters, plus the monotone (negation-free)
/// fragment.  Cached for the KB's life and rebuilt on edit; **no model is
/// evaluated here** (materialization is the demand-driven Level-2 step).
#[derive(Debug, Clone)]
pub(crate) struct ModelProgram {
    /// Extracted Horn rules + role-schema rules for DIRECTLY-declared roles.
    pub program:  Program,
    /// Stratifiable definitional clusters (negation cycles isolated).
    pub clusters: Vec<cluster::Cluster>,
    /// The negation-free fragment — a sound positive model for every predicate,
    /// the home for heavily-shared relations (taxonomy) that SCC tainting drops.
    pub monotone: Program,
    /// Predicates eligible for NEGATIVE/complete decisions.  Slice-1 cut: those
    /// living in a stratifiable cluster.  Kept with its historical semantics
    /// (stratifiability only — condition (b) of certification); the full
    /// definitional-completeness gate it was waiting for is [`certified`]
    /// below, which refines this set.
    pub complete: HashSet<Pred>,
    /// COMPLETION-CERTIFIED relations (the Clark-completion gate): relations
    /// whose extracted rules are PROVABLY their only definition in the KB —
    /// (a) no skipped root could derive their atoms, (b) they live in a
    /// stratifiable cluster, (c) their rule bodies reach only certified /
    /// EDB-closed relations (fixpoint), (d) they are not oracle-owned
    /// taxonomy role relations nor reified as terms elsewhere (a derived
    /// role membership could enrich their definition at Level 2).  For a
    /// certified `R`, model-ABSENCE of a ground tuple is a sound negative
    /// answer under Clark completion — see [`complete_absent`](Self::complete_absent).
    pub certified: HashSet<Pred>,
    /// Build-time breakdown of why candidate relations were refused
    /// certification (SIGMA_STATS `certification_blocked_by`).
    pub cert_blocked: CertBlocked,
    /// Recognized role symbols (dialect-agnostic) — for the Level-2 derivation
    /// of the inherited transitive/symmetric set over the evaluated model.
    pub roles:    crate::semantics::roles::TaxonomyRoles,
    /// Extracted denial constraints (disjointness declarations flattened to
    /// pairwise ⊥-rules) — the integrity constraints [`refutes`](Self::refutes)
    /// chases.  Empty on a KB with no disjointness.
    pub denials:  Vec<extract::Denial>,
}

impl ModelProgram {
    /// Build the Level-1 program from the syntactic store.  Cheap (extraction +
    /// partition only — no evaluation); safe to compute eagerly and cache.
    ///
    /// SOUNDNESS: schemas are instantiated ONLY for relations the KB's own
    /// axioms *directly declare* (`(instance R TransitiveRelation)`,
    /// `(subrelation R S)`).  Nothing is seeded by convention — a relation like
    /// `subclass` becomes transitive only if the KB entails it (declared, or
    /// inherited through the relation-class hierarchy, which is derived at
    /// materialization).  So the program never asserts a fact a self-contained
    /// problem doesn't entail.
    pub(crate) fn build(syn: &SyntacticLayer) -> Self {
        use crate::semantics::roles::TaxonomyRoles;

        let ex = extract::extract_horn_program_full(syn);
        let mut program = ex.program;
        let roles = TaxonomyRoles::recognize(syn, syn.root_sids());
        // NOTE: clause-signature recognition (`recognize`) is validated as a
        // dialect-robust role recognizer, but using its bridges to *override*
        // the sentence-recognized roles here was net-negative on the CSR sweep
        // (it picked a wrong bridge — `element`/`subset` — when instance/
        // subclass aren't a first-order bridge, regressing CSR176+1 for zero
        // gain).  The right home for clause-sig + reification handling is
        // Milestone A (OpenCyc recognition), not a blind override.
        let decls = extract::collect_role_decls(syn, &roles);
        // Directly-declared TRANSITIVE relations become BUILT-IN closures
        // (task #32 Part 2): no transitivity schema rule is emitted — the
        // kernel resolves their literals by on-demand BFS, so the dense
        // closure is never materialized.  Subrelation/symmetric schemas
        // stay ordinary rules.
        for r in extract::schema_rules(&extract::RoleDecls {
            subrelation: decls.subrelation.clone(),
            transitive:  Vec::new(),
            symmetric:   decls.symmetric.clone(),
        }, &[]) {
            program.rules.push(r);
        }
        for &(r, sid) in &decls.transitive {
            program.builtin_transitive.insert(r, sid);
        }
        // EGDs (task #32 Part 1) + the rigid-symbol set + the instance
        // relation their guards probe.
        program.egds = extract::collect_egds(syn, &roles);
        program.instance_pred = Some(roles.instance);
        program.rigid = extract::collect_rigid(&program, syn);

        let clusters = cluster::partition(&program);
        let monotone = cluster::positive_program(&program);
        let mut complete: HashSet<Pred> =
            clusters.iter().flat_map(|c| c.preds.iter().copied()).collect();
        // A builtin-transitive relation's stored extension is the UN-CLOSED
        // base — model-absence says nothing about the closure, so it is
        // never complete/certified (and, via the body fixpoint in `certify`,
        // neither is anything defined over it).
        for r in program.builtin_transitive.keys() {
            complete.remove(r);
        }
        let denials = extract::collect_denials(syn, &roles);

        // The Clark-completion certification (conditions (a)–(d); see the
        // `certified` field doc and `certify` itself).  Role relations are
        // the oracle's Complete coverage — never double-owned here.
        let role_syms: HashSet<Pred> = [
            roles.instance, roles.subclass, roles.subrelation, roles.transitive,
            roles.symmetric, roles.domain, roles.range, roles.disjoint,
            roles.partition,
        ]
        .into_iter()
        .collect();
        let builtin: HashSet<Pred> = program.builtin_transitive.keys().copied().collect();
        let (certified, cert_blocked) =
            certify(&program, &complete, &ex.skipped_heads, ex.wildcard_skip, &role_syms, &builtin);

        ModelProgram {
            program, clusters, monotone, complete, certified, cert_blocked, roles, denials,
        }
    }

    /// The sound positive model: the monotone fragment evaluated, then closed
    /// under **derived** transitivity — relations the KB makes transitive
    /// (`(R, TransitiveRelation) ∈ instance-closure`, covering direct and
    /// hierarchy-inherited declarations) get their transitivity rule and the
    /// model is re-evaluated to a fixpoint.  No conventional seeding, so every
    /// emitted fact is entailed by the KB's own axioms.
    pub(crate) fn positive_model(&self) -> Option<(Model, Provenance)> {
        // Materialization budget — bail (→ resolution) rather than blow up on a
        // large un-scoped KB.  Demand scoping (SInE, slice 4) is the real fix;
        // this keeps slice 2 from regressing problems resolution already solves.
        const BUDGET: usize = 250_000;
        let mut work = self.monotone.clone();
        let mut known: HashSet<Pred> = work.builtin_transitive.keys().copied().collect();
        let (mut model, mut prov) = work.evaluate_within(BUDGET, None).ok()?;
        loop {
            let trans = extract::transitive_members(&model, &self.roles);
            let fresh: Vec<Pred> = trans.into_iter().filter(|r| known.insert(*r)).collect();
            if fresh.is_empty() {
                break;
            }
            // Each newly-transitive relation becomes a BUILT-IN closure
            // (no schema rule — task #32 Part 2), citing the
            // `(R, TransitiveRelation)` membership's own source when the
            // provenance reaches it (the direct declaration, or the first
            // leaf of the hierarchy chain that entailed it); `None` when it
            // doesn't.  NOTE: the returned Model materializes only the BASE
            // edges of builtin relations (positive-only consumers — the
            // guide model, the join's full-model union — under-enumerate
            // soundly; the demand-scoped `answer` path resolves closures).
            for &r in &fresh {
                let membership = vec![r, self.roles.transitive];
                let sids = prov.cite(self.roles.instance, &membership);
                work.builtin_transitive.insert(r, sids.first().copied());
            }
            (model, prov) = work.evaluate_within(BUDGET, None).ok()?;
        }
        Some((model, prov))
    }

    /// Demand-scoped positive model (Phase 5, slice 4): materialize only the
    /// dependency cone of `goal` (the conjecture's relations) — plus the
    /// taxonomy role relations, so transitivity can still be DERIVED — instead
    /// of the whole monotone program.  This is what makes a real OpenCyc/SUMO
    /// include tractable: the ~3800-rule program shrinks to the handful of
    /// rules the query actually needs.  Sound: the cone is the exact dependency
    /// closure, so the scoped model agrees with the full model on `goal`.
    /// Answer one conjecture atom `rel(args)` (slice 4b): scope to the goal's
    /// dependency cone, magic-set-rewrite on the goal's *constants*, evaluate
    /// the demanded slice, and return the matching ground tuples.  This is what
    /// makes a dense relation (OpenCyc `genls`) affordable — derivation is
    /// restricted to the facts reachable from the conjecture's constants, not
    /// the whole relation.  Budgeted; `None` ⇒ bail to resolution.
    ///
    /// Thin wrapper over [`answer_stats`](Self::answer_stats) that discards
    /// the bail-reason breakdown and the provenance — unchanged
    /// signature/behavior for existing callers.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn answer(
        &self,
        rel:      Pred,
        args:     &[DTerm],
        deadline: Option<std::time::Instant>,
    ) -> Option<Vec<Tuple>> {
        let mut stats = ModelStats::default();
        self.answer_stats(rel, args, deadline, &mut stats).map(|(rows, _)| rows)
    }

    /// As [`answer`](Self::answer), but records WHY a bail happened (or that
    /// an answer was produced) into `stats`, and returns the evaluation's
    /// [`Provenance`] alongside the rows so the caller can [`cite`](Self::cite)
    /// each answer.  The provenance is per-evaluation state (rule indices
    /// refer to the magic-rewritten cone evaluated here) — it is returned by
    /// value and must NOT be cached on the KB-lifetime registry.
    pub(crate) fn answer_stats(
        &self,
        rel:      Pred,
        args:     &[DTerm],
        deadline: Option<std::time::Instant>,
        stats:    &mut ModelStats,
    ) -> Option<(Vec<Tuple>, Provenance)> {
        // Positive-path policy: an unsafe rule in the cone FAILS FAST
        // (`ModelError::Unsafe`), the long-standing behavior — on full SUMO
        // the `instance` cone always contains a few, and burning the whole
        // per-prove deadline evaluating a cone that then overflows anyway
        // would tax every SIGMA_MODEL prove for nothing.  The denial chase
        // (`refutes`) opts into the sound unsafe-rule drop instead.
        self.answer_stats_impl(rel, args, deadline, stats, false)
    }

    /// [`answer_stats`] with the unsafe-rule policy explicit — see there.
    ///
    /// TWO-TIER evaluation (task #32): the FULL dependency cone is tried
    /// first (complete for the demanded slice); if it bails on budget /
    /// deadline / an unsafe rule, the query retries on the SHALLOW cone —
    /// only rules HEADED by the goal relation, every body relation reduced
    /// to its stored facts (plus built-in closure resolution).  Sound: a
    /// rule subset only shrinks the least model, so every positive answer
    /// stays entailed; and magic demand provably cannot cascade (depth-1
    /// body relations have no rules, hence no further magic).  This is what
    /// makes a full-SUMO ground `instance`-membership query affordable: the
    /// full instance cone's DEMAND explodes through pair-multiplying typing
    /// rules (independent of transitivity), while the shallow cone is the
    /// taxonomy bridge + one layer of typing rules over the store.  The
    /// caller's deadline is split between the two attempts.
    fn answer_stats_impl(
        &self,
        rel:         Pred,
        args:        &[DTerm],
        deadline:    Option<std::time::Instant>,
        stats:       &mut ModelStats,
        drop_unsafe: bool,
    ) -> Option<(Vec<Tuple>, Provenance)> {
        let first_deadline = deadline.map(|d| {
            let now = std::time::Instant::now();
            let half = (d.saturating_duration_since(now)) / 2;
            now + half
        });
        match self.answer_cone_impl(rel, args, first_deadline, stats, drop_unsafe, false) {
            Some(ans) => Some(ans),
            None if stats.retry_eligible => {
                stats.shallow_retries += 1;
                self.answer_cone_impl(rel, args, deadline, stats, drop_unsafe, true)
            }
            None => None,
        }
    }

    /// One cone evaluation — `shallow` selects the goal-headed rule subset
    /// (see [`answer_stats_impl`]).  Sets `stats.retry_eligible` when the
    /// bail reason is one a shallow retry could get past (overflow /
    /// deadline / unsafe rule).
    #[allow(clippy::too_many_arguments)]
    fn answer_cone_impl(
        &self,
        rel:         Pred,
        args:        &[DTerm],
        deadline:    Option<std::time::Instant>,
        stats:       &mut ModelStats,
        drop_unsafe: bool,
        shallow:     bool,
    ) -> Option<(Vec<Tuple>, Provenance)> {
        stats.retry_eligible = false;
        // Per-evaluation tuple budget.  `SIGMA_MODEL_BUDGET` overrides the
        // default (diagnosis / experimentation on dense KBs — full SUMO's
        // `instance` cone materializes past the default and bails; the real
        // fix is built-in transitive closure, which stops materializing the
        // dense closure altogether).  Gated path only.
        let budget: usize = std::env::var("SIGMA_MODEL_BUDGET")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(250_000);
        // Magic restricts *facts*, but the naive evaluator still processes
        // every rule in the cone each round.  When the cone is the whole
        // program — OpenCyc `genls` depends transitively on ~everything, so its
        // cone is ~3800 rules — that is too slow regardless of magic.  Bail to
        // resolution on a non-selective cone.  (The dense-KB unlock needs an
        // indexed/semi-naive evaluator, Phase 6; magic pays off on selective
        // cones now.)
        // With the indexed semi-naive engine the evaluator scales; these caps
        // are now just a safety net against a pathologically large cone (the
        // per-eval tuple BUDGET is the real bound).
        const MAX_CONE_RULES: usize = 50_000;
        const MAX_CONE_FACTS: usize = 200_000;

        let mut goal = HashSet::new();
        goal.insert(rel);
        let cone = if shallow {
            // Goal-headed rules only; their body relations join as EDB.
            let mut c: HashSet<Pred> = goal.clone();
            for r in self.monotone.rules.iter().filter(|r| r.head.pred == rel) {
                for l in &r.body {
                    c.insert(l.atom.pred);
                }
            }
            c
        } else {
            cluster::dependency_cone(&self.monotone, &goal)
        };
        let mut scoped = cluster::scope_program(&self.monotone, &cone);
        if shallow {
            // `scope_program` keeps every rule whose head is in the pred
            // set; the shallow tier keeps only the GOAL-headed ones (body
            // relations are extensional here).
            scoped.rules.retain(|r| r.head.pred == rel);
        }
        // Denial-chase policy (`drop_unsafe`): drop UNSAFE rules (a head /
        // negated variable unbound by any positive body literal) from the
        // cone instead of letting one such rule poison the whole evaluation
        // (`validate_safe` fails the entire program — on full SUMO the
        // `instance` cone always contains a few).  Sound for the monotone
        // fragment: removing a rule only SHRINKS the least model, so
        // positive answers stay entailed and `refutes` only under-refutes,
        // never over-refutes.
        if drop_unsafe {
            let n_rules = scoped.rules.len();
            scoped.rules.retain(rule_is_safe);
            stats.unsafe_rules_dropped += (n_rules - scoped.rules.len()) as u32;
        }
        let cone_facts: usize = scoped.edb.values().map(|s| s.len()).sum();
        if scoped.rules.len() > MAX_CONE_RULES || cone_facts > MAX_CONE_FACTS {
            stats.budget_overflows += 1;
            stats.retry_eligible = !shallow;
            return None;
        }
        let rewritten = magic::magic_rewrite(&scoped, rel, args);
        let (model, mut prov) = match rewritten.evaluate_within(budget, deadline) {
            Ok(mp) => mp,
            Err(ModelError::Unsafe) => {
                stats.unsafe_bails += 1;
                // The unsafe rule may sit elsewhere in the full cone — the
                // shallow (goal-headed) subset can still be evaluable.
                stats.retry_eligible = !shallow;
                return None;
            }
            Err(ModelError::Unstratifiable) => {
                stats.unstratifiable_bails += 1;
                return None;
            }
            Err(ModelError::Overflow) => {
                // Deadline vs tuple-budget overflow share one variant (see
                // `model/seminaive.rs`'s `over_deadline` bail sites) — counted
                // together here rather than threading a second return
                // channel through `evaluate_within` for this instrumentation
                // pass.
                stats.budget_overflows += 1;
                stats.retry_eligible = !shallow;
                return None;
            }
            Err(ModelError::Inconsistent(_)) => {
                // An EGD union of two rigid symbols: the demanded slice is
                // inconsistent — bail soundly (a future refutation channel).
                stats.rigid_conflicts += 1;
                return None;
            }
        };
        let Some(rows) = model.get(&rel) else {
            stats.undefined_relation += 1;
            return None;
        };
        // Canonicalize the goal's constants through the evaluation's
        // equality classes: a goal about `a` where the model proved things
        // about rep `b` must still answer (the emitted rows RESTORE the
        // original constants below; `cite` re-canonicalizes and cites the
        // equality chain).
        let cargs: Vec<DTerm> = if prov.eq.merged() {
            args.iter()
                .map(|a| match a {
                    DTerm::Const(c) => DTerm::Const(prov.eq.find(*c)),
                    v => v.clone(),
                })
                .collect()
        } else {
            args.to_vec()
        };
        // A BUILTIN-transitive goal relation materializes only its base
        // edges — resolve the closure on demand over the returned rows
        // (bound-side BFS; a both-free/repeated-variable pattern falls back
        // to the base rows, the documented under-enumeration).
        let mut ans: Vec<Tuple> = match builtin_goal_answers(
            rows,
            &cargs,
            rel,
            &mut prov,
            scoped.builtin_transitive.contains_key(&rel),
            budget,
        ) {
            Ok(Some(a)) => a,
            Ok(None) => {
                // Tuples matching the conjecture's bound (constant) positions
                // AND consistent at repeated-variable positions: the same goal
                // variable in two seats requires equal values there (goal
                // `p(X, X)` must not match tuple `(a, b)`).  Constants and
                // variables share one binding check via `unify` — an
                // over-approximating wildcard here would be unsound the moment
                // answers feed a NEGATIVE decision.
                rows.iter()
                    .filter(|row| {
                        let mut binding: HashMap<u32, SymbolId> = HashMap::new();
                        unify(&cargs, row, &mut binding).is_some()
                    })
                    .cloned()
                    .collect()
            }
            Err(()) => {
                stats.budget_overflows += 1;
                stats.retry_eligible = !shallow;
                return None;
            }
        };
        // Emitted rows keep the ORIGINAL goal constants at bound positions.
        if prov.eq.merged() {
            for row in ans.iter_mut() {
                for (i, a) in args.iter().enumerate() {
                    if let DTerm::Const(orig) = a {
                        if i < row.len() {
                            row[i] = *orig;
                        }
                    }
                }
            }
        }
        stats.budget_used = stats.budget_used.saturating_add(prov.budget_used as u64);
        stats.answered += 1;
        Some((ans, prov))
    }

    /// Reconstruct the KB citation for a model fact from per-evaluation
    /// provenance — the sentence ids (EDB leaves first, then rules) its
    /// derivation used.  See [`Provenance::cite`].
    pub(crate) fn cite(&self, prov: &Provenance, pred: Pred, t: &Tuple) -> Vec<SentenceId> {
        prov.cite(pred, t)
    }

    /// The equality-class representative of `s` under one evaluation's
    /// equality state.  Per-evaluation, like [`Provenance`] itself — never
    /// cached on the registry.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn eq_rep(&self, prov: &Provenance, s: SymbolId) -> SymbolId {
        prov.eq.find(s)
    }

    /// The KB citation for an equality `a = b` entailed by one evaluation's
    /// EGD merges: each justification edge's witness derivations + EGD axiom
    /// sid.  Empty when the evaluation never merged them.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn eq_explain(&self, prov: &Provenance, a: SymbolId, b: SymbolId) -> Vec<SentenceId> {
        prov.explain_eq(a, b)
    }

    /// Denial-constraint refutation of one ground `instance`-shaped atom
    /// (sub-milestone B): `(instance x C)` is REFUTED when the model entails
    /// `(instance x D)` for some `D` forming a denial pair with `C` or one of
    /// `C`'s ancestors in the model's subclass closure.
    ///
    /// SOUNDNESS (open-world): `KB ⊨ ¬A` iff `KB ∪ {A}` is inconsistent; for
    /// the Horn+denial fragment that inconsistency is exactly a chase hit —
    /// a derived membership meeting a disjointness declaration.  Denials are
    /// integrity constraints, not closed-world assumptions, so no Clark
    /// completion is involved.  Both closure queries run through the same
    /// budgeted, magic-scoped cone machinery as [`answer`](Self::answer)
    /// (demand-seeded on `x` and on `C` respectively); a budget/deadline
    /// bail ANYWHERE returns `None` — a refutation is only reported when the
    /// tuple's class chains materialized fully inside the model's cone.
    ///
    /// The membership set is checked directly against the denial pairs: with
    /// the KB's own instance/subclass bridge rule in the cone the set is
    /// upward-closed, which subsumes climbing the member-side chain (the
    /// oracle's ancestor×ancestor walk).  Without a bridge rule the model is
    /// simply weaker — it under-refutes, never over-refutes.
    ///
    /// Returns the refutation with its KB citation chain: the instance
    /// derivation (EDB leaves first, then rules — [`Provenance::cite`]), the
    /// goal-side subclass chain, and the denial declaration LAST.
    pub(crate) fn refutes(
        &self,
        rel:      Pred,
        tuple:    &[SymbolId],
        deadline: Option<std::time::Instant>,
        stats:    &mut ModelStats,
    ) -> Option<ModelRefutation> {
        if self.denials.is_empty() || rel != self.roles.instance || tuple.len() != 2 {
            return None;
        }
        let (x, c) = (tuple[0], tuple[1]);
        let norm = |a: SymbolId, b: SymbolId| if a <= b { (a, b) } else { (b, a) };
        let pairs: HashMap<(SymbolId, SymbolId), SentenceId> =
            self.denials.iter().map(|d| (d.classes, d.sid)).collect();

        // The model's instance closure of x (magic-scoped on x).  `Some` ⇒
        // the demanded cone materialized fully within budget.  Unsafe rules
        // are dropped from the cone (the sound under-approximation) rather
        // than failing the evaluation — see `answer_stats_impl`.
        let (inst_rows, prov_i) = self.answer_stats_impl(
            self.roles.instance,
            &[DTerm::Const(x), DTerm::Var(0)],
            deadline,
            stats,
            true,
        )?;
        if inst_rows.is_empty() {
            return None;
        }

        // Ancestors of the GOAL class C in the model's subclass closure
        // (magic-scoped on C).  An undefined subclass relation means "no
        // chains" (anc = {C}); a bail on a DEFINED one aborts the refutation.
        let sub_defined = self.monotone.edb.contains_key(&self.roles.subclass)
            || self.monotone.rules.iter().any(|r| r.head.pred == self.roles.subclass);
        let mut anc_c: Vec<SymbolId> = vec![c];
        let mut prov_c: Option<Provenance> = None;
        if sub_defined {
            let (rows, prov) = self.answer_stats_impl(
                self.roles.subclass,
                &[DTerm::Const(c), DTerm::Var(0)],
                deadline,
                stats,
                true,
            )?;
            anc_c.extend(rows.iter().filter(|r| r.len() == 2).map(|r| r[1]));
            prov_c = Some(prov);
        }

        // Chase: a model-entailed membership meets a denial pair.
        for row in &inst_rows {
            if row.len() != 2 {
                continue;
            }
            let d = row[1];
            for &a_c in &anc_c {
                let Some(&decl) = pairs.get(&norm(d, a_c)) else { continue };
                // Citation: instance-derivation chain (leaves, then rules) …
                let mut cited = prov_i.cite(self.roles.instance, &vec![x, d]);
                // … the goal-side subclass chain C ⊑ … ⊑ a_c …
                if a_c != c {
                    if let Some(pc) = prov_c.as_ref() {
                        cited.extend(pc.cite(self.roles.subclass, &vec![c, a_c]));
                    }
                }
                // … and the denial declaration LAST (the referee).
                cited.push(decl);
                let mut seen: HashSet<SentenceId> = HashSet::new();
                cited.retain(|s| seen.insert(*s));
                return Some(ModelRefutation { member: d, goal_ancestor: a_c, cited });
            }
        }
        None
    }

    /// Clark-completion NEGATIVE decision for one ground atom of a
    /// [`certified`](Self::certified) relation: evaluate `rel`'s dependency
    /// cone in the FULL program (negation included — the monotone fragment
    /// under-derives, which is sound for positives and a lie for absence)
    /// and, when the evaluation completes with NO bail and the tuple is
    /// ABSENT, return the completion citation — every defining rule sid in
    /// the cone (the axioms whose provable exhaustiveness licenses the
    /// closed-world step; the absence itself has no sentence to cite).
    ///
    /// `None` when: `rel` is not certified, the tuple is present, or ANY
    /// budget / deadline / safety / stratification bail occurred — an
    /// incomplete evaluation licenses nothing.  No magic rewrite (magic
    /// narrows derivations toward the goal's constants: sound for positive
    /// answers, unsound for absence) and no unsafe-rule dropping (dropping
    /// shrinks the model, also unsound for absence).
    pub(crate) fn complete_absent(
        &self,
        rel:      Pred,
        tuple:    &[SymbolId],
        deadline: Option<std::time::Instant>,
        stats:    &mut ModelStats,
    ) -> Option<Vec<SentenceId>> {
        if !self.certified.contains(&rel) {
            return None;
        }
        let budget: usize = std::env::var("SIGMA_MODEL_BUDGET")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(250_000);
        const MAX_CONE_RULES: usize = 50_000;
        const MAX_CONE_FACTS: usize = 200_000;

        let mut goal = HashSet::new();
        goal.insert(rel);
        let cone = cluster::dependency_cone(&self.program, &goal);
        let scoped = cluster::scope_program(&self.program, &cone);
        let cone_facts: usize = scoped.edb.values().map(|s| s.len()).sum();
        if scoped.rules.len() > MAX_CONE_RULES || cone_facts > MAX_CONE_FACTS {
            stats.budget_overflows += 1;
            return None;
        }
        let (model, prov) = match scoped.evaluate_within(budget, deadline) {
            Ok(mp) => mp,
            Err(ModelError::Unsafe) => {
                stats.unsafe_bails += 1;
                return None;
            }
            Err(ModelError::Unstratifiable) => {
                stats.unstratifiable_bails += 1;
                return None;
            }
            Err(ModelError::Overflow) => {
                stats.budget_overflows += 1;
                return None;
            }
            Err(ModelError::Inconsistent(_)) => {
                stats.rigid_conflicts += 1;
                return None;
            }
        };
        // Probe in CANONICAL form: a tuple present under merged constants is
        // present — reporting it absent would be an unsound negative.
        let probe = prov.eq.canon_tuple(&tuple.to_vec());
        if model.get(&rel).is_some_and(|rows| rows.contains(&probe)) {
            return None; // present — no negative to give
        }
        // The completion citation: every defining rule sid of the cone,
        // first-appearance order, deduped.
        let mut cited: Vec<SentenceId> = Vec::new();
        let mut seen: HashSet<SentenceId> = HashSet::new();
        for r in &scoped.rules {
            if let Some(sid) = r.sid {
                if seen.insert(sid) {
                    cited.push(sid);
                }
            }
        }
        stats.answered += 1;
        Some(cited)
    }
}

/// Build-time breakdown of why candidate relations were refused completion
/// certification — the `certification_blocked_by` counters (SIGMA_STATS).
/// One relation is counted under exactly one reason, checked in the order
/// role → skipped-head → unstratifiable, with the body-chain fixpoint
/// shrink counted last.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CertBlocked {
    /// (a) the relation heads a root the extractor skipped (its definition
    /// escapes the program) — or a skipped predicate-variable root poisoned
    /// certification wholesale (every candidate lands here).
    pub(crate) skipped_head: u32,
    /// (b) the relation is not in any stratifiable cluster.
    pub(crate) unstratifiable: u32,
    /// (c) decertified by the body fixpoint: some rule chain from the
    /// relation reaches an uncertified relation.
    pub(crate) body_chain: u32,
    /// (d) a recognized taxonomy role relation (the oracle's Complete
    /// coverage — no double ownership), or a relation REIFIED as a term
    /// elsewhere in the program (a derived role membership — e.g.
    /// hierarchy-inherited transitivity — could enrich its definition
    /// beyond the build-time cone).
    pub(crate) role: u32,
    /// (e) a BUILT-IN transitive relation (task #32 Part 2): its stored
    /// extension is the un-closed base — a both-free literal over it
    /// under-enumerates the closure, so neither it nor (via the body
    /// fixpoint) anything defined over it may license a negative.
    pub(crate) builtin: u32,
}

/// The Clark-completion certification predicate (see the
/// [`ModelProgram::certified`] field doc for the conditions).  Exposed as a
/// free function over an arbitrary program so the event-calculus narrative
/// program can be certified too (its skipped set is empty: `parse_narrative`
/// consumed the defining only-if roots wholesale).
///
/// `cluster_preds` is the union of stratifiable-cluster predicates
/// (condition (b) — [`ModelProgram::complete`]'s historical semantics);
/// `skipped_heads` / `wildcard_skip` come from extraction (condition (a));
/// `role_syms` are the oracle-owned role relations (condition (d)).
/// Condition (c) is the shrink fixpoint at the end.  Everything errs toward
/// NOT certifying.
pub(crate) fn certify(
    program:       &Program,
    cluster_preds: &HashSet<Pred>,
    skipped_heads: &HashSet<Pred>,
    wildcard_skip: bool,
    role_syms:     &HashSet<Pred>,
    builtin:       &HashSet<Pred>,
) -> (HashSet<Pred>, CertBlocked) {
    let mut blocked = CertBlocked::default();

    // Candidate universe: every predicate the program mentions.
    let mut universe: HashSet<Pred> = HashSet::new();
    for r in &program.rules {
        universe.insert(r.head.pred);
        for l in &r.body {
            universe.insert(l.atom.pred);
        }
    }
    for p in program.edb.keys() {
        universe.insert(*p);
    }

    // A skipped predicate-variable root could derive atoms of ANY relation:
    // nothing is certifiable.
    if wildcard_skip {
        blocked.skipped_head = universe.len() as u32;
        return (HashSet::new(), blocked);
    }

    // Relations REIFIED as terms: symbols in argument position of any EDB
    // tuple or any rule-atom constant.  A ground model tuple can only carry
    // constants that appear somewhere as terms (Datalog invents nothing),
    // so a relation NOT in this set can never acquire a derived role
    // membership (`(instance R TransitiveRelation)` via the hierarchy) that
    // would add rules outside the build-time cone.
    let mut reified: HashSet<Pred> = HashSet::new();
    for rows in program.edb.values() {
        for t in rows {
            reified.extend(t.iter().copied());
        }
    }
    for r in &program.rules {
        for a in std::iter::once(&r.head).chain(r.body.iter().map(|l| &l.atom)) {
            for arg in &a.args {
                if let DTerm::Const(c) = arg {
                    reified.insert(*c);
                }
            }
        }
    }

    let mut certified: HashSet<Pred> = HashSet::new();
    for &p in &universe {
        if builtin.contains(&p) {
            blocked.builtin += 1;
        } else if role_syms.contains(&p) || reified.contains(&p) {
            blocked.role += 1;
        } else if skipped_heads.contains(&p) {
            blocked.skipped_head += 1;
        } else if !cluster_preds.contains(&p) {
            blocked.unstratifiable += 1;
        } else {
            certified.insert(p);
        }
    }

    // (c) shrink fixpoint: a certified relation whose rules' bodies (either
    // polarity — the perfect model needs the negated relation complete too)
    // reference an uncertified relation is decertified, until stable.
    loop {
        let drop: Vec<Pred> = certified
            .iter()
            .copied()
            .filter(|&p| {
                program
                    .rules
                    .iter()
                    .filter(|r| r.head.pred == p)
                    .any(|r| r.body.iter().any(|l| !certified.contains(&l.atom.pred)))
            })
            .collect();
        if drop.is_empty() {
            break;
        }
        for p in drop {
            certified.remove(&p);
            blocked.body_chain += 1;
        }
    }

    (certified, blocked)
}

/// One denial-constraint refutation of a ground `(instance x C)` atom — see
/// [`ModelProgram::refutes`].
#[derive(Debug, Clone)]
pub(crate) struct ModelRefutation {
    /// The model-entailed class of `x` that met a denial pair.
    pub member:        SymbolId,
    /// The goal-side class the pair was hit through: `C` itself or the
    /// ancestor `C ⊑ goal_ancestor` reached in the subclass closure.
    pub goal_ancestor: SymbolId,
    /// KB citation chain: instance-derivation chain (EDB leaves first, then
    /// rules), goal-side subclass chain, denial declaration last.
    pub cited:         Vec<SentenceId>,
}

/// SIGMA_STATS instrumentation only (Part 1): why `ModelProgram::answer`
/// bailed to `None`, or that it produced an answer — surfaced to callers via
/// [`ModelProgram::answer_stats`] so the prover's per-run counters
/// (`ProverStats::model_*`) can report where model-discharge time is spent.
/// Zero behavior change: `answer` itself still returns `Option<Vec<Tuple>>`
/// unchanged; this is purely an additional out-parameter.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ModelStats {
    pub(crate) unsafe_bails: u32,
    pub(crate) unstratifiable_bails: u32,
    /// Tuple-budget overflow (either the cone-size pre-check or the
    /// evaluator's own `ModelError::Overflow`) AND wall-clock-deadline
    /// overflow, combined — see the note on `evaluate_within`'s single
    /// `Overflow` variant covering both.
    pub(crate) budget_overflows: u32,
    pub(crate) undefined_relation: u32,
    pub(crate) answered: u32,
    /// Goal atoms REJECTED for model discharge because an argument was a
    /// compound (function) term or literal — not representable as a
    /// `DTerm`, so the bridge refuses the atom rather than wildcarding it
    /// (an over-approximation that would be unsound for negative
    /// decisions).  Counted by the prover-side bridge (`bridge_dargs`).
    pub(crate) bridge_rejected_atoms: u32,
    /// Unsafe rules dropped from a demanded cone before evaluation (the
    /// sound under-approximation in `answer_stats` — see the filter there).
    pub(crate) unsafe_rules_dropped: u32,
    /// Evaluations aborted by [`ModelError::Inconsistent`]: an EGD forced a
    /// union of two distinct rigid (numeric-literal) symbols.
    pub(crate) rigid_conflicts: u32,
    /// Total budget consumption of the answered evaluations (stored tuples +
    /// builtin-BFS work) — the probe the chain-1000 / headline gates read.
    pub(crate) budget_used: u64,
    /// Full-cone bails retried on the SHALLOW (goal-headed) cone — see
    /// `answer_stats_impl`'s two-tier evaluation.
    pub(crate) shallow_retries: u32,
    /// TRANSIENT channel between the two tiers (not a counter): the last
    /// bail's reason admits a shallow retry.  Reset per evaluation.
    pub(crate) retry_eligible: bool,
}

/// A predicate is identified by its relation-name symbol.
pub(crate) type Pred = SymbolId;
/// A ground tuple — the argument symbols of one fact.
pub(crate) type Tuple = Vec<SymbolId>;
/// A materialized model: each predicate's set of true ground tuples.
pub(crate) type Model = HashMap<Pred, HashSet<Tuple>>;

/// A rule term: a logical variable (by small index, rule-local) or a constant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DTerm {
    Var(u32),
    Const(SymbolId),
}

/// An atom: a predicate applied to argument terms.
#[derive(Clone, Debug)]
pub(crate) struct Atom {
    pub pred: Pred,
    pub args: Vec<DTerm>,
}

/// A body literal: an atom with a polarity.
#[derive(Clone, Debug)]
pub(crate) struct Literal {
    pub atom:    Atom,
    pub negated: bool,
}

/// A safe, range-restricted rule `head :- body` (body is a conjunction).
#[derive(Clone, Debug)]
pub(crate) struct Rule {
    pub head: Atom,
    pub body: Vec<Literal>,
    /// The KB sentence this rule was extracted / instantiated from — the
    /// `(=> …)` root for extracted Horn rules, the declaring
    /// `(subrelation R S)` / `(instance R TransitiveRelation)` sentence for
    /// schema rules.  `None` for synthetic rules (magic guards, hand-authored
    /// narratives), which contribute no citation of their own.
    pub sid:  Option<SentenceId>,
}

/// A Datalog(¬) program: intensional rules + extensional ground facts.
#[derive(Clone, Debug, Default)]
pub(crate) struct Program {
    pub rules: Vec<Rule>,
    pub edb:   HashMap<Pred, HashSet<Tuple>>,
    /// Source sentence of each EDB fact — the provenance leaves.  Facts
    /// seeded without a source (magic seeds, hand-authored programs) are
    /// simply absent and contribute no citation.
    pub edb_sids: HashMap<(Pred, Tuple), SentenceId>,
    /// Mined equality-generating dependencies (binary FD-style uniqueness
    /// constraints — see [`extract::Egd`]).  The kernel fires them on
    /// insertion: two tuples sharing an EGD key with distinct value reps
    /// union those values in the evaluation's equality classes.
    pub egds: Vec<extract::Egd>,
    /// Relations whose transitive closure is BUILT-IN: no transitivity
    /// schema rule is emitted for them — the kernel resolves their body
    /// literals by on-demand BFS over the stored base edges instead of
    /// materializing the closure.  Value = the `(R, TransitiveRelation)`
    /// declaring sentence (cited by closure-derived provenance).  (The spec
    /// sketched `Set64<Pred>` + a separate sid channel through
    /// `transitive_members`; a map carries the same information with less
    /// plumbing.)
    pub builtin_transitive: super::hash64::Map64<Pred, Option<SentenceId>>,
    /// RIGID symbols (numeric literals interned as symbols): an EGD union
    /// over two distinct rigid symbols aborts the evaluation with
    /// [`ModelError::Inconsistent`] instead of merging.
    pub rigid: super::hash64::Set64<SymbolId>,
    /// The recognized `instance` relation — EGD guard checks probe its
    /// stored facts.  `None` ⇒ guarded EGDs never fire (sound: under-fires).
    pub instance_pred: Option<Pred>,
}

/// How one derived fact was FIRST obtained: the deriving rule (an index into
/// the evaluated program's `rules`) and the ground tuples its positive body
/// literals matched.  First-parent only — later re-derivations of the same
/// fact are not recorded (cheap, and sufficient for citation).  Negated
/// literals contribute no parents (they cite absence, not a fact).
///
/// Two sentinel `rule` values (both far above any real rule index):
/// [`BUILTIN_RULE`] and [`EQ_CANON_RULE`].
#[derive(Clone, Debug)]
pub(crate) struct Derivation {
    pub rule:    u32,
    pub parents: SmallVec<[(Pred, Tuple); 4]>,
}

/// `Derivation::rule` sentinel: the fact is a BUILT-IN transitive-closure
/// tuple — `parents` are the BFS path's base-edge tuples, and `cite` adds
/// the relation's `(R, TransitiveRelation)` declaring sid
/// ([`Provenance::builtin_sids`]).
pub(crate) const BUILTIN_RULE: u32 = u32::MAX;

/// `Derivation::rule` sentinel: the fact is the RE-CANONICALIZED form of an
/// earlier fact after an EGD merge — `parents[0]` is the original row, and
/// `cite` adds the equality chain(s) for each rewritten position.
pub(crate) const EQ_CANON_RULE: u32 = u32::MAX - 1;

/// Per-evaluation provenance: everything needed to reconstruct, for any fact
/// of the computed model, the KB sentences (EDB facts + rules) its derivation
/// used.  Returned by value from each evaluation — NEVER cached on the
/// KB-lifetime [`ModelProgram`] registry (rule indices refer to the program
/// instance that was evaluated, e.g. a magic-rewritten cone).
#[derive(Clone, Debug, Default)]
pub(crate) struct Provenance {
    /// `rule_sids[i]` = source sentence of the evaluated program's rule `i`.
    pub rule_sids: Vec<Option<SentenceId>>,
    /// Source sentence of each EDB fact (copied from the evaluated program).
    pub edb_sids:  HashMap<(Pred, Tuple), SentenceId>,
    /// First derivation of each IDB fact.
    pub derived:   HashMap<(Pred, Tuple), Derivation>,
    /// The evaluation's equality classes (EGD union-find + justification
    /// forest).  Per-evaluation state like everything else here — NEVER
    /// cached on the KB-lifetime registry.
    pub eq: seminaive::EqClasses,
    /// `(R, TransitiveRelation)` declaring sid per builtin-transitive
    /// relation — what the [`BUILTIN_RULE`] sentinel cites.
    pub builtin_sids: HashMap<Pred, SentenceId>,
    /// The evaluation's budget consumption: stored tuples + builtin-BFS
    /// nodes visited + expanded closure driver tuples.  The probe the
    /// chain-1000 gate asserts on.
    pub budget_used: usize,
}

impl Provenance {
    /// Reconstruct the KB citation for one model fact: walk the derivation
    /// DAG (iterative, memoized), collecting the EDB leaf sentences and each
    /// step's rule sentence, dedup'd — leaf facts first, then rules, matching
    /// the taxonomy oracle's bottom-up citation style.  Depth-guarded.
    ///
    /// EGD-aware: a probe tuple whose constants were canonicalized during
    /// evaluation is looked up in canonical form, and the equality chain(s)
    /// for each merged position are cited too — each chain edge's EGD axiom
    /// sid plus both witness tuples' derivations (recursively, through this
    /// same walk).  [`BUILTIN_RULE`] facts cite their path edges + the
    /// `(R, TransitiveRelation)` declaration; [`EQ_CANON_RULE`] facts cite
    /// their original row + the rewriting chain.
    pub(crate) fn cite(&self, pred: Pred, t: &Tuple) -> Vec<SentenceId> {
        const MAX_STEPS: usize = 10_000;
        let mut fact_sids: Vec<SentenceId> = Vec::new();
        let mut rule_sids: Vec<SentenceId> = Vec::new();
        let mut visited: HashSet<(Pred, Tuple)> = HashSet::new();
        let mut cited_edges: HashSet<(SymbolId, SymbolId)> = HashSet::new();
        let mut stack: Vec<(Pred, Tuple)> = vec![(pred, t.clone())];
        let mut steps = 0usize;
        while let Some(key) = stack.pop() {
            if !visited.insert(key.clone()) {
                continue;
            }
            steps += 1;
            if steps > MAX_STEPS {
                break;
            }
            // An EDB leaf cites its own sentence directly — checked on the
            // ORIGINAL form first, before canonicalization (the canonical
            // twin may already be visited, which must not swallow the leaf).
            let (p, tup) = key;
            if let Some(&sid) = self.edb_sids.get(&(p, tup.clone())) {
                fact_sids.push(sid);
                continue;
            }
            // Canonicalize the probe: a tuple carrying pre-merge constants is
            // stored under its canonical form; the equality chain that
            // justifies the rewrite is part of the citation.
            let ctup = self.eq.canon_tuple(&tup);
            let key = if ctup != tup {
                for i in 0..tup.len() {
                    if tup[i] != ctup[i] {
                        self.push_eq_chain(tup[i], ctup[i], &mut cited_edges, &mut rule_sids, &mut stack);
                    }
                }
                let ckey = (p, ctup);
                if !visited.insert(ckey.clone()) {
                    continue;
                }
                ckey
            } else {
                (p, tup)
            };
            if let Some(d) = self.derived.get(&key) {
                match d.rule {
                    BUILTIN_RULE => {
                        if let Some(&sid) = self.builtin_sids.get(&key.0) {
                            rule_sids.push(sid);
                        }
                    }
                    EQ_CANON_RULE => {
                        // The rewriting chain: original row → canonical row.
                        if let Some((_, orig)) = d.parents.first() {
                            for i in 0..key.1.len().min(orig.len()) {
                                if orig[i] != key.1[i] {
                                    self.push_eq_chain(
                                        orig[i], key.1[i],
                                        &mut cited_edges, &mut rule_sids, &mut stack,
                                    );
                                }
                            }
                        }
                    }
                    r => {
                        if let Some(sid) = self.rule_sids.get(r as usize).copied().flatten() {
                            rule_sids.push(sid);
                        }
                    }
                }
                // Reversed: the walk is a stack, so pushing in reverse pops
                // parents in their natural (e.g. path) order.
                for p in d.parents.iter().rev() {
                    if !visited.contains(p) {
                        stack.push(p.clone());
                    }
                }
            } else if let Some(&sid) = self.edb_sids.get(&key) {
                fact_sids.push(sid);
            }
        }
        let mut out: Vec<SentenceId> = Vec::with_capacity(fact_sids.len() + rule_sids.len());
        let mut seen: HashSet<SentenceId> = HashSet::new();
        for s in fact_sids.into_iter().chain(rule_sids) {
            if seen.insert(s) {
                out.push(s);
            }
        }
        out
    }

    /// Push one equality chain `a = … = b` onto a `cite` walk: each proof-
    /// forest edge's EGD axiom sid goes to the rule bucket and both witness
    /// tuples go onto the fact stack (their derivations resolve through the
    /// normal walk).  Edge-visited-set guarded.
    fn push_eq_chain(
        &self,
        a: SymbolId,
        b: SymbolId,
        cited_edges: &mut HashSet<(SymbolId, SymbolId)>,
        rule_sids:   &mut Vec<SentenceId>,
        stack:       &mut Vec<(Pred, Tuple)>,
    ) {
        for ek in self.eq.explain(a, b) {
            if !cited_edges.insert(ek) {
                continue;
            }
            if let Some(e) = self.eq.edges.get(&ek) {
                if let Some(sid) = e.egd_sid {
                    rule_sids.push(sid);
                }
                stack.push(e.witness_a.clone());
                stack.push(e.witness_b.clone());
            }
        }
    }

    /// The KB citation for one entailed equality `a = b`: walk the proof
    /// forest between them, collecting each edge's witness-tuple derivations
    /// (through [`cite`](Self::cite)) and EGD axiom sid.  Empty when the
    /// evaluation never merged them.
    pub(crate) fn explain_eq(&self, a: SymbolId, b: SymbolId) -> Vec<SentenceId> {
        let mut out: Vec<SentenceId> = Vec::new();
        let mut seen: HashSet<SentenceId> = HashSet::new();
        for ek in self.eq.explain(a, b) {
            let Some(e) = self.eq.edges.get(&ek) else { continue };
            for w in [&e.witness_a, &e.witness_b] {
                for s in self.cite(w.0, &w.1) {
                    if seen.insert(s) {
                        out.push(s);
                    }
                }
            }
            if let Some(sid) = e.egd_sid {
                if seen.insert(sid) {
                    out.push(sid);
                }
            }
        }
        out
    }
}

/// Why a program could not be evaluated as a stratified Datalog program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ModelError {
    /// Negation occurs inside a recursive cycle — no perfect model.
    Unstratifiable,
    /// A head or negated-literal variable is not bound by a positive body
    /// literal (the rule is not range-restricted / safe).
    Unsafe,
    /// The materialized model exceeded the tuple budget — bail to resolution
    /// rather than blow up (the un-scoped-materialization guard; demand
    /// scoping via SInE is the real fix).
    Overflow,
    /// An EGD forced a union of two distinct RIGID (numeric-literal)
    /// symbols: the program + EGDs are inconsistent.  Carries the citation
    /// chain — the equality path's witness-tuple derivations and EGD
    /// axiom sids (a future refutation channel; for now the evaluation
    /// just bails soundly).
    Inconsistent(Vec<SentenceId>),
}

impl Program {
    /// Add a ground EDB fact (no citable source).
    pub(crate) fn fact(&mut self, pred: Pred, tuple: Tuple) {
        self.edb.entry(pred).or_default().insert(tuple);
    }

    /// Add a ground EDB fact recording the KB sentence it came from.
    pub(crate) fn fact_src(&mut self, pred: Pred, tuple: Tuple, sid: SentenceId) {
        self.edb_sids.insert((pred, tuple.clone()), sid);
        self.edb.entry(pred).or_default().insert(tuple);
    }

    /// Add a rule (no citable source — hand-authored / synthetic).
    pub(crate) fn rule(&mut self, head: Atom, body: Vec<Literal>) {
        self.rules.push(Rule { head, body, sid: None });
    }

    /// Evaluate the program to its perfect model (bottom-up, stratum by
    /// stratum; positive recursion within a stratum, negation only against
    /// fully-computed lower strata).  Model-only convenience (provenance
    /// discarded) — see [`evaluate_within`](Self::evaluate_within).
    pub(crate) fn evaluate(&self) -> Result<Model, ModelError> {
        self.evaluate_budgeted(usize::MAX)
    }

    /// Evaluate, but abort with [`ModelError::Overflow`] once the materialized
    /// model exceeds `max_tuples` total facts — the guard that keeps an
    /// un-scoped evaluation over a large KB from blowing up (it bails to
    /// resolution instead).  `usize::MAX` ⇒ unbounded (see [`evaluate`]).
    /// Model-only convenience (provenance discarded).
    pub(crate) fn evaluate_budgeted(&self, max_tuples: usize) -> Result<Model, ModelError> {
        self.evaluate_within(max_tuples, None).map(|(m, _)| m)
    }

    /// As [`evaluate_budgeted`], but also aborts (`Overflow`) past a wall-clock
    /// `deadline` — so a query-time materialization can never eat the prover's
    /// time budget (it bails to resolution instead).  Returns the model
    /// TOGETHER with its per-evaluation [`Provenance`], so callers that emit
    /// model facts into a proof can cite the KB sentences behind them.
    pub(crate) fn evaluate_within(
        &self,
        max_tuples: usize,
        deadline:   Option<std::time::Instant>,
    ) -> Result<(Model, Provenance), ModelError> {
        self.validate_safe()?;
        let strata = self.stratify()?;
        seminaive::run(self, &strata, max_tuples, deadline)
    }

    /// Safety: every head variable and every negated-literal variable must
    /// appear in some positive body literal (range restriction).
    fn validate_safe(&self) -> Result<(), ModelError> {
        if self.rules.iter().all(rule_is_safe) {
            Ok(())
        } else {
            Err(ModelError::Unsafe)
        }
    }

    /// Assign each predicate a stratum number: `level(head) >= level(b)` for a
    /// positive body predicate `b`, and `level(head) > level(b)` for a negated
    /// one.  Iterates to a fixpoint; if levels keep rising past the predicate
    /// count there is a negative cycle → `Unstratifiable`.  Returns the
    /// predicates grouped by stratum, lowest first.
    pub(crate) fn stratify(&self) -> Result<Vec<Vec<Pred>>, ModelError> {
        let mut preds: HashSet<Pred> = HashSet::new();
        for r in &self.rules {
            preds.insert(r.head.pred);
            for l in &r.body {
                preds.insert(l.atom.pred);
            }
        }
        for p in self.edb.keys() {
            preds.insert(*p);
        }

        let mut level: HashMap<Pred, usize> = preds.iter().map(|p| (*p, 0)).collect();
        let bound = preds.len() + 2;
        for _ in 0..bound {
            let mut changed = false;
            for r in &self.rules {
                for l in &r.body {
                    let bl = level[&l.atom.pred];
                    let need = if l.negated { bl + 1 } else { bl };
                    if need > level[&r.head.pred] {
                        *level.get_mut(&r.head.pred).unwrap() = need;
                        changed = true;
                    }
                }
            }
            if !changed {
                let max = level.values().copied().max().unwrap_or(0);
                let mut strata = vec![Vec::new(); max + 1];
                for (p, l) in &level {
                    strata[*l].push(*p);
                }
                return Ok(strata);
            }
        }
        Err(ModelError::Unstratifiable)
    }
}

/// One rule's range-restriction (safety) check: every head variable and
/// every negated-literal variable appears in some positive body literal.
/// Used both by [`Program::validate_safe`] (whole-program gate) and by the
/// cone machinery's unsafe-rule filter in [`ModelProgram::answer_stats`].
fn rule_is_safe(r: &Rule) -> bool {
    let mut pos_vars: HashSet<u32> = HashSet::new();
    for l in &r.body {
        if !l.negated {
            for a in &l.atom.args {
                if let DTerm::Var(v) = a {
                    pos_vars.insert(*v);
                }
            }
        }
    }
    for a in &r.head.args {
        if let DTerm::Var(v) = a {
            if !pos_vars.contains(v) {
                return false;
            }
        }
    }
    for l in &r.body {
        if l.negated {
            for a in &l.atom.args {
                if let DTerm::Var(v) = a {
                    if !pos_vars.contains(v) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

// (The naive recursive `join_body` was retired when `evaluate_budgeted` moved
//  to the indexed semi-naive engine in `seminaive`.)

/// Match an atom's argument terms against a ground tuple, extending `binding`.
/// Returns the variables newly bound (for undo), or `None` on a clash.
pub(super) fn unify(args: &[DTerm], tuple: &[SymbolId], binding: &mut HashMap<u32, SymbolId>) -> Option<Vec<u32>> {
    if args.len() != tuple.len() {
        return None;
    }
    let mut undo = Vec::new();
    for (a, &val) in args.iter().zip(tuple) {
        match a {
            DTerm::Const(c) => {
                if *c != val {
                    for v in &undo { binding.remove(v); }
                    return None;
                }
            }
            DTerm::Var(v) => match binding.get(v) {
                Some(&b) => {
                    if b != val {
                        for v in &undo { binding.remove(v); }
                        return None;
                    }
                }
                None => {
                    binding.insert(*v, val);
                    undo.push(*v);
                }
            },
        }
    }
    Some(undo)
}

/// Resolve a BUILTIN-transitive GOAL relation's answers over the returned
/// model rows (its BASE edges): bound-left → forward BFS, bound-right →
/// reverse BFS, both-bound → reachability; each closure answer gets a
/// [`BUILTIN_RULE`] derivation (path edges as parents) recorded into `prov`
/// so `cite` resolves it.  `Ok(None)` when not applicable (non-builtin goal,
/// wrong arity, or a both-free / repeated-variable pattern — those fall back
/// to the caller's base-row filter, the documented under-enumeration).
/// `Err(())` when the BFS work would exceed the remaining tuple budget.
fn builtin_goal_answers(
    rows:       &HashSet<Tuple>,
    cargs:      &[DTerm],
    rel:        Pred,
    prov:       &mut Provenance,
    is_builtin: bool,
    budget:     usize,
) -> Result<Option<Vec<Tuple>>, ()> {
    if !is_builtin || cargs.len() != 2 {
        return Ok(None);
    }
    let bound = |a: &DTerm| match a {
        DTerm::Const(c) => Some(*c),
        DTerm::Var(_) => None,
    };
    let (b0, b1) = (bound(&cargs[0]), bound(&cargs[1]));
    if b0.is_none() && b1.is_none() {
        return Ok(None); // both free: base rows only
    }
    // Adjacency over the base edges — forward when the LEFT side seeds the
    // BFS, reversed when only the right side is bound.
    let fwd = b0.is_some();
    let seed = if fwd { b0.unwrap() } else { b1.unwrap() };
    let mut adj: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
    for row in rows {
        if row.len() == 2 {
            let (a, b) = if fwd { (row[0], row[1]) } else { (row[1], row[0]) };
            adj.entry(a).or_default().push(b);
        }
    }
    let mut parent: HashMap<SymbolId, SymbolId> = HashMap::new();
    let mut reached: Vec<SymbolId> = Vec::new();
    let mut seen: HashSet<SymbolId> = HashSet::new();
    seen.insert(seed);
    let mut q: std::collections::VecDeque<SymbolId> = std::collections::VecDeque::new();
    q.push_back(seed);
    while let Some(u) = q.pop_front() {
        for &v in adj.get(&u).map(Vec::as_slice).unwrap_or(&[]) {
            if seen.insert(v) {
                parent.insert(v, u);
                reached.push(v);
                q.push_back(v);
            }
        }
    }
    prov.budget_used = prov.budget_used.saturating_add(reached.len() + 1);
    if prov.budget_used > budget {
        return Err(());
    }
    // The ORIGINAL-orientation path seed →+ node, from the BFS tree.
    let path_of = |node: SymbolId| -> SmallVec<[(Pred, Tuple); 4]> {
        let mut edges: Vec<(Pred, Tuple)> = Vec::new();
        let mut cur = node;
        while cur != seed {
            let Some(&w) = parent.get(&cur) else { return SmallVec::new() };
            // Forward BFS: tree edge (w → cur) is a real edge w→cur.
            // Reverse BFS: tree edge means a real edge cur→w.
            edges.push((rel, if fwd { vec![w, cur] } else { vec![cur, w] }));
            cur = w;
        }
        if fwd {
            edges.reverse();
        }
        edges.into_iter().collect()
    };
    let record = |prov: &mut Provenance, pair: Tuple, node: SymbolId| {
        if !rows.contains(&pair) && !prov.derived.contains_key(&(rel, pair.clone())) {
            prov.derived
                .insert((rel, pair), Derivation { rule: BUILTIN_RULE, parents: path_of(node) });
        }
    };
    let ans: Vec<Tuple> = match (b0, b1) {
        (Some(x), Some(y)) => {
            if x != y && parent.contains_key(&y) {
                let pair = vec![x, y];
                record(prov, pair.clone(), y);
                vec![pair]
            } else if rows.contains(&vec![x, y]) {
                vec![vec![x, y]] // direct base edge (covers x == y self-loops)
            } else {
                Vec::new()
            }
        }
        (Some(x), None) => reached
            .iter()
            .map(|&z| {
                let pair = vec![x, z];
                record(prov, pair.clone(), z);
                pair
            })
            .collect(),
        (None, Some(y)) => reached
            .iter()
            .map(|&x| {
                let pair = vec![x, y];
                record(prov, pair.clone(), x);
                pair
            })
            .collect(),
        (None, None) => unreachable!("handled above"),
    };
    Ok(Some(ans))
}

/// Ground an atom under a binding, or `None` if a variable is unbound.
pub(super) fn ground_atom(a: &Atom, binding: &HashMap<u32, SymbolId>) -> Option<Tuple> {
    let mut t = Vec::with_capacity(a.args.len());
    for arg in &a.args {
        match arg {
            DTerm::Const(c) => t.push(*c),
            DTerm::Var(v) => t.push(*binding.get(v)?),
        }
    }
    Some(t)
}

// ---------------------------------------------------------------------------
// Reproduction (Phase 2): the inertial event calculus as a Datalog program.
// ---------------------------------------------------------------------------

/// A predicate id from its relation name.
fn pid(name: &str) -> Pred {
    crate::types::Symbol::hash_name(name)
}

/// Encode an inertial DEC narrative ([`super::eventcalc::Narrative`]) as a
/// stratified Datalog(¬) program — the SOLE evaluation path for
/// `discharge_event_calculus` (the bespoke `simulate` forward-simulator this
/// once cross-checked against has been retired; see `ec_kernel_holds_grid`
/// for the golden-grid regression that replaced the parity test).
///
/// The program (stratified: EDB < {initiates,terminates,initiated,terminated}
/// < holdsAt):
/// ```text
///   initiates(e,f,T)  :- time(T) [, happens(p,T)]* [, not happens(n,T)]*   (per Effect)
///   terminates(e,f,T) :- time(T) [, happens(p,T)]* [, not happens(n,T)]*   (per Effect)
///   initiated(F,T)    :- happens(E,T), initiates(E,F,T)
///   terminated(F,T)   :- happens(E,T), terminates(E,F,T)
///   holdsAt(F,T1)     :- succ(T,T1), initiated(F,T)
///   holdsAt(F,T1)     :- succ(T,T1), holdsAt(F,T), not terminated(F,T)
///   holdsAt(F,t0)     :- (EDB, from the initial state)
/// ```
///
/// `succ` is the narrative's OWN order-axiom-derived chain (`n.succ`) when
/// the KB carried one (timeline honesty); falls back to adjacency over
/// `n.times` (already lexically ranked by `parse_narrative` in that case)
/// only when no order axioms were found.  EDB facts are recorded with their
/// source sid (`fact_src`) so the grid reconstruction can cite real KB
/// provenance for positive cells.
pub(crate) fn narrative_to_program(n: &super::eventcalc::Narrative) -> Program {
    let happens = pid("happens");
    let initiates = pid("initiates");
    let terminates = pid("terminates");
    let initiated = pid("initiated");
    let terminated = pid("terminated");
    let holds = pid("holdsAt");
    let time = pid("time");
    let succ = pid("succ");

    let mut p = Program::default();

    for &t in &n.times {
        p.fact(time, vec![t]);
    }
    match &n.succ {
        Some(edges) => {
            for (&from, &to) in edges {
                p.fact(succ, vec![from, to]);
            }
        }
        None => {
            for w in n.times.windows(2) {
                p.fact(succ, vec![w[0], w[1]]);
            }
        }
    }
    for (&t, evs) in &n.happens {
        for &e in evs {
            match n.happens_sid {
                Some(sid) => p.fact_src(happens, vec![e, t], sid),
                None => p.fact(happens, vec![e, t]),
            }
        }
    }
    if let Some(&t0) = n.times.first() {
        for (&f, &val) in &n.initial {
            if val {
                match n.initial_sid.get(&(f, t0)) {
                    Some(&sid) => p.fact_src(holds, vec![f, t0], sid),
                    None => p.fact(holds, vec![f, t0]),
                }
            }
        }
    }

    // One rule per effect, with the concurrent-event guards.  `time(T)` binds
    // the time variable (safety); `happens(p,T)` is a positive guard,
    // `not happens(n,T)` a negative one.  T is variable 0.  Each rule cites
    // the narrative's only-if root that defined its relation, for provenance.
    let effect_rule = |head_pred: Pred, e: &super::eventcalc::Effect, rule_sid: Option<SentenceId>| -> Rule {
        let mut body = vec![Literal {
            atom: Atom { pred: time, args: vec![DTerm::Var(0)] },
            negated: false,
        }];
        for &pe in &e.pos_concurrent {
            body.push(Literal {
                atom: Atom { pred: happens, args: vec![DTerm::Const(pe), DTerm::Var(0)] },
                negated: false,
            });
        }
        for &ne in &e.neg_concurrent {
            body.push(Literal {
                atom: Atom { pred: happens, args: vec![DTerm::Const(ne), DTerm::Var(0)] },
                negated: true,
            });
        }
        Rule {
            head: Atom {
                pred: head_pred,
                args: vec![DTerm::Const(e.event), DTerm::Const(e.fluent), DTerm::Var(0)],
            },
            body,
            sid: rule_sid,
        }
    };
    for e in &n.initiates {
        p.rules.push(effect_rule(initiates, e, n.initiates_sid));
    }
    for e in &n.terminates {
        p.rules.push(effect_rule(terminates, e, n.terminates_sid));
    }

    // initiated(F,T) :- happens(E,T), initiates(E,F,T)   (E=0, F=1, T=2)
    // Cites `initiates_sid` — the same only-if root the `initiates` facts
    // above already resolve through, so this bridge rule adds no NEW leaf,
    // just the connecting step; `happens_sid` is picked up transitively
    // through the `happens` EDB fact's own `fact_src`.
    p.rules.push(Rule {
        head: Atom { pred: initiated, args: vec![DTerm::Var(1), DTerm::Var(2)] },
        body: vec![
            Literal { atom: Atom { pred: happens, args: vec![DTerm::Var(0), DTerm::Var(2)] }, negated: false },
            Literal { atom: Atom { pred: initiates, args: vec![DTerm::Var(0), DTerm::Var(1), DTerm::Var(2)] }, negated: false },
        ],
        sid: n.initiates_sid,
    });
    // terminated(F,T) :- happens(E,T), terminates(E,F,T)
    p.rules.push(Rule {
        head: Atom { pred: terminated, args: vec![DTerm::Var(1), DTerm::Var(2)] },
        body: vec![
            Literal { atom: Atom { pred: happens, args: vec![DTerm::Var(0), DTerm::Var(2)] }, negated: false },
            Literal { atom: Atom { pred: terminates, args: vec![DTerm::Var(0), DTerm::Var(1), DTerm::Var(2)] }, negated: false },
        ],
        sid: n.terminates_sid,
    });
    // holdsAt(F,T1) :- succ(T,T1), initiated(F,T)         (F=0, T=1, T1=2)
    p.rule(
        Atom { pred: holds, args: vec![DTerm::Var(0), DTerm::Var(2)] },
        vec![
            Literal { atom: Atom { pred: succ, args: vec![DTerm::Var(1), DTerm::Var(2)] }, negated: false },
            Literal { atom: Atom { pred: initiated, args: vec![DTerm::Var(0), DTerm::Var(1)] }, negated: false },
        ],
    );
    // holdsAt(F,T1) :- succ(T,T1), holdsAt(F,T), not terminated(F,T)
    p.rule(
        Atom { pred: holds, args: vec![DTerm::Var(0), DTerm::Var(2)] },
        vec![
            Literal { atom: Atom { pred: succ, args: vec![DTerm::Var(1), DTerm::Var(2)] }, negated: false },
            Literal { atom: Atom { pred: holds, args: vec![DTerm::Var(0), DTerm::Var(1)] }, negated: false },
            Literal { atom: Atom { pred: terminated, args: vec![DTerm::Var(0), DTerm::Var(1)] }, negated: true },
        ],
    );

    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::eventcalc::{self, Effect, Narrative};
    use crate::types::Symbol;

    fn s(name: &str) -> SymbolId { Symbol::hash_name(name) }

    // Readable rule builders for the small hand-authored programs.
    fn atom(pred: &str, args: Vec<DTerm>) -> Atom { Atom { pred: s(pred), args } }
    fn pos(a: Atom) -> Literal { Literal { atom: a, negated: false } }
    fn neg(a: Atom) -> Literal { Literal { atom: a, negated: true } }
    fn v(i: u32) -> DTerm { DTerm::Var(i) }
    fn c(name: &str) -> DTerm { DTerm::Const(s(name)) }

    fn holds(model: &Model, pred: &str, args: &[&str]) -> bool {
        let t: Tuple = args.iter().map(|a| s(a)).collect();
        model.get(&s(pred)).is_some_and(|r| r.contains(&t))
    }

    // -- (a) Taxonomy closure: transitive subclass + instance/subclass bridge --
    // Reproduces the SemanticOracle's reachability (the Merge.kif bridge axiom
    // `(=> (and (subclass ?X ?Y) (instance ?Z ?X)) (instance ?Z ?Y))`).
    #[test]
    fn taxonomy_closure() {
        let mut p = Program::default();
        p.fact(s("subclass"), vec![s("RoadVehicle"), s("LandVehicle")]);
        p.fact(s("subclass"), vec![s("LandVehicle"), s("Vehicle")]);
        p.fact(s("instance"), vec![s("Bus1"), s("RoadVehicle")]);
        // subclass(X,Z) :- subclass(X,Y), subclass(Y,Z)
        p.rule(atom("subclass", vec![v(0), v(2)]),
               vec![pos(atom("subclass", vec![v(0), v(1)])),
                    pos(atom("subclass", vec![v(1), v(2)]))]);
        // instance(Z,Y) :- instance(Z,X), subclass(X,Y)
        p.rule(atom("instance", vec![v(2), v(1)]),
               vec![pos(atom("instance", vec![v(2), v(0)])),
                    pos(atom("subclass", vec![v(0), v(1)]))]);
        let m = p.evaluate().unwrap();
        assert!(holds(&m, "subclass", &["RoadVehicle", "Vehicle"]));
        assert!(holds(&m, "instance", &["Bus1", "LandVehicle"]));
        assert!(holds(&m, "instance", &["Bus1", "Vehicle"]));
        assert!(!holds(&m, "instance", &["Bus1", "Artifact"]));
    }

    // -- (b) Horn rule-join: multi-premise body + chained heads (the jail shape) --
    #[test]
    fn horn_rule_join_chain() {
        let mut p = Program::default();
        p.fact(s("driving"), vec![s("Bob")]);
        p.fact(s("usingPhone"), vec![s("Bob")]);
        p.fact(s("driving"), vec![s("Ann")]); // Ann drives but no phone ⇒ no breach
        // breaksLaw(P) :- driving(P), usingPhone(P)
        p.rule(atom("breaksLaw", vec![v(0)]),
               vec![pos(atom("driving", vec![v(0)])),
                    pos(atom("usingPhone", vec![v(0)]))]);
        // goesToJail(P) :- breaksLaw(P)   (chains via the fixpoint)
        p.rule(atom("goesToJail", vec![v(0)]),
               vec![pos(atom("breaksLaw", vec![v(0)]))]);
        let m = p.evaluate().unwrap();
        assert!(holds(&m, "breaksLaw", &["Bob"]));
        assert!(holds(&m, "goesToJail", &["Bob"]));
        assert!(!holds(&m, "breaksLaw", &["Ann"]));
        assert!(!holds(&m, "goesToJail", &["Ann"]));
    }

    // Stratified negation: a defined predicate decided by the ABSENCE of a fact.
    #[test]
    fn stratified_negation_decides_absence() {
        let mut p = Program::default();
        p.fact(s("thing"), vec![s("a")]);
        p.fact(s("thing"), vec![s("b")]);
        p.fact(s("flagged"), vec![s("a")]);
        // clear(X) :- thing(X), not flagged(X)
        p.rule(atom("clear", vec![v(0)]),
               vec![pos(atom("thing", vec![v(0)])),
                    neg(atom("flagged", vec![v(0)]))]);
        let m = p.evaluate().unwrap();
        assert!(holds(&m, "clear", &["b"]));
        assert!(!holds(&m, "clear", &["a"]));
    }

    // A negation cycle has no perfect model ⇒ the engine refuses (bails).
    #[test]
    fn negation_cycle_is_unstratifiable() {
        let mut p = Program::default();
        p.fact(s("dom"), vec![s("x")]);
        // p(X) :- dom(X), not q(X)  ;  q(X) :- dom(X), not p(X)
        p.rule(atom("p", vec![v(0)]),
               vec![pos(atom("dom", vec![v(0)])), neg(atom("q", vec![v(0)]))]);
        p.rule(atom("q", vec![v(0)]),
               vec![pos(atom("dom", vec![v(0)])), neg(atom("p", vec![v(0)]))]);
        assert_eq!(p.evaluate(), Err(ModelError::Unstratifiable));
    }

    // Provenance: a 2-hop derived fact cites both EDB leaf sentences AND both
    // rule sentences its derivation chained through — leaf facts first, then
    // rules (the taxonomy oracle's bottom-up citation style).
    #[test]
    fn cite_two_hop_derivation_cites_edb_and_rules() {
        let (f1, f2, r1, r2): (SentenceId, SentenceId, SentenceId, SentenceId) =
            (0x11, 0x22, 0x33, 0x44);
        let mut p = Program::default();
        p.fact_src(s("edge"), vec![s("a"), s("b")], f1);
        p.fact_src(s("link"), vec![s("b"), s("c")], f2);
        // step(X,Y) :- edge(X,Y)                      [rule sid r1]
        p.rules.push(Rule {
            head: atom("step", vec![v(0), v(1)]),
            body: vec![pos(atom("edge", vec![v(0), v(1)]))],
            sid:  Some(r1),
        });
        // reach(X,Z) :- step(X,Y), link(Y,Z)          [rule sid r2]
        p.rules.push(Rule {
            head: atom("reach", vec![v(0), v(2)]),
            body: vec![pos(atom("step", vec![v(0), v(1)])), pos(atom("link", vec![v(1), v(2)]))],
            sid:  Some(r2),
        });
        let (model, prov) = p.evaluate_within(usize::MAX, None).unwrap();
        assert!(holds(&model, "reach", &["a", "c"]));

        let cited = prov.cite(s("reach"), &vec![s("a"), s("c")]);
        assert!(cited.contains(&f1), "cites the edge EDB leaf");
        assert!(cited.contains(&f2), "cites the link EDB leaf");
        assert!(cited.contains(&r1), "cites the 1st-hop rule");
        assert!(cited.contains(&r2), "cites the 2nd-hop rule");
        assert_eq!(cited.len(), 4, "nothing else cited, no duplicates");
        // Leaf facts precede rules.
        let pos_of = |sid: SentenceId| cited.iter().position(|x| *x == sid).unwrap();
        assert!(
            pos_of(f1).max(pos_of(f2)) < pos_of(r1).min(pos_of(r2)),
            "EDB leaves come before rules: {cited:?}"
        );
        // An EDB fact cites exactly its own sentence.
        assert_eq!(prov.cite(s("edge"), &vec![s("a"), s("b")]), vec![f1]);
        // An unknown fact cites nothing.
        assert!(prov.cite(s("reach"), &vec![s("c"), s("a")]).is_empty());
    }

    // -- Milestone A (negatives package): faithful goal-variable bridging. --
    // A repeated goal variable constrains the answer: `p(X, X)` matches only
    // tuples with equal seats, never `(a, b)`.  Distinct variables stay
    // independent wildcards.
    #[test]
    fn answer_repeated_var_goal_requires_equal_values() {
        use crate::semantics::caches::test_support::kif_layer;
        let sem = kif_layer("(p a b)\n(p c c)");
        let mp = ModelProgram::build(&sem.syntactic);

        // p(X, X): only the diagonal tuple.
        let rows = mp
            .answer(s("p"), &[DTerm::Var(0), DTerm::Var(0)], None)
            .expect("p is stored");
        assert_eq!(rows, vec![vec![s("c"), s("c")]], "p(X, X) must not match (a, b)");

        // p(X, Y): both tuples.
        let mut rows = mp
            .answer(s("p"), &[DTerm::Var(0), DTerm::Var(1)], None)
            .expect("p is stored");
        rows.sort();
        let mut want = vec![vec![s("a"), s("b")], vec![s("c"), s("c")]];
        want.sort();
        assert_eq!(rows, want, "distinct variables stay independent");

        // Constant + repeated-var mix: p(c, X) hits the diagonal row.
        let rows = mp
            .answer(s("p"), &[DTerm::Const(s("c")), DTerm::Var(0)], None)
            .expect("p is stored");
        assert_eq!(rows, vec![vec![s("c"), s("c")]]);
    }

    // -- Milestone B (negatives package): denial constraints → refutation. --

    // Pairwise flattening of partition tails into denial pairs.
    #[test]
    fn collect_denials_flattens_partition_pairwise() {
        use crate::semantics::caches::test_support::kif_layer;
        let sem = kif_layer(
            "(partition Animal DomesticAnimal WildAnimal FeralAnimal)\n\
             (disjoint Rock Cloud)",
        );
        let mp = ModelProgram::build(&sem.syntactic);
        let norm = |a: SymbolId, b: SymbolId| if a <= b { (a, b) } else { (b, a) };
        let pairs: HashSet<(SymbolId, SymbolId)> =
            mp.denials.iter().map(|d| d.classes).collect();
        assert_eq!(pairs.len(), 4, "3 partition pairs + 1 disjoint pair");
        for (a, b) in [
            ("DomesticAnimal", "WildAnimal"),
            ("DomesticAnimal", "FeralAnimal"),
            ("WildAnimal", "FeralAnimal"),
            ("Rock", "Cloud"),
        ] {
            assert!(pairs.contains(&norm(s(a), s(b))), "missing pair {a}/{b}");
        }
        // Every denial cites its declaring root.
        for d in &mp.denials {
            assert!(sem.syntactic.sentence(d.sid).is_some(), "denial sid resolvable");
        }
    }

    // A partition-derived denial refutes an instance atom whose membership is
    // TWO subclass hops away, and the citation chain carries every step:
    // the anchoring instance fact, both member-side subclass edges, the
    // goal-side subclass edge, a chain rule, and the partition declaration
    // LAST.  A class pair with no denial between them does NOT refute.
    #[test]
    fn partition_denial_refutes_two_hop_instance_with_citations() {
        use crate::semantics::caches::test_support::kif_layer;
        use crate::types::{Element, OpKind};
        let kif = "\
            (instance subclass TransitiveRelation)\n\
            (=> (and (instance ?Z ?X) (subclass ?X ?Y)) (instance ?Z ?Y))\n\
            (partition Animal DomesticAnimal WildAnimal)\n\
            (subclass Dog DomesticAnimal)\n\
            (subclass Poodle Dog)\n\
            (instance Rex Poodle)\n\
            (subclass Wolf WildAnimal)\n";
        let sem = kif_layer(kif);
        let syn = &sem.syntactic;
        let mp = ModelProgram::build(syn);

        // Locate the citable roots.
        let find2 = |head: &str, a: &str, b: &str| -> SentenceId {
            syn.by_head_id(&s(head))
                .into_iter()
                .find(|sid| {
                    syn.sentence(*sid).is_some_and(|sent| {
                        sent.elements.len() == 3
                            && matches!(&sent.elements[1], Element::Symbol(x) if x.id() == s(a))
                            && matches!(&sent.elements[2], Element::Symbol(y) if y.id() == s(b))
                    })
                })
                .expect("fixture root present")
        };
        let f_rex     = find2("instance", "Rex", "Poodle");
        let f_poodle  = find2("subclass", "Poodle", "Dog");
        let f_dog     = find2("subclass", "Dog", "DomesticAnimal");
        let f_wolf    = find2("subclass", "Wolf", "WildAnimal");
        let f_part    = syn
            .by_head_id(&s("partition"))
            .into_iter()
            .next()
            .expect("partition root");
        let f_bridge  = syn
            .root_sids()
            .into_iter()
            .find(|sid| syn.sentence(*sid).is_some_and(|x| x.op() == Some(&OpKind::Implies)))
            .expect("bridge rule root");

        // (instance Rex Wolf) is REFUTED: Rex ⊑… DomesticAnimal (2 hops),
        // Wolf ⊑ WildAnimal, and partition makes those disjoint.
        let mut stats = ModelStats::default();
        let r = mp
            .refutes(s("instance"), &[s("Rex"), s("Wolf")], None, &mut stats)
            .expect("denial refutes (instance Rex Wolf)");
        assert_eq!(r.member, s("DomesticAnimal"), "clashing membership");
        assert_eq!(r.goal_ancestor, s("WildAnimal"), "goal-side ancestor");

        // Full citation chain: every leaf edge + a chain rule + the denial.
        for (sid, what) in [
            (f_rex, "instance Rex Poodle"),
            (f_poodle, "subclass Poodle Dog"),
            (f_dog, "subclass Dog DomesticAnimal"),
            (f_wolf, "subclass Wolf WildAnimal"),
            (f_part, "partition declaration"),
        ] {
            assert!(r.cited.contains(&sid), "citation chain missing {what}: {:?}", r.cited);
        }
        // The chain climbed through a rule (the bridge, or the derived
        // subclass-transitivity schema citing its declaration).
        let f_trans = syn
            .by_head_id(&s("instance"))
            .into_iter()
            .find(|sid| {
                syn.sentence(*sid).is_some_and(|sent| {
                    sent.elements.len() == 3
                        && matches!(&sent.elements[1], Element::Symbol(x) if x.id() == s("subclass"))
                })
            })
            .expect("transitivity declaration root");
        assert!(
            r.cited.contains(&f_bridge) || r.cited.contains(&f_trans),
            "chain rule cited: {:?}",
            r.cited
        );
        // The denial declaration is the LAST step (the referee), and the
        // chain starts from a leaf fact.
        assert_eq!(r.cited.last(), Some(&f_part), "denial axiom last");
        assert_ne!(r.cited.first(), Some(&f_part));

        // No denial between Rex's classes and Dog / an unknown class: no
        // refutation (and membership of a class ON Rex's own chain never
        // refutes).
        assert!(mp.refutes(s("instance"), &[s("Rex"), s("Dog")], None, &mut stats).is_none());
        assert!(mp.refutes(s("instance"), &[s("Rex"), s("Cat")], None, &mut stats).is_none());
        // Non-instance relations are never refuted here.
        assert!(mp.refutes(s("subclass"), &[s("Dog"), s("Wolf")], None, &mut stats).is_none());
    }

    // Cross-check: on a fixture where BOTH engines see the same information
    // (stored taxonomy edges + explicit bridge/transitivity axioms + the
    // disjointness declarations), `ModelProgram::refutes` must agree with the
    // taxonomy oracle's `refutes_instance` — the ground truth — on EVERY
    // (individual, class) atom.  Non-vacuous: the grid contains several real
    // refutations (asserted below), so agreement is exercised both ways.
    #[test]
    fn refutes_agrees_with_taxonomy_oracle_on_shared_taxonomy() {
        use super::super::oracle::SemanticOracle;
        use super::super::theory::TheoryOracle;
        use crate::semantics::caches::test_support::kif_layer;
        use crate::semantics::types::Scope;
        let kif = "\
            (instance subclass TransitiveRelation)\n\
            (=> (and (instance ?Z ?X) (subclass ?X ?Y)) (instance ?Z ?Y))\n\
            (partition Animal DomesticAnimal WildAnimal)\n\
            (disjoint Bird Fish)\n\
            (subclass Dog DomesticAnimal)\n\
            (subclass Poodle Dog)\n\
            (subclass Wolf WildAnimal)\n\
            (subclass Bird Animal)\n\
            (subclass Fish Animal)\n\
            (subclass Canary Bird)\n\
            (instance Rex Poodle)\n\
            (instance Tweety Canary)\n\
            (instance Nemo Fish)\n";
        let sem = kif_layer(kif);
        let mp = ModelProgram::build(&sem.syntactic);
        let oracle = SemanticOracle::new(&sem, Scope::Base);

        let individuals = ["Rex", "Tweety", "Nemo"];
        let classes = [
            "Animal", "DomesticAnimal", "WildAnimal", "Dog", "Poodle", "Wolf",
            "Bird", "Fish", "Canary",
        ];
        let mut refuted = 0usize;
        for x in individuals {
            for c in classes {
                let o = oracle.refutes_instance(s("instance"), s(x), s(c), None);
                let mut stats = ModelStats::default();
                let m = mp
                    .refutes(s("instance"), &[s(x), s(c)], None, &mut stats)
                    .is_some();
                assert_eq!(o, m, "oracle/model disagreement on (instance {x} {c})");
                refuted += usize::from(m);
            }
        }
        assert!(refuted >= 5, "cross-check must be non-vacuous, got {refuted} refutations");
    }

    // Unsafe rule (head var not bound by a positive body literal) is rejected.
    #[test]
    fn unsafe_rule_is_rejected() {
        let mut p = Program::default();
        p.fact(s("dom"), vec![s("x")]);
        // bad(X,Y) :- dom(X)   -- Y unbound
        p.rule(atom("bad", vec![v(0), v(1)]), vec![pos(atom("dom", vec![v(0)]))]);
        assert_eq!(p.evaluate(), Err(ModelError::Unsafe));
    }

    // -- (c) Golden-grid regression for the CSR001+2 spinning narrative. -----
    //
    // Replaces the former `ec_kernel_matches_simulate` byte-parity
    // cross-check (the bespoke `eventcalc::simulate` forward-simulator it
    // compared against has been retired — the kernel is now the ONLY
    // evaluation path `discharge_event_calculus` uses).  The 12 expected
    // cells below were captured from that cross-check's last passing run
    // (kernel == simulate, both computing DEC6/7/10/11 inertia over
    // happens: push@n0, pull@n1, {pull,push}@n2 — see the retired test's
    // docstring in git history for the by-hand derivation), so this test
    // still pins the exact same invariant, just without the now-deleted
    // engine as a witness.
    #[test]
    fn ec_kernel_holds_grid() {
        let (n0, n1, n2, n3) = (s("n0"), s("n1"), s("n2"), s("n3"));
        let (push, pull) = (s("push"), s("pull"));
        let (fwd, bwd, spin) = (s("forwards"), s("backwards"), s("spinning"));
        let mut happens = HashMap::new();
        happens.insert(n0, vec![push]);
        happens.insert(n1, vec![pull]);
        happens.insert(n2, vec![pull, push]);
        let initiates = vec![
            Effect { event: push, fluent: fwd,  pos_concurrent: vec![],     neg_concurrent: vec![pull] },
            Effect { event: pull, fluent: bwd,  pos_concurrent: vec![],     neg_concurrent: vec![push] },
            Effect { event: pull, fluent: spin, pos_concurrent: vec![push], neg_concurrent: vec![] },
        ];
        let terminates = vec![
            Effect { event: push, fluent: bwd,  pos_concurrent: vec![],     neg_concurrent: vec![pull] },
            Effect { event: pull, fluent: fwd,  pos_concurrent: vec![],     neg_concurrent: vec![push] },
            Effect { event: pull, fluent: fwd,  pos_concurrent: vec![push], neg_concurrent: vec![] },
            Effect { event: pull, fluent: bwd,  pos_concurrent: vec![push], neg_concurrent: vec![] },
            Effect { event: push, fluent: spin, pos_concurrent: vec![],     neg_concurrent: vec![pull] },
            Effect { event: pull, fluent: spin, pos_concurrent: vec![],     neg_concurrent: vec![push] },
        ];
        let nar = Narrative {
            times: vec![n0, n1, n2, n3],
            happens,
            initiates,
            terminates,
            initial: HashMap::new(),
            initial_at: Vec::new(),
            initial_sid: HashMap::new(),
            happens_sid: None,
            initiates_sid: None,
            terminates_sid: None,
            succ: None,
        };

        let prog = narrative_to_program(&nar);
        let model = prog.evaluate().expect("spinning narrative is stratified");
        let holds_rel = model.get(&pid("holdsAt")).cloned().unwrap_or_default();

        // The golden grid: every (fluent, time) cell, captured from the
        // narrative's DEC6/7/10/11 semantics (initiated ∨ (held ∧
        // ¬terminated)) — literal expected values, no simulator involved.
        let golden: [((SymbolId, SymbolId), bool); 12] = [
            ((fwd, n0), false), ((fwd, n1), true),  ((fwd, n2), false), ((fwd, n3), false),
            ((bwd, n0), false), ((bwd, n1), false), ((bwd, n2), true),  ((bwd, n3), false),
            ((spin, n0), false), ((spin, n1), false), ((spin, n2), false), ((spin, n3), true),
        ];
        for &((f, t), expected) in &golden {
            let actual = holds_rel.contains(&vec![f, t]);
            assert_eq!(actual, expected, "golden-grid mismatch at fluent/time cell");
        }
        // And the key CSR conjecture cells, explicitly (the family this
        // narrative backs: CSR015-023+1).
        assert!(!holds_rel.contains(&vec![spin, n1])); // ¬spinning@n1 (CSR017)
        assert!(!holds_rel.contains(&vec![spin, n2])); // ¬spinning@n2 (CSR020)
        assert!(holds_rel.contains(&vec![spin, n3]));  //  spinning@n3
        assert!(holds_rel.contains(&vec![fwd, n1]));   //  forwards@n1
    }

    // `succ` EDB honesty: when the narrative carries a derived order chain,
    // the kernel program's `succ` facts are read from THAT chain, not
    // synthesized from `times.windows(2)` adjacency.  A deliberately
    // OUT-OF-LEXICAL-ORDER `times` vector proves the
    // distinction: if the kernel used adjacency it would wire the wrong
    // successor and the grid would come out wrong.
    #[test]
    fn ec_kernel_uses_narrative_succ_when_present() {
        let (n0, n1, n2) = (s("n0"), s("n1"), s("n2"));
        let (ev, fl) = (s("ev"), s("fl"));
        let mut happens = HashMap::new();
        happens.insert(n0, vec![ev]);
        let initiates = vec![
            Effect { event: ev, fluent: fl, pos_concurrent: vec![], neg_concurrent: vec![] },
        ];
        let mut succ = HashMap::new();
        succ.insert(n0, n1);
        succ.insert(n1, n2);
        let nar = Narrative {
            // Deliberately NOT in n0,n1,n2 adjacency order — `times` is only
            // used for the `time(T)` EDB and initial-state anchor here;
            // `succ` alone drives the transition wiring.
            times: vec![n0, n2, n1],
            happens,
            initiates,
            terminates: Vec::new(),
            initial: HashMap::new(),
            initial_at: Vec::new(),
            initial_sid: HashMap::new(),
            happens_sid: None,
            initiates_sid: None,
            terminates_sid: None,
            succ: Some(succ),
        };
        let prog = narrative_to_program(&nar);
        let model = prog.evaluate().expect("stratified");
        let holds_rel = model.get(&pid("holdsAt")).cloned().unwrap_or_default();
        // ev@n0 initiates fl; succ(n0,n1) ⇒ fl holds at n1; succ(n1,n2) ⇒
        // fl still holds at n2 (inertia, nothing terminates it).
        assert!(holds_rel.contains(&vec![fl, n1]), "fl must hold at n1 via the derived succ edge");
        assert!(holds_rel.contains(&vec![fl, n2]), "fl must persist to n2 via inertia over the derived succ chain");
        assert!(!holds_rel.contains(&vec![fl, n0]), "fl must not hold at n0 (initiated only at the n0->n1 step)");
    }

    // -- Clark-completion certifier -------------------------------------------

    // (i) A fully-extracted 2-rule KB certifies its relations, and an ABSENT
    // tuple yields the completion decision citing EVERY defining rule sid of
    // the goal's cone; a PRESENT tuple and an uncertified relation yield
    // nothing.
    #[test]
    fn certified_absence_yields_negative_with_all_defining_sids() {
        use crate::semantics::caches::test_support::kif_layer;
        let kif = "\
            (=> (and (parent ?X ?Y) (parent ?Y ?Z)) (grandparent ?X ?Z))\n\
            (=> (adoptedBy ?Y ?X) (parent ?X ?Y))\n\
            (parent Alice Bob)\n\
            (parent Bob Carol)\n\
            (adoptedBy Dave Carol)\n";
        let sem = kif_layer(kif);
        let mp = ModelProgram::build(&sem.syntactic);

        for r in ["grandparent", "parent", "adoptedBy"] {
            assert!(mp.certified.contains(&s(r)), "{r} must certify: {:?}", mp.cert_blocked);
        }

        // grandparent(Alice, Dave) is absent (Alice's chain ends at Carol;
        // Dave is Carol's child): certified absence, citing BOTH rule roots
        // — the grandparent rule and the parent-via-adoption rule.
        let mut stats = ModelStats::default();
        let cited = mp
            .complete_absent(s("grandparent"), &[s("Alice"), s("Dave")], None, &mut stats)
            .expect("certified absence must decide the negative");
        let rule_sids: Vec<SentenceId> =
            mp.program.rules.iter().filter_map(|r| r.sid).collect();
        assert_eq!(rule_sids.len(), 2, "two extracted rules define the cone");
        for sid in &rule_sids {
            assert!(cited.contains(sid), "completion citation missing a defining rule sid");
        }
        assert_eq!(cited.len(), 2, "nothing beyond the defining rules is cited");
        assert_eq!(stats.answered, 1);

        // Present tuples decide nothing (both are model-derived).
        assert!(mp
            .complete_absent(s("grandparent"), &[s("Alice"), s("Carol")], None, &mut stats)
            .is_none());
        assert!(mp
            .complete_absent(s("grandparent"), &[s("Bob"), s("Dave")], None, &mut stats)
            .is_none());
        // An uncertified (unknown) relation decides nothing.
        assert!(mp
            .complete_absent(s("instance"), &[s("Alice"), s("Dave")], None, &mut stats)
            .is_none());
    }

    // (ii) The SAME KB plus one extra sentence the extractor must SKIP
    // (compound argument in the consequent) whose consequent head is
    // `grandparent` ⇒ grandparent is NOT certified and decides nothing;
    // relations untouched by the skip stay certified.
    #[test]
    fn skipped_consequent_head_blocks_certification() {
        use crate::semantics::caches::test_support::kif_layer;
        let kif = "\
            (=> (and (parent ?X ?Y) (parent ?Y ?Z)) (grandparent ?X ?Z))\n\
            (=> (adoptedBy ?Y ?X) (parent ?X ?Y))\n\
            (parent Alice Bob)\n\
            (parent Bob Carol)\n\
            (adoptedBy Dave Carol)\n\
            (=> (relative ?X ?Y) (grandparent ?X (MotherFn ?Y)))\n";
        let sem = kif_layer(kif);
        let mp = ModelProgram::build(&sem.syntactic);

        assert!(
            !mp.certified.contains(&s("grandparent")),
            "a skipped potential definition must block certification"
        );
        assert!(mp.cert_blocked.skipped_head >= 1, "{:?}", mp.cert_blocked);
        // The untouched relations keep their certification.
        assert!(mp.certified.contains(&s("parent")));
        assert!(mp.certified.contains(&s("adoptedBy")));

        let mut stats = ModelStats::default();
        assert!(
            mp.complete_absent(s("grandparent"), &[s("Alice"), s("Dave")], None, &mut stats)
                .is_none(),
            "no negative may be decided for a blocked relation"
        );
    }

    // (iii) A body reference to an UNCERTIFIED relation decertifies the
    // referring relation — and the shrink propagates along rule chains —
    // while an independent clean chain stays certified.
    #[test]
    fn uncertified_body_relation_decertifies_by_fixpoint() {
        use crate::semantics::caches::test_support::kif_layer;
        let kif = "\
            (=> (r ?X) (q ?X))\n\
            (=> (q ?X) (top ?X))\n\
            (r a)\n\
            (=> (s ?X) (r (FooFn ?X)))\n\
            (=> (u ?X) (v ?X))\n\
            (u b)\n";
        let sem = kif_layer(kif);
        let mp = ModelProgram::build(&sem.syntactic);

        assert!(!mp.certified.contains(&s("r")), "skipped-head block on r");
        assert!(!mp.certified.contains(&s("q")), "one-step body chain decertifies q");
        assert!(!mp.certified.contains(&s("top")), "two-step body chain decertifies top");
        assert!(mp.cert_blocked.body_chain >= 2, "{:?}", mp.cert_blocked);
        // The clean chain is untouched by the shrink.
        assert!(mp.certified.contains(&s("u")));
        assert!(mp.certified.contains(&s("v")));
    }

    // (iv) Recognized taxonomy role relations NEVER certify — they are the
    // oracle's Complete coverage (no double ownership) — even when their
    // cluster is stratifiable and nothing was skipped.
    #[test]
    fn taxonomy_role_relations_never_certify() {
        use crate::semantics::caches::test_support::kif_layer;
        let kif = "\
            (subclass Dog Animal)\n\
            (instance Rex Dog)\n\
            (=> (and (instance ?Z ?X) (subclass ?X ?Y)) (instance ?Z ?Y))\n";
        let sem = kif_layer(kif);
        let mp = ModelProgram::build(&sem.syntactic);

        assert!(!mp.certified.contains(&mp.roles.instance), "instance is oracle-owned");
        assert!(!mp.certified.contains(&mp.roles.subclass), "subclass is oracle-owned");
        assert!(mp.cert_blocked.role >= 2, "{:?}", mp.cert_blocked);

        let mut stats = ModelStats::default();
        // Neither the entailed nor the un-entailed instance atom is decided.
        assert!(mp
            .complete_absent(s("instance"), &[s("Rex"), s("Animal")], None, &mut stats)
            .is_none());
        assert!(mp
            .complete_absent(s("instance"), &[s("Rex"), s("Wolf")], None, &mut stats)
            .is_none());
    }

    // (v) EC narrative predicates: the spinning-narrative PROGRAM (whose
    // defining only-if roots `parse_narrative` consumed wholesale — the
    // skipped set is empty) certifies holdsAt/initiated/terminated, and the
    // certifier's negative decisions agree with the kernel grid's negative
    // cells on every fluent×time cell.  (In the prover, `discharge_event_calculus`
    // already emits those negatives — the KB-level ModelProgram sees the
    // <=>-split `(=> holdsAt-head (or …))` roots as SKIPPED, so no double
    // emission; any residual duplicate dedups through `make` like any other.)
    #[test]
    fn ec_narrative_program_certifies_and_negatives_agree_with_grid() {
        let (n0, n1, n2, n3) = (s("n0"), s("n1"), s("n2"), s("n3"));
        let (push, pull) = (s("push"), s("pull"));
        let (fwd, bwd, spin) = (s("forwards"), s("backwards"), s("spinning"));
        let mut happens = HashMap::new();
        happens.insert(n0, vec![push]);
        happens.insert(n1, vec![pull]);
        happens.insert(n2, vec![pull, push]);
        let initiates = vec![
            Effect { event: push, fluent: fwd,  pos_concurrent: vec![],     neg_concurrent: vec![pull] },
            Effect { event: pull, fluent: bwd,  pos_concurrent: vec![],     neg_concurrent: vec![push] },
            Effect { event: pull, fluent: spin, pos_concurrent: vec![push], neg_concurrent: vec![] },
        ];
        let terminates = vec![
            Effect { event: push, fluent: bwd,  pos_concurrent: vec![],     neg_concurrent: vec![pull] },
            Effect { event: pull, fluent: fwd,  pos_concurrent: vec![],     neg_concurrent: vec![push] },
            Effect { event: pull, fluent: fwd,  pos_concurrent: vec![push], neg_concurrent: vec![] },
            Effect { event: pull, fluent: bwd,  pos_concurrent: vec![push], neg_concurrent: vec![] },
            Effect { event: push, fluent: spin, pos_concurrent: vec![],     neg_concurrent: vec![pull] },
            Effect { event: pull, fluent: spin, pos_concurrent: vec![],     neg_concurrent: vec![push] },
        ];
        let nar = Narrative {
            times: vec![n0, n1, n2, n3],
            happens,
            initiates,
            terminates,
            initial: HashMap::new(),
            initial_at: Vec::new(),
            initial_sid: HashMap::new(),
            happens_sid: None,
            initiates_sid: None,
            terminates_sid: None,
            succ: None,
        };

        let prog = narrative_to_program(&nar);
        let clusters = cluster::partition(&prog);
        let complete: HashSet<Pred> =
            clusters.iter().flat_map(|c| c.preds.iter().copied()).collect();
        let roles = crate::semantics::roles::TaxonomyRoles::default();
        let role_syms: HashSet<Pred> = [
            roles.instance, roles.subclass, roles.subrelation, roles.transitive,
            roles.symmetric, roles.domain, roles.range, roles.disjoint, roles.partition,
        ]
        .into_iter()
        .collect();
        let (certified, cert_blocked) =
            certify(&prog, &complete, &HashSet::new(), false, &role_syms, &HashSet::new());
        for p in ["holdsAt", "initiated", "terminated", "initiates", "terminates"] {
            assert!(
                certified.contains(&pid(p)),
                "{p} must certify on the narrative program: {cert_blocked:?}"
            );
        }

        // Wrap into a ModelProgram so `complete_absent` runs the exact
        // machinery the discharge pass uses.
        let mp = ModelProgram {
            monotone: cluster::positive_program(&prog),
            program: prog.clone(),
            clusters,
            complete,
            certified,
            cert_blocked,
            roles,
            denials: Vec::new(),
        };
        let model = prog.evaluate().expect("spinning narrative is stratified");
        let holds_rel = model.get(&pid("holdsAt")).cloned().unwrap_or_default();

        // The same golden grid as `ec_kernel_holds_grid`.
        let golden: [((SymbolId, SymbolId), bool); 12] = [
            ((fwd, n0), false), ((fwd, n1), true),  ((fwd, n2), false), ((fwd, n3), false),
            ((bwd, n0), false), ((bwd, n1), false), ((bwd, n2), true),  ((bwd, n3), false),
            ((spin, n0), false), ((spin, n1), false), ((spin, n2), false), ((spin, n3), true),
        ];
        let mut stats = ModelStats::default();
        for &((f, t), expected) in &golden {
            assert_eq!(holds_rel.contains(&vec![f, t]), expected, "grid cell");
            let neg = mp.complete_absent(pid("holdsAt"), &[f, t], None, &mut stats);
            assert_eq!(
                neg.is_some(),
                !expected,
                "certifier negative must agree with the kernel grid on every cell"
            );
        }
    }

    // =======================================================================
    // Task #32 — EGDs (Part 1) + built-in transitive closure (Part 2).
    // =======================================================================

    /// Root sid of the flat binary fact `(head a b)`.
    fn find_fact(syn: &crate::syntactic::SyntacticLayer, head: &str, a: &str, b: &str) -> SentenceId {
        use crate::types::Element;
        syn.by_head_id(&s(head))
            .into_iter()
            .find(|sid| {
                syn.sentence(*sid).is_some_and(|sent| {
                    sent.elements.len() == 3
                        && matches!(&sent.elements[1], Element::Symbol(x) if x.id() == s(a))
                        && matches!(&sent.elements[2], Element::Symbol(y) if y.id() == s(b))
                })
            })
            .expect("fixture fact present")
    }

    // -- EGD extraction: both uniqueness-clause shapes + the declaration. ---

    #[test]
    fn egd_extraction_recognizes_all_three_shapes() {
        use crate::semantics::caches::test_support::{kif_layer, tptp_layer};

        // (=>)-shape + SingleValuedRelation declaration (KIF).
        let sem = kif_layer(
            "(=> (and (age ?P ?A1) (age ?P ?A2)) (equal ?A1 ?A2))\n\
             (instance ssn SingleValuedRelation)\n",
        );
        let mp = ModelProgram::build(&sem.syntactic);
        let age = mp.program.egds.iter().find(|e| e.rel == s("age")).expect("age EGD mined");
        assert_eq!((age.key_pos, age.val_pos), (0, 1), "key at arg1, values at arg2");
        assert!(age.sid.is_some(), "uniqueness clause cited");
        assert!(age.key_guards.is_empty() && age.val_guards.is_empty());
        let ssn = mp.program.egds.iter().find(|e| e.rel == s("ssn")).expect("ssn EGD mined");
        assert_eq!((ssn.key_pos, ssn.val_pos), (0, 1));
        assert!(ssn.sid.is_some(), "declaration cited");

        // (or)-shape (TPTP CNF).
        let sem = tptp_layer(
            "cnf(uniq, axiom, ( ~ age(P,A) | ~ age(P,B) | A = B ) ).\n",
            "egd_or.p",
        );
        let mp = ModelProgram::build(&sem.syntactic);
        let e = mp.program.egds.iter().find(|e| e.rel == s("age")).expect("or-shape mined");
        assert_eq!((e.key_pos, e.val_pos), (0, 1));
        assert!(e.sid.is_some());

        // Key-guarded (TQG14 nucleus shape): guards KEPT (design delta —
        // ignoring them would over-apply the FD, an unsound merge).
        let sem = kif_layer(
            "(=> (and (instance ?N AtomicNucleus) (part ?P1 ?N) (part ?P2 ?N)) \
                 (equal ?P1 ?P2))\n",
        );
        let mp = ModelProgram::build(&sem.syntactic);
        let e = mp.program.egds.iter().find(|e| e.rel == s("part")).expect("guarded shape mined");
        assert_eq!((e.key_pos, e.val_pos), (1, 0), "keyed on the whole (arg2)");
        assert_eq!(e.key_guards, vec![s("AtomicNucleus")]);
        assert!(e.val_guards.is_empty());

        // A guard over an UNRELATED variable disqualifies the sentence.
        let sem = kif_layer(
            "(=> (and (age ?P ?A1) (age ?P ?A2) (instance ?Q Human)) (equal ?A1 ?A2))\n",
        );
        let mp = ModelProgram::build(&sem.syntactic);
        assert!(mp.program.egds.is_empty(), "unrelated-guard sentence skipped");
    }

    // -- GATE: EGD merge with a 2-hop citation chain (both witnesses + EGD
    //    sids cited), and the canonicalized answer still matching the
    //    ORIGINAL goal constant. -------------------------------------------

    #[test]
    fn egd_merge_two_hop_chain_cites_witnesses_and_answers_original_constant() {
        use crate::semantics::caches::test_support::kif_layer;
        let kif = "\
            (=> (and (age ?P ?A1) (age ?P ?A2)) (equal ?A1 ?A2))\n\
            (instance ssn SingleValuedRelation)\n\
            (=> (and (age ?P ?A) (ssn ?P ?B)) (ageOf ?P ?A))\n\
            (age Bob AgeA)\n\
            (age Bob AgeB)\n\
            (ssn Bob AgeB)\n\
            (ssn Bob AgeC)\n";
        let sem = kif_layer(kif);
        let syn = &sem.syntactic;
        let mp = ModelProgram::build(syn);
        assert_eq!(mp.program.egds.len(), 2, "both EGDs mined");

        let uniq_root = syn
            .root_sids()
            .into_iter()
            .find(|sid| {
                syn.sentence(*sid).is_some_and(|x| {
                    x.op() == Some(&crate::types::OpKind::Implies)
                        && mp.program.egds.iter().any(|e| e.sid == Some(*sid))
                })
            })
            .expect("uniqueness root");
        let svr_decl = find_fact(syn, "instance", "ssn", "SingleValuedRelation");
        let f_age_a = find_fact(syn, "age", "Bob", "AgeA");
        let f_age_b = find_fact(syn, "age", "Bob", "AgeB");
        let f_ssn_b = find_fact(syn, "ssn", "Bob", "AgeB");
        let f_ssn_c = find_fact(syn, "ssn", "Bob", "AgeC");

        // The goal names AgeA; the evaluation merges AgeA ~ AgeB ~ AgeC (a
        // 2-hop chain across two different EGDs) — the answer must still
        // match, and the returned row must carry the ORIGINAL constant.
        let mut stats = ModelStats::default();
        let (rows, prov) = mp
            .answer_stats(s("ageOf"), &[DTerm::Const(s("Bob")), DTerm::Const(s("AgeA"))], None, &mut stats)
            .expect("ageOf answers");
        assert_eq!(rows, vec![vec![s("Bob"), s("AgeA")]], "original goal constant kept");

        // All three symbols share one rep (eq_rep API).
        let rep = mp.eq_rep(&prov, s("AgeA"));
        assert_eq!(mp.eq_rep(&prov, s("AgeB")), rep);
        assert_eq!(mp.eq_rep(&prov, s("AgeC")), rep);

        // The 2-hop chain AgeA = AgeB = AgeC cites BOTH edges' witnesses and
        // BOTH EGD axioms (eq_explain API).
        let chain = mp.eq_explain(&prov, s("AgeA"), s("AgeC"));
        for (sid, what) in [
            (f_age_a, "witness (age Bob AgeA)"),
            (f_age_b, "witness (age Bob AgeB)"),
            (f_ssn_b, "witness (ssn Bob AgeB)"),
            (f_ssn_c, "witness (ssn Bob AgeC)"),
            (uniq_root, "uniqueness clause"),
            (svr_decl, "SingleValuedRelation declaration"),
        ] {
            assert!(chain.contains(&sid), "2-hop chain missing {what}: {chain:?}");
        }

        // Probe with a NON-REP constant (which of the three wins rep-hood is
        // union-order-dependent): the answer must still match — the goal
        // constant canonicalizes to the rep — the row must come back with
        // the ORIGINAL constant, and its citation must cross the merge,
        // citing an EGD axiom.
        let probe = [s("AgeC"), s("AgeB"), s("AgeA")]
            .into_iter()
            .find(|&x| x != rep)
            .unwrap();
        let mut stats = ModelStats::default();
        let (rows, prov) = mp
            .answer_stats(s("ageOf"), &[DTerm::Const(s("Bob")), DTerm::Const(probe)], None, &mut stats)
            .expect("non-rep goal constant still answers");
        assert_eq!(rows, vec![vec![s("Bob"), probe]], "original (non-rep) goal constant kept");
        let cited = mp.cite(&prov, s("ageOf"), &vec![s("Bob"), probe]);
        assert!(cited.contains(&f_age_a) || cited.contains(&f_age_b), "age leaf cited: {cited:?}");
        assert!(
            cited.contains(&uniq_root) || cited.contains(&svr_decl),
            "an EGD axiom is cited when the probe crosses a merge: {cited:?}"
        );
    }

    // -- GATE: a rigid conflict bails `Inconsistent` with the citation chain.

    #[test]
    fn egd_rigid_conflict_bails_inconsistent_with_chain() {
        let (f1, f2, u1): (SentenceId, SentenceId, SentenceId) = (0x111, 0x222, 0x333);
        let mut p = Program::default();
        p.fact_src(s("val"), vec![s("k"), s("1")], f1);
        p.fact_src(s("val"), vec![s("k"), s("2")], f2);
        p.egds.push(extract::Egd {
            rel: s("val"), key_pos: 0, val_pos: 1,
            key_guards: Vec::new(), val_guards: Vec::new(), sid: Some(u1),
        });
        p.rigid.insert(s("1"));
        p.rigid.insert(s("2"));
        match p.evaluate_within(usize::MAX, None) {
            Err(ModelError::Inconsistent(chain)) => {
                for (sid, what) in [(f1, "witness 1"), (f2, "witness 2"), (u1, "EGD axiom")] {
                    assert!(chain.contains(&sid), "conflict chain missing {what}: {chain:?}");
                }
            }
            other => panic!("expected Inconsistent, got {other:?}"),
        }

        // Surfaced in ModelStats through the answer path.
        let clusters = cluster::partition(&p);
        let complete: HashSet<Pred> =
            clusters.iter().flat_map(|c| c.preds.iter().copied()).collect();
        let mp = ModelProgram {
            monotone: cluster::positive_program(&p),
            program: p.clone(),
            clusters,
            complete,
            certified: HashSet::new(),
            cert_blocked: CertBlocked::default(),
            roles: crate::semantics::roles::TaxonomyRoles::default(),
            denials: Vec::new(),
        };
        let mut stats = ModelStats::default();
        assert!(mp
            .answer_stats(s("val"), &[DTerm::Const(s("k")), DTerm::Var(0)], None, &mut stats)
            .is_none());
        assert_eq!(stats.rigid_conflicts, 1, "conflict surfaced in ModelStats");

        // Without rigidity the same program MERGES instead (sanity check
        // that the conflict is exactly the rigid-pair case).
        let mut p2 = p.clone();
        p2.rigid.clear();
        let (m, prov) = p2.evaluate_within(usize::MAX, None).expect("merges fine");
        assert_eq!(prov.eq.find(s("1")), prov.eq.find(s("2")), "values merged");
        let vals = m.get(&s("val")).unwrap();
        let rep = prov.eq.find(s("1"));
        assert!(vals.contains(&vec![s("k"), rep]), "canonical row present");
    }

    // -- EGD guard enforcement: fires only when the instance guard holds. ---

    #[test]
    fn egd_key_guard_enforced_against_store_instances() {
        use crate::semantics::caches::test_support::kif_layer;
        let base = "\
            (=> (and (instance ?N AtomicNucleus) (part ?P1 ?N) (part ?P2 ?N)) \
                (equal ?P1 ?P2))\n\
            (part Pa Nuc)\n\
            (part Pb Nuc)\n";

        // Guard UNSATISFIED: no merge.
        let sem = kif_layer(base);
        let mp = ModelProgram::build(&sem.syntactic);
        let (_, prov) = mp.monotone.evaluate_within(usize::MAX, None).expect("evaluates");
        assert_ne!(prov.eq.find(s("Pa")), prov.eq.find(s("Pb")), "guard blocks the merge");

        // Guard SATISFIED: merge fires, citing the guarded axiom.
        let kif = format!("{base}(instance Nuc AtomicNucleus)\n");
        let sem = kif_layer(&kif);
        let mp = ModelProgram::build(&sem.syntactic);
        let (_, prov) = mp.monotone.evaluate_within(usize::MAX, None).expect("evaluates");
        assert_eq!(prov.eq.find(s("Pa")), prov.eq.find(s("Pb")), "guarded EGD fires");
        let chain = prov.explain_eq(s("Pa"), s("Pb"));
        let uniq = mp.program.egds.iter().find(|e| e.rel == s("part")).unwrap().sid.unwrap();
        assert!(chain.contains(&uniq), "guarded axiom cited: {chain:?}");
    }

    // -- EGD merges + negation: refuse (a merge can retroactively change an
    //    absence check), surfacing as Unstratifiable. -----------------------

    #[test]
    fn egd_merge_under_negation_refuses_evaluation() {
        let mut p = Program::default();
        p.fact(s("val"), vec![s("k"), s("a")]);
        p.fact(s("val"), vec![s("k"), s("b")]);
        p.fact(s("dom"), vec![s("x")]);
        p.egds.push(extract::Egd {
            rel: s("val"), key_pos: 0, val_pos: 1,
            key_guards: Vec::new(), val_guards: Vec::new(), sid: None,
        });
        // q(X) :- dom(X), not flagged(X)  — stratifiable, but negation +
        // a firing EGD is refused.
        p.rule(atom("q", vec![v(0)]),
               vec![pos(atom("dom", vec![v(0)])), neg(atom("flagged", vec![v(0)]))]);
        assert_eq!(p.evaluate(), Err(ModelError::Unstratifiable));
    }

    // -- GATE: builtin chain-1000 — reachability answers under a fraction of
    //    the tuple budget (linear BFS, NOT the quadratic closure). ----------

    #[test]
    fn builtin_chain_1000_reachability_under_budget() {
        use crate::semantics::caches::test_support::kif_layer;
        let mut kif = String::from("(instance r TransitiveRelation)\n");
        for i in 0..1000 {
            kif.push_str(&format!("(r n{} n{})\n", i, i + 1));
        }
        let sem = kif_layer(&kif);
        let mp = ModelProgram::build(&sem.syntactic);
        assert!(
            mp.program.builtin_transitive.contains_key(&s("r")),
            "declared transitive relation is builtin"
        );
        assert!(
            !mp.program.rules.iter().any(|r| r.head.pred == s("r")),
            "no transitivity schema rule emitted"
        );

        let mut stats = ModelStats::default();
        let (rows, prov) = mp
            .answer_stats(s("r"), &[DTerm::Const(s("n0")), DTerm::Const(s("n1000"))], None, &mut stats)
            .expect("reachability answers");
        assert_eq!(rows, vec![vec![s("n0"), s("n1000")]]);
        assert!(
            stats.budget_used < 10_000,
            "budget consumption must be linear (BFS), not the ~500k closure: {}",
            stats.budget_used
        );

        // The BUILTIN citation: the declaring sid + every chain edge.
        let decl = find_fact(&sem.syntactic, "instance", "r", "TransitiveRelation");
        let cited = mp.cite(&prov, s("r"), &vec![s("n0"), s("n1000")]);
        assert!(cited.contains(&decl), "TransitiveRelation declaration cited");
        let e0 = find_fact(&sem.syntactic, "r", "n0", "n1");
        let e999 = find_fact(&sem.syntactic, "r", "n999", "n1000");
        assert!(cited.contains(&e0), "first chain edge cited");
        assert!(cited.contains(&e999), "last chain edge cited");
        assert_eq!(cited.len(), 1001, "1000 edges + the declaration, nothing else");
    }

    // -- Builtin citation chain, small and exact (the report example). ------

    #[test]
    fn builtin_three_edge_chain_citation_exact() {
        use crate::semantics::caches::test_support::kif_layer;
        let kif = "\
            (instance r TransitiveRelation)\n\
            (r a b)\n\
            (r b c)\n\
            (r c d)\n";
        let sem = kif_layer(kif);
        let mp = ModelProgram::build(&sem.syntactic);
        let mut stats = ModelStats::default();
        let (rows, prov) = mp
            .answer_stats(s("r"), &[DTerm::Const(s("a")), DTerm::Var(0)], None, &mut stats)
            .expect("closure answers");
        let mut got: Vec<Tuple> = rows.clone();
        got.sort();
        let mut want = vec![
            vec![s("a"), s("b")],
            vec![s("a"), s("c")],
            vec![s("a"), s("d")],
        ];
        want.sort();
        assert_eq!(got, want, "forward closure from a");

        let decl = find_fact(&sem.syntactic, "instance", "r", "TransitiveRelation");
        let e1 = find_fact(&sem.syntactic, "r", "a", "b");
        let e2 = find_fact(&sem.syntactic, "r", "b", "c");
        let e3 = find_fact(&sem.syntactic, "r", "c", "d");
        let cited = mp.cite(&prov, s("r"), &vec![s("a"), s("d")]);
        assert_eq!(cited, vec![e1, e2, e3, decl], "path edges in order, then the declaration");
    }

    // -- Builtin body-literal resolution inside the kernel (a rule reads the
    //    closure through BFS), reverse orientation included. ----------------

    #[test]
    fn builtin_body_literal_resolves_closure_in_kernel() {
        use crate::semantics::caches::test_support::kif_layer;
        let kif = "\
            (instance r TransitiveRelation)\n\
            (r a b)\n\
            (r b c)\n\
            (=> (r ?X ?Y) (linked ?X ?Y))\n";
        let sem = kif_layer(kif);
        let mp = ModelProgram::build(&sem.syntactic);

        // Forward: goal linked(a, ?) — magic binds X=a, the r-literal
        // BFS-resolves the closure.
        let rows = mp
            .answer(s("linked"), &[DTerm::Const(s("a")), DTerm::Var(0)], None)
            .expect("linked answers");
        assert!(rows.contains(&vec![s("a"), s("c")]), "closure through the body literal: {rows:?}");

        // Reverse: goal linked(?, c) — the r-literal resolves bound-right.
        let rows = mp
            .answer(s("linked"), &[DTerm::Var(0), DTerm::Const(s("c"))], None)
            .expect("linked answers (reverse)");
        assert!(rows.contains(&vec![s("a"), s("c")]), "reverse closure: {rows:?}");

        // Citation of the closure-derived head: the rule root + both edges +
        // the declaring sid.
        let mut stats = ModelStats::default();
        let (_, prov) = mp
            .answer_stats(s("linked"), &[DTerm::Const(s("a")), DTerm::Const(s("c"))], None, &mut stats)
            .expect("ground goal answers");
        let cited = mp.cite(&prov, s("linked"), &vec![s("a"), s("c")]);
        let decl = find_fact(&sem.syntactic, "instance", "r", "TransitiveRelation");
        let e1 = find_fact(&sem.syntactic, "r", "a", "b");
        let e2 = find_fact(&sem.syntactic, "r", "b", "c");
        let rule_root = sem
            .syntactic
            .root_sids()
            .into_iter()
            .find(|sid| {
                sem.syntactic
                    .sentence(*sid)
                    .is_some_and(|x| x.op() == Some(&crate::types::OpKind::Implies))
            })
            .expect("rule root");
        for (sid, what) in [
            (e1, "edge (r a b)"), (e2, "edge (r b c)"),
            (decl, "TransitiveRelation declaration"), (rule_root, "the => rule"),
        ] {
            assert!(cited.contains(&sid), "head citation missing {what}: {cited:?}");
        }
    }

    // -- Builtin rels are EDB-class nodes: no rules, stratifiable clusters,
    //    never complete/certified (closest-sound rendering of the spec's
    //    "un-tainting" test — removing a POSITIVE SELF-LOOP provably cannot
    //    change SCC badness or taint reachability, so the literal
    //    negation-cycle claim is vacuous; what Part 2 actually changes is
    //    that the relation contributes NO rules at all, which this pins). ---

    // -- GATE 3 (headline): the full-SUMO dense-cone case.  Previously the
    //    `instance`-cone evaluation (the refutes/answer machinery, unsafe
    //    rules dropped) materialized the transitive closures and overflowed
    //    at ~4M tuples / 60s under raised SIGMA_MODEL_BUDGET/SIGMA_MODEL_MS
    //    (negatives-package finding).  With BUILT-IN transitive closure the
    //    same query must complete under the DEFAULT 250k budget.
    //
    //    #[ignore]: needs the local SUMO checkout (~/.sigmakee/config.xml
    //    constituents).  Run with:
    //      cargo test --release -p sigmakee-rs-core --features native-prover \
    //        --lib headline_full_sumo -- --ignored --nocapture
    #[test]
    #[ignore]
    fn headline_full_sumo_instance_cone_under_default_budget() {
        use crate::semantics::caches::test_support::kif_layer;
        use std::time::Instant;

        // Load the full KB from the config's constituent list.
        let home = std::env::var("HOME").unwrap_or_default();
        let cfg_path = format!("{home}/.sigmakee/config.xml");
        let Ok(cfg) = std::fs::read_to_string(&cfg_path) else {
            eprintln!("HEADLINE skipped: {cfg_path} not found");
            return;
        };
        let kb_dir = format!("{home}/projects/sumo");
        let mut kif = String::new();
        let mut n_files = 0usize;
        for chunk in cfg.split("constituent filename=\"").skip(1) {
            let Some(end) = chunk.find('"') else { continue };
            let f = &chunk[..end];
            if let Ok(text) = std::fs::read_to_string(format!("{kb_dir}/{f}")) {
                kif.push_str(&text);
                kif.push('\n');
                n_files += 1;
            }
        }
        assert!(n_files > 10, "expected the full constituent list, got {n_files}");
        let t0 = Instant::now();
        let sem = kif_layer(&kif);
        let mp = ModelProgram::build(&sem.syntactic);
        eprintln!(
            "HEADLINE: {n_files} constituents loaded+built in {:?}; monotone {} rules / {} edb \
             facts; builtin_transitive={} egds={}",
            t0.elapsed(),
            mp.monotone.rules.len(),
            mp.monotone.edb.values().map(HashSet::len).sum::<usize>(),
            mp.monotone.builtin_transitive.len(),
            mp.monotone.egds.len(),
        );

        // A concrete instance-membership query subject: prefer UnitedStates,
        // else any EDB instance tuple's member (deterministic: smallest id).
        let (subj, direct_class) = {
            let us = s("UnitedStates");
            let rows = &mp.monotone.edb[&mp.roles.instance];
            let picked = rows
                .iter()
                .filter(|r| r.len() == 2)
                .find(|r| r[0] == us)
                .cloned()
                .or_else(|| {
                    rows.iter()
                        .filter(|r| r.len() == 2)
                        .min_by_key(|r| r[0])
                        .cloned()
                })
                .expect("instance EDB nonempty");
            (picked[0], picked[1])
        };
        // An INDIRECT ancestor of the subject's direct class — so the ground
        // query below must climb the (built-in) subclass closure to answer.
        let anc = {
            let mut st = ModelStats::default();
            let (rows, _) = mp
                .answer_stats_impl(
                    mp.roles.subclass,
                    &[DTerm::Const(direct_class), DTerm::Var(0)],
                    None,
                    &mut st,
                    true,
                )
                .expect("subclass ancestors answer");
            rows.iter()
                .filter(|r| r.len() == 2)
                .map(|r| r[1])
                .max()
                .unwrap_or(direct_class)
        };

        // Optional: unbounded ground-query probe (SIGMA_HEADLINE_DIAG=1).
        if std::env::var_os("SIGMA_HEADLINE_DIAG").is_some() {
            let mut st = ModelStats::default();
            let t = Instant::now();
            let r = mp.answer_stats_impl(
                mp.roles.instance,
                &[DTerm::Const(subj), DTerm::Const(anc)],
                None,
                &mut st,
                true,
            );
            eprintln!(
                "HEADLINE diag unbounded GROUND: answered={} rows={} budget_used={} elapsed={:?}",
                r.is_some(),
                r.as_ref().map_or(0, |(x, _)| x.len()),
                st.budget_used,
                t.elapsed(),
            );
        }

        // AFTER (builtin closures): the GROUND instance-membership query —
        // the shape a CSR/plain conjecture routes through discharge_models —
        // through the refutes/answer machinery (unsafe rules dropped),
        // DEFAULT budget + the production 800ms deadline.
        let mut stats = ModelStats::default();
        let deadline = Some(Instant::now() + std::time::Duration::from_millis(800));
        let t1 = Instant::now();
        let after = mp.answer_stats_impl(
            mp.roles.instance,
            &[DTerm::Const(subj), DTerm::Const(anc)],
            deadline,
            &mut stats,
            true,
        );
        eprintln!(
            "HEADLINE after (builtin, ground instance membership): answered={} rows={} \
             budget_used={} elapsed={:?} bails(unsafe/unstrat/budget)=({}, {}, {})",
            after.is_some(),
            after.as_ref().map_or(0, |(r, _)| r.len()),
            stats.budget_used,
            t1.elapsed(),
            stats.unsafe_bails, stats.unstratifiable_bails, stats.budget_overflows,
        );
        // The goal-side subclass closure (the other half of `refutes`).
        let mut stats_sub = ModelStats::default();
        let deadline = Some(Instant::now() + std::time::Duration::from_millis(800));
        let t2 = Instant::now();
        let sub_after = mp.answer_stats_impl(
            mp.roles.subclass,
            &[DTerm::Const(s("Human")), DTerm::Var(0)],
            deadline,
            &mut stats_sub,
            true,
        );
        eprintln!(
            "HEADLINE after (subclass Human cone): answered={} rows={} budget_used={} elapsed={:?}",
            sub_after.is_some(),
            sub_after.as_ref().map_or(0, |(r, _)| r.len()),
            stats_sub.budget_used,
            t2.elapsed(),
        );

        // Optional diagnostics (SIGMA_HEADLINE_DIAG=1): the MEMBER-BOUND
        // all-memberships query `instance(subj, ?Y)` — the direction that
        // historically overflowed at 4M tuples / 60s.  Built-ins + the
        // bound-first SIPS improve it substantially but it can still exceed
        // the default budget: its magic demand crosses rules with
        // class×class cross-product prefixes (quadratic DEMAND pairs),
        // which no transitivity treatment can remove.  Reported, not gated.
        if std::env::var_os("SIGMA_HEADLINE_DIAG").is_some() {
            let mut st = ModelStats::default();
            let t = Instant::now();
            let r = mp.answer_stats_impl(
                mp.roles.instance,
                &[DTerm::Const(subj), DTerm::Var(0)],
                None,
                &mut st,
                true,
            );
            eprintln!(
                "HEADLINE diag member-bound instance: answered={} rows={} budget_used={} elapsed={:?}",
                r.is_some(),
                r.as_ref().map_or(0, |(x, _)| x.len()),
                st.budget_used,
                t.elapsed(),
            );
        }

        // BEFORE (schema-rule materialization, simulated on the same build):
        // put the transitivity schema rules back, clear the builtin set, and
        // run the identical GROUND query — the dense-closure materialization
        // must blow the default budget (the negatives-package finding).
        let mut before_mp = ModelProgram {
            program: mp.program.clone(),
            clusters: mp.clusters.clone(),
            monotone: mp.monotone.clone(),
            complete: mp.complete.clone(),
            certified: mp.certified.clone(),
            cert_blocked: mp.cert_blocked,
            roles: mp.roles.clone(),
            denials: mp.denials.clone(),
        };
        let extra: Vec<(Pred, Option<SentenceId>)> = before_mp
            .monotone
            .builtin_transitive
            .iter()
            .map(|(r, sid)| (*r, *sid))
            .collect();
        for r in extract::schema_rules(&extract::RoleDecls::default(), &extra) {
            before_mp.monotone.rules.push(r);
        }
        before_mp.monotone.builtin_transitive.clear();
        let mut stats_b = ModelStats::default();
        let deadline = Some(Instant::now() + std::time::Duration::from_millis(800));
        let t3 = Instant::now();
        let before = before_mp.answer_stats_impl(
            mp.roles.instance,
            &[DTerm::Const(subj), DTerm::Const(anc)],
            deadline,
            &mut stats_b,
            true,
        );
        eprintln!(
            "HEADLINE before (schema rules, same ground query): answered={} \
             budget_used={} budget_overflows={} elapsed={:?}",
            before.is_some(),
            stats_b.budget_used,
            stats_b.budget_overflows,
            t3.elapsed(),
        );
        // The PURE Part-2 pair: the subclass-ancestor cone, before vs after.
        let mut stats_bs = ModelStats::default();
        let deadline = Some(Instant::now() + std::time::Duration::from_millis(800));
        let t4 = Instant::now();
        let before_sub = before_mp.answer_stats_impl(
            mp.roles.subclass,
            &[DTerm::Const(s("Human")), DTerm::Var(0)],
            deadline,
            &mut stats_bs,
            true,
        );
        eprintln!(
            "HEADLINE before (schema rules, subclass Human cone): answered={} rows={} \
             budget_used={} budget_overflows={} shallow_retries={} elapsed={:?}",
            before_sub.is_some(),
            before_sub.as_ref().map_or(0, |(r, _)| r.len()),
            stats_bs.budget_used,
            stats_bs.budget_overflows,
            stats_bs.shallow_retries,
            t4.elapsed(),
        );

        assert!(
            after.is_some(),
            "builtin closure must answer the ground instance-membership query under \
             the default 250k/800ms budget (bails: {stats:?})"
        );
        assert_eq!(
            after.as_ref().map(|(r, _)| r.len()),
            Some(1),
            "the derived membership row is returned"
        );
        assert!(
            sub_after.is_some(),
            "builtin closure must answer the subclass-ancestor query under the \
             default budget (bails: {stats_sub:?})"
        );
    }

    #[test]
    fn builtin_rel_is_edb_class_node_and_never_certified() {
        use crate::semantics::caches::test_support::kif_layer;
        let kif = "\
            (instance r TransitiveRelation)\n\
            (r a b)\n\
            (r b c)\n\
            (=> (r ?X ?Y) (linked ?X ?Y))\n\
            (=> (and (thing ?X) (not (linked ?X ?X))) (loner ?X))\n\
            (thing a)\n";
        let sem = kif_layer(kif);
        let mp = ModelProgram::build(&sem.syntactic);

        // (a) EDB-class node: r heads no rule anywhere in the program.
        assert!(mp.program.builtin_transitive.contains_key(&s("r")));
        assert!(
            !mp.program.rules.iter().any(|r| r.head.pred == s("r")),
            "builtin relation contributes no rules"
        );

        // (b) every discovered cluster is stratifiable (evaluates clean).
        assert!(!mp.clusters.is_empty());
        for c in &mp.clusters {
            assert!(c.program.evaluate().is_ok(), "cluster must evaluate: {:?}", c.preds);
        }

        // (c) never complete/certified — its stored extension is the
        // un-closed base — and the body fixpoint decertifies dependents.
        assert!(!mp.complete.contains(&s("r")), "builtin not complete");
        assert!(!mp.certified.contains(&s("r")), "builtin not certified");
        assert!(!mp.certified.contains(&s("linked")), "dependent decertified (body fixpoint)");
        assert!(mp.cert_blocked.builtin >= 1, "{:?}", mp.cert_blocked);
    }
}
