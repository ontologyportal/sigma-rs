// crates/core/src/saturate/model/mod.rs
//
// The ontology model-builder — Phase 0-2 (scaffold + kernel + reproduction).
//
// This is the generic engine the bespoke oracles (taxonomy closure, Horn
// rule-join, inertial event calculus) are special cases of: a runtime,
// semi-naive evaluator for **stratified Datalog with negation** over tuples of
// `SymbolId`.  Given a logic program (rules + EDB ground facts) extracted from
// the axioms, it computes the program's perfect model — the materialized
// relations — which the prover will (Phase 5+) consult to decide ground
// literals and retrieve entailed background units.
//
// See docs/model-builder-implementation.md for the full plan.  This module is
// the standalone engine; it is NOT yet wired into the prover (zero call sites,
// so the saturation path is byte-identical).  Phase 2's claim — that ONE
// engine reproduces all three bespoke oracles — is proven by the cross-check
// tests below (notably `ec_kernel_matches_simulate`, which asserts the kernel
// computes exactly the state `eventcalc::simulate` does).

use std::collections::{HashMap, HashSet};

use crate::types::SymbolId;

pub(crate) mod cluster;
pub(crate) mod extract;
pub(crate) mod magic;
pub(crate) mod recognize;
mod seminaive;

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
    /// living in a stratifiable cluster.  (The full definitional-completeness
    /// gate — comparing against non-Horn axioms the extractor skipped — refines
    /// this later; positive decisions never need it.)
    pub complete: HashSet<Pred>,
    /// Recognized role symbols (dialect-agnostic) — for the Level-2 derivation
    /// of the inherited transitive/symmetric set over the evaluated model.
    pub roles:    crate::semantics::roles::TaxonomyRoles,
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

        let mut program = extract::extract_horn_program(syn);
        let roles = TaxonomyRoles::recognize(syn, syn.root_sids());
        // NOTE: clause-signature recognition (`recognize`) is validated as a
        // dialect-robust role recognizer, but using its bridges to *override*
        // the sentence-recognized roles here was net-negative on the CSR sweep
        // (it picked a wrong bridge — `element`/`subset` — when instance/
        // subclass aren't a first-order bridge, regressing CSR176+1 for zero
        // gain).  The right home for clause-sig + reification handling is
        // Milestone A (OpenCyc recognition), not a blind override.
        let decls = extract::collect_role_decls(syn, &roles);
        for r in extract::schema_rules(&decls, &[]) {
            program.rules.push(r);
        }

        let clusters = cluster::partition(&program);
        let monotone = cluster::positive_program(&program);
        let complete: HashSet<Pred> =
            clusters.iter().flat_map(|c| c.preds.iter().copied()).collect();

        ModelProgram { program, clusters, monotone, complete, roles }
    }

    /// The sound positive model: the monotone fragment evaluated, then closed
    /// under **derived** transitivity — relations the KB makes transitive
    /// (`(R, TransitiveRelation) ∈ instance-closure`, covering direct and
    /// hierarchy-inherited declarations) get their transitivity rule and the
    /// model is re-evaluated to a fixpoint.  No conventional seeding, so every
    /// emitted fact is entailed by the KB's own axioms.
    pub(crate) fn positive_model(&self) -> Option<Model> {
        // Materialization budget — bail (→ resolution) rather than blow up on a
        // large un-scoped KB.  Demand scoping (SInE, slice 4) is the real fix;
        // this keeps slice 2 from regressing problems resolution already solves.
        const BUDGET: usize = 250_000;
        let mut work = self.monotone.clone();
        let mut known: HashSet<Pred> = HashSet::new();
        let mut model = work.evaluate_budgeted(BUDGET).ok()?;
        loop {
            let trans = extract::transitive_members(&model, &self.roles);
            let fresh: Vec<Pred> = trans.into_iter().filter(|r| known.insert(*r)).collect();
            if fresh.is_empty() {
                break;
            }
            let decls = extract::RoleDecls::default();
            for r in extract::schema_rules(&decls, &fresh) {
                work.rules.push(r);
            }
            model = work.evaluate_budgeted(BUDGET).ok()?;
        }
        Some(model)
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
    /// the bail-reason breakdown — unchanged signature/behavior for existing
    /// callers.
    pub(crate) fn answer(
        &self,
        rel:      Pred,
        args:     &[DTerm],
        deadline: Option<std::time::Instant>,
    ) -> Option<Vec<Tuple>> {
        let mut stats = ModelStats::default();
        self.answer_stats(rel, args, deadline, &mut stats)
    }

    /// As [`answer`](Self::answer), but records WHY a bail happened (or that
    /// an answer was produced) into `stats` — SIGMA_STATS instrumentation
    /// only, zero behavior change vs `answer`.
    pub(crate) fn answer_stats(
        &self,
        rel:      Pred,
        args:     &[DTerm],
        deadline: Option<std::time::Instant>,
        stats:    &mut ModelStats,
    ) -> Option<Vec<Tuple>> {
        const BUDGET: usize = 250_000;
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
        let cone = cluster::dependency_cone(&self.monotone, &goal);
        let scoped = cluster::scope_program(&self.monotone, &cone);
        let cone_facts: usize = scoped.edb.values().map(|s| s.len()).sum();
        if scoped.rules.len() > MAX_CONE_RULES || cone_facts > MAX_CONE_FACTS {
            stats.budget_overflows += 1;
            return None;
        }
        let rewritten = magic::magic_rewrite(&scoped, rel, args);
        let model = match rewritten.evaluate_within(BUDGET, deadline) {
            Ok(m) => m,
            Err(ModelError::Unsafe) => {
                stats.unsafe_bails += 1;
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
                return None;
            }
        };
        let Some(rows) = model.get(&rel) else {
            stats.undefined_relation += 1;
            return None;
        };
        // Tuples matching the conjecture's bound (constant) positions.
        let ans: Vec<Tuple> = rows
            .iter()
            .filter(|row| {
                row.len() == args.len()
                    && args.iter().zip(row.iter()).all(|(a, v)| match a {
                        DTerm::Const(c) => c == v,
                        DTerm::Var(_) => true,
                    })
            })
            .cloned()
            .collect();
        stats.answered += 1;
        Some(ans)
    }
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
}

