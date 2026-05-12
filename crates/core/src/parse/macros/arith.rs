// crates/core/src/parse/macros/arith.rs
//
// Ingest-time constant folding of ground arithmetic: any
// `(AdditionFn 3.0 5.0)`-shaped subtree (the four SUMO arithmetic
// functions over two numeric literals) rewrites to its value, bottom-
// up, so nested expressions collapse fully.  Runs in the parser-
// agnostic macro pipeline — every frontend (KIF, TPTP, …) and every
// consumer (store, translation, validation, the native prover's
// detached conjecture parse) inherits the same interpretation, and
// content addressing makes arithmetically-equal formulas the SAME
// sentence.
//
// The evaluation/rendering brain lives in `crate::numeric`, shared
// with the prover's run-time `arith_norm` (terms born from
// substitution mid-proof can't be folded at ingest), so ingest and
// saturation can never disagree about arithmetic.

use crate::numeric::{eval_binary_fn, format_num, parse_num};

use super::super::AstNode;

/// Fold ground arithmetic subtrees in place, bottom-up.
pub(crate) fn fold_arithmetic(node: &mut AstNode) {
    let AstNode::List { elements, span } = node else { return };
    for el in elements.iter_mut() {
        fold_arithmetic(el);
    }
    if elements.len() != 3 {
        return;
    }
    let (AstNode::Symbol { name, .. }, AstNode::Number { value: a, .. }, AstNode::Number { value: b, .. }) =
        (&elements[0], &elements[1], &elements[2])
    else {
        return;
    };
    let (Some(x), Some(y)) = (parse_num(a), parse_num(b)) else { return };
    let Some(v) = eval_binary_fn(name, x, y) else { return };
    *node = AstNode::Number { value: format_num(v), span: span.clone() };
}
