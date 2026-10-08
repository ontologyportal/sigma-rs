//! E003 root-not-formula: a top-level sentence must be a formula (headed by an
//! operator or a predicate), not a term (headed by a function).

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::FormulaValidator;
use crate::SentenceId;

use super::common::is_logical_sentence;

#[derive(Debug, Clone, Error)]
#[error("top-level sentence headed by function '{sym}' is a term, not a formula")]
pub struct RootNotFormula {
    pub sid: SentenceId,
    pub sym: String,
}
semantic_error!(
    RootNotFormula,
    "E003",
    "root-not-formula",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], 0)
    },
);

pub(crate) struct RootFormula;

impl FormulaValidator for RootFormula {
    type Error = RootNotFormula;

    fn check(&self, cx: &Cx<'_>, root: SentenceId) -> Vec<RootNotFormula> {
        if is_logical_sentence(cx, root) {
            return Vec::new();
        }
        let sym = cx
            .sentence(root)
            .and_then(|s| s.head_symbol())
            .map(|head| cx.sym_name(head))
            .unwrap_or_default();
        vec![RootNotFormula { sid: root, sym }]
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::{codes_in, kif_layer, root_by_head, roots};

    const BASE: &str = "
        (subclass Relation Entity)
        (subclass Function Relation)
        (subclass UnaryFunction Function)
        (subclass Predicate Relation)
        (subclass BinaryPredicate Predicate)
        (instance MotherFn UnaryFunction)
        (instance parent BinaryPredicate)
    ";

    #[test]
    fn e003_flags_function_headed_root() {
        let layer = kif_layer(&format!("{BASE}\n(MotherFn Bill)"));
        let sid = root_by_head(&layer, "MotherFn");
        assert!(codes_in(&layer, sid).contains(&"E003"));
    }

    #[test]
    fn e003_not_flagged_for_predicate_operator_or_variable_roots() {
        let layer = kif_layer(&format!(
            "{BASE}
            (parent Bill Jane)
            (=> (parent ?X ?Y) (parent ?X ?Y))
            (=> (instance ?REL BinaryPredicate) (?REL Bill Jane))
            (parent Bill (MotherFn Jane))"
        ));
        for sid in roots(&layer) {
            assert!(
                !codes_in(&layer, sid).contains(&"E003"),
                "unexpected E003 on root {sid}"
            );
        }
    }

    #[test]
    fn e003_anchors_the_head() {
        let layer = kif_layer(&format!("{BASE}\n(MotherFn Bill)"));
        let sid = root_by_head(&layer, "MotherFn");
        let errs = layer
            .validator_scoped(crate::semantics::types::Scope::Base)
            .validate_sentence_collect(sid);
        let e = errs.iter().find(|e| e.code() == "E003").unwrap();
        assert_eq!(e.anchors(), (vec![sid], 0));
        assert!(e.to_string().contains("MotherFn"), "got {e}");
    }
}
