//! Semantic validation: do the sentences adhere to SUMO semantics?
//!
//!   mod.rs      -- the driver: walks a formula once and dispatches
//!   cx.rs       -- `Cx`, the scope-applied read surface validators see
//!   traits.rs   -- the four validator traits + object-safe companions
//!   validators/ -- one validator per file, with its finding type and tests

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::errors::SemanticError;
use crate::{Element, OpKind, SentenceId};

use super::types::Scope;
use super::SemanticLayer;

pub(crate) mod cx;
pub(crate) mod traits;
pub(crate) mod validators;

#[cfg(test)]
mod test_support;

use cx::Cx;
use traits::SymbolPos;

/// A finding with no more specific type -- currently the "sentence does not
/// exist" case in the validation driver.
#[derive(Debug, Clone, Error)]
#[error("{msg}")]
pub struct Other {
    pub msg: String,
}
semantic_error!(Other, "E016", "other", Error);

impl SemanticLayer {
    /// A validation context borrowing this layer, reasoning in an explicit
    /// [`Scope`]. One per validation pass; the layer's caches do the
    /// memoisation, so a context is cheap.
    pub(crate) fn validator_scoped(&self, scope: Scope) -> Cx<'_> {
        self.validation_cx(scope)
    }
}

impl Cx<'_> {
    /// Every semantic finding for root sentence `sid`, in traversal order.
    ///
    /// Every validator runs to completion; nothing short-circuits on the first
    /// finding. Whether a finding is an error or a warning is decided by the
    /// finding itself ([`SemanticError::severity`]) and applied downstream when
    /// it is rendered as a [`Diagnostic`](crate::Diagnostic).
    pub(crate) fn validate_sentence_collect(&self, sid: SentenceId) -> Vec<Box<dyn SemanticError>> {
        let mut out = Vec::new();
        collect_root(self, sid, &mut out);
        out
    }
}

fn collect_root(cx: &Cx<'_>, sid: SentenceId, out: &mut Vec<Box<dyn SemanticError>>) {
    if cx.sentence(sid).is_none() {
        out.push(Box::new(Other {
            msg: format!("Sentence {sid} does not exist"),
        }));
        return;
    }
    // Formula validators walk the whole tree themselves, so they run at the
    // root only -- the sentence walk below recurses and would double-count.
    for v in validators::FORMULA {
        v.run(cx, sid, out);
    }
    cx.reset_root();
    walk_sentence(cx, sid, out);
}

/// Structural well-formedness of one sentence, recursing into nested
/// sub-sentence arguments. Does not re-run the whole-tree formula validators.
fn walk_sentence(cx: &Cx<'_>, sid: SentenceId, out: &mut Vec<Box<dyn SemanticError>>) {
    if !cx.claim_sentence(sid) {
        return;
    }
    let Some(sentence) = cx.sentence(sid) else {
        return;
    };
    if sentence.is_operator() {
        walk_operator(cx, sid, out);
        return;
    }
    crate::log!(
        Trace,
        "sigmakee_rs_core::semantic",
        format!("validating sentence sid={}", sid)
    );

    for v in validators::SENTENCE {
        v.run(cx, sid, out);
    }

    match sentence.elements.first() {
        Some(Element::Symbol(sym)) => visit_symbol(cx, sym.id(), SymbolPos { sid, index: 0 }, out),
        Some(Element::Sub(sub)) => walk_sentence(cx, *sub, out),
        _ => {}
    }

    // Recurse into nested sub-sentence arguments (function terms such as
    // `(MeasureFn 35 Cm)`) and visit argument symbols: a brand-new symbol
    // typically appears ONLY in argument position (`(instance Foo Bar)`), so
    // visiting heads alone would never see it.
    for (i, arg) in sentence.elements[1..].iter().enumerate() {
        match arg {
            Element::Sub(sub_id) => walk_sentence(cx, *sub_id, out),
            Element::Symbol(sym) => {
                visit_symbol(cx, sym.id(), SymbolPos { sid, index: i + 1 }, out)
            }
            _ => {}
        }
    }
}

fn walk_operator(cx: &Cx<'_>, sid: SentenceId, out: &mut Vec<Box<dyn SemanticError>>) {
    let Some(sentence) = cx.sentence(sid) else {
        return;
    };
    let Some(op) = sentence.op().cloned() else {
        return;
    };

    for v in validators::OPERATOR {
        if v.claims(&op) {
            v.run(cx, sid, &op, out);
        }
    }

    // `=` is a term-level equality, not a connective over sentences: its
    // arguments are terms and must not be walked as sub-sentences.
    if matches!(op, OpKind::Equal) {
        return;
    }

    let args_start = if matches!(op, OpKind::ForAll | OpKind::Exists) {
        2
    } else {
        1
    };
    for el in &sentence.elements[args_start..] {
        if let Element::Sub(sub_id) = el {
            walk_sentence(cx, *sub_id, out);
        }
    }
}

fn visit_symbol(
    cx: &Cx<'_>,
    sym: crate::SymbolId,
    pos: SymbolPos,
    out: &mut Vec<Box<dyn SemanticError>>,
) {
    for v in validators::SYMBOL {
        v.run(cx, sym, pos, out);
    }
}

#[cfg(test)]
mod symbol_anchoring {
    use super::test_support::{kif_layer, root_by_head_with};
    use crate::semantics::types::Scope;

    /// A symbol-level finding anchors to the sentence the symbol occurs in and
    /// the element index within it, so a renderer can highlight the exact
    /// argument rather than the whole formula.
    #[test]
    fn symbol_findings_carry_their_occurrence_site() {
        let layer = kif_layer(
            "
            (subclass Abstract Entity)
            (subclass Relation Abstract)
            (subclass Predicate Relation)
            (subclass BinaryPredicate Predicate)
            (instance instance BinaryPredicate)
            (instance Adam Undeclared)
        ",
        );
        let sid = root_by_head_with(&layer, "instance", "Adam");
        let errs = layer
            .validator_scoped(Scope::Base)
            .validate_sentence_collect(sid);
        let e = errs
            .iter()
            .find(|e| e.code() == "E001" && e.to_string().contains("Undeclared"))
            .expect("expected E001 for `Undeclared`");
        assert_eq!(
            e.anchors(),
            (vec![sid], 2),
            "`Undeclared` is argument 2 of the sentence it occurs in"
        );
    }

    /// The site is the *nested* sentence when the symbol occurs inside a
    /// sub-term, not the enclosing root.
    #[test]
    fn nested_occurrence_anchors_to_the_sub_sentence() {
        let layer = kif_layer(
            "
            (subclass Abstract Entity)
            (subclass Relation Abstract)
            (subclass Function Relation)
            (subclass BinaryFunction Function)
            (subclass Predicate Relation)
            (subclass BinaryPredicate Predicate)
            (instance instance BinaryPredicate)
            (instance MeasureFn BinaryFunction)
            (instance Adam (MeasureFn 35 Undeclared))
        ",
        );
        let root = root_by_head_with(&layer, "instance", "Adam");
        let errs = layer
            .validator_scoped(Scope::Base)
            .validate_sentence_collect(root);
        let e = errs
            .iter()
            .find(|e| e.code() == "E001" && e.to_string().contains("Undeclared"))
            .expect("expected E001 for `Undeclared`");
        let (sids, arg) = e.anchors();
        assert_ne!(
            sids,
            vec![root],
            "should anchor to the nested function term"
        );
        assert_eq!(
            arg, 2,
            "`Undeclared` is argument 2 of `(MeasureFn 35 Undeclared)`"
        );
    }
}
