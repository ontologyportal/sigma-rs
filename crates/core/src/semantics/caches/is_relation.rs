//! `semantic::is_relation` cache: memoises whether a symbol is a relation
//! (predicate or function).

use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::consts::RELATION_CLASS;
use crate::semantics::types::{Scope, Scoped};
use crate::semantics::SemanticLayer;
use crate::SymbolId;

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
    type Key = Scoped<SymbolId>;
    type Value = bool;
    type Side = ();
    type SideSnapshot = ();
    type Tag = SymbolId;

    const NAME: &'static str = "semantic::is_relation";
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
            && parent.has_ancestor_scoped(sym, RELATION_CLASS.id(), scope)
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
        let layer = kif_layer(
            "
            (instance Fido Dog)
            (subclass Dog Animal)
        ",
        );
        let fido = layer.syntactic.sym_id("Fido").unwrap();
        assert!(!layer.is_relation(fido));
    }
}
