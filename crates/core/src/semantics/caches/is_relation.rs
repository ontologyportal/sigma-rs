//! `semantic::is_relation` cache: memoises whether a symbol is a relation
//! (predicate or function).

use crate::SymbolId;
use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::SemanticLayer;
use crate::semantics::consts::RELATION_CLASS;
use crate::semantics::types::{Scope, Scoped};

impl SemanticLayer {
    /// Whether `sym` denotes a relation (function or predicate) in the `Base`
    /// taxonomy.
    pub(crate) fn is_relation(&self, sym: SymbolId) -> bool {
        self.is_relation_scoped(sym, Scope::Base)
    }

    /// `is_relation` in an explicit [`Scope`].
    pub(crate) fn is_relation_scoped(&self, sym: SymbolId, scope: Scope) -> bool {
        let scope = self.closure_scope(scope);
        self.is_relation.get(self, Scoped { scope, key: sym })
    }
}

/// Behavior for the `semantic::is_relation` cache.
#[derive(Debug, Default)]
pub(crate) struct IsRelation;

impl CacheBehavior for IsRelation {
    type Parent = SemanticLayer;
    type Key    = Scoped<SymbolId>;
    type Value  = bool;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "semantic::is_relation";

    fn generate(&self, parent: &SemanticLayer, &Scoped { scope, key: sym }: &Scoped<SymbolId>) -> bool {
        parent.is_instance_scoped(sym, scope) && parent.has_ancestor_scoped(sym, RELATION_CLASS.id(), scope)
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

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::{base_layer, kif_layer};

    #[test]
    fn is_relation_true_for_declared_relation() {
        let layer = base_layer();
        let sub = layer.syntactic.sym_id("subclass").unwrap();
        assert!(layer.is_relation(sub));
    }

    #[test]
    fn is_relation_false_for_class_symbol() {
        let layer = base_layer();
        let entity = layer.syntactic.sym_id("Entity").unwrap();
        assert!(!layer.is_relation(entity));
    }

    #[test]
    fn is_relation_false_when_no_relation_ancestor() {
        let layer = kif_layer("
            (instance Fido Dog)
            (subclass Dog Animal)
        ");
        let fido = layer.syntactic.sym_id("Fido").unwrap();
        assert!(!layer.is_relation(fido));
    }
}
