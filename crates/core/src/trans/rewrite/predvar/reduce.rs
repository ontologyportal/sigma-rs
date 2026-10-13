//! Definitional term reduction for instantiated schemas.
//!
//! SUMO states the signature axioms through two meta-level terms that become
//! redundant once a binding has made their arguments concrete:
//!
//!   (ListOrderFn (ListFn a1 .. an) k)  ->  ak          k a numeral, 1 <= k <= n
//!   (AssignmentFn f a1 .. an)          ->  (f a1 .. an) f a function symbol
//!
//! Both rewrites are definitional (the value of `ListOrderFn` on a literal list
//! and the value of applying `f`), so they preserve meaning; without them the
//! instantiated domain / range rules conclude types for list elements that no
//! bridge axiom connects back to the arguments.

use crate::semantics::consts::{ASSIGNMENT_FN, LIST_FN, LIST_ORDER_FN};
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, ElementVec, Literal, SentenceId, SymbolId};

/// The three symbols the reducer rewrites, resolved from `semantics::consts`
/// so a renamed dialect overrides them like every other role.
#[derive(Debug, Clone, Copy)]
pub(super) struct ReduceSymbols {
    pub list_fn: SymbolId,
    pub list_order_fn: SymbolId,
    pub assignment_fn: SymbolId,
}

impl ReduceSymbols {
    pub(super) fn from_consts() -> Self {
        Self {
            list_fn: LIST_FN.id(),
            list_order_fn: LIST_ORDER_FN.id(),
            assignment_fn: ASSIGNMENT_FN.id(),
        }
    }
}

/// Reduce every reducible term under the root `sid`, returning the reduced
/// root.  Returns `sid` itself when nothing under it changed, so an
/// unaffected instantiation costs no new sentences.
pub(super) fn reduce_root(
    syntactic: &SyntacticLayer,
    sid: SentenceId,
    origin: SentenceId,
    syms: &ReduceSymbols,
) -> SentenceId {
    match reduce_node(syntactic, sid, origin, syms) {
        Element::Sub(reduced) => reduced,
        // A root is a formula, never a bare term, so a reduction at the root
        // cannot happen; keep the original rather than invent a sentence.
        _ => sid,
    }
}

/// Reduce the sentence `sid` and return the element that replaces it in its
/// parent: `Sub(sid)` when unchanged, `Sub(new)` when a child changed, or the
/// bare replacement element when `sid` itself was a reducible term.
fn reduce_node(
    syntactic: &SyntacticLayer,
    sid: SentenceId,
    origin: SentenceId,
    syms: &ReduceSymbols,
) -> Element {
    let elements: Vec<Element> = syntactic
        .sentence(sid)
        .map(|sentence| sentence.elements.to_vec())
        .unwrap_or_default();
    let mut changed = false;
    let mut reduced: ElementVec = ElementVec::with_capacity(elements.len());
    for element in elements {
        reduced.push(match element {
            Element::Sub(child) => {
                let out = reduce_node(syntactic, child, origin, syms);
                if !matches!(out, Element::Sub(id) if id == child) {
                    changed = true;
                }
                out
            }
            other => other,
        });
    }

    if let Some(argument) = list_order_argument(syntactic, &reduced, syms) {
        return argument;
    }
    if let Some(application) = assignment_application(&reduced, syms) {
        return Element::Sub(syntactic.push_synthetic_sentence(application, origin));
    }
    if changed {
        Element::Sub(syntactic.push_synthetic_sentence(reduced, origin))
    } else {
        Element::Sub(sid)
    }
}

/// `(ListOrderFn (ListFn a1 .. an) k)` -> `ak`, when `k` is a numeral in
/// `1..=n`.  Anything else (unresolved index, out of range, not a literal
/// list) is left for the caller to keep or reject.
fn list_order_argument(
    syntactic: &SyntacticLayer,
    elements: &[Element],
    syms: &ReduceSymbols,
) -> Option<Element> {
    let [Element::Symbol(head), Element::Sub(list_sid), Element::Literal(Literal::Number(position))] =
        elements
    else {
        return None;
    };
    if head.id() != syms.list_order_fn {
        return None;
    }
    let position: usize = position.parse().ok().filter(|&p| p > 0)?;
    let list = syntactic.sentence(*list_sid)?;
    if !matches!(list.elements.first(), Some(Element::Symbol(sym)) if sym.id() == syms.list_fn) {
        return None;
    }
    list.elements.get(position).cloned()
}

/// `(AssignmentFn f a1 .. an)` -> `(f a1 .. an)`, when `f` is a concrete
/// symbol.
fn assignment_application(elements: &[Element], syms: &ReduceSymbols) -> Option<ElementVec> {
    let [Element::Symbol(head), Element::Symbol(_), ..] = elements else {
        return None;
    };
    if head.id() != syms.assignment_fn {
        return None;
    }
    Some(elements[1..].iter().cloned().collect())
}

/// `true` iff some sentence under `sid` (inclusive) is headed by `head`.
pub(super) fn contains_head(syntactic: &SyntacticLayer, sid: SentenceId, head: SymbolId) -> bool {
    let Some(sentence) = syntactic.sentence(sid) else {
        return false;
    };
    if matches!(sentence.elements.first(), Some(Element::Symbol(sym)) if sym.id() == head) {
        return true;
    }
    sentence.elements.iter().any(|e| match e {
        Element::Sub(child) => contains_head(syntactic, *child, head),
        _ => false,
    })
}
