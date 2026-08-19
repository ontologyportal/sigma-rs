//! E001 no-entity-ancestor: every symbol must trace to `Entity` through the
//! taxonomy.

use thiserror::Error;

use crate::semantics::consts::ROOT_SYMBOL;
use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::{SymbolPos, SymbolValidator};
use crate::{SentenceId, SymbolId};

#[derive(Debug, Clone, Error)]
#[error("symbol '{sym}' must have a valid derivation to Entity")]
pub struct NoEntityAncestor {
    pub sid: SentenceId,
    pub index: usize,
    pub sym: String,
}
semantic_error!(
    NoEntityAncestor,
    "E001",
    "no-entity-ancestor",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], self.index as i32)
    },
);

pub(crate) struct EntityAncestor;

impl SymbolValidator for EntityAncestor {
    type Error = NoEntityAncestor;

    fn check(&self, cx: &Cx<'_>, sym: SymbolId, pos: SymbolPos) -> Vec<NoEntityAncestor> {
        // Deduplicated per root pass: the same symbol commonly recurs across a
        // formula (head, arguments, nested subs) and should yield one finding.
        if !cx.claim_symbol("E001", sym) {
            return Vec::new();
        }
        if cx.has_ancestor_by_name(sym, &ROOT_SYMBOL.name()) {
            return Vec::new();
        }
        vec![NoEntityAncestor {
            sid: pos.sid,
            index: pos.index,
            sym: cx.sym_name(sym),
        }]
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::types::Scope;
    use crate::semantics::validate::test_support::{kif_layer, root_by_op, roots};

    #[test]
    fn e001_fires_for_argument_symbols() {
        // A brand-new symbol typically appears ONLY in argument position
        // (`(instance Foo Bar)`), so visiting heads alone never sees it.
        let layer = kif_layer(
            "
            (subclass Relation Entity)
            (subclass BinaryRelation Relation)
            (instance subclass BinaryRelation)
            (instance instance BinaryRelation)
            (instance MyNewThing MyNewClass)
        ",
        );
        let errs: Vec<_> = roots(&layer)
            .into_iter()
            .flat_map(|sid| {
                layer
                    .validator_scoped(Scope::Base)
                    .validate_sentence_collect(sid)
            })
            .collect();
        for want in ["MyNewThing", "MyNewClass"] {
            assert!(
                errs.iter()
                    .any(|e| e.code() == "E001" && e.to_string().contains(want)),
                "expected E001 no-entity-ancestor for {want}"
            );
        }
    }

    #[test]
    fn e001_deduplicated_per_formula() {
        // The same disconnected symbol recurring in one formula yields one
        // E001, not one per occurrence.
        let layer = kif_layer(
            "
            (subclass Relation Entity)
            (instance instance Relation)
            (=> (instance Loner Loner) (instance Loner Loner))
        ",
        );
        let sid = root_by_op(&layer, crate::OpKind::Implies);
        let errs = layer
            .validator_scoped(Scope::Base)
            .validate_sentence_collect(sid);
        let e001s = errs.iter().filter(|e| e.code() == "E001").count();
        assert_eq!(e001s, 1, "expected exactly one E001 for Loner");
    }

    #[test]
    fn e001_not_flagged_for_connected_symbol() {
        let layer = crate::semantics::validate::test_support::base_layer();
        let sid = *layer.syntactic.by_head("subclass").first().unwrap();
        let codes = crate::semantics::validate::test_support::codes_in(&layer, sid);
        assert!(!codes.contains(&"E001"), "got {codes:?}");
    }
}
