//! `semantic::has_ancestor` cache: memoises whether `ancestor` lies anywhere in
//! `sym`'s taxonomy chain.

use crate::SymbolId;
use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::SemanticLayer;
use crate::semantics::types::{Scope, Scoped};

impl SemanticLayer {
    /// Whether `sym` has `ancestor` anywhere in its `Base` taxonomy chain.
    pub(crate) fn has_ancestor(&self, sym: SymbolId, ancestor: SymbolId) -> bool {
        self.has_ancestor_scoped(sym, ancestor, Scope::Base)
    }

    /// `has_ancestor` in an explicit [`Scope`] — reasons over `Base` ∪ the
    /// session overlay when `scope` is a session.
    pub(crate) fn has_ancestor_scoped(&self, sym: SymbolId, ancestor: SymbolId, scope: Scope) -> bool {
        if sym == ancestor { return true; }
        let scope = self.closure_scope(scope);
        self.has_ancestor.get(self, Scoped { scope, key: (sym, ancestor) })
    }
}

/// Behavior for the `semantic::has_ancestor` cache.
///
/// Keyed by `(sym, ancestor)`. `on_cycle` returns `false` so a malformed
/// taxonomy cycle (`(subclass A B)(subclass B A)`) terminates rather than
/// recursing forever.
#[derive(Debug, Default)]
pub(crate) struct HasAncestor;

impl CacheBehavior for HasAncestor {
    type Parent = SemanticLayer;
    type Key    = Scoped<(SymbolId, SymbolId)>;
    type Value  = bool;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "semantic::has_ancestor";

    fn generate(&self, parent: &SemanticLayer, &Scoped { scope, key: (sym, ancestor) }: &Scoped<(SymbolId, SymbolId)>) -> bool {
        if sym == ancestor { return true; }
        parent.parents_of_scoped(sym, scope).into_iter().any(|(from, _)| parent.has_ancestor_scoped(from, ancestor, scope))
    }

    fn on_cycle(&self, _parent: &SemanticLayer, _key: &Scoped<(SymbolId, SymbolId)>) -> bool {
        false
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
        store:   &EntryCache<Scoped<(SymbolId, SymbolId)>, bool>,
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
    fn has_ancestor_true_for_same_symbol() {
        let layer = base_layer();
        let human = layer.syntactic.sym_id("Human").unwrap();
        assert!(layer.has_ancestor(human, human),
            "every symbol is its own ancestor (short-circuit)");
    }

    #[test]
    fn has_ancestor_false_for_sibling() {
        let layer = kif_layer("
            (subclass Dog Animal)
            (subclass Cat Animal)
        ");
        let dog = layer.syntactic.sym_id("Dog").unwrap();
        let cat = layer.syntactic.sym_id("Cat").unwrap();
        assert!(!layer.has_ancestor(dog, cat));
    }
}
