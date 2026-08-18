// crates/core/src/semantics/validate/test_support.rs
//
// Sentence-lookup helpers shared by the validator's per-file test modules
// (`mod.rs`, `structural.rs`, `diagnostics.rs`), so each fixture is defined
// once.  The `SemanticLayer` fixtures themselves are reused from the semantic
// caches' `test_support` rather than redefined.

use crate::semantics::types::Scope;
use crate::semantics::SemanticLayer;
use crate::{Element, OpKind, SentenceId};

pub(super) use crate::semantics::caches::test_support::{base_layer, kif_layer};

/// Unwrap a search that must have matched exactly one sentence.
fn only(mut found: Vec<SentenceId>, what: &str) -> SentenceId {
    found.sort_unstable();
    found.dedup();
    assert_eq!(found.len(), 1, "expected exactly one {what}, got {found:?}");
    found[0]
}

/// Root sentence ids in ascending [`SentenceId`] order.
pub(super) fn roots(layer: &SemanticLayer) -> Vec<SentenceId> {
    let mut r = layer.syntactic.root_sids();
    r.sort_unstable();
    r
}

/// The single root sentence headed by predicate `head`.
pub(super) fn root_by_head(layer: &SemanticLayer, head: &str) -> SentenceId {
    only(
        layer.syntactic.by_head(head),
        &format!("root headed by `{head}`"),
    )
}

/// The single root whose operator is `op` (for operator-headed roots like
/// `and` / `=>` / `forall`, which `by_head` does not index).
pub(super) fn root_by_op(layer: &SemanticLayer, op: OpKind) -> SentenceId {
    let found = roots(layer)
        .into_iter()
        .filter(|&sid| has_op(layer, sid, &op))
        .collect();
    only(found, &format!("root with op {op:?}"))
}

/// The single sentence (root or nested sub, at any depth) whose operator is
/// `op`.
pub(super) fn sub_by_op(layer: &SemanticLayer, op: OpKind) -> SentenceId {
    fn walk(layer: &SemanticLayer, sid: SentenceId, op: &OpKind, out: &mut Vec<SentenceId>) {
        let Some(sent) = layer.syntactic.sentence(sid) else {
            return;
        };
        if sent.op().is_some_and(|o| o == op) {
            out.push(sid);
        }
        for el in &sent.elements {
            if let Element::Sub(sub) = el {
                walk(layer, *sub, op, out);
            }
        }
    }
    let mut found = Vec::new();
    for r in roots(layer) {
        walk(layer, r, &op, &mut found);
    }
    only(found, &format!("sentence with op {op:?}"))
}

fn has_op(layer: &SemanticLayer, sid: SentenceId, op: &OpKind) -> bool {
    layer
        .syntactic
        .sentence(sid)
        .and_then(|s| s.op().cloned())
        .is_some_and(|o| o == *op)
}

/// The diagnostic codes produced by validating `sid` in [`Scope::Base`].
pub(super) fn codes_in(layer: &SemanticLayer, sid: SentenceId) -> Vec<&'static str> {
    layer
        .validator_scoped(Scope::Base)
        .validate_sentence_collect(sid)
        .iter()
        .map(|e| e.code())
        .collect()
}

/// The single root headed by `head` that mentions the symbol named `sym`.
///
/// `by_head` order is not insertion order, so a fixture with several roots
/// under the same predicate cannot be indexed positionally.
pub(crate) fn root_by_head_with(layer: &SemanticLayer, head: &str, sym: &str) -> SentenceId {
    let mut matches: Vec<SentenceId> = layer
        .syntactic
        .by_head(head)
        .into_iter()
        .filter(|sid| {
            layer.syntactic.sentence(*sid).is_some_and(|s| {
                s.elements.iter().any(|el| match el {
                    Element::Symbol(x) => layer
                        .syntactic
                        .sym_name(x.id())
                        .is_some_and(|n| *n.name() == *sym),
                    _ => false,
                })
            })
        })
        .collect();
    matches.sort_unstable();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one root headed by `{head}` mentioning `{sym}`, got {matches:?}"
    );
    matches[0]
}
