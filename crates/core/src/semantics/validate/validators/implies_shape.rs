//! E022 existential-in-antecedent: an `exists` under an implication antecedent
//! traps its witness, which the consequent then cannot reference.

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::OperatorValidator;
use crate::{Element, OpKind, SentenceId};

use super::common::subtree_has_existential;

#[derive(Debug, Clone, Error)]
#[error("existential quantifier in implication antecedent or biconditional: any witness will not be available in the consequent")]
pub struct ExistentialInAntecedent {
    pub sid: SentenceId,
}
semantic_error!(
    ExistentialInAntecedent,
    "E022",
    "existential-in-antecedent",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], 0)
    },
);

pub(crate) struct ImpliesShape;

impl OperatorValidator for ImpliesShape {
    type Error = ExistentialInAntecedent;
    const OPS: &'static [OpKind] = &[OpKind::Implies];

    fn check(&self, cx: &Cx<'_>, sid: SentenceId, _op: &OpKind) -> Vec<ExistentialInAntecedent> {
        let Some(s) = cx.sentence(sid) else {
            return Vec::new();
        };
        // elements: [Op{Implies}, Sub{antecedent}, Sub{consequent}]
        let Some(Element::Sub(ant)) = s.elements.get(1) else {
            return Vec::new();
        };
        if subtree_has_existential(cx, *ant) {
            vec![ExistentialInAntecedent { sid: *ant }]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::types::Scope;
    use crate::semantics::validate::test_support::{codes_in, kif_layer, root_by_op};

    #[test]
    fn e022_existential_in_antecedent() {
        let layer = kif_layer(
            r#"
            (instance Human Class)
            (=> (exists (?X) (instance ?X Human)) (instance ?X Human))
        "#,
        );
        let sid = root_by_op(&layer, crate::OpKind::Implies);
        let errors = layer
            .validator_scoped(Scope::Base)
            .validate_sentence_collect(sid);
        let error = errors.iter().find(|e| e.code() == "E022").unwrap();
        assert_eq!(error.severity(), crate::Severity::Error);
    }

    #[test]
    fn e022_not_flagged_for_existential_in_consequent() {
        // A witness introduced in the consequent is perfectly usable there.
        let layer = kif_layer(
            r#"
            (instance Human Class)
            (=> (instance ?X Human) (exists (?Y) (instance ?Y Human)))
        "#,
        );
        let sid = root_by_op(&layer, crate::OpKind::Implies);
        assert!(!codes_in(&layer, sid).contains(&"E022"));
    }
}
