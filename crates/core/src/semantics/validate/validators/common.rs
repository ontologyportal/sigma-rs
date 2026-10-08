//! Formula-tree walks shared by more than one validator.
//!
//! Row variables (`is_row = true`, e.g. `@ARGS`) are excluded from every walk
//! here: they are macro placeholders, not first-order variables.

use std::collections::HashSet;

use crate::semantics::validate::cx::Cx;
use crate::{Element, OpKind, SentenceId, SymbolId, TaxRelation};

/// Call `f` for every non-row variable occurrence reachable from `sid`,
/// including those inside quantifier var-lists. Callers needing a
/// binder-vs-use distinction filter from the resulting counts.
pub(super) fn walk_vars(cx: &Cx<'_>, sid: SentenceId, f: &mut dyn FnMut(&str)) {
    let Some(s) = cx.sentence(sid) else {
        return;
    };
    for el in &s.elements {
        match el {
            Element::Sub(sub) => walk_vars(cx, *sub, f),
            Element::Variable {
                name,
                is_row: false,
                ..
            } => f(name),
            _ => {}
        }
    }
}

/// Walk the formula tree once, partitioning variable occurrences:
///   * `bound` -- variables that bind their scope: every antecedent's
///     variables (both halves of an `<=>`, since each binds the other), plus
///     every `forall` / `exists` var-list.
///   * `consequent` -- variables occurring in an implication consequent.
///
/// A variable in both sets (bound by a *nested* antecedent that sits inside an
/// outer consequent) is therefore not free.
pub(super) fn collect_binding_structure(
    cx: &Cx<'_>,
    sid: SentenceId,
    bound: &mut HashSet<String>,
    consequent: &mut HashSet<String>,
) {
    let Some(s) = cx.sentence(sid) else {
        return;
    };
    match s.op() {
        Some(OpKind::Implies) => {
            if let (Some(Element::Sub(a)), Some(Element::Sub(c))) =
                (s.elements.get(1), s.elements.get(2))
            {
                walk_vars(cx, *a, &mut |n: &str| {
                    bound.insert(n.to_string());
                });
                walk_vars(cx, *c, &mut |n: &str| {
                    consequent.insert(n.to_string());
                });
            }
        }
        Some(OpKind::Iff) => {
            if let (Some(Element::Sub(a)), Some(Element::Sub(c))) =
                (s.elements.get(1), s.elements.get(2))
            {
                walk_vars(cx, *a, &mut |n: &str| {
                    bound.insert(n.to_string());
                });
                walk_vars(cx, *c, &mut |n: &str| {
                    bound.insert(n.to_string());
                });
            }
        }
        Some(OpKind::ForAll | OpKind::Exists) => {
            if let Some(Element::Sub(varlist_sid)) = s.elements.get(1) {
                if let Some(vl) = cx.sentence(*varlist_sid) {
                    for el in &vl.elements {
                        if let Element::Variable {
                            name,
                            is_row: false,
                            ..
                        } = el
                        {
                            bound.insert(name.clone());
                        }
                    }
                }
            }
        }
        _ => {}
    }
    for el in &s.elements {
        if let Element::Sub(child) = el {
            collect_binding_structure(cx, *child, bound, consequent);
        }
    }
}

/// Does the subtree rooted at `sid` contain an `exists` anywhere?
pub(super) fn subtree_has_existential(cx: &Cx<'_>, sid: SentenceId) -> bool {
    let Some(s) = cx.sentence(sid) else {
        return false;
    };
    if matches!(s.op(), Some(OpKind::Exists)) {
        return true;
    }
    s.elements.iter().any(|el| match el {
        Element::Sub(sub) => subtree_has_existential(cx, *sub),
        _ => false,
    })
}

/// Whether `sid` denotes a truth-valued sentence rather than a term.
pub(super) fn is_logical_sentence(cx: &Cx<'_>, sid: SentenceId) -> bool {
    let Some(sentence) = cx.sentence(sid) else {
        return false;
    };
    if sentence.is_operator() {
        return true;
    }
    let head_id = match sentence.elements.first() {
        Some(Element::Symbol(sym)) => sym.id(),
        Some(Element::Variable { .. }) => return true,
        _ => return false,
    };
    !cx.is_function(head_id)
}

/// `class` and every class above it along `subclass` edges, in discovery
/// order.
pub(super) fn superclasses(cx: &Cx<'_>, class: SymbolId) -> Vec<SymbolId> {
    let mut seen = HashSet::from([class]);
    let mut order = vec![class];
    let mut i = 0;
    while let Some(&c) = order.get(i) {
        i += 1;
        for (parent, rel) in cx.parents(c) {
            if matches!(rel, TaxRelation::Subclass) && seen.insert(parent) {
                order.push(parent);
            }
        }
    }
    order
}
