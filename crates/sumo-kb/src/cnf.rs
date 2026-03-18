// crates/sumo-kb/src/cnf.rs
// #[cfg(feature = "cnf")]
//
// CNF (Conjunctive Normal Form) conversion for KIF/SUMO formulas.
//
// Ported from sumo-store/src/cnf.rs.
// Changes:
//   - `sumo_parser_core` imports → local `crate::types` / `crate::kif_store`
//   - `StoredSymbol` → `crate::types::Symbol`
//   - `StoreError::ClauseCountExceeded` → `crate::error::KbError::Other(...)`
//   - `sentences[sid as usize]` → `sentences[store.sent_idx(sid)]`
//   - `id_map` parameter removed — stable IDs mean symbols already have correct IDs
//   - Entire file gated on `#[cfg(feature = "cnf")]` in lib.rs

use crate::error::KbError;
use crate::kif_store::KifStore;
use crate::types::{
    Clause, CnfLiteral, CnfTerm,
    Element, Literal as KifLiteral,
    SentenceId, Symbol, SymbolId,
};
use crate::parse::kif::OpKind;

// ── Intermediate Formula tree ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Formula {
    Atom { pred: FTerm, args: Vec<FTerm> },
    Not(Box<Formula>),
    And(Vec<Formula>),
    Or(Vec<Formula>),
    Forall { vars: Vec<SymbolId>, body: Box<Formula> },
    Exists { vars: Vec<SymbolId>, body: Box<Formula> },
    Equal(FTerm, FTerm),
    True,
    False,
}

#[derive(Debug, Clone)]
enum FTerm {
    Const(SymbolId),
    Var(SymbolId),
    SkolemFn { id: SymbolId, args: Vec<FTerm> },
    Num(String),
    Str(String),
}

impl FTerm {
    fn to_cnf(&self) -> CnfTerm {
        match self {
            FTerm::Const(id)             => CnfTerm::Const(*id),
            FTerm::Var(id)               => CnfTerm::Var(*id),
            FTerm::SkolemFn { id, args } => CnfTerm::SkolemFn {
                id:   *id,
                args: args.iter().map(FTerm::to_cnf).collect(),
            },
            FTerm::Num(s) => CnfTerm::Num(s.clone()),
            FTerm::Str(s) => CnfTerm::Str(s.clone()),
        }
    }
}

// ── Build Formula from KifStore ───────────────────────────────────────────────

fn build_formula(store: &KifStore, sid: SentenceId) -> Formula {
    let sentence = &store.sentences[store.sent_idx(sid)];
    if sentence.elements.is_empty() { return Formula::True; }
    match sentence.elements.first() {
        Some(Element::Op(op)) => build_op_formula(store, sid, op.clone()),
        Some(Element::Symbol(_)) | Some(Element::Variable { .. }) => build_atom(store, sid),
        _ => {
            log::warn!(target: "sumo_kb::cnf",
                "unexpected element at head position in sentence {}", sid);
            Formula::True
        }
    }
}

fn build_op_formula(store: &KifStore, sid: SentenceId, op: OpKind) -> Formula {
    let sentence = &store.sentences[store.sent_idx(sid)];
    let args: Vec<&Element> = sentence.elements[1..].iter().collect();
    match op {
        OpKind::And => {
            let parts = args.iter().filter_map(|e| sub_formula(store, e)).collect();
            Formula::And(parts)
        }
        OpKind::Or => {
            let parts = args.iter().filter_map(|e| sub_formula(store, e)).collect();
            Formula::Or(parts)
        }
        OpKind::Not => {
            if let Some(inner) = args.first().and_then(|e| sub_formula(store, e)) {
                Formula::Not(Box::new(inner))
            } else {
                Formula::True
            }
        }
        OpKind::Implies => {
            let ant = args.first().and_then(|e| sub_formula(store, e)).unwrap_or(Formula::True);
            let con = args.get(1).and_then(|e| sub_formula(store, e)).unwrap_or(Formula::True);
            Formula::Or(vec![Formula::Not(Box::new(ant)), con])
        }
        OpKind::Iff => {
            let a = args.first().and_then(|e| sub_formula(store, e)).unwrap_or(Formula::True);
            let b = args.get(1).and_then(|e| sub_formula(store, e)).unwrap_or(Formula::True);
            let ab = Formula::Or(vec![Formula::Not(Box::new(a.clone())), b.clone()]);
            let ba = Formula::Or(vec![Formula::Not(Box::new(b)), a]);
            Formula::And(vec![ab, ba])
        }
        OpKind::Equal => {
            let a = args.first().map(|e| build_fterm(store, e)).unwrap_or(FTerm::Const(u64::MAX));
            let b = args.get(1).map(|e| build_fterm(store, e)).unwrap_or(FTerm::Const(u64::MAX));
            Formula::Equal(a, b)
        }
        OpKind::ForAll => {
            let vars = extract_quantifier_vars(store, args.first());
            let body = args.get(1).and_then(|e| sub_formula(store, e)).unwrap_or(Formula::True);
            if vars.is_empty() { body } else { Formula::Forall { vars, body: Box::new(body) } }
        }
        OpKind::Exists => {
            let vars = extract_quantifier_vars(store, args.first());
            let body = args.get(1).and_then(|e| sub_formula(store, e)).unwrap_or(Formula::True);
            if vars.is_empty() { body } else { Formula::Exists { vars, body: Box::new(body) } }
        }
    }
}

