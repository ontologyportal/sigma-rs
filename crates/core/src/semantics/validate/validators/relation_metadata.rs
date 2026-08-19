//! Declaration-completeness of a relation used as a sentence head:
//! E010 missing-domain, W009 missing-arity, E008 missing-range.

use thiserror::Error;

use crate::semantics::errors::{semantic_error, BoxedError};
use crate::semantics::types::RelationRange;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::{SymbolPos, SymbolValidator};
use crate::{SentenceId, SymbolId};

#[derive(Debug, Clone, Error)]
#[error("symbol '{sym}' is missing a domain declaration for argument {idx}")]
pub struct MissingDomain {
    pub sid: SentenceId,
    pub index: usize,
    pub sym: String,
    pub idx: usize,
}
semantic_error!(
    MissingDomain,
    "E010",
    "missing-domain",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], self.index as i32)
    },
);

#[derive(Debug, Clone, Error)]
#[error("relation '{sym}' is missing inheritance from a specific arity stating class (e.g. BinaryRelation)")]
pub struct MissingArity {
    pub sid: SentenceId,
    pub index: usize,
    pub sym: String,
}
semantic_error!(
    MissingArity,
    "W009",
    "missing-arity",
    Warning,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], self.index as i32)
    },
);

#[derive(Debug, Clone, Error)]
#[error("function '{sym}' has no range declaration")]
pub struct MissingRange {
    pub sid: SentenceId,
    pub index: usize,
    pub sym: String,
}
semantic_error!(
    MissingRange,
    "E008",
    "missing-range",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], self.index as i32)
    },
);

/// Raises three distinct findings, so it erases to [`BoxedError`] rather than
/// naming a single `type Error`.
pub(crate) struct RelationMetadata;

impl SymbolValidator for RelationMetadata {
    type Error = BoxedError;

    fn check(&self, cx: &Cx<'_>, sym: SymbolId, pos: SymbolPos) -> Vec<BoxedError> {
        // Relation-signature completeness is a property of the head position;
        // an argument symbol is not being used as a relation here.
        if !pos.is_head() || !cx.is_relation(sym) {
            return Vec::new();
        }
        let name = cx.sym_name(sym);
        let mut out: Vec<BoxedError> = Vec::new();

        // Each declared argument position must name a domain class; an
        // `Unknown` gap (`rd.id() == None`) means none was declared there.
        for (idx, rd) in cx.domain(sym).iter().enumerate() {
            if rd.id().is_none() {
                out.push(Box::new(MissingDomain {
                    sid: pos.sid,
                    index: pos.index,
                    sym: name.clone(),
                    idx,
                }));
            }
        }

        // A relation must declare its arity via its `BinaryRelation` / ...
        // ancestry.
        if cx.arity(sym).is_none() {
            out.push(Box::new(MissingArity {
                sid: pos.sid,
                index: pos.index,
                sym: name.clone(),
            }));
        }

        // A function needs a declared range. `Unknown` covers both "no range"
        // and "conflicting range/rangeSubclass"; the latter is additionally
        // surfaced as a DoubleRange diagnostic by the `semantic::range` cache
        // reactor on ingest, so only the missing case is flagged here.
        if cx.is_function(sym) && matches!(cx.range(sym), RelationRange::Unknown) {
            out.push(Box::new(MissingRange {
                sid: pos.sid,
                index: pos.index,
                sym: name,
            }));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::{codes_in, kif_layer, root_by_head};

    #[test]
    fn e008_flags_function_without_range() {
        let layer = kif_layer(
            r#"
            (subclass Relation Entity)
            (subclass Function Relation)
            (subclass UnaryFunction Function)
            (instance AbsoluteValueFn UnaryFunction)
            (instance AbsoluteValueFn Function)
            (AbsoluteValueFn N)
        "#,
        );
        let sid = root_by_head(&layer, "AbsoluteValueFn");
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"E008"),
            "a function with no range declaration must be flagged; got {codes:?}"
        );
    }

    #[test]
    fn w009_flags_relation_without_arity() {
        let layer = kif_layer(
            r#"
            (subclass Relation Entity)
            (instance likes Relation)
            (likes A B)
        "#,
        );
        let sid = root_by_head(&layer, "likes");
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"W009"),
            "a relation with no arity-stating ancestor must be flagged; got {codes:?}"
        );
    }

    #[test]
    fn metadata_checks_do_not_fire_for_argument_symbols() {
        // `likes` lacks arity, but as an *argument* it is not being used as a
        // relation, so its signature is not this sentence's concern.
        let layer = kif_layer(
            r#"
            (subclass Relation Entity)
            (subclass BinaryRelation Relation)
            (instance instance BinaryRelation)
            (instance likes Relation)
            (instance likes Relation)
        "#,
        );
        let sid = *layer.syntactic.by_head("instance").last().unwrap();
        let codes = codes_in(&layer, sid);
        assert!(
            !codes.contains(&"W009"),
            "arity completeness is a head-position property; got {codes:?}"
        );
    }
}
