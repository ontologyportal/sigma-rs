//! `semantic::has_ancestor` cache: memoises whether `ancestor` lies anywhere in
//! `sym`'s taxonomy chain.

use crate::cache::{CacheBehavior, EagerMapBehavior, EntryCache};
use crate::semantics::types::{Scope, Scoped};
use crate::semantics::SemanticLayer;
use crate::SymbolId;

impl SemanticLayer {
    /// Whether `sym` has `ancestor` anywhere in its `Base` taxonomy chain.
    pub(crate) fn has_ancestor(&self, sym: SymbolId, ancestor: SymbolId) -> bool {
        self.has_ancestor_scoped(sym, ancestor, Scope::Base)
    }

    /// `has_ancestor` in an explicit [`Scope`] — reasons over `Base` ∪ the
    /// session overlay when `scope` is a session.
    pub(crate) fn has_ancestor_scoped(
        &self,
        sym: SymbolId,
        ancestor: SymbolId,
        scope: Scope,
    ) -> bool {
        if sym == ancestor {
            return true;
        }
        let scope = self.closure_scope(scope);
        self.has_ancestor.get(
            self,
            Scoped {
                scope,
                key: (sym, ancestor),
            },
        )
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
    type Key = Scoped<(SymbolId, SymbolId)>;
    type Value = bool;
    type Side = ();
    type SideSnapshot = ();
    type Tag = SymbolId;

    const NAME: &'static str = "semantic::has_ancestor";
    const TAG_INDEXED: bool = true;

    /// Unlike `is_class`/`is_instance`/etc (one entry per `sym` per scope, so
    /// a Base-scope entry's key is fully deterministic and cheap to
    /// reconstruct), `has_ancestor`'s key also varies over `ancestor` -- a
    /// single `sym` can have arbitrarily many cached `(sym, ancestor)` pairs
    /// even within Base scope. So every entry is indexed, Base included;
    /// there's no cheaper direct-reconstruction path here.
    fn tag_of(key: &Scoped<(SymbolId, SymbolId)>) -> Option<SymbolId> {
        Some(key.key.0)
    }

    fn generate(
        &self,
        parent: &SemanticLayer,
        &Scoped {
            scope,
            key: (sym, ancestor),
        }: &Scoped<(SymbolId, SymbolId)>,
    ) -> bool {
        if sym == ancestor {
            return true;
        }
        parent
            .parents_of_scoped(sym, scope)
            .into_iter()
            .any(|(from, _)| parent.has_ancestor_scoped(from, ancestor, scope))
    }

    fn on_cycle(&self, _parent: &SemanticLayer, _key: &Scoped<(SymbolId, SymbolId)>) -> bool {
        false
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        &[crate::cache::events::EventKind::TaxonomyChanged]
    }

    fn reads(&self) -> &'static [&'static str] {
        &[
            super::tax_edges::TaxEdges::NAME,
            crate::syntactic::caches::session::SessionCache::NAME,
        ]
    }

    fn react(
        &self,
        _parent: &SemanticLayer,
        events: &[&crate::cache::events::Event],
        store: &EntryCache<Scoped<(SymbolId, SymbolId)>, bool, SymbolId>,
        _side: &Self::Side,
    ) -> Vec<crate::cache::events::Event> {
        use crate::cache::events::Event;
        for event in events.iter() {
            if let Event::TaxonomyChanged { syms } = event {
                store.evict_by_tag(syms);
            }
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
        assert!(
            layer.has_ancestor(human, human),
            "every symbol is its own ancestor (short-circuit)"
        );
    }

    #[test]
    fn has_ancestor_true_for_chain() {
        let layer = kif_layer(
            "
            (subclass Dog Animal)
            (instance Rex Dog)
        ",
        );
        let rex = layer.syntactic.sym_id("Rex").unwrap();
        let animal = layer.syntactic.sym_id("Animal").unwrap();
        assert!(layer.has_ancestor(rex, animal));
    }

    #[test]
    fn has_ancestor_false_for_sibling() {
        let layer = kif_layer(
            "
            (subclass Dog Animal)
            (subclass Cat Animal)
        ",
        );
        let dog = layer.syntactic.sym_id("Dog").unwrap();
        let cat = layer.syntactic.sym_id("Cat").unwrap();
        assert!(!layer.has_ancestor(dog, cat));
    }
}
