/// CNF (Conjunctive Normal Form) conversion for KIF/SUMO formulas.
///
/// # Pipeline
///
/// 1. **Build intermediate `Formula` tree** from a KifStore `SentenceId`.
/// 2. **Eliminate biconditionals and implications** (`<=>`, `=>`).
/// 3. **NNF** — push `not` inward via De Morgan's; eliminate double negation.
/// 4. Variables are already standardised apart in KifStore (scope-naming
///    `X@<scope_id>`), so step 3 of classical CNF is skipped.
/// 5. **Skolemize** — replace `∃x` (with universal vars `y₁…yₙ` in scope)
///    with a fresh Skolem symbol `sk_N(y₁,…,yₙ)`.  Skolem symbols are
///    appended to `skolem_symbols` for the caller to intern.
/// 6. **Drop universal quantifiers** — all remaining vars are universally
///    quantified (implicit in CNF).
/// 7. **Distribute `∨` over `∧`** — convert to CNF.  Aborts with
///    `StoreError::ClauseCountExceeded` if the intermediate clause count
///    exceeds `max_clauses`.
/// 8. **Extract clauses**.

use sumo_parser_core::store::{Element, KifStore, Literal as KifLiteral, SentenceId, SymbolId};
use sumo_parser_core::tokenizer::OpKind;
use log;

use crate::schema::{Clause, CnfLiteral, CnfTerm, StoredSymbol};
use crate::StoreError;

// ── Intermediate Formula tree ─────────────────────────────────────────────────

/// An intermediate formula used only during CNF conversion.  All identifiers
/// are persistent `SymbolId`s.
#[derive(Debug, Clone)]
enum Formula {
    /// A ground or partially-ground atomic formula: `pred(args…)`.
    Atom { pred: FTerm, args: Vec<FTerm> },
    Not(Box<Formula>),
    And(Vec<Formula>),
    Or(Vec<Formula>),
    /// Universal quantifier with a list of variable SymbolIds.
    Forall { vars: Vec<SymbolId>, body: Box<Formula> },
    /// Existential quantifier.
    Exists { vars: Vec<SymbolId>, body: Box<Formula> },
    Equal(FTerm, FTerm),
    True,
    False,
}

/// A term in the intermediate representation.
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
            FTerm::Const(id)              => CnfTerm::Const(*id),
            FTerm::Var(id)                => CnfTerm::Var(*id),
            FTerm::SkolemFn { id, args }  => CnfTerm::SkolemFn {
                id: *id,
                args: args.iter().map(FTerm::to_cnf).collect(),
            },
            FTerm::Num(s)                 => CnfTerm::Num(s.clone()),
            FTerm::Str(s)                 => CnfTerm::Str(s.clone()),
        }
    }
}

// ── Build Formula from KifStore ───────────────────────────────────────────────

/// Convert a KifStore sentence into the intermediate `Formula` tree.
/// `universal_scope` contains variables that were bound by outer `forall`
/// nodes — needed to determine the implicit outer universal quantifier.
fn build_formula(
    store: &KifStore,
    sid: SentenceId,
    id_map: &dyn Fn(&str) -> SymbolId,
) -> Formula {
    let sentence = &store.sentences[sid as usize];

    if sentence.elements.is_empty() {
        return Formula::True;
    }

    match sentence.elements.first() {
        Some(Element::Op(op)) => {
            build_op_formula(store, sid, op.clone(), id_map)
        }
        Some(Element::Symbol(_)) | Some(Element::Variable { .. }) => {
            build_atom(store, sid, id_map)
        }
        _ => {
            log::warn!("CNF: unexpected element at head position in sentence {}", sid);
            Formula::True
        }
    }
}

