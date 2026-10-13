use crate::parse::ast::OpKind;
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, ElementVec, SentenceId, SymbolId};

/// Recursively substitute all occurrences of `var_id` in the sentence tree
/// rooted at `sid`, returning a fresh synthetic sentence id.
pub(super) fn substitute_var(
    syntactic: &SyntacticLayer,
    sid: SentenceId,
    var_id: SymbolId,
    replacement: &Element,
    origin: SentenceId,
) -> SentenceId {
    let elements: Vec<Element> = syntactic
        .sentence(sid)
        .map(|s| s.elements.to_vec())
        .unwrap_or_default();

    let mut new_elems: ElementVec = ElementVec::with_capacity(elements.len());
    for elem in elements {
        let new_elem = match elem {
            Element::Variable { id, .. } if id == var_id => replacement.clone(),
            Element::Sub(child_sid) => {
                let new_child = substitute_var(syntactic, child_sid, var_id, replacement, origin);
                Element::Sub(new_child)
            }
            other => other,
        };
        new_elems.push(new_elem);
    }
    syntactic.push_synthetic_sentence(new_elems, origin)
}

/// Collect the direct conjunct sentence ids of `ant_sid`, e.g.
/// decompose `(and (A) (B) (C))` into
/// `Vec<[SentenceId(A)>, SentenceId(B), SentenceId(C)]>`. If there is no
/// direct conjunction, return a vector containing `SentenceId(ant_sid)`
pub(super) fn collect_conjuncts(
    syntactic: &SyntacticLayer,
    ant_sid: SentenceId,
) -> Vec<SentenceId> {
    let Some(ant_s) = syntactic.sentence(ant_sid) else {
        return vec![ant_sid];
    };
    if !matches!(ant_s.elements.first(), Some(Element::Op(OpKind::And))) {
        return vec![ant_sid];
    }
    ant_s.elements[1..]
        .iter()
        .filter_map(|e| match e {
            Element::Sub(sid) => Some(*sid),
            _ => None,
        })
        .collect()
}

/// Helper function which decomposes an implication into its antecedent and
/// consequent, e.g. `(=> Sub(ant) Sub(con))`. Returns `None` if the sentence
/// is NOT an implication
pub(super) fn decompose_implication(
    syntactic: &SyntacticLayer,
    sid: SentenceId,
) -> Option<(SentenceId, SentenceId)> {
    let s = syntactic.sentence(sid)?;
    if !matches!(s.elements.first(), Some(Element::Op(OpKind::Implies))) {
        return None;
    }
    match (s.elements.get(1), s.elements.get(2)) {
        (Some(Element::Sub(a)), Some(Element::Sub(c))) => Some((*a, *c)),
        _ => None,
    }
}