/// A Datalog(¬) program: intensional rules + extensional ground facts.
#[derive(Clone, Debug, Default)]
pub(crate) struct Program {
    pub rules: Vec<Rule>,
    pub edb:   HashMap<Pred, HashSet<Tuple>>,
}

/// Why a program could not be evaluated as a stratified Datalog program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}

impl Program {
    /// Add a ground EDB fact.
    pub(crate) fn fact(&mut self, pred: Pred, tuple: Tuple) {
        self.edb.entry(pred).or_default().insert(tuple);
    }

    /// Add a rule.
    pub(crate) fn rule(&mut self, head: Atom, body: Vec<Literal>) {
        self.rules.push(Rule { head, body });
    }

    /// Evaluate the program to its perfect model (bottom-up, stratum by
    /// stratum; positive recursion within a stratum, negation only against
    /// fully-computed lower strata).
    pub(crate) fn evaluate(&self) -> Result<Model, ModelError> {
        self.evaluate_budgeted(usize::MAX)
    }

    /// Evaluate, but abort with [`ModelError::Overflow`] once the materialized
    /// model exceeds `max_tuples` total facts — the guard that keeps an
    /// un-scoped evaluation over a large KB from blowing up (it bails to
    /// resolution instead).  `usize::MAX` ⇒ unbounded (see [`evaluate`]).
    pub(crate) fn evaluate_budgeted(&self, max_tuples: usize) -> Result<Model, ModelError> {
        self.evaluate_within(max_tuples, None)
    }

    /// As [`evaluate_budgeted`], but also aborts (`Overflow`) past a wall-clock
    /// `deadline` — so a query-time materialization can never eat the prover's
    /// time budget (it bails to resolution instead).
    pub(crate) fn evaluate_within(
        &self,
        max_tuples: usize,
        deadline:   Option<std::time::Instant>,
    ) -> Result<Model, ModelError> {
        self.validate_safe()?;
        let strata = self.stratify()?;
        seminaive::run(self, &strata, max_tuples, deadline)
    }

