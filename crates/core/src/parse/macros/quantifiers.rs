//! Collapse directly nested quantifiers.

use crate::OpKind;

use super::super::AstNode;

/// Collapse nested quantifiers in place so that adjacent quantifiers of the
/// same kind share a single variable list.
///
/// For example, `(forall (?A) (forall (?B) ...))` → `(forall (?A ?B) ...)`.
pub fn collapse_quantifiers(node: &mut AstNode) {
    let AstNode::List { elements, .. } = node else {
        return;
    };

    let is_quantifier = matches!(
        elements.first(),
        Some(AstNode::Operator { op, .. }) if op.is_quantifier()
    );

    if !is_quantifier {
        for child in elements.iter_mut() {
            collapse_quantifiers(child);
        }
        return;
    }

    let Some(body) = elements.get_mut(2) else {
        return;
    };

    collapse_quantifiers(body);

    let should_collapse = {
        let outer_disc = match &elements[0] {
            AstNode::Operator { op, .. } => std::mem::discriminant(op),
            _ => return,
        };
        match &elements[2] {
            AstNode::List { elements: inner_els, .. } if inner_els.len() == 3 => {
                match &inner_els[0] {
                    AstNode::Operator { op: inner_op, .. } => {
                        std::mem::discriminant(inner_op) == outer_disc
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    };

    if !should_collapse {
        return;
    }

    let Some(inner_list) = elements.pop() else { unreachable!("shape already verified above") };

    let AstNode::List { elements: mut inner_els, .. } = inner_list else {
        unreachable!("shape already verified above")
    };

    // inner_els: [inner_op, inner_var_list, inner_body]
    let Some(inner_body) = inner_els.pop() else { unreachable!("shape already verified above") };
    let Some(inner_var_list) = inner_els.pop() else { unreachable!("shape already verified above") };

    if let (
        AstNode::List { elements: outer_vars, .. },
        AstNode::List { elements: inner_vars, .. },
    ) = (&mut elements[1], inner_var_list)
    {
        outer_vars.extend(inner_vars);
    }

    // Restore the [op, vars, body] shape.
    elements.push(inner_body);
}

/// Strip leading top-level `ForAll` quantifiers from a node in place.
///
/// Free variables are implicitly universally quantified, so an outermost
/// `(forall ...)` wrapper is redundant. Any number of stacked leading blocks
/// are removed: `(forall (?X0) (forall (?X1) body))` → `body`.
pub fn strip_top_level_forall(node: &mut AstNode) {
    loop {
        match node {
            AstNode::List { elements, .. }
                if elements.len() == 3
                    && matches!(
                        elements[0],
                        AstNode::Operator { op: OpKind::ForAll, .. }
                    ) =>
            {
                let body = elements.remove(2);
                *node = body;
            }
            _ => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{AstNode, Span};

    fn dummy_span() -> Span {
        Span::default()
    }

    fn dummy_symbol(name: &str) -> AstNode {
        AstNode::Symbol { name: name.to_owned(), span: dummy_span() }
    }

    #[test]
    fn single_forall_stripped() {
        let body = dummy_symbol("p");
        let var_list = AstNode::List { elements: vec![], span: dummy_span() };
        let mut node = AstNode::List {
            elements: vec![
                AstNode::Operator { op: OpKind::ForAll, span: dummy_span() },
                var_list,
                body,
            ],
            span: dummy_span(),
        };
        strip_top_level_forall(&mut node);
        assert!(matches!(node, AstNode::Symbol { name, .. } if name == "p"));
    }

    #[test]
    fn stacked_foralls_all_stripped() {
        let body = dummy_symbol("p");
        let inner = AstNode::List {
            elements: vec![
                AstNode::Operator { op: OpKind::ForAll, span: dummy_span() },
                AstNode::List { elements: vec![], span: dummy_span() },
                body,
            ],
            span: dummy_span(),
        };
        let mut node = AstNode::List {
            elements: vec![
                AstNode::Operator { op: OpKind::ForAll, span: dummy_span() },
                AstNode::List { elements: vec![], span: dummy_span() },
                inner,
            ],
            span: dummy_span(),
        };
        strip_top_level_forall(&mut node);
        assert!(matches!(node, AstNode::Symbol { name, .. } if name == "p"));
    }

    #[test]
    fn exists_at_top_not_stripped() {
        let body = dummy_symbol("p");
        let var_list = AstNode::List { elements: vec![], span: dummy_span() };
        let mut node = AstNode::List {
            elements: vec![
                AstNode::Operator { op: OpKind::Exists, span: dummy_span() },
                var_list,
                body,
            ],
            span: dummy_span(),
        };
        strip_top_level_forall(&mut node);
        assert!(matches!(node, AstNode::List { ref elements, .. }
            if matches!(&elements[0], AstNode::Operator { op: OpKind::Exists, .. })));
    }

    #[test]
    fn forall_under_connective_not_stripped() {
        // (=> (forall ...) p) — the forall is not at the top level.
        let inner_forall = AstNode::List {
            elements: vec![
                AstNode::Operator { op: OpKind::ForAll, span: dummy_span() },
                AstNode::List { elements: vec![], span: dummy_span() },
                dummy_symbol("q"),
            ],
            span: dummy_span(),
        };
        let mut node = AstNode::List {
            elements: vec![
                AstNode::Operator { op: OpKind::Implies, span: dummy_span() },
                inner_forall,
                dummy_symbol("p"),
            ],
            span: dummy_span(),
        };
        strip_top_level_forall(&mut node);
        // Top-level node must still be Implies.
        assert!(matches!(node, AstNode::List { ref elements, .. }
            if matches!(&elements[0], AstNode::Operator { op: OpKind::Implies, .. })));
    }
}