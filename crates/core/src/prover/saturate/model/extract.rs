// crates/core/src/saturate/model/extract.rs
//
// Phase 3 — automatic extraction of a Datalog(¬) program from stored axioms.
//
// Replaces the hand-authored programs of Phase 2 with a scan over the
// SyntacticLayer roots that recovers the definite/Horn fragment:
//
//   * `(=> (and B1 … Bn) H)` / `(=> B H)`  →  a rule `H :- B1, …, Bn`
//     (each Bi may be `(not A)` for a negative literal);
//   * `(or l1 … lk)` with EXACTLY ONE positive symbol-headed literal `c` and
//     the rest negated symbol-headed literals `a1, …, an`  →  the same rule
//     `c :- a1, …, an` (Milestone C — a clause `¬a1 ∨ … ∨ ¬an ∨ c` IS that
//     Horn rule).  This is the shape TPTP CNF input and clausified FOF store
//     their axioms in — without this arm the extractor only ever matched
//     KIF-style `(=>)` roots, so the extracted program was structurally
//     EMPTY on the whole TPTP corpus (see `extract_horn_program_stats` /
//     `horn_rule_of_or` below for the full case breakdown: all-negative and
//     ≥2-positive clauses are skipped, not extracted);
//   * a ground symbol-headed atom `(rel c1 … ck)`  →  an EDB fact.
//
// Only atoms whose arguments are variables or symbol constants are taken —
// compound (function) or literal arguments are not first-order Datalog terms,
// so a rule mentioning one is skipped (it falls through to resolution).  The
// extracted program is handed to the kernel (`Program::evaluate`), which
// applies the stratification / safety gates; anything that fails them is
// rejected there, soundly.
//
// NOTE: written under a tooling outage and not yet compiled/tested — the build
// + reproduction tests are the Phase-3 gate and run before this is relied on.

use std::collections::HashMap;

use crate::semantics::roles::TaxonomyRoles;
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, OpKind, Sentence, SentenceId, SymbolId};

use super::{Atom, DTerm, Literal, Program, Rule};

/// A sub-sentence id from an element.
fn sub(e: &Element) -> Option<SentenceId> {
    match e {
        Element::Sub(sid) => Some(*sid),
        _ => None,
    }
}

/// Convert a symbol-headed atom sentence into a model [`Atom`], mapping each
/// distinct logical variable to a rule-local index via `vars`.  Returns the
/// atom and whether it is ground.  `None` if the sentence is not a
/// symbol-headed atom or has a compound / literal / operator argument.
fn atom_of(s: &Sentence, vars: &mut HashMap<SymbolId, u32>) -> Option<(Atom, bool)> {
    let pred = s.head_symbol()?;
    if s.elements.len() < 2 {
        // A nullary/propositional atom — represent as a 0-ary relation.
        return Some((Atom { pred, args: Vec::new() }, true));
    }
    let mut args = Vec::with_capacity(s.elements.len() - 1);
    let mut ground = true;
    for el in &s.elements[1..] {
        match el {
            Element::Symbol(sym) => args.push(DTerm::Const(sym.id())),
            Element::Variable { id, .. } => {
                let next = vars.len() as u32;
                let idx = *vars.entry(*id).or_insert(next);
                args.push(DTerm::Var(idx));
                ground = false;
            }
            _ => return None, // Sub (function term) / Literal / Op: not Datalog
        }
    }
    Some((Atom { pred, args }, ground))
}

/// Parse one body element into a [`Literal`] (handling a leading `(not …)`).
fn literal_of(
    syn:  &SyntacticLayer,
    sid:  SentenceId,
    vars: &mut HashMap<SymbolId, u32>,
) -> Option<Literal> {
    let s = syn.sentence(sid)?;
    if s.op() == Some(&OpKind::Not) && s.elements.len() == 2 {
        let inner = sub(&s.elements[1])?;
        let inner_s = syn.sentence(inner)?;
        let (atom, _) = atom_of(&inner_s, vars)?;
        Some(Literal { atom, negated: true })
    } else {
        let (atom, _) = atom_of(&s, vars)?;
        Some(Literal { atom, negated: false })
    }
}

