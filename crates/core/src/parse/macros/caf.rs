//! Conjunctive Antecedent Form (CAF) normalization on the AST.
//!
//!   T1  (<=> A B)              → (=> A B) and (=> B A)
//!   T2  (=> A (=> B C))        → (=> (and A B) C)
//!   T3  (=> (or A B) C)        → (=> A C) and (=> B C)      [C cloned per branch]
//!   T4  (=> (and A (and …)) C) → (=> (and A …) C)
//!
//! Only top-level implication / biconditional roots are transformed; ground
//! assertions and other forms pass through unchanged.

use crate::OpKind;
use crate::parse::{AstNode, Span};

/// Associatively flatten nested `and` / `or` connectives throughout the tree:
/// `(and (and (and A B) C) D)` → `(and A B C D)`, `(or (or A B) C)` →
/// `(or A B C)`.  Only a connective nested directly under the same connective is
/// merged — `(and A (or B C))` is left intact.
///
/// Runs bottom-up (children flattened before their parent), so arbitrarily deep
/// same-op nesting collapses in a single pass.  Spans on retained nodes are
/// preserved.
pub(crate) fn flatten_connectives(node: &mut AstNode) {
    let AstNode::List { elements, .. } = node else { return };

    for child in elements.iter_mut() {
        flatten_connectives(child);
    }

    let Some(op) = list_connective(elements) else { return };

    let mut merged: Vec<AstNode> = Vec::with_capacity(elements.len());
    for (i, child) in elements.drain(..).enumerate() {
        if i == 0 {
            merged.push(child);
            continue;
        }
        // Same-op child is already flat (bottom-up); splice its args, skipping
        // the child's own head operator.
        if let AstNode::List { elements: child_els, .. } = &child {
            if list_connective(child_els).as_ref() == Some(&op) {
                merged.extend(child_els.iter().skip(1).cloned());
                continue;
            }
        }
        merged.push(child);
    }
    *elements = merged;
}

/// The connective [`OpKind`] (`And` / `Or` only) heading a list's elements, if
/// any.  `Implies` / `Iff` / quantifiers are *not* associative connectives and
/// are never flattened.
fn list_connective(elements: &[AstNode]) -> Option<OpKind> {
    match elements.first() {
        Some(AstNode::Operator { op: op @ (OpKind::And | OpKind::Or), .. }) => Some(op.clone()),
        _ => None,
    }
}

/// Negation-normal-form for the boolean `and`/`or`/`not` fragment: push every
/// `not` inward via De Morgan and cancel double negations, so negation reaches
/// the atoms.
///
///   * `(not (and A B …))` → `(or  (not A) (not B) …)`
///   * `(not (or  A B …))` → `(and (not A) (not B) …)`
///   * `(not (not X))`     → `X`
///
/// Applied recursively.  Only the boolean fragment is rewritten — a `not` over
/// `=>` / `<=>` / a quantifier / an atom is left intact.  Retained leaves keep
/// their spans.
pub(crate) fn push_negation_inward(node: &mut AstNode) {
    // Replacement is computed in an inner scope so `node`'s shared borrow is
    // released before the mutable assignment.
    loop {
        let replacement: Option<AstNode> = {
            let Some(arg) = not_argument(node) else { break };
            match top_op(arg) {
                Some(OpKind::Not) => not_argument(arg).cloned(),
                Some(op @ (OpKind::And | OpKind::Or)) => {
                    let dual = if matches!(op, OpKind::And) { OpKind::Or } else { OpKind::And };
                    match arg {
                        AstNode::List { elements, .. } => Some(operator_list(
                            dual,
                            elements[1..].iter().map(|c| wrap_not(c.clone())).collect(),
                        )),
                        _ => None,
                    }
                }
                _ => None,
            }
        };
        match replacement {
            Some(r) => *node = r,
            None    => break,
        }
    }
    if let AstNode::List { elements, .. } = node {
        for child in elements.iter_mut() {
            push_negation_inward(child);
        }
    }
}

/// If `node` is a unary `(not X)`, return `&X`; else `None`.
fn not_argument(node: &AstNode) -> Option<&AstNode> {
    let AstNode::List { elements, .. } = node else { return None };
    if elements.len() != 2 { return None; }
    match elements.first() {
        Some(AstNode::Operator { op: OpKind::Not, .. }) => elements.get(1),
        _ => None,
    }
}

