//! W030 existential-in-biimplication: an `exists` under either half of an
//! `<=>` traps its witness, which the other half then cannot reference.
//!
//! A *top-level* `<=>` is rewritten into two implications at ingest, so the
//! `Iff` shape this validator claims only ever reaches it as a nested
//! sub-sentence. The tests nest accordingly.

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::OperatorValidator;
use crate::{Element, OpKind, SentenceId};

use super::common::subtree_has_existential;

#[derive(Debug, Clone, Error)]
#[error("existential quantifier in biimplication: any witness will not be available to the other sub-statement")]
pub struct ExistentialInIff {
    pub sid: SentenceId,
}
semantic_error!(
    ExistentialInIff,
    "W030",
    "existential-in-biimplication",
    Warning,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], 0)
    },
);

pub(crate) struct IffShape;

impl OperatorValidator for IffShape {
    type Error = ExistentialInIff;
    const OPS: &'static [OpKind] = &[OpKind::Iff];

    fn check(&self, cx: &Cx<'_>, sid: SentenceId, _op: &OpKind) -> Vec<ExistentialInIff> {
        let Some(s) = cx.sentence(sid) else {
            return Vec::new();
        };
        // elements: [Op{Iff}, Sub{lhs}, Sub{rhs}]
        [s.elements.get(1), s.elements.get(2)]
            .into_iter()
            .flatten()
            .filter_map(|el| match el {
                Element::Sub(half) => Some(*half),
                _ => None,
            })
            .filter(|half| subtree_has_existential(cx, *half))
            .map(|half| ExistentialInIff { sid: half })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::{codes_in, kif_layer, sub_by_op};

    #[test]
    fn w030_flags_existential_on_the_left() {
        let layer = kif_layer(
            r#"
            (instance Human Class)
            (=> (instance Adam Human)
                (<=> (exists (?X) (instance ?X Human)) (instance Adam Human)))
        "#,
        );
        let sid = sub_by_op(&layer, crate::OpKind::Iff);
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"W030"),
            "expected W030 for an exists in the left half; got {codes:?}"
        );
    }

    #[test]
    fn w030_flags_existential_on_the_right() {
        let layer = kif_layer(
            r#"
            (instance Human Class)
            (=> (instance Adam Human)
                (<=> (instance Adam Human) (exists (?X) (instance ?X Human))))
        "#,
        );
        let sid = sub_by_op(&layer, crate::OpKind::Iff);
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"W030"),
            "expected W030 for an exists in the right half; got {codes:?}"
        );
    }

    #[test]
    fn w030_not_flagged_without_existential() {
        let layer = kif_layer(
            r#"
            (instance Human Class)
            (=> (instance Adam Human)
                (<=> (instance Adam Human) (instance Eve Human)))
        "#,
        );
        let sid = sub_by_op(&layer, crate::OpKind::Iff);
        assert!(!codes_in(&layer, sid).contains(&"W030"));
    }
}