fn build_op_formula(
    store: &KifStore,
    sid: SentenceId,
    op: OpKind,
    id_map: &dyn Fn(&str) -> SymbolId,
) -> Formula {
    let sentence = &store.sentences[sid as usize];
    let args: Vec<&Element> = sentence.elements[1..].iter().collect();

    match op {
        OpKind::And => {
            let parts = args.iter().filter_map(|e| sub_formula(store, e, id_map)).collect();
            Formula::And(parts)
        }
        OpKind::Or => {
            let parts = args.iter().filter_map(|e| sub_formula(store, e, id_map)).collect();
            Formula::Or(parts)
        }
        OpKind::Not => {
            if let Some(inner) = args.first().and_then(|e| sub_formula(store, e, id_map)) {
                Formula::Not(Box::new(inner))
            } else {
                Formula::True
            }
        }
        OpKind::Implies => {
            let ant = args.first().and_then(|e| sub_formula(store, e, id_map)).unwrap_or(Formula::True);
            let con = args.get(1).and_then(|e| sub_formula(store, e, id_map)).unwrap_or(Formula::True);
            Formula::Or(vec![Formula::Not(Box::new(ant)), con])
        }
        OpKind::Iff => {
            let a = args.first().and_then(|e| sub_formula(store, e, id_map)).unwrap_or(Formula::True);
            let b = args.get(1).and_then(|e| sub_formula(store, e, id_map)).unwrap_or(Formula::True);
            // A <=> B  ==  (A => B) & (B => A)
            //           ==  (~A | B) & (~B | A)
            let ab = Formula::Or(vec![Formula::Not(Box::new(a.clone())), b.clone()]);
            let ba = Formula::Or(vec![Formula::Not(Box::new(b)), a]);
            Formula::And(vec![ab, ba])
        }
        OpKind::Equal => {
            let a = args.first().map(|e| build_fterm(store, e, id_map)).unwrap_or(FTerm::Const(u64::MAX));
            let b = args.get(1).map(|e| build_fterm(store, e, id_map)).unwrap_or(FTerm::Const(u64::MAX));
            Formula::Equal(a, b)
        }
        OpKind::ForAll => {
            let vars = extract_quantifier_vars(store, args.first(), id_map);
            let body = args.get(1).and_then(|e| sub_formula(store, e, id_map)).unwrap_or(Formula::True);
            if vars.is_empty() { body } else { Formula::Forall { vars, body: Box::new(body) } }
        }
        OpKind::Exists => {
            let vars = extract_quantifier_vars(store, args.first(), id_map);
            let body = args.get(1).and_then(|e| sub_formula(store, e, id_map)).unwrap_or(Formula::True);
            if vars.is_empty() { body } else { Formula::Exists { vars, body: Box::new(body) } }
        }
    }
}

fn extract_quantifier_vars(
    store: &KifStore,
    var_list_elem: Option<&&Element>,
    _id_map: &dyn Fn(&str) -> SymbolId,
) -> Vec<SymbolId> {
    match var_list_elem {
        Some(Element::Sub(list_sid)) => {
            store.sentences[*list_sid as usize].elements.iter().filter_map(|e| {
                if let Element::Variable { id, .. } = e { Some(*id) } else { None }
            }).collect()
        }
        _ => Vec::new(),
    }
}

fn build_atom(
    store: &KifStore,
    sid: SentenceId,
    id_map: &dyn Fn(&str) -> SymbolId,
) -> Formula {
    let sentence = &store.sentences[sid as usize];
    let head = sentence.elements.first().map(|e| build_fterm(store, e, id_map))
        .unwrap_or(FTerm::Const(u64::MAX));
    let args = sentence.elements[1..].iter()
        .map(|e| build_fterm(store, e, id_map))
        .collect();
    Formula::Atom { pred: head, args }
}

fn sub_formula(
    store: &KifStore,
    elem: &Element,
    id_map: &dyn Fn(&str) -> SymbolId,
) -> Option<Formula> {
    match elem {
        Element::Sub(sid) => Some(build_formula(store, *sid, id_map)),
        Element::Variable { id, .. } => {
            // Bare variable used as a proposition (e.g. `?P` in `(=> (foo ?X) ?P)`)
            Some(Formula::Atom { pred: FTerm::Var(*id), args: vec![] })
        }
        Element::Symbol(id) => {
            // Bare symbol used as a formula — treat as 0-arity atom
            Some(Formula::Atom { pred: FTerm::Const(*id), args: vec![] })
        }
        _ => None,
    }
}