/// Wrap `node` as `(not node)` (synthetic operator span).
fn wrap_not(node: AstNode) -> AstNode {
    operator_list(OpKind::Not, vec![node])
}

/// Build `(op args…)` with synthetic operator + list spans.
fn operator_list(op: OpKind, args: Vec<AstNode>) -> AstNode {
    let mut elements = Vec::with_capacity(args.len() + 1);
    elements.push(AstNode::Operator { op, span: Span::synthetic() });
    elements.extend(args);
    AstNode::List { elements, span: Span::synthetic() }
}

/// Split a top-level `(and A B …)` assertion into one independent root per
/// conjunct.
///
/// Recurses, so a conjunct that is itself an `and` is split too.  A non-`and`
/// node passes through as a one-element vec; a degenerate empty `(and)` is kept
/// rather than dropped.
pub(crate) fn split_top_level_and(node: AstNode) -> Vec<AstNode> {
    let AstNode::List { elements, .. } = &node else { return vec![node] };
    if !matches!(elements.first(), Some(AstNode::Operator { op: OpKind::And, .. })) {
        return vec![node];
    }
    let conjuncts: Vec<AstNode> =
        elements[1..].iter().cloned().flat_map(split_top_level_and).collect();
    if conjuncts.is_empty() { vec![node] } else { conjuncts }
}

/// Normalize one (already macro-expanded) root AST node into CAF, returning the
/// one-or-more normalized roots.
pub(crate) fn normalize_ast(node: &AstNode) -> Vec<AstNode> {
    match top_op(node) {
        Some(OpKind::Iff)     => expand_biconditional(node),
        Some(OpKind::Implies) => normalize_implication(node.clone()),
        _                     => vec![node.clone()],
    }
}

