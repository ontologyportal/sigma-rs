// crates/core/src/trans/caches/numeric_ancestor_set.rs
//
// `translation::numeric_ancestor_set` — all SUMO class SymbolIds that are
// ancestors (superclasses) of the three numeric roots.  Installed by
// `prime_caches`; can also build lazily via `generate`.  Read via `with_ref`.

use std::collections::HashSet;

use crate::cache::events::{Event, EventKind};
use crate::semantics::caches::tax_edges::TaxEdges;
use crate::SymbolId;
use crate::cache::{EagerMapBehavior, WholeCacheBehavior};
use crate::semantics::SemanticLayer;
use crate::trans::TranslationLayer;
use crate::trans::caches::numeric_sorts::NumericSorts;

/// Behavior for the `translation::numeric_ancestor_set` cache.
#[derive(Debug, Default)]
pub(crate) struct NumericAncestorSet;

impl WholeCacheBehavior for NumericAncestorSet {
    type Parent = TranslationLayer;
    type Value  = HashSet<SymbolId>;

    const NAME: &'static str = "translation::numeric_ancestor_set";

    fn generate(&self, parent: &TranslationLayer) -> HashSet<SymbolId> {
        // The union of the strict superclasses of every numeric-sorted class.
        let mut ancestors = HashSet::new();
        parent.numeric_sorts.for_each(|(sym_id, _)| {
            collect_ancestors(&parent.semantic, *sym_id, &mut ancestors);
        });
        ancestors
    }

    // The ancestors of every numeric-sorted class: iterates `numeric_sorts` and
    // walks each one's superclasses over the taxonomy adjacency (`tax_edges`).
    fn reads(&self) -> &'static [&'static str] {
        &[NumericSorts::NAME, TaxEdges::NAME]
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        &[EventKind::NumericSortAdded, EventKind::NumericSortRemoved]
    }

    /// Targeted maintenance of the (whole-value) ancestor set.  Runs only when
    /// the set is already populated (primed at construction / on first read); a
    /// cold cache stays cold and rebuilds via `generate` on the next read.
    ///
    /// * `NumericSortAdded(x)` → union in `x`'s strict superclasses (monotone).
    /// * `NumericSortRemoved(x)` → its superclasses *may* drop, but only when no
    ///   other numeric class still descends from them — so each candidate is
    ///   re-justified against the (already-updated) `numeric_sorts` and the
    ///   unjustified ones removed.  This is the only path that shrinks the set.
    fn react(
        &self,
        parent: &Self::Parent,
        events: &[&crate::cache::events::Event],
        store:  &crate::cache::LayerCache<Self::Value>,
    ) -> Vec<crate::cache::events::Event>
    {
        if !store.is_populated() {
            return Vec::new(); // cold → `generate` rebuilds the full set on next read
        }
        let mut set = store.snapshot().unwrap_or_default();

        let mut removed: Vec<SymbolId> = Vec::new();
        for ev in events {
            match ev {
                Event::NumericSortAdded(x)   => collect_ancestors(&parent.semantic, *x, &mut set),
                Event::NumericSortRemoved(x) => removed.push(*x),
                _ => {}
            }
        }

        if !removed.is_empty() {
            // A removed class's superclasses survive iff some numeric class still
            // sits below them; recheck just those candidates.
            let mut candidates: HashSet<SymbolId> = HashSet::new();
            for x in &removed {
                collect_ancestors(&parent.semantic, *x, &mut candidates);
            }
            for a in candidates {
                if set.contains(&a) && !still_numeric_below(parent, a) {
                    set.remove(&a);
                }
            }
        }

        store.install(set);
        Vec::new()
    }
}

/// Insert every strict superclass of `sym` (subclass-transitive) into `out`.
/// `sym` itself is not inserted.
fn collect_ancestors(semantic: &SemanticLayer, sym: SymbolId, out: &mut HashSet<SymbolId>) {
    semantic.walk_subclass_closure(sym, /*up*/ true, |p| {
        out.insert(p);
        true
    });
}

/// `true` iff some strict subclass-descendant of `a` is currently numeric-sorted
/// — i.e. `a` is still justified as a numeric ancestor.
fn still_numeric_below(parent: &TranslationLayer, a: SymbolId) -> bool {
    let mut found = false;
    parent.semantic.walk_subclass_closure(a, /*up*/ false, |n| {
        found = parent.numeric_sorts.get(&n).is_some();
        !found
    });
    found
}