fn build_fterm(
    store: &KifStore,
    elem: &Element,
    id_map: &dyn Fn(&str) -> SymbolId,
) -> FTerm {
    match elem {
        Element::Symbol(id)                   => FTerm::Const(*id),
        Element::Variable { id, .. }          => FTerm::Var(*id),
        Element::Literal(KifLiteral::Number(n)) => FTerm::Num(n.clone()),
        Element::Literal(KifLiteral::Str(s))  => FTerm::Str(s.clone()),
        Element::Op(op)                       => {
            // Operator used as a term — create a constant for its name
            FTerm::Const(id_map(op.name()))
        }
        Element::Sub(sid) => {
            // Sub-sentence used as a term — build as a function application
            let sentence = &store.sentences[*sid as usize];
            if sentence.elements.is_empty() { return FTerm::Const(u64::MAX); }
            let head = sentence.elements.first().map(|e| build_fterm(store, e, id_map))
                .unwrap_or(FTerm::Const(u64::MAX));
            let args: Vec<FTerm> = sentence.elements[1..].iter()
                .map(|e| build_fterm(store, e, id_map))
                .collect();
            // Represent as SkolemFn-like structure using the head symbol as the fn id
            let fn_id = match &head { FTerm::Const(id) => *id, _ => 0 };
            if args.is_empty() { head } else { FTerm::SkolemFn { id: fn_id, args } }
        }
    }
}

// ── CNF transformation passes ─────────────────────────────────────────────────

/// Pass 1+2: eliminate biconditionals/implications and push NOT inward (NNF).
/// Implications are already eliminated in build_formula (Implies → Or(Not(A), B),
/// Iff → And(Or(Not(A),B), Or(Not(B),A))), so this pass only handles NNF.
fn to_nnf(f: Formula) -> Formula {
    match f {
        Formula::Not(inner) => negate(to_nnf(*inner)),
        Formula::And(parts) => Formula::And(parts.into_iter().map(to_nnf).collect()),
        Formula::Or(parts)  => Formula::Or(parts.into_iter().map(to_nnf).collect()),
        Formula::Forall { vars, body } => Formula::Forall { vars, body: Box::new(to_nnf(*body)) },
        Formula::Exists { vars, body } => Formula::Exists { vars, body: Box::new(to_nnf(*body)) },
        other => other,
    }
}

/// Push a negation inward (De Morgan) during NNF construction.
fn negate(f: Formula) -> Formula {
    match f {
        Formula::Not(inner)             => *inner,   // double negation
        Formula::And(parts)             => Formula::Or(parts.into_iter().map(|p| negate(p)).collect()),
        Formula::Or(parts)              => Formula::And(parts.into_iter().map(|p| negate(p)).collect()),
        Formula::True                   => Formula::False,
        Formula::False                  => Formula::True,
        Formula::Forall { vars, body }  => Formula::Exists { vars, body: Box::new(negate(*body)) },
        Formula::Exists { vars, body }  => Formula::Forall { vars, body: Box::new(negate(*body)) },
        other                           => Formula::Not(Box::new(other)),
    }
}

/// Pass 3 (Skolemize): replace `∃` with Skolem functions.
/// `universal_vars` is the list of universally quantified variables in scope.
/// New Skolem symbols are pushed to `skolem_out`.
fn skolemize(
    f: Formula,
    universal_vars: &[SymbolId],
    skolem_counter: &mut u64,
    skolem_out: &mut Vec<StoredSymbol>,
) -> Formula {
    match f {
        Formula::Forall { vars, body } => {
            let mut new_universal = universal_vars.to_vec();
            new_universal.extend_from_slice(&vars);
            let body = skolemize(*body, &new_universal, skolem_counter, skolem_out);
            // Keep the Forall wrapper — it's dropped in the next pass
            Formula::Forall { vars, body: Box::new(body) }
        }
        Formula::Exists { vars, body } => {
            // For each existential variable, create a Skolem symbol
            let arity = universal_vars.len();
            let mut subst: Vec<(SymbolId, FTerm)> = Vec::new();
            for var_id in &vars {
                let sk_id_synthetic = 0x8000_0000_0000_0000u64 | *skolem_counter;
                *skolem_counter += 1;
                let sk_name = format!("sk_{}", sk_id_synthetic);
                skolem_out.push(StoredSymbol {
                    id:           sk_id_synthetic,
                    name:         sk_name.clone(),
                    is_skolem:    true,
                    skolem_arity: Some(arity),
                });
                let sk_term = if arity == 0 {
                    FTerm::Const(sk_id_synthetic)
                } else {
                    FTerm::SkolemFn {
                        id:   sk_id_synthetic,
                        args: universal_vars.iter().map(|v| FTerm::Var(*v)).collect(),
                    }
                };
                subst.push((*var_id, sk_term));
                log::debug!(
                    "CNF Skolemize: existential var {:?} → Skolem fn '{}' (arity {})",
                    var_id, sk_name, arity
                );
            }
            // Apply substitution to the body, then skolemize it further
            let body = subst_formula(*body, &subst);
            skolemize(body, universal_vars, skolem_counter, skolem_out)
        }
        Formula::Not(inner)   => Formula::Not(Box::new(skolemize(*inner, universal_vars, skolem_counter, skolem_out))),
        Formula::And(parts)   => Formula::And(parts.into_iter().map(|p| skolemize(p, universal_vars, skolem_counter, skolem_out)).collect()),
        Formula::Or(parts)    => Formula::Or(parts.into_iter().map(|p| skolemize(p, universal_vars, skolem_counter, skolem_out)).collect()),
        other                 => other,
    }
}

