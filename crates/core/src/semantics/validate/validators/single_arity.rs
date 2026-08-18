//! E017 single-arity: a one-argument `and` / `or` is well-formed but
//! meaningless.

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::OperatorValidator;
use crate::{OpKind, SentenceId};

#[derive(Debug, Clone, Error)]
#[error("only one argument was passed to an conjunctive/disjunctive operator. Not technically incorrect, but meaningless")]
pub struct SingleArity {
    pub sid: SentenceId,
}
semantic_error!(
    SingleArity,
    "E017",
    "single-arity",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], 0)
    },
);

pub(crate) struct SingleArityCheck;

impl OperatorValidator for SingleArityCheck {
    type Error = SingleArity;
    const OPS: &'static [OpKind] = &[OpKind::And, OpKind::Or];

    fn check(&self, cx: &Cx<'_>, sid: SentenceId, _op: &OpKind) -> Vec<SingleArity> {
        match cx.sentence(sid) {
            Some(s) if s.arity() == 1 => vec![SingleArity { sid }],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::kif_layer;

    /// A one-argument `and` / `or` never reaches validation: the parser drops
    /// the whole root, so no `Iff`-style nesting rescues it either. Recorded
    /// here so the gap is visible rather than looking like missing coverage.
    #[test]
    fn single_argument_connective_is_rejected_before_validation() {
        let layer = kif_layer(
            r#"
            (instance Human Class)
            (=> (instance ?X Human) (and (instance ?X Human)))
        "#,
        );
        let ops: Vec<_> = layer
            .syntactic
            .root_sids()
            .into_iter()
            .filter_map(|sid| layer.syntactic.sentence(sid).and_then(|s| s.op().cloned()))
            .collect();
        assert!(
            ops.is_empty(),
            "a single-argument `and` is expected to be dropped at parse; got {ops:?}"
        );
    }
}