fn extract_quantifier_vars(store: &KifStore, var_list_elem: Option<&&Element>) -> Vec<SymbolId> {
    match var_list_elem {
        Some(Element::Sub(list_sid)) => {
            store.sentences[store.sent_idx(*list_sid)].elements.iter()
                .filter_map(|e| if let Element::Variable { id, .. } = e { Some(*id) } else { None })
                .collect()
        }
        _ => Vec::new(),
    }
}

fn build_atom(store: &KifStore, sid: SentenceId) -> Formula {
    let sentence = &store.sentences[store.sent_idx(sid)];
    let head = sentence.elements.first().map(|e| build_fterm(store, e))
        .unwrap_or(FTerm::Const(u64::MAX));
    let args = sentence.elements[1..].iter().map(|e| build_fterm(store, e)).collect();
    Formula::Atom { pred: head, args }
}

fn sub_formula(store: &KifStore, elem: &Element) -> Option<Formula> {
    match elem {
        Element::Sub(sid)           => Some(build_formula(store, *sid)),
        Element::Variable { id, .. } => Some(Formula::Atom { pred: FTerm::Var(*id), args: vec![] }),
        Element::Symbol(id)          => Some(Formula::Atom { pred: FTerm::Const(*id), args: vec![] }),
        _                            => None,
    }
}

fn build_fterm(store: &KifStore, elem: &Element) -> FTerm {
    match elem {
        Element::Symbol(id)                    => FTerm::Const(*id),
        Element::Variable { id, .. }           => FTerm::Var(*id),
        Element::Literal(KifLiteral::Number(n)) => FTerm::Num(n.clone()),
        Element::Literal(KifLiteral::Str(s))   => FTerm::Str(s.clone()),
        Element::Op(op)                        => {
            // Operator used as a term — treat as a constant with a synthetic id
            let id = store.sym_id(op.name()).unwrap_or(u64::MAX);
            FTerm::Const(id)
        }
        Element::Sub(sid) => {
            let sentence = &store.sentences[store.sent_idx(*sid)];
            if sentence.elements.is_empty() { return FTerm::Const(u64::MAX); }
            let head = sentence.elements.first().map(|e| build_fterm(store, e))
                .unwrap_or(FTerm::Const(u64::MAX));
            let args: Vec<FTerm> = sentence.elements[1..]
                .iter().map(|e| build_fterm(store, e)).collect();
            let fn_id = match &head { FTerm::Const(id) => *id, _ => 0 };
            if args.is_empty() { head } else { FTerm::SkolemFn { id: fn_id, args } }
        }
    }
}

// ── CNF transformation passes ─────────────────────────────────────────────────

fn to_nnf(f: Formula) -> Formula {
    match f {
        Formula::Not(inner)            => negate(to_nnf(*inner)),
        Formula::And(parts)            => Formula::And(parts.into_iter().map(to_nnf).collect()),
        Formula::Or(parts)             => Formula::Or(parts.into_iter().map(to_nnf).collect()),
        Formula::Forall { vars, body } => Formula::Forall { vars, body: Box::new(to_nnf(*body)) },
        Formula::Exists { vars, body } => Formula::Exists { vars, body: Box::new(to_nnf(*body)) },
        other                          => other,
    }
}

