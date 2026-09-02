//! `semantic::is_function` cache: memoises whether a symbol is a function.

use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::consts::FUNCTION_CLASS;
use crate::semantics::types::{Scope, Scoped};
use crate::semantics::SemanticLayer;
use crate::SymbolId;

/// Behavior for the `semantic::is_function` cache.
#[derive(Debug, Default)]
pub(crate) struct IsFunction;

impl CacheBehavior for IsFunction {
    type Parent = SemanticLayer;
    type Key = Scoped<SymbolId>;
    type Value = bool;
    type Side = ();
    type SideSnapshot = ();
    type Tag = SymbolId;

    const NAME: &'static str = "semantic::is_function";
    const TAG_INDEXED: bool = true;

    /// Same Base-vs-session split as `is_class`'s `tag_of`.
    fn tag_of(key: &Scoped<SymbolId>) -> Option<SymbolId> {
        (key.scope != Scope::Base).then_some(key.key)
    }

    fn generate(
        &self,
        parent: &SemanticLayer,
        &Scoped { scope, key: sym }: &Scoped<SymbolId>,
    ) -> bool {
        parent.is_instance_scoped(sym, scope)
            && parent.has_ancestor_scoped(sym, FUNCTION_CLASS.id(), scope)
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        &[crate::cache::events::EventKind::TaxonomyChanged]
    }

    fn reads(&self) -> &'static [&'static str] {
        &[
            super::is_instance::IsInstance::NAME,
            super::has_ancestor::HasAncestor::NAME,
        ]
    }

    fn react(
        &self,
        _parent: &SemanticLayer,
        events: &[&crate::cache::events::Event],
        store: &EntryCache<Scoped<SymbolId>, bool, SymbolId>,
        _side: &Self::Side,
    ) -> Vec<crate::cache::events::Event> {
        use crate::cache::events::Event;
        for event in events.iter() {
            if let Event::TaxonomyChanged { syms } = event {
                let base_keys: Vec<_> = syms
                    .iter()
                    .map(|&sym| Scoped {
                        scope: Scope::Base,
                        key: sym,
                    })
                    .collect();
                store.evict_keys(&base_keys);
                store.evict_by_tag(syms);
            }
        }
        Vec::new()
    }
}

impl SemanticLayer {
    /// Whether `sym` denotes a function in the `Base` taxonomy.
    pub(crate) fn is_function(&self, sym: SymbolId) -> bool {
        self.is_function_scoped(sym, Scope::Base)
    }

    /// `is_function` in an explicit [`Scope`].
    pub(crate) fn is_function_scoped(&self, sym: SymbolId, scope: Scope) -> bool {
        let scope = self.closure_scope(scope);
        self.is_function.get(self, Scoped { scope, key: sym })
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::{base_layer, kif_layer};

    #[test]
    fn is_function_true() {
        let layer = kif_layer(
            "
            (subclass Relation Entity)
            (subclass BinaryRelation Relation)
            (subclass Function Relation)
            (subclass UnaryFunction Function)
            (subclass UnaryFunction BinaryRelation)
            (instance successor UnaryFunction)
        ",
        );
        let successor = layer.syntactic.sym_id("successor").unwrap();
        assert!(layer.is_function(successor));
    }

    #[test]
    fn is_function_false_for_predicate() {
        let layer = base_layer();
        let inst = layer.syntactic.sym_id("instance").unwrap();
        assert!(!layer.is_function(inst));
    }
}
