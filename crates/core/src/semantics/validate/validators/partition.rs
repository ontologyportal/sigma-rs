//! W035 partition-non-member: an instance of a class exhaustively decomposed
//! by `(partition C M ...)` or `(exhaustiveDecomposition C M ...)` is not
//! known to fall under any member.
//!
//! Exhaustive decomposition constrains instances only: every instance of `C`
//! is an instance of some member.  Subclasses of `C` may cross-cut the
//! members (`HumanAdult` under `(partition Human Man Woman)`), so subclass
//! edges are never checked.  A missing member is incompleteness, not a
//! contradiction, hence a warning.
//!
//! Checked at each direct edge `(instance X C)` into a decomposed `C`. `X`
//! is covered when any class above any of its instance parents is a member.

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::FormulaValidator;
use crate::{Element, SentenceId, SymbolId, TaxRelation};

use super::common::superclasses;

/// An instance of an exhaustively decomposed class matches no partition member.
#[derive(Debug, Clone, Error)]
#[error("'{sym}' is an instance of '{partition_class}' but does not match any partition member")]
pub struct PartitionNonMember {
    /// The instance edge and the decomposition fact.
    pub sid: Vec<SentenceId>,
    pub sym: String,
    pub partition_class: String,
}
semantic_error!(
    PartitionNonMember,
    "W035",
    "partition-non-member",
    Warning,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (self.sid.clone(), 2)
    },
);

pub(crate) struct PartitionCheck;

impl FormulaValidator for PartitionCheck {
    type Error = PartitionNonMember;

    fn check(&self, cx: &Cx<'_>, root: SentenceId) -> Vec<PartitionNonMember> {
        let Some(s) = cx.sentence(root) else {
            return Vec::new();
        };
        let [Element::Symbol(head), Element::Symbol(sym), Element::Symbol(class)] =
            s.elements.as_slice()
        else {
            return Vec::new();
        };
        if cx.tax_role(head.id()) != Some(TaxRelation::Instance) {
            return Vec::new();
        }
        let decompositions = cx.exhaustive_decompositions(class.id());
        if decompositions.is_empty() {
            return Vec::new();
        }

        let own_classes = classes_of(cx, head.id(), sym.id(), class.id());
        decompositions
            .into_iter()
            .filter(|(_, members)| !members.iter().any(|m| own_classes.contains(m)))
            .map(|(fact, _)| PartitionNonMember {
                sid: vec![root, fact],
                sym: cx.sym_name(sym.id()),
                partition_class: cx.sym_name(class.id()),
            })
            .collect()
    }
}

/// Every class above any `head`-parent of `sym` (`class` included).
fn classes_of(cx: &Cx<'_>, head: SymbolId, sym: SymbolId, class: SymbolId) -> Vec<SymbolId> {
    let mut parents: Vec<SymbolId> = cx
        .binary_objects(head, sym)
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    parents.push(class);
    parents
        .into_iter()
        .flat_map(|p| superclasses(cx, p))
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::{codes_in, kif_layer, root_by_head_with};
    use crate::semantics::SemanticLayer;
    use crate::{Element, SentenceId};

    const BASE: &str = "
        (subclass Organism Entity)
        (subclass Animal Organism)
        (subclass Plant Organism)
        (partition Organism Animal Plant)
        (subclass Dog Animal)
    ";

    /// The root `(head a b)`.
    fn edge(layer: &SemanticLayer, head: &str, a: &str, b: &str) -> SentenceId {
        let id = |n: &str| layer.syntactic.sym_id(n).unwrap();
        let (a, b) = (id(a), id(b));
        layer
            .syntactic
            .by_head(head)
            .into_iter()
            .find(|&sid| {
                let s = layer.syntactic.sentence(sid).unwrap();
                matches!(
                    s.elements.as_slice(),
                    [_, Element::Symbol(x), Element::Symbol(y)] if x.id() == a && y.id() == b
                )
            })
            .unwrap()
    }

    #[test]
    fn subclass_edges_are_never_checked() {
        let layer = kif_layer(&format!(
            "{BASE}\n(subclass Fungus Organism)\n(exhaustiveDecomposition Shape Round Angular)\n(subclass Blob Shape)"
        ));
        for (a, b) in [("Fungus", "Organism"), ("Blob", "Shape"), ("Dog", "Animal")] {
            let sid = edge(&layer, "subclass", a, b);
            assert!(!codes_in(&layer, sid).contains(&"W035"), "{a} -> {b}");
        }
    }

    #[test]
    fn disjoint_decomposition_is_not_exhaustive() {
        let layer = kif_layer(
            "
            (disjointDecomposition Shape Round Angular)
            (subclass Blob Shape)
            (instance Thing Shape)
        ",
        );
        for sid in layer.syntactic.root_sids() {
            let codes = codes_in(&layer, sid);
            assert!(!codes.contains(&"W035"));
        }
    }

    #[test]
    fn w035_flags_instance_matching_no_member() {
        let layer = kif_layer(&format!("{BASE}\n(instance Blob Organism)"));
        let sid = root_by_head_with(&layer, "instance", "Blob");
        let fact = *layer.syntactic.by_head("partition").first().unwrap();
        let errs = layer
            .validator_scoped(crate::semantics::types::Scope::Base)
            .validate_sentence_collect(sid);
        let e = errs.iter().find(|e| e.code() == "W035").unwrap();
        assert_eq!(e.severity(), crate::Severity::Warning);
        assert_eq!(e.anchors(), (vec![sid, fact], 2));
        assert!(e.to_string().contains("'Blob'"), "got {e}");
    }

    #[test]
    fn w035_not_flagged_when_another_edge_reaches_a_member() {
        let layer = kif_layer(&format!(
            "{BASE}\n(instance Rex Organism)\n(instance Rex Dog)"
        ));
        let sid = edge(&layer, "instance", "Rex", "Organism");
        assert!(!codes_in(&layer, sid).contains(&"W035"));
    }

    #[test]
    fn instances_below_a_member_are_not_rechecked() {
        let layer = kif_layer(&format!("{BASE}\n(instance Rex Dog)"));
        let sid = root_by_head_with(&layer, "instance", "Rex");
        assert!(!codes_in(&layer, sid).contains(&"W035"));
    }
}
