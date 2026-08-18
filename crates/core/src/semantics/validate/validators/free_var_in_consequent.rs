//! W021 free-var-in-consequent, computed once over the entire root formula.
//!
//! A variable is flagged iff it occurs in some implication consequent yet is
//! bound nowhere in the rule -- it never appears in any antecedent (at any
//! nesting depth) and is never introduced by a `forall` / `exists`. Computing
//! over the whole tree keeps variables bound by an enclosing antecedent or
//! quantifier from being mistaken for free occurrences.

use std::collections::HashSet;

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::FormulaValidator;
use crate::SentenceId;

use super::common::collect_binding_structure;

#[derive(Debug, Clone, Error)]
#[error("variable '{var}' in consequent is not bound by antecedent or quantifier")]
pub struct FreeVarInConsequent {
    pub sid: SentenceId,
    pub var: String,
}
semantic_error!(
    FreeVarInConsequent,
    "W021",
    "free-var-in-consequent",
    Warning,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], -1)
    },
    fn highlight_var(&self) -> Option<String> {
        Some(self.var.clone())
    },
);

pub(crate) struct FreeVarInConsequentCheck;

impl FormulaValidator for FreeVarInConsequentCheck {
    type Error = FreeVarInConsequent;

    fn check(&self, cx: &Cx<'_>, root: SentenceId) -> Vec<FreeVarInConsequent> {
        let mut bound: HashSet<String> = HashSet::new();
        let mut consequent: HashSet<String> = HashSet::new();
        collect_binding_structure(cx, root, &mut bound, &mut consequent);

        let mut free: Vec<String> = consequent
            .into_iter()
            .filter(|v| !bound.contains(v))
            .collect();
        free.sort_unstable();
        free.into_iter()
            .map(|var| FreeVarInConsequent { sid: root, var })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::{codes_in, kif_layer, root_by_op, sub_by_op};

    #[test]
    fn w021_free_var_in_consequent() {
        let layer = kif_layer(
            r#"
            (instance Human Class)
            (=> (instance ?X Human) (instance ?Y Human))
        "#,
        );
        let sid = root_by_op(&layer, crate::OpKind::Implies);
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"W021"),
            "expected W021 free-var-in-consequent; got {codes:?}"
        );
    }

    #[test]
    fn w021_not_flagged_when_consequent_var_is_existentially_bound() {
        // `?Y` appears only in the consequent, but is bound by a nested
        // `(exists ...)`, so it is not free.
        let layer = kif_layer(
            r#"
            (instance Human Class)
            (=> (instance ?X Human)
                (exists (?Y) (instance ?Y Human)))
        "#,
        );
        let sid = root_by_op(&layer, crate::OpKind::Implies);
        let codes = codes_in(&layer, sid);
        assert!(
            !codes.contains(&"W021"),
            "W021 must not fire for an exists-bound consequent var; got {codes:?}"
        );
    }

    #[test]
    fn w021_not_flagged_when_consequent_var_bound_by_enclosing_antecedent() {
        // `?A` occurs in the consequent's inner implication `(part ?C ?A)` but
        // is bound by the outer antecedent `(surface ?A ?B)`.
        let layer = kif_layer(
            r#"
            (instance Object Class)
            (=> (surface ?A ?B)
                (forall (?C)
                    (=> (superficialPart ?C ?B) (part ?C ?A))))
        "#,
        );
        let sid = root_by_op(&layer, crate::OpKind::Implies);
        let codes = codes_in(&layer, sid);
        assert!(
            !codes.contains(&"W021"),
            "W021 must not fire for a var bound by an enclosing antecedent; got {codes:?}"
        );
    }

    #[test]
    fn w021_not_flagged_across_biimplication_halves() {
        // Both halves of `<=>` bind each other, so neither is a pure
        // consequent and a variable on one side is not free.
        let layer = kif_layer(
            r#"
            (instance Human Class)
            (=> (instance Adam Human)
                (<=> (instance ?X Human) (instance ?X Human)))
        "#,
        );
        let sid = sub_by_op(&layer, crate::OpKind::Iff);
        assert!(!codes_in(&layer, sid).contains(&"W021"));
    }
}
