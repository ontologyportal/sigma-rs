// crates/core/src/prover/saturate/prover/discharge.rs
//
// Whole-goal semantic discharge passes, run once before the given-clause
// loop (see `NativeProver::run`): Horn-rule joins, discrete event
// calculus, inductive-model (Datalog-ish) discharge, and goal-directed
// backward chaining.  Every pass but the first is gated by its own env
// var and a no-op when unset, so the saturation baseline stays
// byte-identical unless the corresponding SIGMA_* flag is set.
// `discharge_horn_joins` is the exception: it runs BY DEFAULT, gated by a
// cheap per-goal applicability guard (see its doc comment) rather than an
// env var, because it has a measured win (the "jail" proof) that a
// blanket flag can't safely ship default-on.  `SIGMA_RULE_JOIN=1` forces
// it on unconditionally and `SIGMA_NO_RULE_JOIN=1` forces it off
// unconditionally, for A/B testing and backward compat.

use std::collections::{HashMap, HashSet};
use crate::clock::Instant;

use crate::types::{Element, SentenceId, Symbol, SymbolId};

use super::super::clause::{AtomId, Term};
use super::super::oracle::Witness;
use super::super::theory::TheoryOracle;
use super::super::unify::slot_atom;
use super::{term_kif, NativeProver, CONJECTURE, SUPPORT};

impl<'a> NativeProver<'a> {
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
    /// Applicability is decided PER GOAL by a guard predicate
    /// (`guard_applicable`, computed below from the same scan that used to
    /// only drive `suppress_rules`) — `Strategy.rule_join` (default `true`)
    /// just gates whether the guard even runs: with it on, the pass fires
    /// by default exactly on the Horn-chain / conclusion-rule goal shape it
    /// wins on (e.g. the "jail" proof) while staying inert everywhere else,
    /// so the saturation baseline is byte-identical off the guarded path;
    /// `false` (`SIGMA_NO_RULE_JOIN`) disables the pass unconditionally —
    /// the safety valve.
    pub(crate) fn discharge_horn_joins(&mut self) {
        if !self.opts.strategy.rule_join {
            return;
        }
        let cov = self.oracle.coverage();
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
            if cov.owns(head_rel) {
                continue;
            }
            if self.head_has_visible_fact(head_rel) {
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
                        if !cov.owns(r) {
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
                        if !cov.owns(r) {
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
                            if cov.owns(r) { "[theory]" }
                            else if !self.head_has_visible_fact(r) { "[nofacts]" }
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
                Some((r, _)) => cov.owns(r) || facts.contains_key(&r),
                None => false,
            })
        });

