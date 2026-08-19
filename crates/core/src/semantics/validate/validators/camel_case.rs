//! W032 term-camel-case: multi-word terms are CamelCase, never underscore- or
//! hyphen-separated.

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::{SymbolPos, SymbolValidator};
use crate::{SentenceId, SymbolId};

#[derive(Debug, Clone, Error)]
#[error("term '{sym}' should use CamelCase, not underscores or hyphens")]
pub struct TermCamelCase {
    pub sid: SentenceId,
    pub index: usize,
    pub sym: String,
}
semantic_error!(
    TermCamelCase,
    "W032",
    "term-camel-case",
    Warning,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], self.index as i32)
    },
);

pub(crate) struct CamelCase;

impl CamelCase {
    const TAG: &'static str = "term-camel-case";
}

impl SymbolValidator for CamelCase {
    type Error = TermCamelCase;

    fn check(&self, cx: &Cx<'_>, sym: SymbolId, pos: SymbolPos) -> Vec<TermCamelCase> {
        if !cx.claim_symbol(Self::TAG, sym) {
            return Vec::new();
        }
        let name = cx.sym_name(sym);
        if name.contains('_') || name.contains('-') {
            return vec![TermCamelCase {
                sid: pos.sid,
                index: pos.index,
                sym: name,
            }];
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::{codes_in, kif_layer, root_by_head_with};

    const BASE: &str = "
        (subclass Abstract Entity)
        (subclass Relation Abstract)
        (subclass Predicate Relation)
        (subclass BinaryPredicate Predicate)
        (subclass Class Abstract)
        (instance instance BinaryPredicate)
        (subclass Human Entity)
    ";

    fn codes_for(term: &str) -> Vec<&'static str> {
        let layer = kif_layer(&format!("{BASE}\n(instance {term} Human)"));
        let sid = root_by_head_with(&layer, "instance", term);
        codes_in(&layer, sid)
    }

    #[test]
    fn w032_flags_underscore_separated_term() {
        let codes = codes_for("Adam_Smith");
        assert!(codes.contains(&"W032"), "got {codes:?}");
    }

    #[test]
    fn w032_flags_hyphen_separated_term() {
        let codes = codes_for("Adam-Smith");
        assert!(codes.contains(&"W032"), "got {codes:?}");
    }

    #[test]
    fn w032_not_flagged_for_camel_case_term() {
        let codes = codes_for("AdamSmith");
        assert!(!codes.contains(&"W032"), "got {codes:?}");
    }

    #[test]
    fn w032_not_flagged_for_single_word_term() {
        let codes = codes_for("Adam");
        assert!(!codes.contains(&"W032"), "got {codes:?}");
    }
}
