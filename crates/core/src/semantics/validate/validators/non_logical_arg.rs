//! E004 non-logical-arg: an operator's arguments must be truth-valued
//! sentences, not terms.

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::OperatorValidator;
use crate::{Element, OpKind, SentenceId};

#[derive(Debug, Clone, Error)]
#[error("argument {arg} of the operator, {op}, must be logical (predicate or operator) sentence")]
pub struct NonLogicalArg {
    pub sid: SentenceId,
    pub arg: usize,
    pub op: String,
}
semantic_error!(
    NonLogicalArg,
    "E004",
    "non-logical-arg",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], self.arg as i32)
    },
);

pub(crate) struct NonLogicalArgCheck;

impl OperatorValidator for NonLogicalArgCheck {
    type Error = NonLogicalArg;
    const OPS: &'static [OpKind] = &[];

    fn check(&self, cx: &Cx<'_>, sid: SentenceId, op: &OpKind) -> Vec<NonLogicalArg> {
        // `=` relates terms, not sentences, so its arguments are exempt.
        if matches!(op, OpKind::Equal) {
            return Vec::new();
        }
        let Some(sentence) = cx.sentence(sid) else {
            return Vec::new();
        };
        // A quantifier's first argument is its variable list, not a sentence.
        let args_start = if matches!(op, OpKind::ForAll | OpKind::Exists) {
            2
        } else {
            1
        };
        sentence.elements[args_start..]
            .iter()
            .filter_map(|e| match e {
                Element::Sub(id) => Some(*id),
                _ => None,
            })
            .enumerate()
            .filter(|(_, sub)| !is_logical_sentence(cx, *sub))
            .map(|(idx, _)| NonLogicalArg {
                sid,
                arg: idx + 1,
                op: op.to_string(),
            })
            .collect()
    }
}

/// Whether `sid` denotes a truth-valued sentence rather than a term.
fn is_logical_sentence(cx: &Cx<'_>, sid: SentenceId) -> bool {
    let Some(sentence) = cx.sentence(sid) else {
        return false;
    };
    if sentence.is_operator() {
        return true;
    }
    let head_id = match sentence.elements.first() {
        Some(Element::Symbol(sym)) => sym.id(),
        Some(Element::Variable { .. }) => return true,
        _ => return false,
    };
    !cx.is_function(head_id)
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::{codes_in, kif_layer, root_by_op, sub_by_op};

    #[test]
    fn e004_not_flagged_for_predicate_variable_head() {
        // `(?REL ?X ?Y)` is a higher-order literal -- a predicate-variable
        // application -- and is logical.
        let layer = kif_layer(
            r#"
            (instance Relation Class)
            (=> (instance ?REL Relation)
                (and (?REL ?X ?Y) (?REL ?Y ?X)))
        "#,
        );
        let sid = root_by_op(&layer, crate::OpKind::Implies);
        let codes = codes_in(&layer, sid);
        assert!(
            !codes.contains(&"E004"),
            "E004 must not fire on predicate-variable heads; got {codes:?}"
        );
    }

    #[test]
    fn e004_flags_function_headed_argument() {
        // A declared function is a term, so it cannot be an argument of `and`.
        let layer = kif_layer(
            r#"
            (instance Human Class)
            (instance AbsoluteValueFn Function)
            (=> (instance ?X Human) (and (AbsoluteValueFn ?X) (instance ?X Human)))
        "#,
        );
        let sid = sub_by_op(&layer, crate::OpKind::And);
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"E004"),
            "expected E004 for a function-headed argument; got {codes:?}"
        );
    }

    #[test]
    fn e004_not_flagged_for_undeclared_head() {
        // Unknown is not not-a-relation: an undeclared head stays logical, and
        // its misuse is already reported as E002.
        let layer = kif_layer(
            r#"
            (instance Human Class)
            (=> (instance ?X Human) (and (NotARelation ?X) (instance ?X Human)))
        "#,
        );
        let sid = sub_by_op(&layer, crate::OpKind::And);
        assert!(!codes_in(&layer, sid).contains(&"E004"));
    }
}
