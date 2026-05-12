// crates/core/src/semantics/caches/test_support.rs
//
// Shared fixtures for the per-cache test modules.  Each cache's tests live in
// its own file (`is_instance.rs`, `arity.rs`, …) and pull `BASE` / `kif_layer`
// / `base_layer` from here so the fixture is defined once.

use crate::semantics::SemanticLayer;
use crate::syntactic::SyntacticLayer;

/// A small SUMO-shaped ontology covering relations, predicates, and a couple of
/// class hierarchies — enough to exercise the IS-A / relation-metadata caches.
pub(crate) const BASE: &str = "
    (subclass Relation Entity)
    (subclass BinaryRelation Relation)
    (subclass Predicate Relation)
    (subclass BinaryPredicate Predicate)
    (subclass BinaryPredicate BinaryRelation)
    (instance subclass BinaryRelation)
    (domain subclass 1 Class)
    (domain subclass 2 Class)
    (instance instance BinaryPredicate)
    (domain instance 1 Entity)
    (domain instance 2 Class)
    (subclass Animal Entity)
    (subclass Human Entity)
    (subclass Human Animal)
";

/// Build a `SemanticLayer` from a KIF string (file tag `base`).
pub(crate) fn kif_layer(kif_str: &str) -> SemanticLayer {
    let mut store = SyntacticLayer::default();
    store.load_kif(kif_str, "base");
    SemanticLayer::new(store)
}

/// A `SemanticLayer` over [`BASE`].
pub(crate) fn base_layer() -> SemanticLayer { kif_layer(BASE) }