fn negate(f: Formula) -> Formula {
    match f {
        Formula::Not(inner)            => *inner,
        Formula::And(parts)            => Formula::Or(parts.into_iter().map(negate).collect()),
        Formula::Or(parts)             => Formula::And(parts.into_iter().map(negate).collect()),
        Formula::True                  => Formula::False,
        Formula::False                 => Formula::True,
        Formula::Forall { vars, body } => Formula::Exists { vars, body: Box::new(negate(*body)) },
        Formula::Exists { vars, body } => Formula::Forall { vars, body: Box::new(negate(*body)) },
        other                          => Formula::Not(Box::new(other)),
    }
}

fn skolemize(
    f:              Formula,
    universal_vars: &[SymbolId],
    counter:        &mut u64,
    skolem_out:     &mut Vec<Symbol>,
) -> Formula {
    match f {
        Formula::Forall { vars, body } => {
            let mut new_universal = universal_vars.to_vec();
            new_universal.extend_from_slice(&vars);
            let body = skolemize(*body, &new_universal, counter, skolem_out);
            Formula::Forall { vars, body: Box::new(body) }
        }
        Formula::Exists { vars, body } => {
            let arity = universal_vars.len();
            let mut subst: Vec<(SymbolId, FTerm)> = Vec::new();
            for var_id in &vars {
                let sk_id = 0x8000_0000_0000_0000u64 | *counter;
                *counter += 1;
                let sk_name = format!("sk_{}", sk_id);
                skolem_out.push(Symbol {
                    name:         sk_name.clone(),
                    head_sentences: Vec::new(),
                    all_sentences:  Vec::new(),
                    is_skolem:    true,
                    skolem_arity: Some(arity),
                });
                let sk_term = if arity == 0 {
                    FTerm::Const(sk_id)
                } else {
                    FTerm::SkolemFn {
                        id:   sk_id,
                        args: universal_vars.iter().map(|v| FTerm::Var(*v)).collect(),
                    }
                };
                subst.push((*var_id, sk_term));
                log::debug!(target: "sumo_kb::cnf",
                    "Skolemize: existential var {} → Skolem '{}' (arity {})",
                    var_id, sk_name, arity);
            }
            let body = subst_formula(*body, &subst);
            skolemize(body, universal_vars, counter, skolem_out)
        }
        Formula::Not(inner)  => Formula::Not(Box::new(skolemize(*inner, universal_vars, counter, skolem_out))),
        Formula::And(parts)  => Formula::And(parts.into_iter().map(|p| skolemize(p, universal_vars, counter, skolem_out)).collect()),
        Formula::Or(parts)   => Formula::Or(parts.into_iter().map(|p| skolemize(p, universal_vars, counter, skolem_out)).collect()),
        other                => other,
    }
}

fn subst_formula(f: Formula, subst: &[(SymbolId, FTerm)]) -> Formula {
    match f {
        Formula::Atom { pred, args } => Formula::Atom {
            pred: subst_term(pred, subst),
            args: args.into_iter().map(|t| subst_term(t, subst)).collect(),
        },
        Formula::Not(inner)             => Formula::Not(Box::new(subst_formula(*inner, subst))),
        Formula::And(parts)             => Formula::And(parts.into_iter().map(|p| subst_formula(p, subst)).collect()),
        Formula::Or(parts)              => Formula::Or(parts.into_iter().map(|p| subst_formula(p, subst)).collect()),
        Formula::Forall { vars, body }  => Formula::Forall { vars, body: Box::new(subst_formula(*body, subst)) },
        Formula::Exists { vars, body }  => Formula::Exists { vars, body: Box::new(subst_formula(*body, subst)) },
        Formula::Equal(a, b)            => Formula::Equal(subst_term(a, subst), subst_term(b, subst)),
        other                           => other,
    }
}

fn subst_term(t: FTerm, subst: &[(SymbolId, FTerm)]) -> FTerm {
    match &t {
        FTerm::Var(id) => {
            if let Some((_, r)) = subst.iter().find(|(v, _)| v == id) { r.clone() } else { t }
        }
        FTerm::SkolemFn { id, args } => FTerm::SkolemFn {
            id:   *id,
            args: args.iter().map(|a| subst_term(a.clone(), subst)).collect(),
        },
        _ => t,
    }
}

fn drop_forall(f: Formula) -> Formula {
    match f {
        Formula::Forall { body, .. } => drop_forall(*body),
        Formula::And(parts)          => Formula::And(parts.into_iter().map(drop_forall).collect()),
        Formula::Or(parts)           => Formula::Or(parts.into_iter().map(drop_forall).collect()),
        Formula::Not(inner)          => Formula::Not(Box::new(drop_forall(*inner))),
        other                        => other,
    }
}

