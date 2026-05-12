//! `semantic::is_class` cache: memoises whether a symbol denotes a class.

use crate::SymbolId;
use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::SemanticLayer;
use crate::semantics::types::{Scope, Scoped, TaxRelation};

/// Behavior for the `semantic::is_class` cache.
///
/// A symbol is a class when all of its taxonomy parents are reached via
/// `subclass` edges (a symbol with no parents counts as a class).
#[derive(Debug, Default)]
pub(crate) struct IsClass;

impl CacheBehavior for IsClass {
    type Parent = SemanticLayer;
    type Key    = Scoped<SymbolId>;
    type Value  = bool;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "semantic::is_class";

    fn generate(&self, parent: &SemanticLayer, &Scoped { scope, key: sym }: &Scoped<SymbolId>) -> bool {
        parent.parents_of_scoped(sym, scope).iter().all(|(_, rel)| *rel == TaxRelation::Subclass)
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        &[crate::cache::events::EventKind::TaxonomyChanged]
    }

    fn reads(&self) -> &'static [&'static str] {
        &["semantic::tax_edges", "syntactic::sessions"]
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
    /// Whether `sym` denotes a class (vs. an instance) in the `Base` taxonomy.
    pub(crate) fn is_class(&self, sym: SymbolId) -> bool {
        self.is_class_scoped(sym, Scope::Base)
    }

    /// `is_class` in an explicit [`Scope`] — reasons over `Base` ∪ the session
    /// overlay when `scope` is a session.
    pub(crate) fn is_class_scoped(&self, sym: SymbolId, scope: Scope) -> bool {
        let scope = self.direct_scope(sym, scope);
        self.is_class.get(self, Scoped { scope, key: sym })
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::{base_layer, kif_layer};

    #[test]
    fn is_class_true_for_subclass_only_target() {
        let layer = base_layer();
        let animal = layer.syntactic.sym_id("Animal").unwrap();
        assert!(layer.is_class(animal));
    }

    #[test]
    fn is_class_false_when_has_instance_edge() {
        let layer = base_layer();
        let sub = layer.syntactic.sym_id("subclass").unwrap();
        assert!(!layer.is_class(sub));
    }

    #[test]
    fn is_class_true_for_symbol_with_no_incoming_edges() {
        // Bar has no incoming edges at all — treated as a class (root symbol).
        let layer = kif_layer("(subclass Foo Bar)");
        let bar = layer.syntactic.sym_id("Bar").unwrap();
        assert!(layer.is_class(bar));
    }
}
