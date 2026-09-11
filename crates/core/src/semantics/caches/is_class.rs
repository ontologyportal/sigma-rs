//! `semantic::is_class` cache: memoises whether a symbol denotes a class.

use std::collections::HashSet;

use crate::cache::{CacheBehavior, EagerMapBehavior, EntryCache};
use crate::semantics::consts::CLASS_SYMBOL;
use crate::semantics::types::{Scope, Scoped, TaxRelation};
use crate::semantics::SemanticLayer;
use crate::SymbolId;

/// Behavior for the `semantic::is_class` cache.
///
/// A symbol is a class when every taxonomy parent is reached via a `subclass`
/// edge, or via an `instance` edge whose parent is `Class` or a subclass of it
/// (a symbol with no parents counts as a class).
#[derive(Debug, Default)]
pub(crate) struct IsClass;

impl CacheBehavior for IsClass {
    type Parent = SemanticLayer;
    type Key = Scoped<SymbolId>;
    type Value = bool;
    type Side = ();
    type SideSnapshot = ();
    type Tag = SymbolId;

    const NAME: &'static str = "semantic::is_class";
    const TAG_INDEXED: bool = true;

    /// `None` for Base-scope keys: their key is deterministic
    /// (`Scoped{Base, sym}`), so `react` reconstructs and evicts it directly
    /// rather than paying to index it. Session-scope keys are indexed so a
    /// batch invalidation by symbol doesn't need to scan the whole store.
    fn tag_of(key: &Scoped<SymbolId>) -> Option<SymbolId> {
        (key.scope != Scope::Base).then_some(key.key)
    }

    fn generate(
        &self,
        parent: &SemanticLayer,
        &Scoped { scope, key: sym }: &Scoped<SymbolId>,
    ) -> bool {
        parent
            .parents_of_scoped(sym, scope)
            .iter()
            .all(|(from, rel)| match rel {
                TaxRelation::Subclass => true,
                TaxRelation::Instance => subclass_of_class(parent, *from, scope),
                _ => false,
            })
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        &[crate::cache::events::EventKind::TaxonomyChanged]
    }

    fn reads(&self) -> &'static [&'static str] {
        &[
            crate::syntactic::caches::session::SessionCache::NAME,
            super::tax_edges::TaxEdges::NAME,
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

/// Whether `sym` is `Class` or reaches it through `subclass` edges alone.
fn subclass_of_class(layer: &SemanticLayer, sym: SymbolId, scope: Scope) -> bool {
    let class = CLASS_SYMBOL.id();
    let mut stack = vec![sym];
    let mut seen: HashSet<SymbolId> = HashSet::new();
    while let Some(n) = stack.pop() {
        if n == class {
            return true;
        }
        if !seen.insert(n) {
            continue;
        }
        for (m, rel) in layer.parents_of_scoped(n, scope) {
            if rel == TaxRelation::Subclass {
                stack.push(m);
            }
        }
    }
    false
}

impl SemanticLayer {
    /// Whether `sym` denotes a class (vs. an instance) in the `Base` taxonomy.
    pub(crate) fn is_class(&self, sym: SymbolId) -> bool {
        self.is_class_scoped(sym, Scope::Base)
    }

    /// `is_class` in an explicit [`Scope`] — reasons over `Base` ∪ the session
    /// overlay when `scope` is a session.
    pub(crate) fn is_class_scoped(&self, sym: SymbolId, scope: Scope) -> bool {
        let scope = self.closure_scope(scope);
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
    fn is_class_true_for_instance_of_class_subclass() {
        let layer = kif_layer(
            "
            (subclass SetOrClass Class)
            (instance Elephant SetOrClass)
            (instance Entity Class)
        ",
        );
        let elephant = layer.syntactic.sym_id("Elephant").unwrap();
        let entity = layer.syntactic.sym_id("Entity").unwrap();
        assert!(layer.is_class(elephant));
        assert!(layer.is_class(entity));
    }

    #[test]
    fn is_class_false_for_instance_of_a_class_instance() {
        let layer = kif_layer(
            "
            (subclass SetOrClass Class)
            (instance Dog SetOrClass)
            (instance Rex Dog)
            (instance Fido Animal)
        ",
        );
        let rex = layer.syntactic.sym_id("Rex").unwrap();
        let fido = layer.syntactic.sym_id("Fido").unwrap();
        assert!(!layer.is_class(rex));
        assert!(!layer.is_class(fido));
    }

    #[test]
    fn is_class_true_for_symbol_with_no_incoming_edges() {
        // Bar has no incoming edges at all — treated as a class (root symbol).
        let layer = kif_layer("(subclass Foo Bar)");
        let bar = layer.syntactic.sym_id("Bar").unwrap();
        assert!(layer.is_class(bar));
    }
}
