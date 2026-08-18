//! W020 single-use-variable: a variable occurring exactly once in an
//! implication consequent, the canonical typo being `?X` written as `?Y`.

use std::collections::{HashMap, HashSet};

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::FormulaValidator;
use crate::SentenceId;

use super::common::{collect_binding_structure, walk_vars};

#[derive(Debug, Clone, Error)]
#[error("variable '{var}' is used only once -- likely a typo")]
pub struct SingleUseVariable {
    pub sid: SentenceId,
    pub var: String,
}
semantic_error!(
    SingleUseVariable,
    "W020",
    "single-use-variable",
    Warning,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], -1)
    },
    fn highlight_var(&self) -> Option<String> {
        Some(self.var.clone())
    },
);

pub(crate) struct SingleUseVariableCheck;

impl FormulaValidator for SingleUseVariableCheck {
    type Error = SingleUseVariable;

    fn check(&self, cx: &Cx<'_>, root: SentenceId) -> Vec<SingleUseVariable> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        walk_vars(cx, root, &mut |name: &str| {
            *counts.entry(name.to_string()).or_insert(0) += 1;
        });

        // Single occurrences outside a consequent -- a top-level fact, or an
        // antecedent "don't care" -- are legitimate implicit universals.
        let mut bound: HashSet<String> = HashSet::new();
        let mut consequent: HashSet<String> = HashSet::new();
        collect_binding_structure(cx, root, &mut bound, &mut consequent);

        let mut flagged: Vec<String> = counts
            .into_iter()
            .filter(|(var, count)| *count == 1 && consequent.contains(var))
            .map(|(var, _)| var)
            .collect();
        flagged.sort_unstable();
        flagged
            .into_iter()
            .map(|var| SingleUseVariable { sid: root, var })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::{codes_in, kif_layer, root_by_op, roots};

    #[test]
    fn w020_single_use_variable_flagged() {
        let layer = kif_layer(
            r#"
            (instance Animal Class)
            (forall (?X) (=> (instance ?X Animal) (instance ?Y Animal)))
        "#,
        );
        let sid = roots(&layer).last().copied().unwrap();
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"W020"),
            "expected W020 single-use-variable; got {codes:?}"
        );
    }

    #[test]
    fn w020_not_flagged_for_non_consequent_single_use_var() {
        // Only consequent single-use vars are flagged; a single-use antecedent
        // var is a legitimate "don't care" universal.
        let layer = kif_layer(
            r#"
            (instance Object Class)
            (=> (diameter ?C ?LEN) (instance ?C Object))
        "#,
        );
        let sid = root_by_op(&layer, crate::OpKind::Implies);
        let codes = codes_in(&layer, sid);
        assert!(
            !codes.contains(&"W020"),
            "W020 must not fire for a single-use *antecedent* var; got {codes:?}"
        );
    }

    #[test]
    fn w020_no_false_positive_when_used_twice() {
        let layer = kif_layer(
            r#"
            (instance Animal Class)
            (forall (?X) (=> (instance ?X Animal) (instance ?X Animal)))
        "#,
        );
        let sid = roots(&layer).last().copied().unwrap();
        let codes = codes_in(&layer, sid);
        assert!(
            !codes.contains(&"W020"),
            "W020 must not fire when a var is used twice; got {codes:?}"
        );
    }
}
