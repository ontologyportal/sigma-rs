//! `semantic::is_predicate` cache: memoises whether a symbol is a predicate.

use crate::SymbolId;
use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::SemanticLayer;
use crate::semantics::consts::PREDICATE_CLASS;
use crate::semantics::types::{Scope, Scoped};

/// Behavior for the `semantic::is_predicate` cache.
#[derive(Debug, Default)]
pub(crate) struct IsPredicate;

impl CacheBehavior for IsPredicate {
    type Parent = SemanticLayer;
    type Key    = Scoped<SymbolId>;
    type Value  = bool;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "semantic::is_predicate";

    fn generate(&self, parent: &SemanticLayer, &Scoped { scope, key: sym }: &Scoped<SymbolId>) -> bool {
        parent.is_instance_scoped(sym, scope) && parent.has_ancestor_scoped(sym, PREDICATE_CLASS.id(), scope)
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        &[crate::cache::events::EventKind::TaxonomyChanged]
    }

    fn reads(&self) -> &'static [&'static str] {
        &["semantic::is_instance", "semantic::has_ancestor"]
    }

    fn react(
        &self,
        _parent: &SemanticLayer,
        events:  &[&crate::cache::events::Event],
        store:   &EntryCache<Scoped<SymbolId>, bool>,
        _side:   &Self::Side,
    ) -> Vec<crate::cache::events::Event> {
        use crate::cache::events::Event;
        if events.iter().any(|e| matches!(e, Event::TaxonomyChanged { .. })) {
            store.clear();
        }
        Vec::new()
    }
}

impl SemanticLayer {
    /// Whether `sym` denotes a predicate in the `Base` taxonomy.
    pub(crate) fn is_predicate(&self, sym: SymbolId) -> bool {
        self.is_predicate_scoped(sym, Scope::Base)
    }

    /// `is_predicate` in an explicit [`Scope`].
    pub(crate) fn is_predicate_scoped(&self, sym: SymbolId, scope: Scope) -> bool {
        let scope = self.closure_scope(scope);
        self.is_predicate.get(self, Scoped { scope, key: sym })
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::base_layer;

    #[test]
    fn is_predicate_true() {
        let layer = base_layer();
        let inst = layer.syntactic.sym_id("instance").unwrap();
        assert!(layer.is_predicate(inst));
    }

    #[test]
    fn is_predicate_false_for_relation_without_predicate_ancestor() {
        // `subclass` is a BinaryRelation but has no path to Predicate in base.
        let layer = base_layer();
        let sub = layer.syntactic.sym_id("subclass").unwrap();
        assert!(!layer.is_predicate(sub));
    }
}