        // PER-GOAL APPLICABILITY GUARD (default-on path): lifts the
        // suppression check above into a positive predicate instead of a
        // disjoint one.  `suppress_rules` already recognises exactly the
        // failure mode that hurts blanket activation — a genuine
        // ground-answerable QA query, where every conjunct is theory- or
        // fact-backed and the conclusion-rule heads (whose relations have
        // NO store facts, by construction of the `rules` scan above) are
        // irrelevant noise that floods the search.  The mirror-image
        // condition is a real win: at least one Horn-chain / conclusion-rule
        // goal is present (`!queries.is_empty()`, the negated existential of
        // a goal like `¬goesToJail`), there is at least one conclusion rule
        // able to fire (`!rules.is_empty()` — a rule head that itself has no
        // asserted facts, i.e. a derived-only relation), and NO genuine
        // fact-query goal is mixed in to get flooded (`!suppress_rules`).
        // This is exactly the jail-proof shape and exactly the shape the
        // existing suppression logic already carves out as safe.
        let guard_applicable = !rules.is_empty() && !queries.is_empty() && !suppress_rules;
        if trace {
            eprintln!(
                "RULE-JOIN scan: {} generator relations, {} ground facts, {} conclusion rules, \
                 {} queries, suppress_rules={}, guard_applicable={}",
                facts.len(),
                facts.values().map(Vec::len).sum::<usize>(),
                rules.len(),
                queries.len(),
                suppress_rules,
                guard_applicable,
            );
        }
        if !guard_applicable {
            return;
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
                    &cov,
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
                    &cov,
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
                    if self.seen_insert(key, id) {
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
    /// read the narrative into effect tables, run it through the GENERIC
    /// Datalog(¬) model kernel (`narrative_to_program` → `Program::evaluate`
    /// → perfect model — the same engine `discharge_models`/
    /// `discharge_model_joins` use for the ontology), reconstruct the
    /// complete fluent×time grid over the model's `holdsAt` relation, and
    /// emit each `(fluent, time)` as a ground `holdsAt` / `~holdsAt` unit.
    /// Those units resolve directly against the (negated) conjecture — a
    /// decision procedure standing in for the frame-axiom search.
    /// Complete-state (closed-world: a grid cell absent from the model's
    /// `holdsAt` relation is false) means negative `holdsAt` queries are
    /// decided too, matching DEC7 negative inertia.
    ///
    /// The bespoke forward simulator (`eventcalc::simulate`) this once
    /// cross-checked against has been retired — the kernel path is now the
    /// ONLY path (formerly gated behind `SIGMA_EC_MODEL`, which no longer
    /// exists); see `model::tests::ec_kernel_holds_grid` for the golden-grid
    /// regression that replaced the parity cross-check.
    pub(crate) fn discharge_event_calculus(&mut self) {
        if !self.opts.ec {
            return;
        }
        let trace = std::env::var_os("SIGMA_ORACLE_TRACE").is_some();
        let Some((nar, names)) =
            super::super::eventcalc::parse_narrative(self.syn(), self.scope) else {
            return;
        };
        let holds_at = Symbol::from("holdsAt");
        let holds_pred = holds_at.id();
        let prog = super::super::model::narrative_to_program(&nar);
        let Ok((model, prov)) = prog.evaluate_within(usize::MAX, None) else {
            if trace { eprintln!("EC: program not stratified/safe — bailing"); }
            return;
        };
        let rel = model.get(&holds_pred).cloned().unwrap_or_default();
        // Reconstruct complete state over the fluent×time grid (closed-world:
        // a cell absent from the relation is false).
        let fluents: HashSet<SymbolId> = nar.initiates.iter()
            .chain(nar.terminates.iter())
            .map(|e| e.fluent)
            .chain(nar.initial.keys().copied())
            .collect();
        let mut state: HashMap<(SymbolId, SymbolId), bool> = HashMap::new();
        for &f in &fluents {
            for &t in &nar.times {
                state.insert((f, t), rel.contains(&vec![f, t]));
            }
        }
        if trace {
            eprintln!(
                "EC: {} times, {} initiates, {} terminates, {} state cells (kernel)",
                nar.times.len(), nar.initiates.len(), nar.terminates.len(), state.len(),
            );
        }
        // Provenance: a positive cell cites the model's real derivation
        // (EDB leaves + rule sids `prov.cite` reconstructs — the
        // `happens`/`initiates`/`terminates` facts and the effect rules that
        // produced it).  A NEGATIVE cell is a closed-world absence — no
        // derivation to walk — so it cites the narrative's defining
        // only-if roots instead (the axioms whose completeness licenses the
        // closed-world assumption).
        let neg_parents: Vec<SentenceId> = [nar.happens_sid, nar.initiates_sid, nar.terminates_sid]
            .into_iter()
            .flatten()
            .collect();
        // Emit each state cell as a ground `holdsAt` / `~holdsAt` unit.  Each
        // is BOTH queued for selection (`push`) and indexed as a resolution /
        // unit-simplification partner (`activate`) — so the complementary
        // conjecture literal is discharged whichever clause the given-clause
        // loop reaches first.
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
            let fact_parents = if holds {
                prov.cite(holds_pred, &vec![fluent, time])
            } else {
                neg_parents.clone()
            };
            if let Some(id) =
                self.make(vec![(holds, atom)], Vec::new(), "event_calculus", SUPPORT, None, true)
            {
                self.clauses[id as usize].fact_parents.extend(fact_parents);
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
        if !self.opts.model {
            return;
        }
        self.discharge_models_forced();
    }

    /// The body of [`discharge_models`](Self::discharge_models) without the
    /// `SIGMA_MODEL` env gate — direct entry for tests (env mutation is
    /// process-global and races parallel tests).  Production callers go
    /// through the gated wrapper above.
    pub(crate) fn discharge_models_forced(&mut self) {
        let trace = std::env::var_os("SIGMA_ORACLE_TRACE").is_some();
        let mp = self.layer.model_program();

        // Certification bookkeeping (SIGMA_STATS): the registry's build-time
        // certified set and blocked-reason breakdown, recorded once per run.
        self.stats.model_certified_relations = mp.certified.len() as u64;
        self.stats.model_cert_blocked_skipped_head = u64::from(mp.cert_blocked.skipped_head);
        self.stats.model_cert_blocked_unstratifiable =
            u64::from(mp.cert_blocked.unstratifiable);
        self.stats.model_cert_blocked_body_chain = u64::from(mp.cert_blocked.body_chain);
        self.stats.model_cert_blocked_role = u64::from(mp.cert_blocked.role);

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
        // Clark-completion negatives can apply even when the MONOTONE
        // fragment defines no goal relation (a certified relation may be
        // defined only through negation-carrying rules), so the early bail
        // also checks for a certified goal relation.
        let has_certified_goal = goal_preds.iter().any(|r| mp.certified.contains(r));
        if !defines && !has_certified_goal {
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
        // resolution proceeds.  `opts.model_ms` (`SIGMA_MODEL_MS`) overrides
        // the default 800ms (diagnosis / experimentation on dense KBs).
        let deadline = Instant::now() + std::time::Duration::from_millis(self.opts.model_ms);
        let mut to_emit: Vec<(SymbolId, Vec<SymbolId>, Vec<SentenceId>)> = Vec::new();
        let mut model_stats = super::super::model::ModelStats::default();
        if defines {
            for (rel, args) in &patterns {
                let Some(dargs) = self.bridge_dargs(args, &mut model_stats) else {
                    self.stats.model_atoms_rejected += 1;
                    continue;
                };
                let answered = mp.answer_stats(*rel, &dargs, Some(deadline), &mut model_stats, self.opts.model_budget);
                if let Some((rows, prov)) = answered {
                    self.stats.model_atoms_answered += 1;
                    for row in rows {
                        // The KB sentences (EDB facts, then rules) this answer's
                        // derivation used — cited on the emitted unit below, the
                        // same way oracle witness sids are.
                        let cited = mp.cite(&prov, *rel, &row);
                        to_emit.push((*rel, row, cited));
                    }
                } else {
                    self.stats.model_atoms_unanswered += 1;
                }
            }
        }
        // NEGATIVE decisions (sub-milestone B): denial-constraint refutation
        // of ground `instance`-shaped atoms the negated conjecture carries
        // POSITIVELY (the goal asks to prove `¬(instance x C)`).  Mirrors the
        // oracle path's `refutes_instance` consumption in `make`: emit a
        // negative ground unit `~(instance x C)` whose `fact_parents` are the
        // full citation chain (instance derivation + subclass chains + the
        // denial declaration), tagged `model_refute`.  It resolves against
        // the positive conjecture literal, collapsing the goal — exactly the
        // verdict the oracle gives, but chased through the generic model.
        // `refutes` only reports when both closure queries materialized
        // fully within budget (no mid-chain bail), so the decision is sound.
        let mut to_refute: Vec<(Term, super::super::model::ModelRefutation)> = Vec::new();
        if !mp.denials.is_empty() {
            let mut seen_refute: HashSet<AtomId> = HashSet::new();
            for c in &self.clauses {
                if c.tier != CONJECTURE {
                    continue;
                }
                for l in &c.lits {
                    if !l.pos {
                        continue; // only atoms the negated conjecture holds positively
                    }
                    if !seen_refute.insert(l.atom) {
                        continue;
                    }
                    let Some(t) = slot_atom(&self.layer.atoms, self.syn(), l.atom, 0) else {
                        continue;
                    };
                    let Some((rel, args)) = lit_pattern(&t) else { continue };
                    if rel != mp.roles.instance || args.len() != 2 {
                        continue; // instance-shaped ground atoms only
                    }
                    let (Some(x), Some(cc)) = (sym_of(&args[0]), sym_of(&args[1])) else {
                        continue;
                    };
                    let Some(r) =
                        mp.refutes(rel, &[x, cc], Some(deadline), &mut model_stats, self.opts.model_budget)
                    else {
                        continue;
                    };
                    if trace {
                        // Cross-check against the taxonomy oracle (ground
                        // truth for disjointness refutations) — any
                        // disagreement is a soundness flag to investigate.
                        let oracle_agrees = self.oracle.refutes_instance(rel, x, cc, None);
                        let chain: Vec<String> = r
                            .cited
                            .iter()
                            .map(|sid| {
                                self.layer
                                    .atoms
                                    .term_of(*sid, self.syn())
                                    .map(|ct| term_kif(&ct, self.syn()))
                                    .unwrap_or_else(|| format!("sid:{sid}"))
                            })
                            .collect();
                        eprintln!(
                            "MODEL-REFUTE ~{} via member {} ⊓ ancestor {} oracle_refutes={} chain:\n    {}",
                            term_kif(&t, self.syn()),
                            self.syn().sym_name(r.member).map(|s| s.to_string()).unwrap_or_default(),
                            self.syn().sym_name(r.goal_ancestor).map(|s| s.to_string()).unwrap_or_default(),
                            oracle_agrees,
                            chain.join("\n    "),
                        );
                    }
                    to_refute.push((t, r));
                }
            }
        }

        // COMPLETE (Clark-certified) negative decisions: for a
        // conjecture-tier ground flat atom `R(args)` that the NEGATED
        // conjecture holds POSITIVELY (the original conjecture asked to
        // prove `¬R(args)` — the same polarity reading as the denial path
        // above) with `R` COMPLETION-CERTIFIED: evaluate R's full cone
        // (`complete_absent` — no magic, no unsafe-rule dropping); if the
        // tuple is ABSENT and the evaluation completed without ANY bail,
        // the Clark completion of R's certified definition licenses the
        // negative.  Emitted below as a negative ground unit tagged
        // `model_complete`, `fact_parents` = every defining rule sid of
        // R's cone (the completion citation).  Denial refutations never
        // overlap (they cover only `instance`, a role — never certified);
        // EC-emitted negatives dedup through `make` like any duplicate.
        // SEMANTICS GUARD: a certified negative is licensed by the Clark
        // COMPLETION of R's definition, which is an assumption ON TOP of
        // the KB unless the only-if axioms are literally present (the EC
        // case).  That is the right semantics for KIF/SUMO query answering
        // (SigmaKEE's closed-world reading) but NOT for classical-verdict
        // regimes: under `strict_saturation` (the TPTP path, where a
        // confident verdict must be a classical entailment) emission is
        // disabled until certification also verifies the completion axioms
        // exist in the KB (condition (e), future work).
        let cwa_ok = !self.opts.strategy.strict_saturation;
        let mut to_complete: Vec<(Term, Vec<SentenceId>)> = Vec::new();
        if has_certified_goal && cwa_ok {
            let mut seen_complete: HashSet<AtomId> = HashSet::new();
            for c in &self.clauses {
                if c.tier != CONJECTURE {
                    continue;
                }
                for l in &c.lits {
                    if !l.pos {
                        continue; // only atoms the negated conjecture holds positively
                    }
                    if !seen_complete.insert(l.atom) {
                        continue;
                    }
                    let Some(t) = slot_atom(&self.layer.atoms, self.syn(), l.atom, 0) else {
                        continue;
                    };
                    let Some((rel, args)) = lit_pattern(&t) else { continue };
                    if !mp.certified.contains(&rel) {
                        continue;
                    }
                    // Ground flat atoms only: every argument a bare symbol.
                    let Some(tuple) = args.iter().map(sym_of).collect::<Option<Vec<SymbolId>>>()
                    else {
                        continue;
                    };
                    let Some(cited) =
                        mp.complete_absent(rel, &tuple, Some(deadline), &mut model_stats, self.opts.model_budget)
                    else {
                        continue;
                    };
                    if trace {
                        eprintln!(
                            "MODEL-COMPLETE ~{} (certified absence; {} defining rule sids cited)",
                            term_kif(&t, self.syn()),
                            cited.len(),
                        );
                    }
                    to_complete.push((t, cited));
                }
            }
        }
        self.merge_model_stats(&model_stats);

        let mut emitted = 0usize;
        for (rel, row, cited) in to_emit {
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
                self.clauses[id as usize].fact_parents.extend(cited);
                if trace && !self.clauses[id as usize].fact_parents.is_empty() {
                    let c = &self.clauses[id as usize];
                    eprintln!(
                        "MODEL emit [{}] {} fact_parents={:?}",
                        id,
                        c.terms.first().map(|(_, t)| term_kif(t, self.syn())).unwrap_or_default(),
                        c.fact_parents,
                    );
                }
                if self.push(Some(id)).is_some() {
                    emitted += 1;
                }
                self.activate(id);
            }
        }

        // Emit each denial refutation as a negative ground unit — the same
        // shape the oracle's `refutes_instance` discharge leaves behind: the
        // unit resolves against the positive conjecture literal, and its
        // `fact_parents` carry the full citation chain (leaf facts, chain
        // rules, denial declaration last).
        let mut emitted_neg = 0usize;
        for (t, r) in to_refute {
            if let Some(id) =
                self.make(vec![(false, t)], Vec::new(), "model_refute", SUPPORT, None, true)
            {
                self.clauses[id as usize].fact_parents.extend(r.cited);
                self.activate(id);
                if self.push(Some(id)).is_some() {
                    emitted_neg += 1;
                }
            }
        }

        // Emit each certified-completion negative the same way: a negative
        // ground unit resolving against the positive conjecture literal,
        // its `fact_parents` the completion citation (every defining rule
        // sid of the relation's cone).
        let mut emitted_complete = 0usize;
        for (t, cited) in to_complete {
            if let Some(id) =
                self.make(vec![(false, t)], Vec::new(), "model_complete", SUPPORT, None, true)
            {
                self.clauses[id as usize].fact_parents.extend(cited);
                self.activate(id);
                if self.push(Some(id)).is_some() {
                    emitted_complete += 1;
                }
                self.stats.model_complete_negatives_emitted += 1;
            }
        }
        if trace {
            eprintln!(
                "MODEL: {emitted} positive / {emitted_neg} refutation / {emitted_complete} \
                 completion-negative units emitted over {} goal relations",
                goal_preds.len(),
            );
        }
    }

    /// Bridge one conjecture atom's argument terms to model-side
    /// [`DTerm`](super::super::model::DTerm)s, FAITHFULLY: bare symbols
    /// become constants, and each distinct goal variable becomes its own
    /// `DTerm::Var(n)` — the SAME variable in two seats maps to the SAME
    /// index, so `ModelProgram::answer`'s row filter enforces the
    /// co-reference (goal `p(X, X)` cannot match tuple `(a, b)`).
    ///
    /// A compound (function) term or literal argument has no `DTerm`
    /// representation; wildcarding it would over-approximate the goal's
    /// instances — sound while answers were positive-emit-only, UNSOUND
    /// once they feed negative decisions.  Such atoms are REJECTED
    /// (`None`), counted into `ModelStats::bridge_rejected_atoms` and the
    /// prover's `model_arg_collapsed_compound` counter.
    fn bridge_dargs(
        &mut self,
        args: &[Term],
        ms:   &mut super::super::model::ModelStats,
    ) -> Option<Vec<super::super::model::DTerm>> {
        let mut var_ix: HashMap<SymbolId, u32> = HashMap::new();
        let mut out = Vec::with_capacity(args.len());
        for t in args {
            match t {
                Term::Sym(s) => out.push(super::super::model::DTerm::Const(s.id())),
                Term::Var(v) => {
                    let next = var_ix.len() as u32;
                    out.push(super::super::model::DTerm::Var(*var_ix.entry(*v).or_insert(next)));
                }
                _ => {
                    self.stats.model_arg_collapsed_compound += 1;
                    ms.bridge_rejected_atoms += 1;
                    return None;
                }
            }
        }
        Some(out)
    }

    /// Fold one discharge pass's [`ModelStats`](super::super::model::ModelStats)
    /// bail-reason breakdown into the prover's per-run counters (the
    /// `answered` count is tracked per-atom by the caller instead).
    fn merge_model_stats(&mut self, ms: &super::super::model::ModelStats) {
        self.stats.model_unsafe_bails += u64::from(ms.unsafe_bails);
        self.stats.model_unstratifiable_bails += u64::from(ms.unstratifiable_bails);
        self.stats.model_budget_or_deadline_overflows += u64::from(ms.budget_overflows);
        self.stats.model_undefined_relation += u64::from(ms.undefined_relation);
        self.stats.model_rigid_conflicts += u64::from(ms.rigid_conflicts);
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
        if !self.opts.model {
            return;
        }
        let trace = std::env::var_os("SIGMA_ORACLE_TRACE").is_some();
        let cov = self.oracle.coverage();
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
            let mut eqmap: HashMap<SymbolId, Term> = HashMap::new();
            for l in &c.lits {
                match slot_atom(&self.layer.atoms, self.syn(), l.atom, 0) {
                    Some(t) => {
                        self.stats.model_atoms_seen += 1;
                        // Negated var-var equality (the `∃X,Y … X=Y`
                        // conjecture shape, e.g. negatedAntonymPattern):
                        // pre-unify the variables and DROP the literal —
                        // once the join binds the unified variable to one
                        // witness, the dropped `X≠Y` instantiates to
                        // `w≠w` and resolves by reflexivity in the loop.
                        if let Term::App(es) = &t {
                            if es.len() == 3
                                && matches!(es[0], Term::Op(crate::parse::OpKind::Equal))
                            {
                                if let (Term::Var(a), Term::Var(b)) = (&es[1], &es[2]) {
                                    eqmap.insert(*b, Term::Var(*a));
                                    continue;
                                }
                            }
                        }
                        if lit_pattern(&t).is_some() {
                            lits.push(t);
                        } else {
                            self.stats.model_atoms_rejected += 1;
                            ok = false;
                            break;
                        }
                    }
                    None => { ok = false; break; }
                }
            }
            if !eqmap.is_empty() {
                // Apply twice: bounded handling for chained pairs (X=Y, Y=Z).
                lits = lits.iter().map(|t| subst(t, &eqmap)).collect();
                lits = lits.iter().map(|t| subst(t, &eqmap)).collect();
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
        // `NativeOpts::chase` (env `SIGMA_CHASE`): answer over the
        // bounded-chase model (existential witnesses from SUMO
        // inhabitation/frame TGDs) instead of the plain positive model.
        // Classically sound for this POSITIVE join path only;
        // certification / negative answers never see chased facts.
        let chase = self.opts.chase;
        let deadline = Instant::now() + std::time::Duration::from_millis(
            if chase { self.opts.chase_ms } else { 1500 });
        let max_facts_per_rel: usize = if chase { 250_000 } else { 50_000 };
        // Provenance of each materialization, in `provs`; a model-sourced
        // `JoinFact` records WHICH evaluation derived it (`FactSrc::Model`
        // index), so a satisfying join can cite the KB sentences behind every
        // model-derived conjunct (per-evaluation state — never cached on the
        // registry).
        let mut provs: Vec<super::super::model::Provenance> = Vec::new();
        let dbgt = std::env::var_os("SIGMA_MODEL_TRACE").is_some();
        let t0 = Instant::now();
        let materialized = if chase {
            mp.chase_model(self.syn(), &needed, Some(deadline))
        } else {
            mp.positive_model(Some(deadline))
        };
        if dbgt {
            eprintln!("[SIGMA_MODEL_TRACE] model_joins: materialize {:?}", t0.elapsed());
        }
        let t1 = Instant::now();
        let full_model = match materialized {
            Some((m, p)) => {
                provs.push(p);
                Some(m)
            }
            None => None,
        };
        if trace {
            match full_model.as_ref() {
                Some(m) => eprintln!(
                    "MODEL-JOIN: materialized {} preds / {} tuples (chase={chase})",
                    m.len(), m.values().map(|s| s.len()).sum::<usize>()
                ),
                None => eprintln!("MODEL-JOIN: full model bailed (chase={chase})"),
            }
        }
        let mut facts: HashMap<SymbolId, Vec<JoinFact>> = HashMap::new();
        for &rel in &needed {
            if cov.owns(rel) {
                continue;
            }
            let mut f = self.store_facts(rel);
            let mut seen: HashSet<Vec<SymbolId>> = f
                .iter()
                .filter_map(|jf| jf.args.iter().map(sym_of).collect())
                .collect();
            // (a) full model, when it materialized (provenance index 0).
            if let Some(model) = full_model.as_ref().and_then(|m| m.get(&rel)) {
                for row in model {
                    push_join_fact(self.syn(), &mut f, &mut seen, row, max_facts_per_rel, 0);
                }
            }
            // (b) per-atom demand-scoped answers, seeded on the conjuncts'
            //     constants — derives constant-bound IDB slices the full model
            //     bailed on.  Redundant (measured: ~10s of magic-cone
            //     evaluations) when the chase already materialized the model.
            if chase && full_model.is_some() {
                if !f.is_empty() {
                    facts.insert(rel, f);
                }
                continue;
            }
            for lits in &queries {
                for t in lits {
                    let Some((r, args)) = lit_pattern(t) else { continue };
                    if r != rel {
                        continue;
                    }
                    // Seed-only bridging: each non-constant seat gets its OWN
                    // fresh variable (pure per-position wildcard).  This is
                    // deliberately NOT the faithful `bridge_dargs` — the rows
                    // only SEED the generator fact map, and `match_args` in
                    // the join re-enforces compound shapes and repeated
                    // variables against the real pattern, so wildcard seeds
                    // stay sound while keeping the demand as wide as before.
                    // (A shared `Var(0)` here would now *enforce equality*
                    // across those seats under the answer filter, silently
                    // narrowing the seeded facts.)
                    let dargs: Vec<super::super::model::DTerm> = args
                        .iter()
                        .enumerate()
                        .map(|(i, a)| match a {
                            Term::Sym(s) => super::super::model::DTerm::Const(s.id()),
                            _ => super::super::model::DTerm::Var(i as u32),
                        })
                        .collect();
                    let mut model_stats = super::super::model::ModelStats::default();
                    let answered = mp.answer_stats(rel, &dargs, Some(deadline), &mut model_stats, self.opts.model_budget);
                    self.merge_model_stats(&model_stats);
                    if let Some((rows, prov)) = answered {
                        self.stats.model_atoms_answered += 1;
                        let pix = provs.len() as u32;
                        provs.push(prov);
                        for row in &rows {
                            push_join_fact(self.syn(), &mut f, &mut seen, row, max_facts_per_rel, pix);
                        }
                    } else {
                        self.stats.model_atoms_unanswered += 1;
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

        if dbgt {
            eprintln!("[SIGMA_MODEL_TRACE] model_joins: facts build {:?}", t1.elapsed());
        }
        // 3) Join each query; on the first satisfying binding, collect the
        //    ground conjuncts to emit.
        let t2 = Instant::now();
        let seat_idx = build_seat_index(&facts);
        if dbgt {
            eprintln!("[SIGMA_MODEL_TRACE] model_joins: seat index {:?}", t2.elapsed());
        }
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
                &cov,
                &mut sols,
                &mut budget,
            );
            if trace {
                eprintln!(
                    "MODEL-JOIN: query of {} atoms joined, {} solutions, budget left {budget}",
                    body.len(), sols.len()
                );
                for (rel, args) in &body {
                    let n = facts.get(rel).map_or(0, |v| {
                        v.iter()
                            .filter(|jf| {
                                jf.args.len() == args.len()
                                    && args.iter().zip(&jf.args).all(|(p, f)| match p {
                                        Term::Sym(_) => p == f,
                                        _ => true,
                                    })
                            })
                            .count()
                    });
                    eprintln!("MODEL-JOIN:   conjunct {rel:?}{args:?} pattern-matches {n}");
                }
            }
            if let Some(sol) = sols.first() {
                // Re-walk the satisfied conjuncts under the binding to gather
                // citations: store facts cite their sentence directly;
                // model-derived facts cite through their evaluation's
                // provenance (EDB leaves + rules — `cite`); ground binary
                // literals the oracle decided cite its witness facts.
                let mut fact_sids: Vec<SentenceId> = Vec::new();
                for (rel, args) in &body {
                    let sargs: Vec<Term> = args.iter().map(|a| subst(a, sol)).collect();
                    if let Some(jf) = facts
                        .get(rel)
                        .and_then(|v| v.iter().find(|jf| jf.args == sargs))
                    {
                        match jf.src {
                            FactSrc::Store(sid) => fact_sids.push(sid),
                            FactSrc::Emitted(_) => {}
                            FactSrc::Model(pix) => {
                                let tuple: Option<Vec<SymbolId>> =
                                    sargs.iter().map(sym_of).collect();
                                if let (Some(prov), Some(tuple)) =
                                    (provs.get(pix as usize), tuple)
                                {
                                    fact_sids.extend(mp.cite(prov, *rel, &tuple));
                                }
                            }
                        }
                        continue;
                    }
                    if sargs.len() == 2 {
                        if let (Some(x), Some(y)) = (sym_of(&sargs[0]), sym_of(&sargs[1])) {
                            let mut why: Vec<Witness> = Vec::new();
                            // Temporal fallback: interval facts (temporalPart …)
                            // live in the point network, not the taxonomy
                            // closure, and the join's use is scoped here.
                            if self.oracle.holds(*rel, x, y, Some(&mut why))
                                || self.oracle.temporal_holds(*rel, x, y, Some(&mut why))
                            {
                                fact_sids.extend(why.iter().filter_map(|w| w.sid));
                            }
                        }
                    }
                }
                // Dedup preserving order (each conjunct's citation stays
                // leaf-facts-first, rules after).
                let mut seen_sids: HashSet<SentenceId> = HashSet::new();
                fact_sids.retain(|s| seen_sids.insert(*s));
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
        if !self.opts.backward {
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
        let deadline = Instant::now() + std::time::Duration::from_millis(self.opts.backward_ms);
        let mut budget = self.opts.backward_nodes as usize;
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
                    // Model-derived facts never enter the rule-join generator
                    // map (only `discharge_model_joins` pushes them, and it
                    // cites through the evaluation provenance instead).
                    FactSrc::Model(_) => {}
                }
                continue;
            }
            // Otherwise a binary literal the oracle decided (taxonomy /
            // subrelation / transitive): cite its witness facts.
            if sargs.len() == 2 {
                if let (Some(x), Some(y)) = (sym_of(&sargs[0]), sym_of(&sargs[1])) {
                    let mut why: Vec<Witness> = Vec::new();
                    if self.oracle.holds(*rel, x, y, Some(&mut why))
                        || self.oracle.temporal_holds(*rel, x, y, Some(&mut why))
                    {
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
    /// Whether `rel` has at least one asserted fact VISIBLE to the asking
    /// scope — the scope-filtered form of `by_head_id(..).is_empty()`.
    /// The raw head index spans base AND every session's transients; an
    /// out-of-scope fact must not re-classify a rule head as generative
    /// for this scope (that would silently disable the whole join pass
    /// for a fact the asking session can never see).
    fn head_has_visible_fact(&self, rel: SymbolId) -> bool {
        let sessions = &self.syn().sessions;
        for sid in self.syn().by_head_id(&rel) {
            let owners = sessions.sessions_of(sid);
            if owners.is_empty()
                || sessions.is_axiom(sid)
                || matches!(self.scope,
                    crate::semantics::types::Scope::Session(id) if owners.contains(&id))
            {
                return true;
            }
        }
        false
    }

    fn store_facts(&self, rel: SymbolId) -> Vec<JoinFact> {
        let sessions = &self.syn().sessions;
        let mut out = Vec::new();
        for sid in self.syn().by_head_id(&rel) {
            // Scope filter: the raw head index spans base AND every
            // session's transient sentences; the join may only see the
            // asking scope (base, plus the current session's overlay).
            let owners = sessions.sessions_of(sid);
            let visible = owners.is_empty()
                || sessions.is_axiom(sid)
                || matches!(self.scope,
                    crate::semantics::types::Scope::Session(id) if owners.contains(&id));
            if !visible {
                continue;
            }
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
        cov: &super::super::theory::CoverageClaim,
        out: &mut Vec<HashMap<SymbolId, Term>>,
        budget: &mut usize,
    ) {
        if *budget == 0 {
            return;
        }
        // Wall/cancel poll: the budget alone does not bound the search —
        // it is only decremented on EMITTED solutions, so a zero-solution
        // cross-product over wide relations recurses freely.  This pass
        // runs in `run()`'s prologue where the loop-top poll can't help;
        // draining the budget aborts the whole pass cheaply (every
        // ancestor frame sees 0), and the anchored deadline keeps the
        // attempt's one budget covering it.
        if self.out_of_time() {
            *budget = 0;
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
                if !self.ground_lit_holds(*rel, &sargs, facts, seat_idx) {
                    return; // dead branch
                }
                let rest: Vec<usize> = pending.iter().copied().filter(|&x| x != li).collect();
                self.join_rec(body, &rest, binding, facts, seat_idx, cov, out, budget);
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
            if cov.owns(*rel) {
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
                        self.join_rec(body, &rest, &b2, facts, seat_idx, cov, out, budget);
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
                        self.join_rec(body, &rest, &b2, facts, seat_idx, cov, out, budget);
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
        seat_idx: &SeatIndex,
    ) -> bool {
        // Seat-indexed check when any argument keys into the index — a
        // full-relation scan here turns the join's ground checks
        // quadratic on SUMO-scale instance extensions (measured: the
        // chase's 35k-row `instance` extension × thousands of checks
        // blew the whole prologue deadline).
        let narrowed: Option<&Vec<u32>> = args.iter().enumerate().find_map(|(seat, a)| {
            seat_key(a).and_then(|k| seat_idx.get(&(rel, seat as u8, k)))
        });
        if let (Some(idxs), Some(v)) = (narrowed, facts.get(&rel)) {
            if idxs.iter().any(|&fi| {
                let jf = &v[fi as usize];
                jf.args.len() == args.len() && jf.args.iter().zip(args).all(|(a, b)| a == b)
            }) {
                return true;
            }
        } else if facts.get(&rel).is_some_and(|v| {
            v.iter().any(|jf| {
                jf.args.len() == args.len() && jf.args.iter().zip(args).all(|(a, b)| a == b)
            })
        }) {
            return true;
        }
        if args.len() == 2 {
            if let (Some(x), Some(y)) = (sym_of(&args[0]), sym_of(&args[1])) {
                return self.oracle.holds(rel, x, y, None)
                    || self.oracle.temporal_holds(rel, x, y, None);
            }
        }
        false
    }
}

// -- discharge-local free helpers -------------------------------------------

/// Lift a symbol-headed atom into `(relation id, argument terms)`.
/// `None` for variable / operator / non-`App` heads (the join only
/// dispatches on named relations).
fn lit_pattern(t: &Term) -> Option<(SymbolId, Vec<Term>)> {
    let Term::App(elems) = t else { return None };
    if elems.len() < 2 { return None; }
    let Term::Sym(h) = &elems[0] else { return None };
    Some((h.id(), elems[1..].to_vec()))
}

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

/// The symbol id of a bare-symbol term (`None` for variables, literals,
/// compounds).
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
    /// Derived by the inductive model (semi-naive evaluation) — the payload
    /// indexes the evaluation's [`Provenance`](super::super::model::Provenance)
    /// in the discharge pass's local `provs` list, through which `cite`
    /// reconstructs the KB sentences (EDB facts + rules) behind the fact.
    Model(u32),
}

/// A ground fact in the join's generator map, with its provenance.
#[derive(Clone)]
struct JoinFact {
    args: Vec<Term>,
    src:  FactSrc,
}

/// Push one model-derived ground tuple into a join generator relation as a
/// `JoinFact`, deduped and capped.  `prov_ix` names the evaluation the tuple
/// came from (an index into the caller's provenance list) so a satisfying
/// join can later cite the tuple's derivation.  A free function (rather than
/// a `NativeProver`-capturing closure) so `discharge_model_joins` can also
/// mutate `self.stats` for the SIGMA_STATS answer/bail counters in the same
/// scope without a borrow conflict.
fn push_join_fact(
    syn:     &crate::syntactic::SyntacticLayer,
    f:       &mut Vec<JoinFact>,
    seen:    &mut HashSet<Vec<SymbolId>>,
    row:     &[SymbolId],
    cap:     usize,
    prov_ix: u32,
) {
    if f.len() >= cap {
        return;
    }
    // Dedup on the raw id row — the old linear `f.iter().any(...)` scan was
    // quadratic (measured 11s for 82k chase-model rows).
    if !seen.insert(row.to_vec()) {
        return;
    }
    let aargs: Vec<Term> = row.iter().filter_map(|v| syn.sym_name(*v).map(Term::Sym)).collect();
    if aargs.len() == row.len() {
        f.push(JoinFact { args: aargs, src: FactSrc::Model(prov_ix) });
    }
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
#[cfg(test)]
fn is_theory_rel(
    rel: SymbolId,
    roles: &crate::semantics::roles::TaxonomyRoles,
    tids: &super::super::temporal::TemporalRelIds,
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

#[cfg(test)]
mod tests {
    use super::super::super::model::{DTerm, ModelStats};
    use super::super::super::ProverLayer;
    use super::super::NativeProver;
    use super::*;
    use crate::semantics::caches::test_support::kif_layer;
    use crate::semantics::types::Scope;

    // Milestone A: the conjecture-atom → model-goal bridge is faithful.
    // Repeated goal variables share one DTerm index (so `answer` can enforce
    // the co-reference), distinct variables stay distinct, and a compound
    // argument REJECTS the atom instead of wildcarding it.
    #[test]
    fn bridge_dargs_faithful_vars_and_compound_reject() {
        let layer = ProverLayer::new(kif_layer("(p a b)"));
        let mut prover = NativeProver::new(&layer, Scope::Base, Default::default());
        let mut ms = ModelStats::default();

        let x = Term::Var(Symbol::hash_name("?X"));
        let y = Term::Var(Symbol::hash_name("?Y"));
        let a = Term::Sym(Symbol::from("a"));

        // p(X, X): one variable, one index — used twice.
        let d = prover
            .bridge_dargs(&[x.clone(), x.clone()], &mut ms)
            .expect("symbol/var args bridge");
        assert_eq!(d[0], d[1], "same goal variable must share an index");

        // p(X, Y): distinct variables, distinct indices.
        let d = prover
            .bridge_dargs(&[x.clone(), y.clone()], &mut ms)
            .expect("symbol/var args bridge");
        assert_ne!(d[0], d[1], "distinct goal variables must not be conflated");

        // p(a, X): constants become Const.
        let d = prover
            .bridge_dargs(&[a.clone(), x.clone()], &mut ms)
            .expect("symbol/var args bridge");
        assert_eq!(d[0], DTerm::Const(a_id()), "bare symbol maps to Const");
        assert!(matches!(d[1], DTerm::Var(_)));

        // p(f(X)): compound argument — atom rejected and counted.
        let f = Term::App(vec![Term::Sym(Symbol::from("f")), x.clone()]);
        assert!(
            prover.bridge_dargs(&[f], &mut ms).is_none(),
            "compound arg must reject the atom for model discharge"
        );
        assert_eq!(ms.bridge_rejected_atoms, 1, "rejection is counted");
    }


    // The NEW coverage() surface must claim EXACTLY the relation set the
    // legacy hardcoded `is_theory_rel` role/temporal lists encoded, and
    // exactly the omission list `decomposition_meaning_axioms` returned —
    // the zero-behavior-change proof for the coverage rewiring.
    #[test]
    fn coverage_equals_legacy_is_theory_rel_lists() {
        use super::super::super::temporal::TemporalRelIds;
        use super::super::super::theory::TheoryOracle;

        let layer = ProverLayer::new(kif_layer("(instance Fido Dog)"));
        let prover = NativeProver::new(&layer, Scope::Base, Default::default());
        let cov = prover.oracle.coverage();
        let roles = prover.oracle.roles();
        let tids = TemporalRelIds::standard();

        // Every claimed relation is one the legacy predicate owned…
        for c in &cov.claims {
            assert!(
                is_theory_rel(c.rel, &roles, &tids),
                "coverage claims a relation the legacy lists never owned"
            );
        }
        // …and every legacy-owned relation is claimed: the same set.
        let legacy: std::collections::HashSet<SymbolId> = [
            roles.instance, roles.subclass, roles.subrelation, roles.transitive,
            roles.symmetric, roles.domain, roles.range, roles.disjoint, roles.partition,
            tids.before, tids.earlier, tids.meets, tids.during, tids.starts,
            tids.finishes, tids.temporal_part,
        ]
        .into_iter()
        .collect();
        for &rel in &legacy {
            assert!(cov.owns(rel), "legacy theory relation missing from coverage");
        }
        assert_eq!(cov.claims.len(), legacy.len(), "claim set == legacy set");

        // The omission license is exactly the meaning-axiom list.
        assert_eq!(
            cov.omitted_axioms,
            prover.oracle.decomposition_meaning_axioms().to_vec()
        );

        // Negative probe: an ordinary relation is owned by neither.
        let jail = Symbol::hash_name("goesToJail");
        assert!(!cov.owns(jail));
        assert!(!is_theory_rel(jail, &roles, &tids));
    }

    fn a_id() -> SymbolId {
        Symbol::hash_name("a")
    }

    // Clark-completion negatives end to end (env-free entry:
    // `discharge_models_forced` bypasses only the SIGMA_MODEL gate).  The
    // goal `(not (grandparent Alice Dave))` is NOT classically entailed
    // (open world) — plain saturation can only saturate — but every
    // definition of `grandparent`'s cone extracted cleanly, so the
    // certifier decides the absence: a `model_complete` unit is emitted
    // whose `fact_parents` cite BOTH defining rule roots, and the run
    // closes with a refutation.
    #[test]
    fn model_complete_negative_closes_certified_absence_goal() {
        use crate::parse::OpKind;
        use super::super::super::clausify::clausify_sentence;
        use super::super::RunVerdict;

        let kif = "\
            (=> (and (parent ?X ?Y) (parent ?Y ?Z)) (grandparent ?X ?Z))\n\
            (=> (adoptedBy ?Y ?X) (parent ?X ?Y))\n\
            (parent Alice Bob)\n\
            (parent Bob Carol)\n\
            (adoptedBy Dave Carol)\n\
            (not (grandparent Alice Dave))\n";
        let layer = ProverLayer::new(kif_layer(kif));
        let syn = &layer.semantic.syntactic;
        let goal = syn
            .root_sids()
            .into_iter()
            .find(|sid| syn.sentence(*sid).is_some_and(|s| s.op() == Some(&OpKind::Not)))
            .expect("goal root stored");
        let rule_sids: Vec<SentenceId> = syn
            .root_sids()
            .into_iter()
            .filter(|sid| {
                syn.sentence(*sid).is_some_and(|s| s.op() == Some(&OpKind::Implies))
            })
            .collect();
        assert_eq!(rule_sids.len(), 2, "two defining rule roots");

        let mut prover = NativeProver::new(&layer, Scope::Base, Default::default());
        // Negated conjecture of `(not (grandparent Alice Dave))`: the
        // positive unit `grandparent(Alice, Dave)` at CONJECTURE tier.
        let sent = layer.semantic.syntactic.sentence(goal).expect("goal sentence");
        let clauses = clausify_sentence(
            &layer.semantic.syntactic, &layer.atoms, &sent, goal, true);
        assert!(!clauses.is_empty(), "conjecture clausifies");
        prover.add_conjecture_clauses(&clauses, Some(goal));

        prover.discharge_models_forced();

        // The completion negative was emitted, citing every defining rule
        // sid of the goal relation's cone.
        let unit = prover
            .clauses
            .iter()
            .find(|c| c.rule == "model_complete")
            .expect("model_complete unit emitted");
        for sid in &rule_sids {
            assert!(
                unit.fact_parents.contains(sid),
                "completion citation must carry every defining rule sid: {:?}",
                unit.fact_parents,
            );
        }

        // And the loop refutes: the emitted `~grandparent(Alice, Dave)`
        // resolves against the positive conjecture literal.
        let (verdict, _steps) = prover.run();
        assert!(
            matches!(verdict, RunVerdict::Refutation(_)),
            "certified negative must close the goal, got {verdict:?}"
        );
    }
}
