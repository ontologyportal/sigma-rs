//! `semantic::transitive_relations` - the relations the KB declares
//! transitive: every instance of `TransitiveRelation` or of a class below it
//! (`Base` scope).  Subrelations of a transitive relation are deliberately not
//! included: transitivity does not inherit through `subrelation`.
//!
//! Eager, because its consumers (the SInE index) need to be told when the set
//! changes: it recomputes on `TaxonomyChanged` and emits the difference as
//! `TransitiveRelationsChanged`.

use std::collections::HashSet;
use std::sync::Arc;

use crate::cache::events::{Event, EventKind};
use crate::cache::{EagerBehavior, EagerIndex, EagerMapBehavior};
use crate::semantics::caches::tax_edges::TaxEdges;
use crate::semantics::consts::TRANSITIVE_RELATION_CLASS;
use crate::semantics::taxonomy::TaxRelation;
use crate::semantics::SemanticLayer;
use crate::types::SymbolId;

/// Behavior for the `semantic::transitive_relations` set.
#[derive(Debug, Default)]
pub(crate) struct TransitiveRelations;

fn compute(parent: &SemanticLayer) -> HashSet<SymbolId> {
    let base = TRANSITIVE_RELATION_CLASS.id();
    let mut classes = vec![base];
    parent.walk_subclass_closure(base, false, |class| {
        classes.push(class);
        true
    });
    classes
        .into_iter()
        .flat_map(|class| parent.children_of(class))
        .filter(|(_, rel)| matches!(rel, TaxRelation::Instance))
        .map(|(relation, _)| relation)
        .collect()
}

impl EagerBehavior for TransitiveRelations {
    type Parent = SemanticLayer;
    type Value = Arc<HashSet<SymbolId>>;

    const NAME: &'static str = "semantic::transitive_relations";

    fn initial(&self) -> Self::Value {
        Arc::default()
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::TaxonomyChanged]
    }

    fn produces(&self) -> &'static [EventKind] {
        &[EventKind::TransitiveRelationsChanged]
    }

    fn reads(&self) -> &'static [&'static str] {
        &[TaxEdges::NAME]
    }

    fn initialize(&self, parent: &SemanticLayer, store: &EagerIndex<Self::Value>) {
        if store.with_ref(|set| set.is_empty()) {
            store.install(Arc::new(compute(parent)));
        }
    }

    fn react(
        &self,
        parent: &SemanticLayer,
        events: &[&Event],
        store: &EagerIndex<Self::Value>,
    ) -> Vec<Event> {
        if !events
            .iter()
            .any(|e| matches!(e, Event::TaxonomyChanged { .. }))
        {
            return Vec::new();
        }
        let new = Arc::new(compute(parent));
        let old = store.update_with(|set| std::mem::replace(set, Arc::clone(&new)));
        let mut added: Vec<SymbolId> = new.difference(&old).copied().collect();
        let mut removed: Vec<SymbolId> = old.difference(&new).copied().collect();
        if added.is_empty() && removed.is_empty() {
            return Vec::new();
        }
        added.sort_unstable();
        removed.sort_unstable();
        vec![Event::TransitiveRelationsChanged { added, removed }]
    }
}

impl SemanticLayer {
    /// The relations the KB declares transitive.
    #[cfg(test)]
    pub(crate) fn transitive_relations(&self) -> Arc<HashSet<SymbolId>> {
        self.transitive_relations.with_ref(Arc::clone)
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::kif_layer;

    fn names(layer: &crate::semantics::SemanticLayer) -> Vec<String> {
        let mut out: Vec<String> = layer
            .transitive_relations()
            .iter()
            .map(|&id| layer.syntactic.sym_name(id).unwrap().name().to_string())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn direct_and_inherited_instances_are_transitive() {
        let layer = kif_layer(
            "(instance part TransitiveRelation)\n\
             (subclass PartialOrderingRelation TransitiveRelation)\n\
             (instance lessThan PartialOrderingRelation)\n\
             (instance color BinaryPredicate)",
        );
        assert_eq!(names(&layer), ["lessThan", "part"]);
    }

    #[test]
    fn subrelations_of_a_transitive_relation_are_not_transitive() {
        let layer = kif_layer(
            "(instance ancestor TransitiveRelation)\n\
             (subrelation parent ancestor)",
        );
        assert_eq!(names(&layer), ["ancestor"]);
    }

    #[test]
    fn empty_without_declarations() {
        assert!(names(&kif_layer("(subclass Dog Animal)")).is_empty());
    }
}