#[derive(Debug, Clone)]
struct FLiteral { positive: bool, pred: FTerm, args: Vec<FTerm> }

fn distribute(f: Formula, max_clauses: usize) -> Result<Vec<Vec<FLiteral>>, KbError> {
    match f {
        Formula::And(parts) => {
            let mut clauses: Vec<Vec<FLiteral>> = Vec::new();
            for part in parts {
                clauses.extend(distribute(part, max_clauses)?);
                if clauses.len() > max_clauses {
                    return Err(KbError::Other(format!(
                        "clause count exceeded limit of {}", max_clauses)));
                }
            }
            Ok(clauses)
        }
        Formula::Or(parts) => {
            let mut result: Vec<Vec<FLiteral>> = vec![vec![]];
            for part in parts {
                let sub = distribute(part, max_clauses)?;
                let mut new_result: Vec<Vec<FLiteral>> = Vec::new();
                for existing in &result {
                    for sub_clause in &sub {
                        let mut merged = existing.clone();
                        merged.extend(sub_clause.iter().cloned());
                        new_result.push(merged);
                        if new_result.len() > max_clauses {
                            return Err(KbError::Other(format!(
                                "clause count exceeded limit of {}", max_clauses)));
                        }
                    }
                }
                result = new_result;
            }
            Ok(result)
        }
        Formula::Atom { pred, args } => {
            Ok(vec![vec![FLiteral { positive: true, pred, args }]])
        }
        Formula::Not(inner) => match *inner {
            Formula::Atom { pred, args } => Ok(vec![vec![FLiteral { positive: false, pred, args }]]),
            Formula::Equal(a, b) => Ok(vec![vec![FLiteral {
                positive: false,
                pred:     FTerm::Const(u64::MAX),
                args:     vec![a, b],
            }]]),
            other => {
                log::warn!(target: "sumo_kb::cnf",
                    "Not(non-atom) after NNF pass; treating as unit clause");
                distribute(Formula::Not(Box::new(other)), max_clauses)
            }
        },
        Formula::Equal(a, b) => Ok(vec![vec![FLiteral {
            positive: true,
            pred:     FTerm::Const(u64::MAX),
            args:     vec![a, b],
        }]]),
        Formula::True  => Ok(vec![]),
        Formula::False => Ok(vec![vec![]]),
        Formula::Forall { body, .. } | Formula::Exists { body, .. } => {
            log::warn!(target: "sumo_kb::cnf", "quantifier survived to distribution pass — dropping");
            distribute(*body, max_clauses)
        }
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Convert a single root sentence into a set of CNF clauses.
///
/// New Skolem symbols are appended to `skolem_out`; the caller must intern
/// them into the `KifStore`.  `skolem_counter` must be unique across all calls
/// within a session to avoid name collisions.
pub(crate) fn sentence_to_cnf(
    store:          &KifStore,
    sid:            SentenceId,
    skolem_counter: &mut u64,
    skolem_out:     &mut Vec<Symbol>,
    max_clauses:    usize,
) -> Result<Vec<Clause>, KbError> {
    log::debug!(target: "sumo_kb::cnf", "converting sentence {}", sid);

    let formula = build_formula(store, sid);
    let formula = to_nnf(formula);

    let formula = skolemize(formula, &[], skolem_counter, skolem_out);
    log::trace!(target: "sumo_kb::cnf",
        "Skolemized, {} new Skolem symbols", skolem_out.len());

    let formula   = drop_forall(formula);
    let raw       = distribute(formula, max_clauses)?;
    log::debug!(target: "sumo_kb::cnf",
        "{} clause(s) from sentence {}", raw.len(), sid);

    let clauses = raw.into_iter().map(|lits| Clause {
        literals: lits.into_iter().map(|fl| CnfLiteral {
            positive: fl.positive,
            pred:     fl.pred.to_cnf(),
            args:     fl.args.iter().map(FTerm::to_cnf).collect(),
        }).collect(),
    }).collect();

    Ok(clauses)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kif_store::{load_kif, KifStore};

    fn to_cnf_helper(kif: &str) -> (Vec<Clause>, KifStore) {
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "test");
        let sid = *store.roots.last().expect("no sentence parsed");
        let mut counter = 0u64;
        let mut skolems = Vec::new();
        let clauses = sentence_to_cnf(&store, sid, &mut counter, &mut skolems, 10_000)
            .expect("CNF conversion failed");
        (clauses, store)
    }

    #[test]
    fn simple_atom_is_single_clause() {
        let (clauses, _) = to_cnf_helper("(subclass Human Animal)");
        assert_eq!(clauses.len(), 1);
        assert_eq!(clauses[0].literals.len(), 1);
        assert!(clauses[0].literals[0].positive);
    }

    #[test]
    fn negated_atom_is_single_negative_clause() {
        let (clauses, _) = to_cnf_helper("(not (subclass Human Animal))");
        assert_eq!(clauses.len(), 1);
        assert!(!clauses[0].literals[0].positive);
    }

    #[test]
    fn conjunction_splits_into_separate_clauses() {
        let (clauses, _) = to_cnf_helper("(and (subclass Human Animal) (subclass Animal Entity))");
        assert_eq!(clauses.len(), 2);
        assert!(clauses.iter().all(|c| c.literals.len() == 1));
    }

    #[test]
    fn implication_becomes_single_disjunction() {
        let (clauses, _) = to_cnf_helper("(=> (subclass ?X Animal) (instance ?X Entity))");
        assert_eq!(clauses.len(), 1);
        assert_eq!(clauses[0].literals.len(), 2);
        let neg_count = clauses[0].literals.iter().filter(|l| !l.positive).count();
        let pos_count = clauses[0].literals.iter().filter(|l|  l.positive).count();
        assert_eq!(neg_count, 1);
        assert_eq!(pos_count, 1);
    }

    #[test]
    fn biconditional_produces_two_clauses() {
        let (clauses, _) = to_cnf_helper("(<=> (subclass ?X Animal) (instance ?X Entity))");
        assert_eq!(clauses.len(), 2);
        for clause in &clauses {
            assert_eq!(clause.literals.len(), 2);
        }
    }

    #[test]
    fn double_negation_eliminated() {
        let (clauses, _) = to_cnf_helper("(not (not (subclass Human Animal)))");
        assert_eq!(clauses.len(), 1);
        assert!(clauses[0].literals[0].positive);
    }

    #[test]
    fn de_morgan_not_and_becomes_disjunction() {
        let (clauses, _) = to_cnf_helper(
            "(not (and (subclass Human Animal) (subclass Animal Entity)))");
        assert_eq!(clauses.len(), 1);
        assert_eq!(clauses[0].literals.len(), 2);
        assert!(clauses[0].literals.iter().all(|l| !l.positive));
    }

    #[test]
    fn de_morgan_not_or_becomes_conjunction() {
        let (clauses, _) = to_cnf_helper(
            "(not (or (subclass Human Animal) (subclass Animal Entity)))");
        assert_eq!(clauses.len(), 2);
        assert!(clauses.iter().all(|c| c.literals.len() == 1 && !c.literals[0].positive));
    }

    #[test]
    fn forall_variable_is_cnf_var() {
        let (clauses, _) = to_cnf_helper("(forall (?X) (subclass ?X Animal))");
        assert_eq!(clauses.len(), 1);
        let lit = &clauses[0].literals[0];
        assert!(matches!(lit.args[0], CnfTerm::Var(_)),
            "universally quantified ?X must be a Var, got: {:?}", lit.args[0]);
    }

    #[test]
    fn exists_no_outer_forall_becomes_skolem_const() {
        let (clauses, _) = to_cnf_helper("(exists (?X) (instance ?X Human))");
        assert_eq!(clauses.len(), 1);
        let lit = &clauses[0].literals[0];
        assert!(matches!(lit.args[0], CnfTerm::Const(_)),
            "?X with no outer forall must become a Skolem constant, got: {:?}", lit.args[0]);
    }

    #[test]
    fn exists_under_forall_becomes_skolem_fn() {
        let (clauses, _) = to_cnf_helper("(forall (?X) (exists (?Y) (instance ?Y ?X)))");
        assert_eq!(clauses.len(), 1);
        let lit = &clauses[0].literals[0];
        let has_skolem_fn = lit.args.iter().any(|t| matches!(t, CnfTerm::SkolemFn { .. }));
        assert!(has_skolem_fn, "?Y must become a Skolem fn of ?X, got: {:?}", lit.args);
    }

    #[test]
    fn equality_roundtrips() {
        let (clauses, _) = to_cnf_helper("(equal ?X ?Y)");
        assert_eq!(clauses.len(), 1);
        let lit = &clauses[0].literals[0];
        assert!(lit.positive);
        assert_eq!(lit.args.len(), 2);
        assert!(matches!(lit.args[0], CnfTerm::Var(_)));
        assert!(matches!(lit.args[1], CnfTerm::Var(_)));
    }
}