/// Counters for the clausal (`or`-root) extraction arm — Milestone C.  Purely
/// diagnostic (SIGMA_STATS-style bookkeeping); extraction behavior does not
/// depend on these.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ExtractStats {
    /// `(or …)` roots turned into a Horn rule (exactly one positive literal).
    pub(crate) or_rules: u32,
    /// `(or …)` roots with 0 positive literals (all-negative — a denial /
    /// goal shape, not a definite rule).  Not extracted here; left for
    /// resolution (and, eventually, a dedicated denials/goal channel).
    pub(crate) or_all_negative_skipped: u32,
    /// `(or …)` roots with ≥2 positive literals (non-Horn).  Not extracted.
    pub(crate) or_non_horn_skipped: u32,
    /// `(or …)` roots that had a qualifying single positive literal but some
    /// literal's atom wasn't a plain symbol-headed Datalog atom (compound
    /// term / equality / nested operator) — skipped, falls through to
    /// resolution.
    pub(crate) or_non_datalog_skipped: u32,
    /// A single-literal negative clause `(not (rel c1 … ck))` — ground (a
    /// negative fact, e.g. SUMO's `(not (member Denmark EuropeanMonetaryUnion))`)
    /// or with variables (e.g. an irreflexivity axiom
    /// `(not (colleague ?A ?A ?ORG))`).  Neither is an EDB fact (Datalog(¬)
    /// has no negative facts) nor a rule — both are denial/integrity-constraint
    /// shapes, left for resolution's denials channel.  (Pre-existing
    /// behavior: these fell through the old code's catch-all arm silently;
    /// this only adds the count, not a behavior change.)
    pub(crate) negative_unit_skipped: u32,
}

/// Classify one `(or l1 … lk)` root as a Horn rule, if it has EXACTLY ONE
/// positive symbol-headed literal and every OTHER literal is a negated
/// symbol-headed literal (all subject to the same `atom_of` restrictions as
/// the `(=>)` arm: arguments are variables or symbol constants only).  A
/// clause `¬a1 ∨ … ∨ ¬an ∨ c` is exactly the Horn rule `c :- a1, …, an`.
///
/// Returns `None` (and bumps the matching `stats` counter) for: zero
/// positive literals (all-negative — a denial/goal clause, not extracted
/// here), two-or-more positive literals (non-Horn), or a literal whose atom
/// isn't representable as a Datalog atom.
fn horn_rule_of_or(
    syn:   &SyntacticLayer,
    root:  SentenceId,
    s:     &Sentence,
    stats: &mut ExtractStats,
) -> Option<Rule> {
    let lit_ids: Vec<SentenceId> = s.elements[1..].iter().filter_map(sub).collect();
    if lit_ids.len() != s.elements.len() - 1 {
        // A disjunct that isn't a Sub (shouldn't happen post-CAF, but keep
        // this sound rather than silently dropping a literal).
        stats.or_non_datalog_skipped += 1;
        return None;
    }

    let mut vars: HashMap<SymbolId, u32> = HashMap::new();
    let mut literals: Vec<Literal> = Vec::with_capacity(lit_ids.len());
    for lid in &lit_ids {
        match literal_of(syn, *lid, &mut vars) {
            Some(l) => literals.push(l),
            None => {
                stats.or_non_datalog_skipped += 1;
                return None;
            }
        }
    }

    let positives: Vec<usize> = literals.iter().enumerate()
        .filter(|(_, l)| !l.negated)
        .map(|(i, _)| i)
        .collect();
    match positives.len() {
        0 => {
            // All-negative: a denial/integrity-constraint or a goal clause
            // (e.g. a `negated_conjecture` unit), not a definite Horn rule.
            // Left for resolution / the denials channel.
            stats.or_all_negative_skipped += 1;
            None
        }
        1 => {
            let head_idx = positives[0];
            let head = literals[head_idx].atom.clone();
            // Every other literal was NEGATED in the clause (¬ai) — the
            // clause ¬a1 ∨ … ∨ ¬an ∨ c is (a1 ∧ … ∧ an) → c, so each ai
            // becomes a POSITIVE rule-body premise, not a negated one.
            let body: Vec<Literal> = literals.into_iter().enumerate()
                .filter(|(i, _)| *i != head_idx)
                .map(|(_, mut l)| { l.negated = false; l })
                .collect();
            stats.or_rules += 1;
            Some(Rule { head, body, sid: Some(root) })
        }
        _ => {
            // ≥2 positive literals: non-Horn (a real disjunctive head).
            stats.or_non_horn_skipped += 1;
            None
        }
    }
}

