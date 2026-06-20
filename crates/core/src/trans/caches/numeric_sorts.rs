//! `translation::numeric_sorts` — `SymbolId -> Sort` for every known SUMO
//! numeric class.  Eagerly built and reacts to Taxonomy changes.

use std::collections::{HashMap, HashSet};

use crate::semantics::caches::tax_edges::TaxEdges;
use crate::{SymbolId, TaxRelation};
use crate::cache::{EagerMapBehavior, EntryCache};
use crate::cache::events::{Event, EventKind};
use crate::semantics::SemanticLayer;
use crate::trans::{Sort, TranslationLayer};

// -- Numeric classes ---------------------------------------------------------

/// SUMO `Integer` class name.
pub(crate) const INTEGER_CLASS:     &str = env!("SUMO_INTEGER_CLASS");
/// SUMO `RationalNumber` class name.
pub(crate) const RATIONAL_CLASS:    &str = env!("SUMO_RATIONAL_CLASS");
/// SUMO `RealNumber` class name.
pub(crate) const REAL_CLASS:        &str = env!("SUMO_REAL_CLASS");
/// The abstract `Number` superclass: not numeric-sorted itself (its class
/// object stays `$i`), but a *value* classified or declared at `Number` types
/// is sorted `$real`.
pub(crate) const NUMBER_CLASS:      &str = env!("SUMO_NUMBER_CLASS");

/// Numeric roots ordered least-specific → most-specific, so that Integer
/// overwrites Rational which overwrites Real when a class descends from
/// multiple roots.
pub(crate) const NUMERIC_ROOTS: &[(&str, Sort)] = &[
    (REAL_CLASS,     Sort::Real),
    (RATIONAL_CLASS, Sort::Rational),
    (INTEGER_CLASS,  Sort::Integer),
];

/// Behavior for the `translation::numeric_sorts` eager keyed index.
#[derive(Debug, Default)]
pub(crate) struct NumericSorts;

impl EagerMapBehavior for NumericSorts {
    type Parent = TranslationLayer;
    type Key    = SymbolId;
    type Value  = Sort;
    type Side   = ();
    type SideSnapshot = ();

    const NAME: &'static str = "translation::numeric_sorts";

    fn reads(&self) -> &'static [&'static str] {
        &[TaxEdges::NAME]
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        &[EventKind::TaxonomyChanged]
    }

    fn produces(&self) -> &'static [crate::cache::events::EventKind] {
        &[EventKind::NumericSortAdded, EventKind::NumericSortRemoved]
    }

    /// Recompute the numeric sort of each changed symbol, recursing into its
    /// subclass-descendants.  Emits `NumericSortAdded`/`Removed` on membership
    /// changes; a Real→Integer value change keeps membership and emits no event.
    ///
    /// Also serves as the build path: on a fresh load the numeric roots' subclass
    /// edges stream in as `TaxonomyChanged` and the cache fills incrementally.
    fn react(
        &self,
        parent: &Self::Parent,
        events: &[&crate::cache::events::Event],
        store:  &crate::cache::EntryCache<Self::Key, Self::Value>,
        _side:  &Self::Side,
    ) -> Vec<crate::cache::events::Event>
    {
        let semantic = &parent.semantic;

        // A root class can be absent in a partial KB / test fixture; skip it.
        let mut root_sort: HashMap<SymbolId, Sort> = HashMap::new();
        for (name, sort) in NUMERIC_ROOTS {
            if let Some(id) = semantic.syntactic.sym_id(name) {
                root_sort.insert(id, *sort);
            }
        }
        if root_sort.is_empty() {
            return Vec::new();
        }

        let mut work: Vec<SymbolId> = Vec::new();
        for ev in events {
            if let Event::TaxonomyChanged { syms } = ev {
                work.extend(syms.iter().copied());
            }
        }

        let mut seen: HashSet<SymbolId> = HashSet::new();
        let mut emitted: Vec<Event> = Vec::new();
        while let Some(sym) = work.pop() {
            if !seen.insert(sym) {
                continue;
            }
            let new = correct_numeric_sort(semantic, &root_sort, sym);
            let old = store.get(&sym);
            if old == new {
                continue;
            }
            match new {
                Some(sort) => {
                    store.update(sym, sort);
                    if old.is_none() {
                        emitted.push(Event::NumericSortAdded(sym));
                    }
                }
                None => {
                    store.evict_keys(&[sym]);
                    emitted.push(Event::NumericSortRemoved(sym));
                }
            }
            for (child, rel) in semantic.children_of(sym) {
                if matches!(rel, TaxRelation::Subclass) {
                    work.push(child);
                }
            }
        }
        emitted
    }

    fn initialize(
        &self,
        parent: &Self::Parent,
        store:  &crate::cache::EntryCache<Self::Key, Self::Value>,
        _side:   &Self::Side,
    )
    {
        for (sym_name, sort) in NUMERIC_ROOTS {
            if let Some(sym_id) = parent.semantic.syntactic.sym_id(*sym_name) {
                assign_descendents_to_sort(&store, &parent.semantic, sym_id, *sort);
            }
            // Missing root class is silently skipped: partial KBs and test
            // fixtures may not define every numeric root.
        }
    }
}

/// Recursively assign all subclass-descendants of a symbol to a given [`Sort`]
/// in the `numeric_sorts` cache.  Subsequent calls overwrite previously written
/// sort values, so call in order of precedence: `$real` → `$rat` → `$int`.
fn assign_descendents_to_sort(store: &EntryCache<SymbolId, Sort>, semantic: &SemanticLayer, sym_id: SymbolId, sort: Sort) {
    if store.get(&sym_id) == Some(sort) { return } // prevent potential cycles
    store.update(sym_id, sort);
    semantic.children_of(sym_id).iter().for_each(|(child_id, rel_type)| {
        if !matches!(rel_type, TaxRelation::Subclass) { return }
        assign_descendents_to_sort(store, semantic, *child_id, sort);
    });
}

/// The numeric [`Sort`] a symbol *should* hold: the most-specific numeric root
/// (Integer ≻ Rational ≻ Real) reachable from it by walking subclass-ancestors
/// (the symbol itself counts — a root maps to its own sort).  `None` when the
/// symbol descends from no numeric root.  One upward walk, collecting every
/// root encountered, so multiple-inheritance picks the most specific.
fn correct_numeric_sort(
    semantic:  &SemanticLayer,
    root_sort: &HashMap<SymbolId, Sort>,
    sym:       SymbolId,
) -> Option<Sort> {
    // `Sort::Ord` is specificity order (Individual < Real < Rational <
    // Integer), so `max` picks the more specific sort.
    let mut best: Option<Sort> = root_sort.get(&sym).copied();
    semantic.walk_subclass_closure(sym, /*up*/ true, |p| {
        if let Some(&sort) = root_sort.get(&p) {
            best = Some(best.map_or(sort, |b| b.max(sort)));
        }
        true
    });
    best
}

