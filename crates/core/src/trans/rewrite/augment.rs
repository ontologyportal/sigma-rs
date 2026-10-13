// crates/core/src/trans/rewrite/augment.rs

use std::collections::HashSet;

use smallvec::smallvec;

use super::extract::RewriteRule;
use super::subst::{collect_conjuncts, substitute_var};
use crate::parse::ast::OpKind;
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, ElementVec, SentenceId};

// ---------------------------------------------------------------------------
// Stage 3 — Augmentation Fixed-Point
// ---------------------------------------------------------------------------

/// Apply `rules` to all implications in `seed` (typically `normal_implications`)
/// until no more augmentations are possible (fixed-point).
///
/// Suppresses augmented-away originals in `suppressed`.
/// New augmented sentences are pushed into the synthetic store of `syntactic`.
pub(crate) fn augment_fixed_point(
    syntactic: &SyntacticLayer,
    rules: &[RewriteRule],
    seed: &[SentenceId],
    suppressed: &mut HashSet<SentenceId>,
) {
    let mut dirty: Vec<SentenceId> = seed.to_vec();

    // Termination key: (matched_conjunct_sid, rule_id).  Keying by the matched
    // conjunct's sid bounds total firings to O(#conjuncts × #rules): each
    // (conjunct, rule) pair fires at most once, regardless of how many
    // descendant sentences carry the same conjunct sid forward.
    let mut applied: HashSet<(SentenceId, usize)> = HashSet::new();

    while let Some(sid) = dirty.pop() {
        if suppressed.contains(&sid) {
            continue;
        }
        for rule in rules {
            if let Some((new_sid, matched_csid)) =
                try_augment_conjunct(syntactic, sid, rule, &applied)
            {
                applied.insert((matched_csid, rule.id));
                suppressed.insert(sid);
                dirty.push(new_sid);
            }
        }
    }
}

/// Try to augment one implication `sid` with `rule`.
///
/// Finds a conjunct in the antecedent that matches `rule.pattern` and whose
/// `(conjunct, rule)` pair has not already fired (checked against `applied`),
/// substitutes `rule.template_var` → captured variable in `rule.consequent_sid`
/// to produce new conjuncts, and builds a new implication.
///
/// Returns `Some((new_root_sid, matched_csid))` if augmented, `None` if no
/// match. The caller records the `(matched_csid, rule.id)` pair after the fire.
fn try_augment_conjunct(
    syntactic: &SyntacticLayer,
    sid: SentenceId,
    rule: &RewriteRule,
    applied: &HashSet<(SentenceId, usize)>,
) -> Option<(SentenceId, SentenceId)> {
    // Sentence must be (=> Sub(ant) Sub(con)).
    let (ant_sid, con_sid) = {
        let s = syntactic.sentence(sid)?;
        if !matches!(s.elements.first(), Some(Element::Op(OpKind::Implies))) {
            return None;
        }
        match (s.elements.get(1), s.elements.get(2)) {
            (Some(Element::Sub(a)), Some(Element::Sub(c))) => (*a, *c),
            _ => return None,
        }
    };

    let conjunct_sids: Vec<SentenceId> = collect_conjuncts(syntactic, ant_sid);

    let (matched_csid, bindings) = conjunct_sids.iter().find_map(|&csid| {
        if applied.contains(&(csid, rule.id)) {
            return None;
        }
        let conjunct_s = syntactic.sentence(csid)?;
        let b = syntactic
            .patterns()
            .match_pattern(&rule.pattern, &conjunct_s)?;
        Some((csid, b))
    })?;

    // The captured element at slot 0 is the variable to substitute for
    // `rule.template_var` throughout the consequent.
    let replacement = bindings.elements.get(&0)?.clone();

    let subst_sid = substitute_var(
        syntactic,
        rule.consequent_sid,
        rule.template_var,
        &replacement,
        sid,
    );

    // If the substituted consequent is (and …), add each child individually so
    // we don't nest (and …) inside the new antecedent.
    let new_conjunct_sids: Vec<SentenceId> = collect_conjuncts(syntactic, subst_sid);

    let mut and_elems: ElementVec =
        ElementVec::with_capacity(conjunct_sids.len() + new_conjunct_sids.len() + 1);
    and_elems.push(Element::Op(OpKind::And));
    for &csid in &conjunct_sids {
        and_elems.push(Element::Sub(csid));
    }
    for &csid in &new_conjunct_sids {
        and_elems.push(Element::Sub(csid));
    }
    let new_ant_sid = syntactic.push_synthetic_sentence(and_elems, sid);

    let impl_elems: ElementVec = smallvec![
        Element::Op(OpKind::Implies),
        Element::Sub(new_ant_sid),
        Element::Sub(con_sid),
    ];
    let new_root = syntactic.push_synthetic_sentence(impl_elems, sid);
    Some((new_root, matched_csid))
}