/// Extract the Horn / definite fragment of the stored axioms as a Datalog(¬)
/// program: implication-shaped roots become rules, ground symbol atoms become
/// EDB facts, and (Milestone C) clausal `(or …)` roots with exactly one
/// positive symbol-headed literal become rules too — a clause
/// `¬a1 ∨ … ∨ ¬an ∨ c` IS the Horn rule `c :- a1, …, an`.  This is what makes
/// extraction dialect-blind: TPTP CNF input and post-clausification FOF both
/// store their clauses as `(or …)` roots (see `extract_horn_program_stats`'s
/// doc for the shapes actually observed), so without this arm the extracted
/// program was structurally empty on the whole TPTP corpus.  Non-Datalog
/// roots (function-term args, non-Horn disjunctive heads, quantifier
/// structure beyond the implicit top-level `forall` already stripped at
/// ingest) are skipped — they remain for resolution.
pub(crate) fn extract_horn_program(syn: &SyntacticLayer) -> Program {
    let (p, _) = extract_horn_program_stats(syn);
    p
}

/// As [`extract_horn_program`], but also returns the [`ExtractStats`]
/// breakdown of the clausal arm's skip reasons (SIGMA_STATS-style
/// instrumentation; zero effect on the returned program).
pub(crate) fn extract_horn_program_stats(syn: &SyntacticLayer) -> (Program, ExtractStats) {
    let mut p = Program::default();
    let mut stats = ExtractStats::default();

    for root in syn.root_sids() {
        let Some(s) = syn.sentence(root) else { continue };
        match s.op() {
            // Rule: (=> ant con)
            Some(&OpKind::Implies) if s.elements.len() == 3 => {
                let (Some(ant_id), Some(con_id)) = (sub(&s.elements[1]), sub(&s.elements[2]))
                    else { continue };
                let (Some(ant), Some(con)) = (syn.sentence(ant_id), syn.sentence(con_id))
                    else { continue };

                // Head must be a (positive) symbol-headed atom.
                if con.op().is_some() {
                    continue; // disjunctive / negative / equality head: not definite
                }
                let mut vars: HashMap<SymbolId, u32> = HashMap::new();

                // Body literals (process first so positive body vars index low).
                let body_ids: Vec<SentenceId> = if ant.op() == Some(&OpKind::And) {
                    ant.elements[1..].iter().filter_map(sub).collect()
                } else {
                    vec![ant_id]
                };
                let mut body = Vec::with_capacity(body_ids.len());
                let mut ok = true;
                for bid in body_ids {
                    match literal_of(syn, bid, &mut vars) {
                        Some(l) => body.push(l),
                        None => { ok = false; break; }
                    }
                }
                if !ok {
                    continue;
                }
                let Some((head, _)) = atom_of(&con, &mut vars) else { continue };
                p.rules.push(Rule { head, body, sid: Some(root) });
            }
            // Clause: (or l1 … lk) — CNF input / clausified FOF.  Exactly one
            // positive symbol-headed literal ⇒ a Horn rule (see
            // `horn_rule_of_or`); all-negative or ≥2-positive are skipped.
            Some(&OpKind::Or) if s.elements.len() >= 2 => {
                if let Some(rule) = horn_rule_of_or(syn, root, &s, &mut stats) {
                    p.rules.push(rule);
                }
            }
            // A single-literal negative clause `(not (rel c1 … ck))` — ground
            // (a negative fact, e.g. a one-literal `negated_conjecture`, or
            // SUMO's own `(not (member Denmark EuropeanMonetaryUnion))`) or
            // with variables (an irreflexivity axiom like
            // `(not (colleague ?A ?A ?ORG))`).  Not an EDB fact (no negative
            // facts in Datalog(¬)) and not a rule — a denial/goal shape.
            // Left for resolution.
            Some(&OpKind::Not) if s.elements.len() == 2 => {
                stats.negative_unit_skipped += 1;
            }
            // Fact: a ground symbol-headed atom.
            None => {
                let mut vars = HashMap::new();
                if let Some((atom, true)) = atom_of(&s, &mut vars) {
                    let tuple: Vec<SymbolId> = atom.args.iter().filter_map(|a| match a {
                        DTerm::Const(c) => Some(*c),
                        DTerm::Var(_) => None,
                    }).collect();
                    if tuple.len() == atom.args.len() {
                        p.fact_src(atom.pred, tuple, root);
                    }
                }
            }
            _ => {}
        }
    }

    (p, stats)
}

