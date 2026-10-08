//! W034 attitude-conflict: an agent holds both a formula and its exact
//! negation under the same propositional attitude.
//!
//! A lint, not a contradiction: `(believes a P)` and `(believes a (not P))`
//! leave the KB satisfiable (agents may be inconsistent). Only exact `P` /
//! `(not P)` pairs in the store's normal form are flagged; conflicts under
//! logical consequence are `KnowledgeBase::doxastic_consistent`'s job.

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::FormulaValidator;
use crate::{Element, SentenceId};

#[derive(Debug, Clone, Error)]
#[error("'{agent}' holds both a formula and its negation under '{attitude}'")]
pub struct AttitudeConflict {
    /// The attitude fact raising the finding and the conflicting one.
    pub sid: Vec<SentenceId>,
    pub agent: String,
    pub attitude: String,
}
semantic_error!(
    AttitudeConflict,
    "W034",
    "attitude-conflict",
    Warning,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (self.sid.clone(), 2)
    },
);

pub(crate) struct AttitudeConflictCheck;

impl FormulaValidator for AttitudeConflictCheck {
    type Error = AttitudeConflict;

    fn check(&self, cx: &Cx<'_>, root: SentenceId) -> Vec<AttitudeConflict> {
        let Some(s) = cx.sentence(root) else {
            return Vec::new();
        };
        let [Element::Symbol(rel), Element::Symbol(agent), Element::Sub(content)] =
            s.elements.as_slice()
        else {
            return Vec::new();
        };
        if !cx.is_attitude(rel.id()) {
            return Vec::new();
        }
        let Some(negation) = cx.negation_id(*content).filter(|n| n != content) else {
            return Vec::new();
        };
        cx.attitude_facts(rel.id(), agent.id())
            .into_iter()
            .find(|&(held, _)| held == negation)
            .map(|(_, other)| AttitudeConflict {
                sid: vec![root, other],
                agent: cx.sym_name(agent.id()),
                attitude: cx.sym_name(rel.id()),
            })
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::{codes_in, kif_layer, roots};
    use crate::semantics::SemanticLayer;

    const ATTITUDES: &str = "
        (instance believes PropositionalAttitude)
        (instance knows PropositionalAttitude)
    ";

    fn flagged(layer: &SemanticLayer) -> usize {
        roots(layer)
            .into_iter()
            .filter(|&sid| codes_in(layer, sid).contains(&"W034"))
            .count()
    }

    #[test]
    fn w034_flags_both_sides_of_a_believed_p_and_not_p() {
        let layer = kif_layer(&format!(
            "{ATTITUDES}
            (believes John (bald Socrates))
            (believes John (not (bald Socrates)))"
        ));
        assert_eq!(flagged(&layer), 2);
    }

    #[test]
    fn w034_sees_through_ingest_normal_form() {
        // Ingest cancels the double negation and pushes `(not (and ..))` to
        // its De Morgan dual; the probe computes the complement the same way.
        let layer = kif_layer(&format!(
            "{ATTITUDES}
            (believes John (not (not (wise Plato))))
            (believes John (not (wise Plato)))
            (believes Ann (and (shape Earth Flat) (orbits Earth Sun)))
            (believes Ann (not (and (shape Earth Flat) (orbits Earth Sun))))
            (believes Ann (not (and (shape Earth Round) (orbits Earth Sun))))"
        ));
        assert_eq!(flagged(&layer), 4, "one pair each for John and Ann");
    }

    #[test]
    fn w034_ignores_other_agents_attitudes_and_rules() {
        let layer = kif_layer(&format!(
            "{ATTITUDES}
            (believes John (bald Socrates))
            (believes Mary (not (bald Socrates)))
            (knows Ann (wise Plato))
            (believes Ann (not (wise Plato)))
            (=> (believes Bob (calm Sea)) (relaxed Bob))
            (believes Bob (not (calm Sea)))"
        ));
        assert_eq!(flagged(&layer), 0);
    }

    #[test]
    fn w034_counts_a_declared_subrelation_as_the_attitude() {
        let layer = kif_layer(&format!(
            "{ATTITUDES}
            (subrelation knows believes)
            (knows Ann (wise Plato))
            (believes Ann (not (wise Plato)))"
        ));
        // The `believes` fact sees the `knows` fact through the subrelation;
        // the converse direction does not hold, so only one side flags.
        assert_eq!(flagged(&layer), 1);
    }

    #[test]
    fn w034_requires_a_declared_attitude() {
        let layer = kif_layer(
            "(believes John (bald Socrates))
             (believes John (not (bald Socrates)))",
        );
        assert_eq!(flagged(&layer), 0, "`believes` is not declared an attitude");
    }
}
