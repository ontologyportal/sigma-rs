//! E002 head-not-relation: a sentence's head symbol must be a declared relation.

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::SentenceValidator;
use crate::{Element, SentenceId};

#[derive(Debug, Clone, Error)]
#[error("sentence head '{sym}' is not a declared relation")]
pub struct HeadNotRelation {
    pub sid: SentenceId,
    pub sym: String,
}
semantic_error!(
    HeadNotRelation,
    "E002",
    "head-not-relation",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], 0)
    },
);

pub(crate) struct HeadIsRelation;

impl SentenceValidator for HeadIsRelation {
    type Error = HeadNotRelation;

    fn check(&self, cx: &Cx<'_>, sid: SentenceId) -> Vec<HeadNotRelation> {
        // Only a concrete symbol head carries a relation declaration. A
        // predicate-variable head `(?REL ...)` is higher-order: keying
        // `is_relation` on its scoped id would always spuriously fail.
        let Some(sentence) = cx.sentence(sid) else {
            return Vec::new();
        };
        let Some(Element::Symbol(head)) = sentence.elements.first() else {
            return Vec::new();
        };
        if cx.is_relation(head.id()) {
            return Vec::new();
        }
        vec![HeadNotRelation {
            sid,
            sym: cx.sym_name(head.id()),
        }]
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::types::Scope;
    use crate::semantics::validate::test_support::{codes_in, kif_layer};

    #[test]
    fn e002_flags_undeclared_head() {
        let layer = kif_layer(
            r#"
            (subclass Foo Entity)
            ;; `Foo` is NOT declared as a relation.
            (Foo Bar Baz)
        "#,
        );
        let sid = *layer.syntactic.by_head("Foo").first().unwrap();
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"E002"),
            "expected E002 head-not-relation; got {codes:?}"
        );
    }

    #[test]
    fn e002_severity_is_error() {
        let layer = kif_layer("(subclass Foo Entity)\n(Foo Bar Baz)");
        let sid = *layer.syntactic.by_head("Foo").first().unwrap();
        let errs = layer
            .validator_scoped(Scope::Base)
            .validate_sentence_collect(sid);
        let e = errs.iter().find(|e| e.code() == "E002").unwrap();
        assert_eq!(
            e.severity(),
            crate::Severity::Error,
            "head-not-relation is structural -- Error severity, matching its E-prefix code"
        );
    }

    #[test]
    fn e002_not_flagged_for_declared_relation() {
        let layer = crate::semantics::validate::test_support::base_layer();
        let sid = *layer.syntactic.by_head("subclass").first().unwrap();
        assert!(!codes_in(&layer, sid).contains(&"E002"));
    }
}
