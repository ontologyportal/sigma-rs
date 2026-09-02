// crates/core/src/semantics/caches/is_instance.rs
//
// `semantic::is_instance` cache: memoises whether a symbol denotes an
// *instance* (as opposed to a *class*).

use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::types::{Scope, Scoped};
use crate::semantics::SemanticLayer;
use crate::SymbolId;

/// Behavior for the `semantic::is_instance` cache.
///
/// A symbol is an instance exactly when it is not a class, so `generate`
/// defers to `is_class` (itself cached).  The pair is mutually recursive at the
/// type level but not at runtime, so the default panic-on-cycle is correct.
#[derive(Debug, Default)]
pub(crate) struct IsInstance;

impl CacheBehavior for IsInstance {
    type Parent = SemanticLayer;
    type Key = Scoped<SymbolId>;
    type Value = bool;
    type Side = ();
    type SideSnapshot = ();
    type Tag = SymbolId;

    const NAME: &'static str = "semantic::is_instance";
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
        !parent.is_class_scoped(sym, scope)
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        &[crate::cache::events::EventKind::TaxonomyChanged]
    }

    fn reads(&self) -> &'static [&'static str] {
        &[super::is_class::IsClass::NAME]
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
    /// Whether `sym` denotes an instance (vs. a class) in the `Base` taxonomy.
    pub(crate) fn is_instance(&self, sym: SymbolId) -> bool {
        self.is_instance_scoped(sym, Scope::Base)
    }

    /// `is_instance` in an explicit [`Scope`].
    pub(crate) fn is_instance_scoped(&self, sym: SymbolId, scope: Scope) -> bool {
        // `is_instance` is `!is_class`, also a direct (parents-of-`sym`-only)
        // query, so the same fall-through-to-Base applies (refinement #2).
        let scope = self.direct_scope(sym, scope);
        self.is_instance.get(self, Scoped { scope, key: sym })
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::{base_layer, kif_layer};
    use crate::semantics::SemanticLayer;
    use crate::syntactic::SyntacticLayer;

    #[test]
    fn is_instance_true_when_declared() {
        let layer = base_layer();
        let sub = layer.syntactic.sym_id("subclass").unwrap();
        assert!(layer.is_instance(sub));
    }

    #[test]
    fn is_instance_false_for_pure_subclass_target() {
        let layer = base_layer();
        let human = layer.syntactic.sym_id("Human").unwrap();
        assert!(!layer.is_instance(human));
    }

    #[test]
    fn is_instance_false_for_root_class() {
        let layer = base_layer();
        let entity = layer.syntactic.sym_id("Entity").unwrap();
        assert!(!layer.is_instance(entity));
    }

    #[test]
    fn is_instance_true_via_subrelation_chain() {
        // If `ancestorOf` is declared as an instance and `parentOf` is a
        // subrelation of `ancestorOf`, the algorithm walks the Subrelation edge
        // upward and finds the Instance edge — so `parentOf` is also an instance.
        let layer = kif_layer(
            "
            (instance ancestorOf BinaryRelation)
            (subrelation parentOf ancestorOf)
        ",
        );
        let parent_of = layer.syntactic.sym_id("parentOf").unwrap();
        let ancestor_of = layer.syntactic.sym_id("ancestorOf").unwrap();
        assert!(
            layer.is_instance(ancestor_of),
            "direct instance declaration"
        );
        assert!(
            layer.is_instance(parent_of),
            "parentOf inherits is_instance via subrelation chain to ancestorOf"
        );
    }

    #[test]
    fn is_instance_cached_on_second_call() {
        let mut syn = SyntacticLayer::default();
        syn.load_kif("(instance Fido Dog)", "t");
        let layer = SemanticLayer::new(syn);
        let fido = layer.syntactic.sym_id("Fido").unwrap();
        let v1 = layer.is_instance(fido);
        assert!(v1);
        assert!(
            layer
                .is_instance
                .peek(&crate::semantics::types::Scoped {
                    scope: crate::semantics::types::Scope::Base,
                    key: fido
                })
                .is_some(),
            "is_instance cache should be populated after first call"
        );
        assert_eq!(layer.is_instance(fido), v1);
    }
}
