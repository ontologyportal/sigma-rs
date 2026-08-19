//! E033 too-general-rel: a symbol declared a `Relation` must be specifically a
//! `Predicate` or a `Function`.

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::{SymbolPos, SymbolValidator};
use crate::{SentenceId, SymbolId};

#[derive(Debug, Clone, Error)]
#[error("the symbol '{sym}' is a relation but needs to be specifically a function or predicate")]
pub struct TooGeneralRel {
    pub sid: SentenceId,
    pub index: usize,
    pub sym: String,
}
semantic_error!(
    TooGeneralRel,
    "E033",
    "too-general-rel",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], self.index as i32)
    },
);

pub(crate) struct OnlyRel;

impl SymbolValidator for OnlyRel {
    type Error = TooGeneralRel;

    fn check(&self, cx: &Cx<'_>, sym: SymbolId, pos: SymbolPos) -> Vec<TooGeneralRel> {
        if !cx.claim_symbol("too-general-rel", sym) {
            return Vec::new();
        }
        if !cx.is_relation(sym) || cx.is_predicate(sym) || cx.is_function(sym) {
            return Vec::new();
        }
        vec![TooGeneralRel {
            sid: pos.sid,
            index: pos.index,
            sym: cx.sym_name(sym),
        }]
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::types::Scope;
    use crate::semantics::validate::test_support::{codes_in, kif_layer, root_by_head_with};

    const BASE: &str = "
        (subclass Abstract Entity)
        (subclass Relation Abstract)
        (subclass Predicate Relation)
        (subclass Function Relation)
        (subclass BinaryRelation Relation)
        (subclass BinaryPredicate Predicate)
        (subclass UnaryFunction Function)
        (instance instance BinaryPredicate)
    ";

    #[test]
    fn e033_flags_a_relation_that_is_neither_predicate_nor_function() {
        let layer = kif_layer(&format!("{BASE}\n(instance adjacent BinaryRelation)"));
        let sid = root_by_head_with(&layer, "instance", "adjacent");
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"E033"),
            "a bare BinaryRelation instance must be flagged; got {codes:?}"
        );
    }

    #[test]
    fn e033_not_flagged_for_a_predicate() {
        let layer = kif_layer(&format!("{BASE}\n(instance likes BinaryPredicate)"));
        let sid = root_by_head_with(&layer, "instance", "likes");
        assert!(!codes_in(&layer, sid).contains(&"E033"));
    }

    #[test]
    fn e033_not_flagged_for_a_function() {
        let layer = kif_layer(&format!("{BASE}\n(instance AbsoluteValueFn UnaryFunction)"));
        let sid = root_by_head_with(&layer, "instance", "AbsoluteValueFn");
        assert!(!codes_in(&layer, sid).contains(&"E033"));
    }

    #[test]
    fn e033_not_flagged_for_a_relation_class() {
        let layer = kif_layer(&format!("{BASE}\n(subclass TernaryRelation Relation)"));
        let sid = *layer.syntactic.by_head("subclass").last().unwrap();
        assert!(
            !codes_in(&layer, sid).contains(&"E033"),
            "`is_relation` requires instance-hood, so a relation *class* is exempt"
        );
    }

    #[test]
    fn e033_anchors_to_the_argument_it_occurs_in() {
        let layer = kif_layer(&format!("{BASE}\n(instance adjacent BinaryRelation)"));
        let sid = root_by_head_with(&layer, "instance", "adjacent");
        let errs = layer
            .validator_scoped(Scope::Base)
            .validate_sentence_collect(sid);
        let e = errs.iter().find(|e| e.code() == "E033").unwrap();
        assert_eq!(
            e.anchors(),
            (vec![sid], 1),
            "`adjacent` is argument 1 of the sentence it occurs in"
        );
    }
}