    /// Safety: every head variable and every negated-literal variable must
    /// appear in some positive body literal (range restriction).
    fn validate_safe(&self) -> Result<(), ModelError> {
        for r in &self.rules {
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
                        return Err(ModelError::Unsafe);
                    }
                }
            }
            for l in &r.body {
                if l.negated {
                    for a in &l.atom.args {
                        if let DTerm::Var(v) = a {
                            if !pos_vars.contains(v) {
                                return Err(ModelError::Unsafe);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
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
/// stratified Datalog(¬) program.  This is the hand-authored Phase-2 stand-in
/// for the Phase-3 automatic extractor; evaluating it reproduces exactly the
/// state [`super::eventcalc::simulate`] computes (see the cross-check test).
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
    for w in n.times.windows(2) {
        p.fact(succ, vec![w[0], w[1]]);
    }
    for (&t, evs) in &n.happens {
        for &e in evs {
            p.fact(happens, vec![e, t]);
        }
    }
    if let Some(&t0) = n.times.first() {
        for (&f, &val) in &n.initial {
            if val {
                p.fact(holds, vec![f, t0]);
            }
        }
    }

    // One rule per effect, with the concurrent-event guards.  `time(T)` binds
    // the time variable (safety); `happens(p,T)` is a positive guard,
    // `not happens(n,T)` a negative one.  T is variable 0.
    let effect_rule = |head_pred: Pred, e: &super::eventcalc::Effect| -> Rule {
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
        }
    };
    for e in &n.initiates {
        p.rules.push(effect_rule(initiates, e));
    }
    for e in &n.terminates {
        p.rules.push(effect_rule(terminates, e));
    }

    // initiated(F,T) :- happens(E,T), initiates(E,F,T)   (E=0, F=1, T=2)
    p.rule(
        Atom { pred: initiated, args: vec![DTerm::Var(1), DTerm::Var(2)] },
        vec![
            Literal { atom: Atom { pred: happens, args: vec![DTerm::Var(0), DTerm::Var(2)] }, negated: false },
            Literal { atom: Atom { pred: initiates, args: vec![DTerm::Var(0), DTerm::Var(1), DTerm::Var(2)] }, negated: false },
        ],
    );
    // terminated(F,T) :- happens(E,T), terminates(E,F,T)
    p.rule(
        Atom { pred: terminated, args: vec![DTerm::Var(1), DTerm::Var(2)] },
        vec![
            Literal { atom: Atom { pred: happens, args: vec![DTerm::Var(0), DTerm::Var(2)] }, negated: false },
            Literal { atom: Atom { pred: terminates, args: vec![DTerm::Var(0), DTerm::Var(1), DTerm::Var(2)] }, negated: false },
        ],
    );
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

    // Unsafe rule (head var not bound by a positive body literal) is rejected.
    #[test]
    fn unsafe_rule_is_rejected() {
        let mut p = Program::default();
        p.fact(s("dom"), vec![s("x")]);
        // bad(X,Y) :- dom(X)   -- Y unbound
        p.rule(atom("bad", vec![v(0), v(1)]), vec![pos(atom("dom", vec![v(0)]))]);
        assert_eq!(p.evaluate(), Err(ModelError::Unsafe));
    }

    // -- (c) THE go/no-go cross-check: the Datalog kernel reproduces exactly --
    //        what `eventcalc::simulate` computes for the spinning narrative.
    #[test]
    fn ec_kernel_matches_simulate() {
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
        };

        let sim = eventcalc::simulate(&nar);
        let prog = narrative_to_program(&nar);
        let model = prog.evaluate().expect("spinning narrative is stratified");
        let holds_rel = model.get(&pid("holdsAt")).cloned().unwrap_or_default();

        // Byte-for-byte equivalence over the (fluent, time) grid: the kernel's
        // holdsAt relation is true exactly where simulate's complete state is.
        let times = [n0, n1, n2, n3];
        let fluents = [fwd, bwd, spin];
        for &f in &fluents {
            for &t in &times {
                let sim_true = sim.get(&(f, t)).copied().unwrap_or(false);
                let kernel_true = holds_rel.contains(&vec![f, t]);
                assert_eq!(
                    sim_true, kernel_true,
                    "mismatch at fluent/time cell"
                );
            }
        }
        // And the key CSR conjecture cells, explicitly.
        assert!(!holds_rel.contains(&vec![spin, n1])); // ¬spinning@n1 (CSR017)
        assert!(!holds_rel.contains(&vec![spin, n2])); // ¬spinning@n2 (CSR020)
        assert!(holds_rel.contains(&vec![spin, n3]));  //  spinning@n3
        assert!(holds_rel.contains(&vec![fwd, n1]));   //  forwards@n1
    }
}
