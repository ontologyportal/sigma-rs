// crates/core/src/trans/cache.rs
//
// Cache management for TranslationLayer

use std::collections::{HashMap, HashSet, VecDeque};

use crate::{SymbolId, TaxRelation};

use super::{TranslationLayer, Sort};
use super::arith::ArithCond;
use super::numeric::NUMERIC_ROOTS;

impl TranslationLayer {
    /// Clear the [`SortAnnotations`] cache.
    ///
    /// Invalidate whenever a `domain` / `range` / `domainSubclass`
    /// axiom is added or removed -- those are the direct sources of
    /// entries in [`SortAnnotations.symbol_arg_sorts`] and
    /// [`SortAnnotations.symbol_return_sorts`].
    pub(crate) fn invalidate_sort_annotations(&self) {
        *self.sort_annotations.write().unwrap() = None;
    }

    /// Build the numeric sort cache by BFS downward from each root in
    /// `NUMERIC_ROOTS`.
    ///
    /// A temporary children index (`parent_id -> [child_ids]`) is constructed
    /// from `tax_edges` so the BFS can walk downward efficiently.  The three
    /// root names are the only hardcoded strings; all subclass SymbolIds are
    /// discovered dynamically.
    ///
    /// Processing order is least-specific -> most-specific (Real -> Rational ->
    /// Integer) so that a more-specific sort overwrites a less-specific one
    /// when a class descends from multiple roots (e.g. NonnegativeInteger is
    /// both under Integer and NonnegativeRealNumber -- it ends up as Integer).
    #[allow(dead_code)]
    fn build_numeric_sort_cache(&self) -> HashMap<SymbolId, Sort> {
        // Build a temporary children index: parent_id -> [child_id, ...]
        // In tax_edges: from = parent (superclass), to = child (subclass).
        let mut children: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
        for edge in &self.semantic.tax_edges {
            if edge.rel == TaxRelation::Subclass {
                children.entry(edge.from).or_default().push(edge.to);
            }
        }

        let mut cache: HashMap<SymbolId, Sort> = HashMap::new();

        // Hoisted across roots; cleared per-iteration.  The per-root
        // `visited` semantics are preserved because multi-root classes
        // are still overwritten in `cache` by the later root's sort
        // (the "more-specific sort wins" contract documented above).
        let mut queue:   VecDeque<SymbolId> = VecDeque::new();
        let mut visited: HashSet<SymbolId>  = HashSet::new();

        for &(root_name, sort) in NUMERIC_ROOTS {
            let root_id = match self.semantic.syntactic.sym_id(root_name) {
                Some(id) => id,
                None     => continue,  // root class not present in this KB
            };

            // BFS downward from root_id, including the root itself.
            queue.clear();
            visited.clear();
            queue.push_back(root_id);
            while let Some(id) = queue.pop_front() {
                if !visited.insert(id) { continue; }  // cycle guard
                cache.insert(id, sort);
                if let Some(kids) = children.get(&id) {
                    for &kid in kids {
                        if !visited.contains(&kid) {
                            queue.push_back(kid);
                        }
                    }
                }
            }
        }

        cache
    }
}

/// Caching structure for the [`TranslationLayer`].
///
/// Most fields are populated by helpers in `trans/mod.rs` that aren't
/// yet wired into the cache-priming path (the persist-side snapshot
/// methods are the only readers today).  `#[allow(dead_code)]` keeps
/// warning-clean until those helpers run.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub(crate) struct TranslationLayerCache {
    /// Maps every known SUMO numeric class `SymbolId` -> its TFF [`Sort`].
    ///
    /// Built by inspecting taxonomy via a downward BFS from the three roots
    /// in `NUMERIC_ROOTS`.  Lookups are O(1) integer comparisons -- no string
    /// operations after the initial taxonomy warm-up.
    pub(crate) numeric_sorts:     HashMap<SymbolId, Sort>,
    /// All SUMO class `SymbolId`s that are ancestors (superclasses) of the
    /// three numeric roots -- i.e., the classes through which numeric classes
    /// inherit: Entity, Abstract, Quantity, Number, RealNumber, etc.
    ///
    /// Used in VTI resolution: a variable constrained by both a numeric class
    /// AND an ancestor class (e.g. [Integer, Entity]) should get the numeric
    /// sort, because Integer IS-A Entity.  A constraint from a non-ancestor
    /// class (e.g. Animal) is a genuine conflict and the variable is left
    /// unannotated (defaults to `$i`).
    ///
    /// Built by `rebuild_taxonomy` via an upward BFS from `NUMERIC_ROOTS`.
    pub(crate) numeric_ancestor_set:   HashSet<SymbolId>,
    /// Relation [`SymbolId`]s that have at least one argument position
    /// whose SUMO domain class is a numeric-ancestor class (in
    /// [`TranslationLayer::numeric_ancestor_set`]) but is NOT itself a 
    /// numeric class (i.e., it maps to `$i` in TFF, not `$int`/`$rat`
    /// /`$real`).
    ///
    /// These symbols need polymorphic TFF variant declarations so that
    /// numeric-sorted arguments (e.g. `$int`) can be passed to positions
    /// whose base declaration says `$i`.  The canonical example: `ListFn`
    /// with `(domain ListFn 1 Entity)`: Entity is an ancestor of Integer,
    /// so a `$int`-sorted variable may legally appear there; the variant
    /// `s__ListFn__1__int: ($int) > $i` makes the TFF type system agree.
    pub(crate) poly_variant_symbols:   HashSet<SymbolId>,
    /// Arithmetic characterizations of numeric subclasses.
    /// Built by `build_numeric_char_cache()` after `numeric_sort_cache` is ready.
    pub(crate) numeric_char:     HashMap<SymbolId, ArithCond>,
}

impl TranslationLayerCache {
    #[cfg(feature = "persist")]
    pub(crate) fn numeric_sort_cache_snapshot(&self) -> HashMap<SymbolId, Sort> {
        self.numeric_sorts.clone()
    }
    #[cfg(feature = "persist")]
    pub(crate) fn numeric_ancestor_set_snapshot(&self) -> HashSet<SymbolId> {
        self.numeric_ancestor_set.clone()
    }
    #[cfg(feature = "persist")]
    pub(crate) fn poly_variant_symbols_snapshot(&self) -> HashSet<SymbolId> {
        self.poly_variant_symbols.clone()
    }
    #[cfg(feature = "persist")]
    pub(crate) fn numeric_char_cache_snapshot(&self) -> HashMap<SymbolId, ArithCond> {
        self.numeric_char.clone()
    }
}