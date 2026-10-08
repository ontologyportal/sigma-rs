//! E014 disjoint-instance / E015 disjoint-subclass: a symbol's `instance` (or
//! `subclass`) parents must not reach two classes declared disjoint.
//!
//! Checked at each taxonomy edge fact `(instance X C)` / `(subclass X C)`,
//! against every other edge of the same kind out of `X`. A conflict is
//! reported only where two branches meet: a disjoint pair already reachable
//! from one parent alone is that parent's conflict, flagged at its own edge.

use std::collections::HashSet;

use thiserror::Error;

use crate::semantics::errors::{semantic_error, BoxedError};
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::FormulaValidator;
use crate::{Element, SentenceId, SymbolId, TaxRelation};

use super::common::superclasses;

/// A symbol is an instance of two disjoint classes.
#[derive(Debug, Clone, Error)]
#[error("'{sym}' is an instance of disjoint classes ({class1} and {class2})")]
pub struct DisjointInstance {
    /// The edge raising the finding, the conflicting edge, and the
    /// disjointness fact.
    pub sid: Vec<SentenceId>,
    pub sym: String,
    pub class1: String,
    pub class2: String,
}
semantic_error!(
    DisjointInstance,
    "E014",
    "disjoint-instance",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (self.sid.clone(), 2)
    },
);

/// A symbol is a subclass of two disjoint classes.
#[derive(Debug, Clone, Error)]
#[error("'{sym}' is a subclass of disjoint classes ({class1} and {class2})")]
pub struct DisjointSubclass {
    /// The edge raising the finding, the conflicting edge, and the
    /// disjointness fact.
    pub sid: Vec<SentenceId>,
    pub sym: String,
    pub class1: String,
    pub class2: String,
}
semantic_error!(
    DisjointSubclass,
    "E015",
    "disjoint-subclass",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (self.sid.clone(), 2)
    },
);

pub(crate) struct DisjointClasses;

impl FormulaValidator for DisjointClasses {
    type Error = BoxedError;

    fn check(&self, cx: &Cx<'_>, root: SentenceId) -> Vec<BoxedError> {
        let Some(s) = cx.sentence(root) else {
            return Vec::new();
        };
        let [Element::Symbol(head), Element::Symbol(sym), Element::Symbol(class)] =
            s.elements.as_slice()
        else {
            return Vec::new();
        };
        let rel = cx.tax_role(head.id());
        if !matches!(rel, Some(TaxRelation::Instance | TaxRelation::Subclass)) {
            return Vec::new();
        }

        let own = superclasses(cx, class.id());
        let mut others = cx.binary_objects(head.id(), sym.id());
        others.sort_unstable_by_key(|&(_, sid)| sid);

        let mut out: Vec<BoxedError> = Vec::new();
        for (other, other_sid) in others {
            if other_sid == root || other == class.id() {
                continue;
            }
            let theirs = superclasses(cx, other);
            let Some((a, b, fact)) = first_conflict(cx, &own, &theirs) else {
                continue;
            };
            let sid = vec![root, other_sid, fact];
            let sym = cx.sym_name(sym.id());
            let (class1, class2) = (cx.sym_name(a), cx.sym_name(b));
            out.push(match rel {
                Some(TaxRelation::Instance) => Box::new(DisjointInstance {
                    sid,
                    sym,
                    class1,
                    class2,
                }),
                _ => Box::new(DisjointSubclass {
                    sid,
                    sym,
                    class1,
                    class2,
                }),
            });
        }
        out
    }
}