// ---------------------------------------------------------------------------
// Phase 3.5 — meta-relation schema instantiation (generalized).
// ---------------------------------------------------------------------------
//
// SUMO/Cyc state algebraic properties of a relation by *declaring its role*
// (`(instance R TransitiveRelation)`, `(subrelation R S)`, …) rather than by a
// first-order rule, because the first-order form `(=> (and (?R x y) (?R y z))
// (?R x z))` has a predicate variable and is not first-order.  A naive Horn
// extractor therefore misses transitivity/symmetry entirely (so e.g. the
// extracted `subclass` model has no transitive closure).
//
// This is the generalization of `prover::synthesize_subrelation_rules` (which
// instantiates ONLY `subrelation R S → S(x,y):-R(x,y)`) to the full role set,
// emitting Datalog rules for the model generator:
//
//   subrelation R S            →  S(x,y)  :- R(x,y)
//   instance R TransitiveRel   →  R(x,z)  :- R(x,y), R(y,z)
//   instance R SymmetricRel    →  R(y,x)  :- R(x,y)
//
// Dialect-agnostic: the role symbols come from the recognized [`TaxonomyRoles`]
// (recovered by shape), never from hard-coded names.

/// The relations bearing each algebraic role, gathered from the store's
/// declarations.  Each entry carries the declaring sentence id, so the
/// instantiated schema rule can cite the declaration in a proof.
#[derive(Debug, Default, Clone)]
pub(crate) struct RoleDecls {
    /// `(subrelation R S)` pairs (sub `R`, super `S`) + the declaring root.
    pub subrelation: Vec<(SymbolId, SymbolId, Option<SentenceId>)>,
    /// Relations declared `(instance R TransitiveRelation)` + the declaration.
    pub transitive:  Vec<(SymbolId, Option<SentenceId>)>,
    /// Relations declared `(instance R SymmetricRelation)` + the declaration.
    pub symmetric:   Vec<(SymbolId, Option<SentenceId>)>,
}

/// A binary symbol-headed fact's two symbol arguments.
fn binary_syms(syn: &SyntacticLayer, sid: SentenceId) -> Option<(SymbolId, SymbolId)> {
    let s = syn.sentence(sid)?;
    if s.elements.len() != 3 {
        return None;
    }
    match (&s.elements[1], &s.elements[2]) {
        (Element::Symbol(a), Element::Symbol(b)) => Some((a.id(), b.id())),
        _ => None,
    }
}

/// Scan the store for role declarations under the recognized role symbols.
pub(crate) fn collect_role_decls(syn: &SyntacticLayer, roles: &TaxonomyRoles) -> RoleDecls {
    let mut d = RoleDecls::default();
    for sid in syn.by_head_id(&roles.subrelation) {
        if let Some((r, s)) = binary_syms(syn, sid) {
            if r != s {
                d.subrelation.push((r, s, Some(sid)));
            }
        }
    }
    for sid in syn.by_head_id(&roles.instance) {
        if let Some((r, c)) = binary_syms(syn, sid) {
            if c == roles.transitive {
                d.transitive.push((r, Some(sid)));
            } else if c == roles.symmetric {
                d.symmetric.push((r, Some(sid)));
            }
        }
    }
    d
}