/// Substitute `var_id` → `term` throughout a formula.
fn subst_formula(f: Formula, subst: &[(SymbolId, FTerm)]) -> Formula {
    match f {
        Formula::Atom { pred, args } => Formula::Atom {
            pred: subst_term(pred, subst),
            args: args.into_iter().map(|t| subst_term(t, subst)).collect(),
        },
        Formula::Not(inner)              => Formula::Not(Box::new(subst_formula(*inner, subst))),
        Formula::And(parts)              => Formula::And(parts.into_iter().map(|p| subst_formula(p, subst)).collect()),
        Formula::Or(parts)               => Formula::Or(parts.into_iter().map(|p| subst_formula(p, subst)).collect()),
        Formula::Forall { vars, body }   => Formula::Forall { vars, body: Box::new(subst_formula(*body, subst)) },
        Formula::Exists { vars, body }   => Formula::Exists { vars, body: Box::new(subst_formula(*body, subst)) },
        Formula::Equal(a, b)             => Formula::Equal(subst_term(a, subst), subst_term(b, subst)),
        other                            => other,
    }
}

fn subst_term(t: FTerm, subst: &[(SymbolId, FTerm)]) -> FTerm {
    match &t {
        FTerm::Var(id) => {
            if let Some((_, replacement)) = subst.iter().find(|(v, _)| v == id) {
                replacement.clone()
            } else {
                t
            }
        }
        FTerm::SkolemFn { id, args } => FTerm::SkolemFn {
            id: *id,
            args: args.iter().map(|a| subst_term(a.clone(), subst)).collect(),
        },
        _ => t,
    }
}

/// Pass 4: drop all `Forall` wrappers (variables are implicitly universal after Skolemization).
fn drop_forall(f: Formula) -> Formula {
    match f {
        Formula::Forall { body, .. }  => drop_forall(*body),
        Formula::And(parts)           => Formula::And(parts.into_iter().map(drop_forall).collect()),
        Formula::Or(parts)            => Formula::Or(parts.into_iter().map(drop_forall).collect()),
        Formula::Not(inner)           => Formula::Not(Box::new(drop_forall(*inner))),
        other                         => other,
    }
}