/// The operator heading a list node, if any.
fn top_op(node: &AstNode) -> Option<OpKind> {
    match node {
        AstNode::List { elements, .. } => match elements.first() {
            Some(AstNode::Operator { op, .. }) => Some(op.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Build `(<op> a b)`.  The operator and list wrapper are CAF-introduced, so
/// they carry synthetic spans; `a`/`b` keep their original source spans.
fn binary(op: OpKind, a: AstNode, b: AstNode) -> AstNode {
    AstNode::List {
        elements: vec![AstNode::Operator { op, span: Span::synthetic() }, a, b],
        span: Span::synthetic(),
    }
}

/// T1: `(<=> A B)` → `(=> A B)` and `(=> B A)`, each then normalized.
fn expand_biconditional(node: &AstNode) -> Vec<AstNode> {
    let AstNode::List { elements, .. } = node else { return vec![node.clone()] };
    let (Some(a), Some(b)) = (elements.get(1), elements.get(2)) else { return vec![node.clone()] };
    let mut out = normalize_implication(binary(OpKind::Implies, a.clone(), b.clone()));
    out.extend(normalize_implication(binary(OpKind::Implies, b.clone(), a.clone())));
    out
}

/// Apply T2/T3/T4 to implication `node` until stable.
fn normalize_implication(node: AstNode) -> Vec<AstNode> {
    let mut worklist = vec![node];
    let mut done = Vec::new();
    while let Some(cur) = worklist.pop() {
        match try_normalize_one(&cur) {
            Some(transformed) => worklist.extend(transformed),
            None              => done.push(cur),
        }
    }
    done
}

/// One normalization step; `Some(new_nodes)` if a transform fired, `None` if
/// already in CAF.
fn try_normalize_one(node: &AstNode) -> Option<Vec<AstNode>> {
    let AstNode::List { elements, .. } = node else { return None };
    if !matches!(elements.first(), Some(AstNode::Operator { op: OpKind::Implies, .. })) {
        return None;
    }
    let ant = elements.get(1)?;
    let con = elements.get(2)?;
    if let Some(t2) = try_t2(ant, con) { return Some(vec![t2]); }
    if let Some(t3) = try_t3(ant, con) { return Some(t3); }
    if let Some(t4) = try_t4(ant, con) { return Some(vec![t4]); }
    None
}

/// T2: `(=> A (=> B C))` → `(=> (and A B) C)`.
fn try_t2(ant: &AstNode, con: &AstNode) -> Option<AstNode> {
    let AstNode::List { elements: con_els, .. } = con else { return None };
    if !matches!(con_els.first(), Some(AstNode::Operator { op: OpKind::Implies, .. })) {
        return None;
    }
    let b = con_els.get(1)?.clone();
    let c = con_els.get(2)?.clone();
    let and = binary(OpKind::And, ant.clone(), b);
    Some(binary(OpKind::Implies, and, c))
}

/// T3: `(=> (or A B …) C)` → one implication per branch, `C` cloned into each.
fn try_t3(ant: &AstNode, con: &AstNode) -> Option<Vec<AstNode>> {
    let AstNode::List { elements: ant_els, .. } = ant else { return None };
    if !matches!(ant_els.first(), Some(AstNode::Operator { op: OpKind::Or, .. })) {
        return None;
    }
    let branches = &ant_els[1..];
    if branches.is_empty() { return None; }
    Some(branches.iter()
        .map(|branch| binary(OpKind::Implies, branch.clone(), con.clone()))
        .collect())
}

/// T4: `(=> (and A (and B C) …) D)` → `(=> (and A B C …) D)` — flatten one
/// level of nested ands in the antecedent.  `None` if already flat.
fn try_t4(ant: &AstNode, con: &AstNode) -> Option<AstNode> {
    let AstNode::List { elements: ant_els, .. } = ant else { return None };
    if !matches!(ant_els.first(), Some(AstNode::Operator { op: OpKind::And, .. })) {
        return None;
    }
    let mut flat: Vec<AstNode> = Vec::new();
    let mut had_nested = false;
    for child in &ant_els[1..] {
        if let AstNode::List { elements: child_els, .. } = child {
            if matches!(child_els.first(), Some(AstNode::Operator { op: OpKind::And, .. })) {
                had_nested = true;
                flat.extend(child_els[1..].iter().cloned());
                continue;
            }
        }
        flat.push(child.clone());
    }
    if !had_nested { return None; }
    let mut and_els: Vec<AstNode> = Vec::with_capacity(flat.len() + 1);
    and_els.push(AstNode::Operator { op: OpKind::And, span: Span::synthetic() });
    and_els.extend(flat);
    let and = AstNode::List { elements: and_els, span: Span::synthetic() };
    Some(binary(OpKind::Implies, and, con.clone()))
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse_document, Parser};

    fn parse_one(kif: &str) -> AstNode {
        let doc = parse_document("test", kif, Parser::Kif);
        assert!(doc.parse_errors.is_empty(), "parse errors: {:?}", doc.parse_errors);
        doc.ast.into_iter().next().expect("one root sentence").as_stmt().cloned().expect("a stmt item")
    }
    fn norm(kif: &str) -> Vec<AstNode> { normalize_ast(&parse_one(kif)) }

    /// Conjunct count if `node` is `(and …)`, else `None`.
    fn and_arity(node: &AstNode) -> Option<usize> {
        match node {
            AstNode::List { elements, .. }
                if matches!(elements.first(), Some(AstNode::Operator { op: OpKind::And, .. })) =>
                Some(elements.len() - 1),
            _ => None,
        }
    }
    /// The antecedent (arg 1) of an implication node.
    fn antecedent(node: &AstNode) -> &AstNode {
        let AstNode::List { elements, .. } = node else { panic!("not a list: {node:?}") };
        &elements[1]
    }

    #[test]
    fn t1_biconditional_splits_into_two_implications() {
        let out = norm("(<=> (A B C) (D E F))");
        assert_eq!(out.len(), 2, "biconditional → two implications");
        assert!(out.iter().all(|n| top_op(n) == Some(OpKind::Implies)));
    }

    #[test]
    fn plain_implication_included_unchanged() {
        let out = norm("(=> (P ?X) (Q ?X))");
        assert_eq!(out.len(), 1);
        assert_eq!(top_op(&out[0]), Some(OpKind::Implies));
        assert!(and_arity(antecedent(&out[0])).is_none(), "antecedent untouched");
    }

    #[test]
    fn t2_nested_implication_flattened() {
        let out = norm("(=> (P ?X) (=> (Q ?X) (R ?X)))");
        assert_eq!(out.len(), 1);
        assert_eq!(top_op(&out[0]), Some(OpKind::Implies));
        assert_eq!(and_arity(antecedent(&out[0])), Some(2), "antecedent should be (and A B)");
    }

    #[test]
    fn t3_disjunctive_antecedent_splits() {
        let out = norm("(=> (or (P ?X) (Q ?X)) (R ?X))");
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|n| top_op(n) == Some(OpKind::Implies)));
    }

    #[test]
    fn t3_three_branch_disjunction_produces_three_implications() {
        assert_eq!(norm("(=> (or (P ?X) (Q ?X) (S ?X)) (R ?X))").len(), 3);
    }

    #[test]
    fn t4_nested_and_in_antecedent_is_flattened() {
        let out = norm("(=> (and (P ?X) (and (Q ?X) (S ?X))) (R ?X))");
        assert_eq!(out.len(), 1);
        assert_eq!(and_arity(antecedent(&out[0])), Some(3));
    }

    #[test]
    fn t4_doubly_nested_and_is_fully_flattened() {
        let out = norm("(=> (and (P ?X) (and (Q ?X) (and (S ?X) (T ?X)))) (R ?X))");
        assert_eq!(out.len(), 1);
        assert_eq!(and_arity(antecedent(&out[0])), Some(4));
    }

    #[test]
    fn t1_biconditional_with_nested_implication_fwd_branch_flattened() {
        let out = norm("(<=> (P ?X) (=> (Q ?X) (R ?X)))");
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|n| top_op(n) == Some(OpKind::Implies)));
        assert!(out.iter().any(|n| and_arity(antecedent(n)) == Some(2)),
            "forward branch should flatten to an (and _ _) antecedent");
    }

    #[test]
    fn ground_assertion_passes_through_unchanged() {
        let out = norm("(instance Fido Dog)");
        assert_eq!(out.len(), 1);
        assert_eq!(top_op(&out[0]), None, "a ground relation has no top operator");
    }

    // -- flatten_connectives --------------------------------------------------

    /// Arity of `node` if it is a list headed by `op`, else `None`.
    fn conn_arity(node: &AstNode, op: OpKind) -> Option<usize> {
        match node {
            AstNode::List { elements, .. }
                if matches!(elements.first(), Some(AstNode::Operator { op: o, .. }) if *o == op) =>
                Some(elements.len() - 1),
            _ => None,
        }
    }
    fn flat(kif: &str) -> AstNode {
        let mut n = parse_one(kif);
        flatten_connectives(&mut n);
        n
    }

    #[test]
    fn flatten_left_nested_and() {
        let n = flat("(and (and (and (p ?x) (q ?x)) (r ?x)) (s ?x))");
        assert_eq!(conn_arity(&n, OpKind::And), Some(4));
    }

    #[test]
    fn flatten_right_nested_or() {
        let n = flat("(or (p ?x) (or (q ?x) (or (r ?x) (s ?x))))");
        assert_eq!(conn_arity(&n, OpKind::Or), Some(4));
    }

    #[test]
    fn flatten_does_not_cross_connectives() {
        let n = flat("(and (p ?x) (or (q ?x) (r ?x)))");
        assert_eq!(conn_arity(&n, OpKind::And), Some(2));
        let AstNode::List { elements, .. } = &n else { panic!("list") };
        assert_eq!(conn_arity(&elements[2], OpKind::Or), Some(2), "inner or untouched");
    }

    #[test]
    fn flatten_recurses_under_other_operators() {
        let n = flat("(=> (and (p ?x) (and (q ?x) (r ?x))) (s ?x))");
        assert_eq!(conn_arity(antecedent(&n), OpKind::And), Some(3));
    }

    #[test]
    fn flatten_leaves_flat_nodes_unchanged() {
        let n = flat("(and (p ?x) (q ?x) (r ?x))");
        assert_eq!(conn_arity(&n, OpKind::And), Some(3));
    }

    // -- push_negation_inward (negation-normal-form) --------------------------

    fn nnf(kif: &str) -> AstNode {
        let mut n = parse_one(kif);
        push_negation_inward(&mut n);
        n
    }
    /// True if `node` is `(not …)`.
    fn is_not(node: &AstNode) -> bool {
        matches!(node, AstNode::List { elements, .. }
            if matches!(elements.first(), Some(AstNode::Operator { op: OpKind::Not, .. })))
    }

    #[test]
    fn double_negation_cancels_to_positive() {
        let n = nnf("(not (not (p ?x)))");
        assert!(!is_not(&n), "double negation should be gone, got {n:?}");
        assert_eq!(top_op(&n), None, "left with the bare atom");
    }

    #[test]
    fn quadruple_negation_cancels_fully() {
        let n = nnf("(not (not (not (not (p ?x)))))");
        assert!(!is_not(&n));
    }

    #[test]
    fn triple_negation_reduces_to_single() {
        let n = nnf("(not (not (not (p ?x))))");
        assert!(is_not(&n), "odd stack leaves one `not`");
        let AstNode::List { elements, .. } = &n else { panic!("list") };
        assert!(!is_not(&elements[1]), "single not over the atom");
    }

    #[test]
    fn double_negation_cancels_when_nested() {
        // (=> A (not (not B))) → (=> A B)
        let n = nnf("(=> (p ?x) (not (not (q ?x))))");
        let AstNode::List { elements, .. } = &n else { panic!("list") };
        assert!(!is_not(&elements[2]), "consequent un-negated, got {:?}", elements[2]);
    }

    #[test]
    fn single_negation_is_untouched() {
        let n = nnf("(not (p ?x))");
        assert!(is_not(&n));
    }

    #[test]
    fn de_morgan_not_and_becomes_or_of_nots() {
        let n = nnf("(not (and (p ?x) (q ?x)))");
        assert_eq!(top_op(&n), Some(OpKind::Or), "top becomes `or`, got {n:?}");
        let AstNode::List { elements, .. } = &n else { panic!("list") };
        assert_eq!(elements.len(), 3);
        assert!(is_not(&elements[1]) && is_not(&elements[2]), "each disjunct is `(not …)`");
    }

    #[test]
    fn de_morgan_not_or_becomes_and_of_nots() {
        let n = nnf("(not (or (p ?x) (q ?x)))");
        assert_eq!(top_op(&n), Some(OpKind::And));
        let AstNode::List { elements, .. } = &n else { panic!("list") };
        assert!(is_not(&elements[1]) && is_not(&elements[2]));
    }

    #[test]
    fn de_morgan_drives_negation_to_literals() {
        let n = nnf("(not (and (or (a ?x) (b ?x)) (c ?x)))");
        assert_eq!(top_op(&n), Some(OpKind::Or));
        let AstNode::List { elements, .. } = &n else { panic!("list") };
        assert_eq!(top_op(&elements[1]), Some(OpKind::And), "inner `or` De-Morganed to `and`");
        let AstNode::List { elements: inner, .. } = &elements[1] else { panic!("list") };
        assert!(is_not(&inner[1]) && is_not(&inner[2]), "negation reached the atoms");
        assert!(is_not(&elements[2]), "(not C) literal");
    }

    #[test]
    fn de_morgan_cancels_introduced_double_negation() {
        let n = nnf("(not (and (not (a ?x)) (b ?x)))");
        assert_eq!(top_op(&n), Some(OpKind::Or));
        let AstNode::List { elements, .. } = &n else { panic!("list") };
        assert!(!is_not(&elements[1]), "first disjunct is the positive atom A, got {:?}", elements[1]);
        assert!(is_not(&elements[2]), "second disjunct is (not B)");
    }

    #[test]
    fn not_over_implication_is_left_intact() {
        let n = nnf("(not (=> (p ?x) (q ?x)))");
        assert!(is_not(&n), "(not (=> …)) is preserved");
    }

    // -- split_top_level_and --------------------------------------------------

    fn split(kif: &str) -> Vec<AstNode> { split_top_level_and(parse_one(kif)) }

    #[test]
    fn split_top_level_and_into_conjuncts() {
        let out = split("(and (p ?x) (q ?x))");
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|n| top_op(n).is_none()), "each conjunct is a bare atom");
    }

    #[test]
    fn split_recurses_through_nested_and() {
        let out = split("(and (p ?x) (and (q ?x) (r ?x)))");
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn split_keeps_operator_conjuncts_intact() {
        let out = split("(and (=> (p ?x) (q ?x)) (instance Dog Animal))");
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|n| top_op(n) == Some(OpKind::Implies)));
        assert!(out.iter().any(|n| top_op(n).is_none()));
    }

    #[test]
    fn split_passes_through_non_and() {
        assert_eq!(split("(instance Fido Dog)").len(), 1);
        assert_eq!(split("(=> (p ?x) (q ?x))").len(), 1);
    }
}