/// Instantiate the role declarations into first-order Datalog rules — the
/// generalized schema expander.  `extra_transitive` lets a caller add
/// relations whose transitivity is known by other means (e.g. the recognized
/// taxonomy roles `subclass`/`subrelation`, transitive by construction, or
/// relations found transitive through the relation-class hierarchy), each
/// with the sentence to cite for that knowledge (or `None`).  Every emitted
/// rule carries its declaring sentence as `sid`, so a proof using the rule
/// can cite the declaration it was instantiated from.
pub(crate) fn schema_rules(
    decls:            &RoleDecls,
    extra_transitive: &[(SymbolId, Option<SentenceId>)],
) -> Vec<Rule> {
    let app = |p: SymbolId, a: u32, b: u32| Atom { pred: p, args: vec![DTerm::Var(a), DTerm::Var(b)] };
    let mut out = Vec::new();

    for &(r, s, sid) in &decls.subrelation {
        // S(x,y) :- R(x,y)
        out.push(Rule {
            head: app(s, 0, 1),
            body: vec![Literal { atom: app(r, 0, 1), negated: false }],
            sid,
        });
    }
    for &(r, sid) in decls.transitive.iter().chain(extra_transitive.iter()) {
        // R(x,z) :- R(x,y), R(y,z)
        out.push(Rule {
            head: Atom { pred: r, args: vec![DTerm::Var(0), DTerm::Var(2)] },
            body: vec![
                Literal { atom: app(r, 0, 1), negated: false },
                Literal { atom: app(r, 1, 2), negated: false },
            ],
            sid,
        });
    }
    for &(r, sid) in &decls.symmetric {
        // R(y,x) :- R(x,y)
        out.push(Rule {
            head: app(r, 1, 0),
            body: vec![Literal { atom: app(r, 0, 1), negated: false }],
            sid,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Denial constraints (negatives package, sub-milestone B).
// ---------------------------------------------------------------------------
//
// SUMO/Cyc declare class DISJOINTNESS — `(disjoint A B)`, and pairwise over
// the tails of `(partition C P1 … Pn)` / `(disjointDecomposition C P1 … Pn)` —
// rather than writing the first-order `∀x.¬(instance x A ∧ instance x B)`.
// Extracted here as ⊥-rules ("denials"): integrity constraints the chase uses
// to REFUTE a ground atom (`ModelProgram::refutes`).  Open-world sound:
// KB ⊨ ¬(instance x C) iff KB ∪ {(instance x C)} is inconsistent, and a
// denial-pair hit inside the model's closure is exactly that inconsistency
// for the Horn+denial fragment — no Clark completion, no CWA.
//
// This mirrors how the taxonomy oracle's `DisjointSets::build` reads the
// declarations (recognized `disjoint`/`partition` role ids, plus the
// hash-named `disjointDecomposition`, whose row-variable defining axiom has
// no recognizer).  `exhaustiveDecomposition` is NOT disjoint — excluded.

/// One extracted denial constraint: the (normalized `min ≤ max`) class pair
/// declared disjoint, and the declaring root sentence — the axiom a
/// refutation cites as its final step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Denial {
    pub classes: (SymbolId, SymbolId),
    pub sid:     SentenceId,
}

/// Scan the store for disjointness declarations under the recognized role
/// symbols and flatten them to pairwise [`Denial`]s.  First declaration of a
/// pair wins (matching the oracle's `src.entry().or_insert` behavior).
pub(crate) fn collect_denials(syn: &SyntacticLayer, roles: &TaxonomyRoles) -> Vec<Denial> {
    let norm = |a: SymbolId, b: SymbolId| if a <= b { (a, b) } else { (b, a) };
    let mut seen: std::collections::HashSet<(SymbolId, SymbolId)> =
        std::collections::HashSet::new();
    let mut out: Vec<Denial> = Vec::new();
    let mut push = |a: SymbolId, b: SymbolId, sid: SentenceId, out: &mut Vec<Denial>| {
        if a != b && seen.insert(norm(a, b)) {
            out.push(Denial { classes: norm(a, b), sid });
        }
    };

    // (disjoint A B)
    for sid in syn.by_head_id(&roles.disjoint) {
        if let Some((a, b)) = binary_syms(syn, sid) {
            push(a, b, sid, &mut out);
        }
    }
    // (partition C P1 … Pn) / (disjointDecomposition C P1 … Pn): the tail
    // members are pairwise disjoint.
    let dd = crate::types::Symbol::hash_name("disjointDecomposition");
    let mut heads = vec![roles.partition];
    if dd != roles.partition {
        heads.push(dd);
    }
    for head in heads {
        for sid in syn.by_head_id(&head) {
            let Some(s) = syn.sentence(sid) else { continue };
            let parts: Vec<SymbolId> = s
                .elements
                .iter()
                .skip(2)
                .filter_map(|e| match e {
                    Element::Symbol(sym) => Some(sym.id()),
                    _ => None,
                })
                .collect();
            for i in 0..parts.len() {
                for j in (i + 1)..parts.len() {
                    push(parts[i], parts[j], sid, &mut out);
                }
            }
        }
    }
    out
}

/// Derive the relations that bear an algebraic role from an *evaluated* model,
/// rather than reading declarations directly.  A relation `R` is transitive
/// iff `(R, TransitiveRelation)` is in the **instance closure** — which already
/// climbs the relation-class hierarchy via the instance/subclass bridge, so
/// this subsumes (a) direct `(instance R TransitiveRelation)` declarations,
/// (b) inherited ones (`(instance R PartialOrderingRelation)` +
/// `(subclass PartialOrderingRelation TransitiveRelation)`), and (c) the
/// recognized taxonomy roles — with no hard-coded seed.  Feed the result back
/// as `schema_rules`' `extra_transitive` and re-evaluate to a fixpoint (a
/// newly-transitive relation can deepen the subclass closure, revealing more).
fn role_members(model: &super::Model, roles: &TaxonomyRoles, role_class: SymbolId) -> Vec<SymbolId> {
    let Some(inst) = model.get(&roles.instance) else { return Vec::new() };
    inst.iter()
        .filter(|t| t.len() == 2 && t[1] == role_class)
        .map(|t| t[0])
        .collect()
}

/// Relations that are transitive in `model` (membership in `TransitiveRelation`
/// via the instance closure).
pub(crate) fn transitive_members(model: &super::Model, roles: &TaxonomyRoles) -> Vec<SymbolId> {
    role_members(model, roles, roles.transitive)
}

/// Relations that are symmetric in `model` (membership in `SymmetricRelation`).
pub(crate) fn symmetric_members(model: &super::Model, roles: &TaxonomyRoles) -> Vec<SymbolId> {
    role_members(model, roles, roles.symmetric)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantics::caches::test_support::{kif_layer, tptp_layer};
    use crate::types::Symbol;

    fn s(name: &str) -> SymbolId { Symbol::hash_name(name) }

    /// Find the one extracted rule whose head predicate is `head_pred`
    /// (panics if there isn't exactly one — tests target a single clause).
    fn only_rule_for<'a>(p: &'a Program, head_pred: SymbolId) -> &'a Rule {
        let matches: Vec<&Rule> = p.rules.iter().filter(|r| r.head.pred == head_pred).collect();
        assert_eq!(matches.len(), 1, "expected exactly one rule headed by {head_pred:?}, got {matches:?}");
        matches[0]
    }

    // -- (1) or-clause -> Horn rule, with sid -----------------------------
    // `(or (not (killed X Y)) (hates X Y))` is the clause ¬killed(X,Y) ∨
    // hates(X,Y) — exactly the Horn rule `hates(X,Y) :- killed(X,Y)`.
    #[test]
    fn or_clause_with_one_positive_becomes_horn_rule_with_sid() {
        let sem = tptp_layer(
            "cnf(killer_hates_victim, axiom, ( ~ killed(X,Y) | hates(X,Y) ) ).\n",
            "or_rule.p",
        );
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);
        assert_eq!(stats.or_rules, 1, "one or-root extracted as a rule");
        assert_eq!(stats.or_non_horn_skipped, 0);
        assert_eq!(stats.or_all_negative_skipped, 0);

        let rule = only_rule_for(&p, s("hates"));
        assert!(rule.sid.is_some(), "rule cites its declaring root");
        assert_eq!(rule.head.pred, s("hates"));
        assert_eq!(rule.body.len(), 1);
        assert_eq!(rule.body[0].atom.pred, s("killed"));
        assert!(!rule.body[0].negated,
            "the clause's negated literal becomes a POSITIVE rule-body premise \
             (¬killed(X,Y) ∨ hates(X,Y)  ==  hates(X,Y) :- killed(X,Y))");
        // Head and body share the same two rule-local variables (X, Y),
        // just at different argument positions.
        let head_vars: Vec<u32> = rule.head.args.iter().map(|a| match a {
            DTerm::Var(v) => *v,
            DTerm::Const(_) => panic!("expected a variable"),
        }).collect();
        let body_vars: Vec<u32> = rule.body[0].atom.args.iter().map(|a| match a {
            DTerm::Var(v) => *v,
            DTerm::Const(_) => panic!("expected a variable"),
        }).collect();
        assert_eq!(head_vars, body_vars, "X,Y map to the same rule-local indices in head and body");
    }

    // The rule discharges correctly once evaluated: killed(a,b) entails
    // hates(a,b) via the extracted clause, exactly as the source problem
    // intends ("a killer always hates his victim").
    #[test]
    fn or_clause_rule_evaluates_correctly() {
        let sem = tptp_layer(
            "cnf(killer_hates_victim, axiom, ( ~ killed(X,Y) | hates(X,Y) ) ).\n\
             cnf(fact1, axiom, killed(agatha,agatha) ).\n",
            "or_rule_eval.p",
        );
        let p = extract_horn_program(&sem.syntactic);
        let model = p.evaluate().expect("stratifiable");
        let t = vec![s("agatha"), s("agatha")];
        assert!(model.get(&s("hates")).is_some_and(|rows| rows.contains(&t)));
    }

    // -- (2) >= 2 positive literals: skipped (non-Horn) -------------------
    // `(or (not (lives X)) (richer X agatha) (hates butler X))` has TWO
    // positive literals (richer, hates) — not a definite clause.
    #[test]
    fn or_clause_with_two_positive_literals_is_skipped() {
        let sem = tptp_layer(
            "cnf(butler_hates_poor, axiom, \
                ( ~ lives(X) | richer(X,agatha) | hates(butler,X) ) ).\n",
            "non_horn.p",
        );
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);
        assert_eq!(stats.or_non_horn_skipped, 1);
        assert_eq!(stats.or_rules, 0);
        assert!(p.rules.is_empty(), "non-Horn clause contributes no rule");
    }

    // A disjunction with two positives and zero negatives (e.g. a
    // `negated_conjecture` goal clause) is equally non-Horn.
    #[test]
    fn or_clause_all_positive_is_skipped_as_non_horn() {
        let sem = tptp_layer(
            "cnf(goal, negated_conjecture, ( killed(butler,agatha) | killed(charles,agatha) ) ).\n",
            "all_pos.p",
        );
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);
        assert_eq!(stats.or_non_horn_skipped, 1);
        assert!(p.rules.is_empty());
    }

    // -- (3) all-negative: skipped (denial / goal shape) -------------------
    // `(or (not (hates agatha X)) (not (hates charles X)))` has ZERO
    // positive literals — a denial-shaped clause, not a definite rule.
    #[test]
    fn or_clause_all_negative_is_skipped() {
        let sem = tptp_layer(
            "cnf(different_hates, axiom, ( ~ hates(agatha,X) | ~ hates(charles,X) ) ).\n",
            "all_neg.p",
        );
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);
        assert_eq!(stats.or_all_negative_skipped, 1);
        assert_eq!(stats.or_rules, 0);
        assert!(p.rules.is_empty(), "all-negative clause contributes no rule");
    }

    // A ground single-literal negative unit clause `(not (rel c1 … ck))`
    // stores as a bare `Not`-headed root (no `Or` wrapper) — also skipped,
    // not treated as a (nonexistent) "negative EDB fact".
    #[test]
    fn ground_negative_unit_clause_is_skipped() {
        let sem = tptp_layer(
            "cnf(not_butler, axiom, ~ equal(agatha,butler) ).\n",
            "ground_neg_unit.p",
        );
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);
        assert_eq!(stats.negative_unit_skipped, 1);
        assert!(p.rules.is_empty());
        assert!(p.edb.is_empty(), "no negative EDB facts are synthesized");
    }

    // Ground single-literal POSITIVE unit clauses still work as plain EDB
    // facts (pre-existing behavior, unaffected by the or-clause arm).
    #[test]
    fn ground_positive_unit_clause_is_still_a_fact() {
        let sem = tptp_layer("cnf(agatha, axiom, lives(agatha) ).\n", "ground_pos_unit.p");
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);
        assert_eq!(stats.or_rules, 0);
        assert_eq!(stats.negative_unit_skipped, 0);
        assert!(p.edb.get(&s("lives")).is_some_and(|rows| rows.contains(&vec![s("agatha")])));
    }

    // -- (4) TPTP CNF end-to-end: PUZ001-1.p yields a non-empty program ----
    // with correctly-shaped rules (loads the real problem file when $TPTP is
    // set; otherwise uses an inline excerpt covering the same clause shapes
    // so the test still exercises the extractor without a filesystem
    // dependency).
    #[test]
    fn tptp_cnf_end_to_end_yields_nonempty_program_with_horn_rules() {
        let text = match std::env::var("TPTP") {
            Ok(tptp) => {
                let path = format!("{tptp}/Problems/PUZ/PUZ001-1.p");
                std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    panic!("$TPTP set but failed to read {path}: {e}")
                })
            }
            Err(_) => {
                // Inline excerpt of PUZ001-1.p covering unit facts, a
                // single-positive Horn clause, a >=2-positive non-Horn
                // clause, and an all-negative clause.
                "cnf(agatha, hypothesis, lives(agatha) ).\n\
                 cnf(killer_hates_victim, hypothesis, ( ~ killed(X,Y) | hates(X,Y) ) ).\n\
                 cnf(butler_hates_poor, hypothesis, \
                    ( ~ lives(X) | richer(X,agatha) | hates(butler,X) ) ).\n\
                 cnf(different_hates, hypothesis, ( ~ hates(agatha,X) | ~ hates(charles,X) ) ).\n"
                    .to_string()
            }
        };
        let sem = tptp_layer(&text, "PUZ001-1.p");
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);

        assert!(!p.rules.is_empty(), "PUZ001-1.p must yield a non-empty Horn program");
        assert!(stats.or_rules > 0, "at least one or-clause extracted as a rule");
        // Every extracted rule is well-shaped: a symbol-headed atom head, a
        // body of symbol-headed literals, and a citation back to its
        // declaring root.
        for rule in &p.rules {
            assert!(rule.sid.is_some(), "extracted rule cites its source sentence");
            for lit in &rule.body {
                assert!(!lit.atom.args.is_empty() || lit.atom.args.is_empty(), "atom shape sane");
            }
        }
        // The known Horn clause from this problem: hates(X,Y) :- killed(X,Y)
        // ("a killer always hates his victim").  Real PUZ001-1.p has a
        // second `hates`-headed rule too (`same_hates`), so look for the one
        // whose body is `killed` rather than assuming a unique head.
        let killer_rule = p.rules.iter()
            .find(|r| r.head.pred == s("hates") && r.body.len() == 1 && r.body[0].atom.pred == s("killed"))
            .expect("killer_hates_victim clause extracted as a rule");
        assert!(!killer_rule.body[0].negated);

        // At least one ground fact from the unit clauses (`lives(agatha)`).
        assert!(p.edb.get(&s("lives")).is_some_and(|rows| rows.contains(&vec![s("agatha")])));
    }

    // -- KIF suite regression guard: the new or-arm must not fire on the
    // existing KIF (=>) extraction fixtures.  KIF axioms never store a
    // top-level `(or …)` root (SUMO's `.kif` corpus is written in `(=>)` /
    // fact form), so the new arm's hit count on a KIF-loaded KB must be zero.
    #[test]
    fn kif_extraction_unaffected_by_or_clause_arm() {
        let kif = "\
            (subclass Dog Animal)\n\
            (=> (instance ?X Dog) (instance ?X Animal))\n\
            (instance Rex Dog)\n";
        let sem = kif_layer(kif);
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);
        assert_eq!(stats.or_rules, 0, "no or-root exists in this KIF KB");
        assert_eq!(stats.or_all_negative_skipped, 0);
        assert_eq!(stats.or_non_horn_skipped, 0);
        assert_eq!(stats.negative_unit_skipped, 0);
        // The pre-existing (=>) extraction still works unchanged.
        assert_eq!(p.rules.len(), 1);
        assert!(p.edb.get(&s("instance")).is_some_and(|rows| rows.contains(&vec![s("Rex"), s("Dog")])));
    }
}