/// Pass 5: distribute `∨` over `∧` to produce CNF.
/// Returns a list of clauses (each clause is a list of literals).
/// Aborts with `ClauseCountExceeded` if `max_clauses` is exceeded.
fn distribute(f: Formula, max_clauses: usize) -> Result<Vec<Vec<FLiteral>>, StoreError> {
    match f {
        Formula::And(parts) => {
            let mut clauses: Vec<Vec<FLiteral>> = Vec::new();
            for part in parts {
                let sub = distribute(part, max_clauses)?;
                clauses.extend(sub);
                if clauses.len() > max_clauses {
                    return Err(StoreError::ClauseCountExceeded { limit: max_clauses });
                }
            }
            Ok(clauses)
        }
        Formula::Or(parts) => {
            // Start with one empty clause and cross-product with each part's clauses
            let mut result: Vec<Vec<FLiteral>> = vec![vec![]];
            for part in parts {
                let sub = distribute(part, max_clauses)?;
                let mut new_result: Vec<Vec<FLiteral>> = Vec::new();
                for existing_clause in &result {
                    for sub_clause in &sub {
                        let mut merged = existing_clause.clone();
                        merged.extend(sub_clause.iter().cloned());
                        new_result.push(merged);
                        if new_result.len() > max_clauses {
                            return Err(StoreError::ClauseCountExceeded { limit: max_clauses });
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
        Formula::Not(inner) => {
            match *inner {
                Formula::Atom { pred, args } => Ok(vec![vec![FLiteral { positive: false, pred, args }]]),
                Formula::Equal(a, b) => {
                    // Treat ~(a = b) as a single negative literal
                    Ok(vec![vec![FLiteral {
                        positive: false,
                        pred: FTerm::Const(u64::MAX), // placeholder; equality handled specially
                        args: vec![a, b],
                    }]])
                }
                other => {
                    // Should not occur after NNF — treat as a single atom
                    log::warn!("CNF: Not(non-atom) after NNF pass; treating as unit clause");
                    distribute(Formula::Not(Box::new(other)), max_clauses)
                }
            }
        }
        Formula::Equal(a, b) => {
            Ok(vec![vec![FLiteral { positive: true, pred: FTerm::Const(u64::MAX), args: vec![a, b] }]])
        }
        Formula::True  => Ok(vec![]),      // Tautology — no clauses needed
        Formula::False => Ok(vec![vec![]]), // Contradiction — empty clause (unsatisfiable)
        // Forall/Exists should have been eliminated before this pass
        Formula::Forall { body, .. } | Formula::Exists { body, .. } => {
            log::warn!("CNF: quantifier survived to distribution pass — dropping");
            distribute(*body, max_clauses)
        }
    }
}

/// An intermediate literal used during distribution.
#[derive(Debug, Clone)]
struct FLiteral {
    positive: bool,
    pred:     FTerm,
    args:     Vec<FTerm>,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Convert a single root sentence (by `sid`) into a set of CNF clauses.
///
/// # Arguments
/// * `store`           — the in-memory KifStore (with persistent IDs already remapped)
/// * `sid`             — the sentence to convert
/// * `id_map`          — closure mapping symbol names to their persistent SymbolIds
/// * `skolem_counter`  — global counter for generating unique Skolem names
/// * `skolem_out`      — accumulates new Skolem symbols for the caller to intern
/// * `max_clauses`     — hard upper bound on clause count per formula
pub fn sentence_to_cnf(
    store:          &KifStore,
    sid:            SentenceId,
    id_map:         &dyn Fn(&str) -> SymbolId,
    skolem_counter: &mut u64,
    skolem_out:     &mut Vec<StoredSymbol>,
    max_clauses:    usize,
) -> Result<Vec<Clause>, StoreError> {
    log::debug!("CNF: converting sentence {}", sid);

    // Step 1+2: build formula tree with implications already eliminated
    let formula = build_formula(store, sid, id_map);

    // Step 3: NNF
    let formula = to_nnf(formula);

    // Step 4: Skolemize (standardize-vars-apart is already done by scope naming)
    let mut sk_syms: Vec<StoredSymbol> = Vec::new();
    let formula = skolemize(formula, &[], skolem_counter, &mut sk_syms);
    log::debug!("CNF: Skolemized, generated {} new Skolem symbols", sk_syms.len());
    skolem_out.extend(sk_syms);

    // Step 5: Drop universal quantifiers
    let formula = drop_forall(formula);

    // Step 6: Distribute → CNF
    let raw_clauses = distribute(formula, max_clauses)?;
    log::debug!("CNF: {} clause(s) produced from sentence {}", raw_clauses.len(), sid);

    // Step 7: Convert intermediate literals to schema types
    let clauses = raw_clauses.into_iter().map(|lits| {
        Clause {
            literals: lits.into_iter().map(|fl| CnfLiteral {
                positive: fl.positive,
                pred:     fl.pred.to_cnf(),
                args:     fl.args.iter().map(FTerm::to_cnf).collect(),
            }).collect(),
        }
    }).collect();

    Ok(clauses)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sumo_parser_core::{KifStore, load_kif};
    use std::collections::HashMap;
    use crate::display::clause_to_kif;
    use crate::schema::CnfTerm;

    /// Build a sym_names map from an in-memory KifStore (id = Vec index).
    fn sym_names(store: &KifStore) -> HashMap<u64, String> {
        store.symbol_data.iter().enumerate()
            .map(|(i, s)| (i as u64, s.name.clone()))
            .collect()
    }

    /// Parse `kif`, convert the last root sentence to CNF, return (clauses, store).
    fn to_cnf(kif: &str) -> (Vec<Clause>, KifStore) {
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "test");
        let sid = *store.roots.last().expect("no sentence parsed");
        let mut counter = 0u64;
        let mut skolems  = vec![];
        let clauses = sentence_to_cnf(
            &store, sid, &|_| 0, &mut counter, &mut skolems, 10_000,
        ).expect("CNF conversion failed");
        (clauses, store)
    }

    // ── Structure tests ───────────────────────────────────────────────────────

    #[test]
    fn simple_atom_is_single_clause() {
        let (clauses, _) = to_cnf("(subclass Human Animal)");
        assert_eq!(clauses.len(), 1);
        assert_eq!(clauses[0].literals.len(), 1);
        assert!(clauses[0].literals[0].positive);
    }

    #[test]
    fn negated_atom_is_single_negative_clause() {
        let (clauses, _) = to_cnf("(not (subclass Human Animal))");
        assert_eq!(clauses.len(), 1);
        assert_eq!(clauses[0].literals.len(), 1);
        assert!(!clauses[0].literals[0].positive);
    }

    #[test]
    fn conjunction_splits_into_separate_clauses() {
        let (clauses, _) = to_cnf("(and (subclass Human Animal) (subclass Animal Entity))");
        assert_eq!(clauses.len(), 2);
        assert!(clauses.iter().all(|c| c.literals.len() == 1));
    }

    #[test]
    fn implication_becomes_single_disjunction() {
        // (=> A B) ≡ (or (not A) B)
        let (clauses, _) = to_cnf("(=> (subclass ?X Animal) (instance ?X Entity))");
        assert_eq!(clauses.len(), 1, "one clause");
        assert_eq!(clauses[0].literals.len(), 2, "two literals");
        let neg_count = clauses[0].literals.iter().filter(|l| !l.positive).count();
        let pos_count = clauses[0].literals.iter().filter(|l|  l.positive).count();
        assert_eq!(neg_count, 1, "one negative literal (antecedent)");
        assert_eq!(pos_count, 1, "one positive literal (consequent)");
    }

    #[test]
    fn biconditional_produces_two_clauses() {
        // (<=> A B) ≡ (and (or (not A) B) (or (not B) A))
        let (clauses, _) = to_cnf("(<=> (subclass ?X Animal) (instance ?X Entity))");
        assert_eq!(clauses.len(), 2);
        for clause in &clauses {
            assert_eq!(clause.literals.len(), 2);
            assert_eq!(clause.literals.iter().filter(|l| !l.positive).count(), 1);
        }
    }

    #[test]
    fn double_negation_eliminated() {
        let (clauses, _) = to_cnf("(not (not (subclass Human Animal)))");
        assert_eq!(clauses.len(), 1);
        assert!(clauses[0].literals[0].positive, "double negation must be removed");
    }

    #[test]
    fn de_morgan_not_and_becomes_disjunction() {
        // ~(A & B) → (~A | ~B): one clause, two negative literals
        let (clauses, _) = to_cnf(
            "(not (and (subclass Human Animal) (subclass Animal Entity)))"
        );
        assert_eq!(clauses.len(), 1);
        assert_eq!(clauses[0].literals.len(), 2);
        assert!(clauses[0].literals.iter().all(|l| !l.positive));
    }

    #[test]
    fn de_morgan_not_or_becomes_conjunction() {
        // ~(A | B) → (~A & ~B): two clauses, each with one negative literal
        let (clauses, _) = to_cnf(
            "(not (or (subclass Human Animal) (subclass Animal Entity)))"
        );
        assert_eq!(clauses.len(), 2);
        assert!(clauses.iter().all(|c| c.literals.len() == 1 && !c.literals[0].positive));
    }

    #[test]
    fn forall_variable_is_cnf_var() {
        let (clauses, _) = to_cnf("(forall (?X) (subclass ?X Animal))");
        assert_eq!(clauses.len(), 1);
        let lit = &clauses[0].literals[0];
        assert!(
            matches!(lit.args[0], CnfTerm::Var(_)),
            "universally quantified ?X must be a Var term, got: {:?}", lit.args[0],
        );
    }

    #[test]
    fn exists_no_outer_forall_becomes_skolem_const() {
        // No enclosing forall → arity-0 Skolem → stored as Const
        let (clauses, _) = to_cnf("(exists (?X) (instance ?X Human))");
        assert_eq!(clauses.len(), 1);
        let lit = &clauses[0].literals[0];
        assert!(
            matches!(lit.args[0], CnfTerm::Const(_)),
            "?X with no outer forall must become a Skolem constant, got: {:?}", lit.args[0],
        );
    }

    #[test]
    fn exists_under_forall_becomes_skolem_fn() {
        // (forall (?X) (exists (?Y) (instance ?Y ?X)))
        // ?Y is replaced by sk(X) — a Skolem function of ?X
        let (clauses, _) = to_cnf(
            "(forall (?X) (exists (?Y) (instance ?Y ?X)))"
        );
        assert_eq!(clauses.len(), 1);
        let lit = &clauses[0].literals[0];
        let has_skolem_fn = lit.args.iter().any(|t| matches!(t, CnfTerm::SkolemFn { .. }));
        assert!(has_skolem_fn, "?Y must become a Skolem fn of ?X, got: {:?}", lit.args);
    }

    #[test]
    fn equality_roundtrips() {
        let (clauses, _) = to_cnf("(equal ?X ?Y)");
        assert_eq!(clauses.len(), 1);
        let lit = &clauses[0].literals[0];
        assert!(lit.positive);
        assert_eq!(lit.args.len(), 2);
        // Both sides are variables
        assert!(matches!(lit.args[0], CnfTerm::Var(_)));
        assert!(matches!(lit.args[1], CnfTerm::Var(_)));
    }

    // ── Display tests ─────────────────────────────────────────────────────────

    #[test]
    fn display_simple_atom() {
        let (clauses, store) = to_cnf("(subclass Human Animal)");
        let names = sym_names(&store);
        let s = clause_to_kif(&clauses[0], &names);
        assert!(s.contains("subclass"), "missing predicate: {}", s);
        assert!(s.contains("Human"),    "missing arg: {}", s);
        assert!(s.contains("Animal"),   "missing arg: {}", s);
        assert!(!s.starts_with("(or"), "unit clause must not be wrapped in or: {}", s);
        assert!(!s.starts_with("(not"), "positive atom must not be negated: {}", s);
    }

    #[test]
    fn display_negated_atom() {
        let (clauses, store) = to_cnf("(not (subclass Human Animal))");
        let names = sym_names(&store);
        let s = clause_to_kif(&clauses[0], &names);
        assert!(s.starts_with("(not "), "negative literal must start with (not: {}", s);
        assert!(s.contains("subclass"), "{}", s);
    }

    #[test]
    fn display_implication_as_disjunction() {
        let (clauses, store) = to_cnf("(=> (subclass ?X Animal) (instance ?X Entity))");
        let names = sym_names(&store);
        let s = clause_to_kif(&clauses[0], &names);
        assert!(s.starts_with("(or "), "implication clause must render as (or ...: {}", s);
        assert!(s.contains("(not "), "antecedent must be negated: {}", s);
    }

    #[test]
    fn display_variable_uses_question_mark() {
        let (clauses, store) = to_cnf("(forall (?X) (subclass ?X Animal))");
        let names = sym_names(&store);
        let s = clause_to_kif(&clauses[0], &names);
        assert!(s.contains("?X"), "variable must render as ?X: {}", s);
    }

    #[test]
    fn display_equality() {
        let (clauses, store) = to_cnf("(equal ?X ?Y)");
        let names = sym_names(&store);
        let s = clause_to_kif(&clauses[0], &names);
        assert!(s.starts_with("(equal "), "equality must render as (equal ...: {}", s);
    }
}
