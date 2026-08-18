//! E005 arity-mismatch, in its two forms: a relation-headed sentence whose
//! argument count contradicts the head's declared arity, and an operator
//! sentence whose argument count contradicts the operator's fixed arity.

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::{OperatorValidator, SentenceValidator};
use crate::{Element, OpKind, SentenceId};

/// Arity mismatch. Raised by the `arity` validators and by the `domain` /
/// `range` / `tax_edges` caches when a declaration axiom is itself malformed.
#[derive(Debug, Clone, Error)]
#[error("arity mismatch for '{rel}': expected {expected}, got {got}")]
pub struct ArityMismatch {
    pub sid: SentenceId,
    pub rel: String,
    pub expected: usize,
    pub got: usize,
}
semantic_error!(
    ArityMismatch,
    "E005",
    "arity-mismatch",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], -1)
    },
);

pub(crate) struct RelationArity;

impl SentenceValidator for RelationArity {
    type Error = ArityMismatch;

    fn check(&self, cx: &Cx<'_>, sid: SentenceId) -> Vec<ArityMismatch> {
        let Some(sentence) = cx.sentence(sid) else {
            return Vec::new();
        };
        let Some(Element::Symbol(head)) = sentence.elements.first() else {
            return Vec::new();
        };
        let arg_count = sentence.elements.len().saturating_sub(1);
        let Some(ar) = cx.arity(head.id()) else {
            return Vec::new();
        };
        // A declared arity of 0 means "variable arity" (row variables), not
        // "takes no arguments", so it never constrains.
        if ar <= 0 || ar as usize == arg_count {
            return Vec::new();
        }
        vec![ArityMismatch {
            sid,
            rel: cx.sym_name(head.id()),
            expected: ar as usize,
            got: arg_count,
        }]
    }
}

pub(crate) struct OperatorArity;

impl OperatorValidator for OperatorArity {
    type Error = ArityMismatch;
    const OPS: &'static [OpKind] = &[];

    fn check(&self, cx: &Cx<'_>, sid: SentenceId, op: &OpKind) -> Vec<ArityMismatch> {
        let Some(sentence) = cx.sentence(sid) else {
            return Vec::new();
        };
        let arity = op.arity();
        if arity == 0 || arity == sentence.arity() {
            return Vec::new();
        }
        vec![ArityMismatch {
            sid,
            rel: op.name().to_string(),
            expected: arity,
            got: sentence.arity(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::{codes_in, kif_layer};

    #[test]
    fn e005_flags_wrong_relation_arity() {
        let layer = kif_layer(
            r#"
            (subclass Relation Entity)
            (subclass BinaryRelation Relation)
            (instance instance BinaryRelation)
            (instance Foo Bar Baz)
        "#,
        );
        let sid = *layer.syntactic.by_head("instance").last().unwrap();
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"E005"),
            "a 3-argument `instance` must be an arity mismatch; got {codes:?}"
        );
    }

    #[test]
    fn e005_not_flagged_when_arity_matches() {
        let layer = crate::semantics::validate::test_support::base_layer();
        let sid = *layer.syntactic.by_head("subclass").first().unwrap();
        assert!(!codes_in(&layer, sid).contains(&"E005"));
    }
}
