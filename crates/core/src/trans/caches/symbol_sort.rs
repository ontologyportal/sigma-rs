// crates/core/src/trans/caches/symbol_sort.rs
//
// `translation::lazy_sort` — memoised TFF [`Sort`] for any symbol or scoped
// variable.  Lazily computed from numeric sorts + taxonomy class inference.

use std::collections::HashSet;

use crate::SymbolId;
use crate::cache::{CacheBehavior, EagerMapBehavior, EntryCache, WholeCacheBehavior};
use crate::semantics::caches::inferred_class::InferredClass;
use crate::trans::caches::numeric_ancestor_set::NumericAncestorSet;
use crate::trans::caches::numeric_sorts::NumericSorts;
use crate::trans::{TranslationError, TranslationLayer};
use crate::trans::Sort;
use crate::cache::events::EventKind;

/// Behavior for the `translation::lazy_sort` cache.
#[derive(Debug, Default)]
pub(crate) struct SymbolSort;

impl CacheBehavior for SymbolSort {
    type Parent = TranslationLayer;
    type Key    = SymbolId;
    type Value  = Result<Sort, TranslationError>;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "translation::lazy_sort";

    fn generate(&self, parent: &TranslationLayer, &sym: &SymbolId) -> Result<Sort, TranslationError> {
        compute_sort_scoped(parent, sym, crate::semantics::types::Scope::Base)
    }

    // If numeric sorts changes OR a class inference changes
    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        &[EventKind::TaxonomyChanged, EventKind::DomainRangeChanged]
    }

    // `generate` first consults `numeric_sorts`, then falls back to the symbol's
    // inferred class (`semantic::inferred_class`).
    fn reads(&self) -> &'static [&'static str] {
        &[NumericSorts::NAME, InferredClass::NAME, NumericAncestorSet::NAME]
    }

    fn react(
        &self,
        _parent: &TranslationLayer,
        events:  &[&crate::cache::events::Event],
        store:   &EntryCache<SymbolId, Result<Sort, TranslationError>>,
        _side:   &Self::Side,
    ) -> Vec<crate::cache::events::Event> {
        use crate::cache::events::Event;
        // Wholesale clear rather than `evict_keys(syms)`.  A cached sort is keyed
        // by the *symbol whose sort it is* (a constant / instance / variable),
        // but the events name the changed *classes* and *relations*:
        //   * `TaxonomyChanged` flips a class's numeric membership — every
        //     instance whose inferred class is (a subclass of) that class then
        //     resolves to a different `Sort`, yet none of those instances appear
        //     in `syms` (only the edge endpoints do);
        //   * `DomainRangeChanged` can change `infer_class` (which reads domain
        //     positions), shifting the sort of constants that occur as arguments
        //     of the affected relation — again unnamed in `syms`.
        // There is no class→instances reverse index to target those, so evicting
        // `syms` alone leaves stale entries behind (a leak).  Clear and let the
        // lazy `generate` refill on demand.
        //
        // There is deliberately NO `PureAddition` fast path here (unlike the
        // formula caches).  A *pure* addition of a taxonomy edge — e.g. newly
        // asserting `(subclass MyInt Integer)`, removing nothing — still flips the
        // numeric membership of an existing class and so changes the cached sort
        // of every existing instance under it.  A taxonomy change therefore must
        // clear whether or not the batch was addition-only.
        let tax = events.iter().any(|e| matches!(e, Event::TaxonomyChanged { .. }));
        let dr  = events.iter().any(|e| matches!(e, Event::DomainRangeChanged { .. }));
        if tax || dr {
            store.clear();
        }
        Vec::new()
    }
}

/// Resolve `sym`'s TFF [`Sort`] in `scope` — the shared compute body for both
/// the (Base-scoped) `symbol_sort` cache and the uncached per-session path.
///
/// Resolves the class inference FIRST, not the `is_class` shortcut: a constant
/// whose only evidence is a defining numeric equality (`(equal V 40.0)`) has no
/// taxonomy edges, and the "edge-less symbols are classes" heuristic in
/// `is_class` would misfile it as a class and strip its numeric sort.  A
/// genuine class comes back as `ClassInference::Class` (or `Unknown`) and still
/// maps to `$i`.
pub(crate) fn compute_sort_scoped(
    parent: &TranslationLayer,
    sym:    SymbolId,
    scope:  crate::semantics::types::Scope,
) -> Result<Sort, TranslationError> {
    use crate::types::ClassInference;
    match parent.semantic.infer_class_scoped(sym, scope) {
        ClassInference::Single(class_id) => {
            Ok(parent.numeric_sort_of_class(class_id).unwrap_or(Sort::Individual))
        },
        ClassInference::Multiple(class_ids) => {
            let (numeric_sort_classes, other_classes) : (Vec<_>, Vec<_>) =
                class_ids.iter().partition(|&id| parent.numeric_sort_of_class(*id).is_some());
            if numeric_sort_classes.len() == 0 {
                Ok(Sort::Individual)
            } else {
                // All non-numeric classes must be superclasses of the numeric
                // ones for this to resolve; dedupe the numeric sorts and take
                // the most specific.
                let numeric_sorts: HashSet<Sort> = HashSet::from_iter(numeric_sort_classes
                    .iter()
                    .map(|c: &&SymbolId| parent.numeric_sort_of_class(**c).unwrap()));
                let sort = numeric_sorts.into_iter().max().unwrap();

                if other_classes.iter().all(|id|
                    parent.numeric_ancestor_set.with_ref(|a|
                        a.is_some() && a.unwrap().get(id).is_some())) {
                    Ok(sort)
                } else {
                    Err(TranslationError::AmbiguousSort { sym, sorts: vec![Sort::Individual, sort] })
                }
            }
        }
        _ => Ok(Sort::Individual),
    }
}

impl TranslationLayer {
    /// Determine the TFF [`Sort`] for any symbol or scoped variable (Base scope,
    /// memoised).
    pub(crate) fn sort_for_symbol(&self, sym: SymbolId) -> Result<Sort, TranslationError> {
        self.symbol_sort.get(self, sym)
    }

    /// [`Self::sort_for_symbol`] in an explicit [`Scope`](crate::semantics::types::Scope).
    /// `Base` takes the memoised cache; a session scope computes directly (a
    /// session's evidence union is transient and small, so this stays uncached
    /// rather than growing a scope-keyed cache).
    pub(crate) fn sort_for_symbol_scoped(
        &self,
        sym:   SymbolId,
        scope: crate::semantics::types::Scope,
    ) -> Result<Sort, TranslationError> {
        match scope {
            crate::semantics::types::Scope::Base => self.sort_for_symbol(sym),
            _ => compute_sort_scoped(self, sym, scope),
        }
    }
}
