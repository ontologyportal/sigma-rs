//! `semantic::is_predicate` cache: memoises whether a symbol is a predicate.

use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::consts::PREDICATE_CLASS;
use crate::semantics::types::{Scope, Scoped};
use crate::semantics::SemanticLayer;
use crate::SymbolId;

/// Behavior for the `semantic::is_predicate` cache.
#[derive(Debug, Default)]
pub(crate) struct IsPredicate;

impl CacheBehavior for IsPredicate {
    type Parent = SemanticLayer;
    type Key = Scoped<SymbolId>;
    type Value = bool;
    type Side = ();
    type SideSnapshot = ();
    type Tag = SymbolId;

    const NAME: &'static str = "semantic::is_predicate";
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
            && parent.has_ancestor_scoped(sym, PREDICATE_CLASS.id(), scope)
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
            match event {
                Event::TaxonomyChanged { syms } => {
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
                _ => continue,
            }
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
