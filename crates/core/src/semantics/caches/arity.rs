//! `semantic::arity` cache: memoises a relation's arity (None for non-relations).

use crate::SymbolId;
use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::SemanticLayer;
use crate::semantics::consts::ARITY;

/// Behavior for the `semantic::arity` cache: a relation's arity, or `None` for
/// non-relations.  Walks the arity-bearing ancestor classes (`ARITY`),
/// adjusting by one for functions (whose last argument is the result).
#[derive(Debug, Default)]
pub(crate) struct Arity;

impl CacheBehavior for Arity {
    type Parent = SemanticLayer;
    type Key    = SymbolId;
    type Value  = Option<i32>;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "semantic::arity";

    fn generate(&self, parent: &SemanticLayer, &sym: &SymbolId) -> Option<i32> {
        if !parent.is_relation(sym) { return None; }
        for &(class, n) in ARITY {
            if parent.has_ancestor_by_name(sym, class) {
                return Some(if n > 0 && parent.is_function(sym) { n - 1 } else { n });
            }
        }
        None
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        &[crate::cache::events::EventKind::TaxonomyChanged, crate::cache::events::EventKind::OtherRootsChanged]
    }

    fn reads(&self) -> &'static [&'static str] {
        &["semantic::is_relation", "semantic::is_function", "semantic::has_ancestor"]
    }

    fn react(
        &self,
        _parent: &SemanticLayer,
        events:  &[&crate::cache::events::Event],
        store:   &EntryCache<SymbolId, Option<i32>>,
        _side:   &Self::Side,
    ) -> Vec<crate::cache::events::Event> {
        use crate::cache::events::Event;
        if events.iter().any(|e| matches!(e, Event::TaxonomyChanged { .. } | Event::OtherRootsChanged { .. })) {
            store.clear();
        }
        Vec::new()
    }
}

impl SemanticLayer {
    /// The arity of `sym` if it is a relation, else `None`.
    pub(crate) fn arity(&self, sym: SymbolId) -> Option<i32> {
        self.arity.get(self, sym)
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::kif_layer;

    #[test]
    fn arity_binary_relation_is_2() {
        let layer = kif_layer("
            (subclass Relation Entity)
            (subclass BinaryRelation Relation)
            (instance likes BinaryRelation)
        ");
        let likes = layer.syntactic.sym_id("likes").unwrap();
        assert_eq!(layer.arity(likes), Some(2));
    }

    #[test]
    fn arity_ternary_relation_is_3() {
        let layer = kif_layer("
            (subclass Relation Entity)
            (subclass TernaryRelation Relation)
            (instance between TernaryRelation)
        ");
        let between = layer.syntactic.sym_id("between").unwrap();
        assert_eq!(layer.arity(between), Some(3));
    }

    #[test]
    fn arity_unary_function_is_1() {
        // UnaryFunction is a subclass of both Function and BinaryRelation (n=2).
        // Because it is a Function the adjustment applies: arity = 2 - 1 = 1.
        let layer = kif_layer("
            (subclass Relation Entity)
            (subclass BinaryRelation Relation)
            (subclass Function Relation)
            (subclass UnaryFunction Function)
            (subclass UnaryFunction BinaryRelation)
            (instance successor UnaryFunction)
        ");
        let successor = layer.syntactic.sym_id("successor").unwrap();
        assert_eq!(layer.arity(successor), Some(1));
    }

    #[test]
    fn arity_none_for_non_relation() {
        let layer = kif_layer("(subclass Dog Animal)");
        let dog = layer.syntactic.sym_id("Dog").unwrap();
        assert_eq!(layer.arity(dog), None);
    }
}
