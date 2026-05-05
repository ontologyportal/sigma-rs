// crates/core/src/trans/annotations.rs
//
// Hold sort annotations

use std::{collections::HashMap, sync::RwLockReadGuard};

use crate::SymbolId;
use crate::semantics::taxonomy::TaxRelation;

use super::{Sort, TranslationLayer};

impl TranslationLayer {
    /// Construct Sort Annotations from a [`SemanticLayer`] definition
    pub(crate) fn build_sort_annotations(&self) -> SortAnnotations {
        let mut symbol_arg_sorts    = HashMap::new();
        let mut symbol_return_sorts = HashMap::new();

        for &id in self.semantic.syntactic.symbols.values() {
            if self.semantic.is_function(id) {
                let arg_sorts: Vec<Sort> = self.semantic.domain(id).iter()
                    .map(|rd| self.sort_for_id(rd.id()))
                    .collect();
                let ret_sort = match self.semantic.range(id) {
                    Ok(Some(rd)) => self.sort_for_id(rd.id()),
                    _            => Sort::Individual,
                };
                symbol_arg_sorts.insert(id, arg_sorts);
                symbol_return_sorts.insert(id, ret_sort);
            } else if self.semantic.is_relation(id) || self.semantic.is_predicate(id) {
                let arg_sorts: Vec<Sort> = self.semantic.domain(id).iter()
                    .map(|rd| self.sort_for_id(rd.id()))
                    .collect();
                if !arg_sorts.is_empty() {
                    symbol_arg_sorts.insert(id, arg_sorts);
                }
            }
        }

        // Compute sorts for individual numeric constants from `instance` edges.
        // E.g. `(instance Pi PositiveRealNumber)` -> Pi maps to Sort::Real.
        //
        // TaxEdge direction: `from = class (PositiveRealNumber), to = individual (Pi)`.
        // So the individual is edge.to and the class is edge.from.
        let mut symbol_individual_sorts = HashMap::new();
        for edge in &self.semantic.tax_edges {
            if edge.rel != TaxRelation::Instance { continue; }
            let individual_id = edge.to;
            // Skip known relations
            if self.semantic.is_relation(individual_id) {
                continue;
            }
            let class_sort = self.sort_for_id(edge.from);
            if class_sort == Sort::Individual { continue; } // Not a numeric class.
            // Keep the most specific (narrowest) sort across all instance edges.
            // Sort is Ord: Individual(1) < Real(2) < Rational(3) < Integer(4).
            // max() picks the more specific sort (Integer > Real).
            let entry = symbol_individual_sorts.entry(individual_id).or_insert(class_sort);
            *entry = (*entry).max(class_sort);
        }

        SortAnnotations { symbol_arg_sorts, symbol_return_sorts, symbol_individual_sorts }
    }

    /// Returns the lazily-computed KB-wide sort annotation table.
    ///
    /// On first call iterates all KB symbols to compute arg/return sorts
    /// from domain and range axioms.  Result is cached; cleared by `invalidate_cache()`.
    pub(crate) fn sort_annotations(&self) -> RwLockReadGuard<'_, Option<SortAnnotations>> {
        {
            let mut guard = self.sort_annotations.write().unwrap();
            if guard.is_none() {
                *guard = Some(self.build_sort_annotations());
            }
        }
        self.sort_annotations.read().unwrap()
    }
}

/// Precomputed TFF sort signatures for all relations and functions in the KB.
///
/// Derived from SUMO `domain` and `range` axioms, keyed by SymbolId.
/// Equivalent to what `TffContext::signatures` and `TffContext::return_sorts`
/// accumulate lazily during translation, but precomputed for the whole KB.
///
/// `DomainSubclass` argument positions map to `Sort::Individual` (variables in
/// subclass positions are ontological individuals in TFF).
/// The sentinel `u64::MAX` in a `RelationDomain` also maps to `Sort::Individual`.
///
/// Built lazily; cleared by `invalidate_cache()`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SortAnnotations {
    /// Ordered argument sorts for all relations, predicates, and functions
    /// that have at least one `domain` axiom.
    pub symbol_arg_sorts:    HashMap<SymbolId, Vec<Sort>>,
    /// Return sort for all function symbols.
    /// Only populated for functions; predicates/relations are absent.
    pub symbol_return_sorts: HashMap<SymbolId, Sort>,
    /// Sort of individual constants (non-function, non-relation symbols) that
    /// are `instance`-related to a numeric SUMO class.
    /// E.g. `(instance Pi PositiveRealNumber)` -> `Pi -> Sort::Real`.
    pub symbol_individual_sorts: HashMap<SymbolId, Sort>,
}

impl SortAnnotations {
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self {
            symbol_arg_sorts:       HashMap::new(),
            symbol_return_sorts:    HashMap::new(),
            symbol_individual_sorts:HashMap::new(),
        }
    }
}