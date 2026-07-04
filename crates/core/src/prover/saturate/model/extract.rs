// crates/core/src/saturate/model/extract.rs
//
// Phase 3 — automatic extraction of a Datalog(¬) program from stored axioms.
//
// Replaces the hand-authored programs of Phase 2 with a scan over the
// SyntacticLayer roots that recovers the definite/Horn fragment:
//
//   * `(=> (and B1 … Bn) H)` / `(=> B H)`  →  a rule `H :- B1, …, Bn`
//     (each Bi may be `(not A)` for a negative literal);
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

/// Extract the Horn / definite fragment of the stored axioms as a Datalog(¬)
/// program: implication-shaped roots become rules, ground symbol atoms become
/// EDB facts.  Non-Datalog roots (function-term args, disjunctive heads,
/// quantifier structure beyond the implicit top-level `forall` already
/// stripped at ingest) are skipped — they remain for resolution.
pub(crate) fn extract_horn_program(syn: &SyntacticLayer) -> Program {
    let mut p = Program::default();

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

    p
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