/// The first disjoint pair `(a, b, fact)` with `a` reachable only from `own`
/// and `b` only from `theirs`.
fn first_conflict(
    cx: &Cx<'_>,
    own: &[SymbolId],
    theirs: &[SymbolId],
) -> Option<(SymbolId, SymbolId, SentenceId)> {
    let own_set: HashSet<SymbolId> = own.iter().copied().collect();
    let their_set: HashSet<SymbolId> = theirs.iter().copied().collect();
    own.iter()
        .filter(|a| !their_set.contains(a))
        .find_map(|&a| {
            cx.disjoint_with(a)
                .iter()
                .find(|(b, _)| their_set.contains(b) && !own_set.contains(b))
                .map(|&(b, fact)| (a, b, fact))
        })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::semantics::validate::test_support::{codes_in, kif_layer, root_by_head_with};
    use crate::semantics::SemanticLayer;
    use crate::{Element, KnowledgeBase, SentenceId};

    const BASE: &str = "
        (subclass Organism Entity)
        (subclass Animal Organism)
        (subclass Plant Organism)
        (subclass Dog Animal)
        (subclass Tree Plant)
        (disjoint Animal Plant)
    ";

    #[test]
    fn e014_flags_both_conflicting_instance_edges() {
        let layer = kif_layer(&format!("{BASE}\n(instance Rex Dog)\n(instance Rex Tree)"));
        let dog = root_by_head_with(&layer, "instance", "Dog");
        let tree = root_by_head_with(&layer, "instance", "Tree");
        assert!(codes_in(&layer, dog).contains(&"E014"));
        assert!(codes_in(&layer, tree).contains(&"E014"));
    }

    #[test]
    fn e014_names_the_disjoint_pair_and_cites_the_evidence() {
        let layer = kif_layer(&format!("{BASE}\n(instance Rex Dog)\n(instance Rex Tree)"));
        let dog = root_by_head_with(&layer, "instance", "Dog");
        let tree = root_by_head_with(&layer, "instance", "Tree");
        let fact = *layer.syntactic.by_head("disjoint").first().unwrap();
        let errs = layer
            .validator_scoped(crate::semantics::types::Scope::Base)
            .validate_sentence_collect(dog);
        let e = errs.iter().find(|e| e.code() == "E014").unwrap();
        assert_eq!(
            e.to_string(),
            "'Rex' is an instance of disjoint classes (Animal and Plant)"
        );
        assert_eq!(e.anchors(), (vec![dog, tree, fact], 2));
    }

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
    fn e015_flags_subclass_of_disjoint_classes() {
        let layer = kif_layer(&format!(
            "{BASE}\n(subclass Triffid Dog)\n(subclass Triffid Tree)"
        ));
        let dog = edge(&layer, "subclass", "Triffid", "Dog");
        let tree = edge(&layer, "subclass", "Triffid", "Tree");
        assert!(codes_in(&layer, dog).contains(&"E015"));
        assert!(codes_in(&layer, tree).contains(&"E015"));
    }

    #[test]
    fn partition_members_conflict() {
        let layer = kif_layer(
            "
            (subclass Animal Entity)
            (subclass Plant Entity)
            (partition Organism Animal Plant)
            (instance Rex Animal)
            (instance Rex Plant)
        ",
        );
        let sid = root_by_head_with(&layer, "instance", "Animal");
        assert!(codes_in(&layer, sid).contains(&"E014"));
    }

    #[test]
    fn compatible_parents_are_not_flagged() {
        let layer = kif_layer(&format!(
            "{BASE}\n(subclass Pet Entity)\n(instance Rex Dog)\n(instance Rex Pet)\n(instance Rex Animal)"
        ));
        for sid in layer.syntactic.by_head("instance") {
            assert!(!codes_in(&layer, sid).contains(&"E014"));
        }
    }

    #[test]
    fn inherited_conflict_is_reported_only_where_branches_meet() {
        let layer = kif_layer(&format!(
            "{BASE}\n(subclass Triffid Dog)\n(subclass Triffid Tree)\n(subclass Seedling Triffid)"
        ));
        let seedling = edge(&layer, "subclass", "Seedling", "Triffid");
        assert!(!codes_in(&layer, seedling).contains(&"E015"));
    }

    #[test]
    fn instance_and_subclass_edges_are_not_compared() {
        let layer = kif_layer(&format!("{BASE}\n(instance Rex Dog)\n(subclass Rex Tree)"));
        for sid in layer.syntactic.root_sids() {
            let codes = codes_in(&layer, sid);
            assert!(!codes.contains(&"E014") && !codes.contains(&"E015"));
        }
    }

    #[test]
    fn later_disjoint_fact_invalidates_cached_validation() {
        let mut kb = KnowledgeBase::new();
        let promote = |kb: &mut KnowledgeBase, file: &str, kif: &str| {
            assert!(kb.reload_kif(kif, &PathBuf::from(file), file).ok);
            assert!(kb.make_session_axiomatic(file).is_ok());
        };
        let count = |kb: &KnowledgeBase| {
            kb.validate(crate::ValidationTarget::All, None)
                .iter()
                .filter(|d| d.code == "disjoint-instance")
                .count()
        };
        promote(
            &mut kb,
            "a.kif",
            "(subclass Animal Entity)\n(subclass Plant Entity)\n(instance Rex Animal)\n(instance Rex Plant)",
        );
        assert_eq!(count(&kb), 0);
        promote(&mut kb, "b.kif", "(disjoint Animal Plant)");
        assert_eq!(count(&kb), 2);
    }
}
