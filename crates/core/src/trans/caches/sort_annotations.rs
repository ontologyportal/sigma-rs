//! The typed sort-annotation table, lazily built from domain/range axioms (or
//! installed from LMDB) and invalidated when domain/range axioms change.

use serde::{Deserialize, Serialize};

use crate::semantics::caches::domain::Domain;
use crate::semantics::caches::is_function::IsFunction;
use crate::semantics::caches::is_relation::IsRelation;
use crate::semantics::caches::range::Range;
use crate::semantics::caches::tax_edges::TaxEdges;
use crate::trans::caches::numeric_sorts::NumericSorts;
use crate::types::{RelationDomain, RelationRange};
use crate::SymbolId;
use crate::cache::{CacheBehavior, EagerMapBehavior, EntryCache};
use crate::trans::{Sort, TranslationLayer};

/// One concrete signature of a relation or function: argument sorts plus an
/// optional return sort.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelSort {
    /// Per-argument sorts (one per declared position; defaults to Individual).
    arg_sorts: Vec<Sort>,
    /// Return sort for function symbols; `None` for predicates / relations.
    ret_sort: Option<Sort>
}

impl RelSort {
    /// The per-argument sorts of this variant (one per declared position).
    #[cfg(feature = "ask")]
    pub(crate) fn arg_sorts(&self) -> &[Sort] {
        &self.arg_sorts
    }
}

/// Per-symbol sort annotation for a single constant, relation, or function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SortAnnotation {
    /// A constant's sort annotation is just a singular sort
    Constant(Sort),
    /// A relation's sort annotation is its entire signature (args and return)
    Relation {
        /// Per-argument sorts (one per declared position; defaults to Individual).
        arg_sorts: Vec<Sort>,
        /// Return sort for function symbols; `None` for predicates / relations.
        ret_sort: Option<Sort>
    },
    /// A special type of Relation which is polymorphic and requires multiple 
    /// signatures for multiple types of arguments. For example, instance
    /// has a singature (Entity, Class), but the first "Entity" could be an $i
    /// or a numeric class. Therefore, these Relations enumerate ALL the various 
    /// forms which it could accept any number of types as its input
    Polymorphic(Vec<RelSort>)
}

/// Behavior for the `translation::sort_annotations` cache.
#[derive(Debug, Default)]
pub(crate) struct SortAnnotationsCache;

impl CacheBehavior for SortAnnotationsCache {
    type Parent       = TranslationLayer;
    type Key          = SymbolId;
    type Value        = SortAnnotation;
    type Side         = ();
    type SideSnapshot = ();

    const NAME: &'static str = "translation::sort_annotations";

    fn generate(&self, parent: &TranslationLayer, key: &Self::Key) -> Self::Value {
        if !parent.semantic.is_relation(*key) {
            return SortAnnotation::Constant(
                parent.sort_for_symbol(*key).unwrap_or_else(|_| Sort::Individual)
            );
        }

        let ret_sort = if !parent.semantic.is_function(*key) {
            None
        } else {
            Some(match parent.semantic.range(*key) {
                RelationRange::RangeSubclass(_)
                | RelationRange::Unknown => Sort::Individual,
                RelationRange::Range(cls) => parent.sort_for_id(cls),
            })
        };

        if !parent.poly_variant_symbols.get(parent).contains(key) {
            // A numeric-classed domain pins its position's declared sort; the
            // abstract `Number` superclass maps to `$real`, everything else to
            // Individual.
            let arg_sorts = parent.semantic.domain(*key).iter().map(|d| {
                match d {
                    RelationDomain::DomainSubclass(_)
                    | RelationDomain::Unknown => Sort::Individual,
                    RelationDomain::Domain(cls) =>
                        parent.numeric_sort_of_class(*cls).unwrap_or(Sort::Individual),
                }
            }).collect();

            SortAnnotation::Relation { arg_sorts, ret_sort }
        } else {
            // Enumerate every concrete signature as the cartesian product of each
            // position's candidate sorts. Cap how many positions get the full
            // numeric expansion (leftmost first), pinning the remainder to
            // Individual, to bound the enumeration at 4^MAX_FLEX_POSITIONS.
            const MAX_FLEX_POSITIONS: usize = 3;
            let mut flex_seen = 0usize;
            let arg_options: Vec<Vec<Sort>> = parent.semantic.domain(*key)
                .iter()
                .map(|d| {
                    let opts = position_sort_options(parent, d);
                    if opts.len() > 1 {
                        flex_seen += 1;
                        if flex_seen > MAX_FLEX_POSITIONS {
                            return vec![Sort::Individual];
                        }
                    }
                    opts
                })
                .collect();

            let variants = cartesian_sorts(&arg_options)
                .into_iter()
                .map(|arg_sorts| RelSort { arg_sorts, ret_sort })
                .collect();
            SortAnnotation::Polymorphic(variants)
        }
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        &[crate::cache::events::EventKind::DomainRangeChanged, crate::cache::events::EventKind::TaxonomyChanged]
    }

    fn reads(&self) -> &'static [&'static str] {
        &[IsRelation::NAME, IsFunction::NAME,
          Domain::NAME, Range::NAME, TaxEdges::NAME,
          NumericSorts::NAME]
    }

    fn react(
        &self,
        _parent: &TranslationLayer,
        events:  &[&crate::cache::events::Event],
        store:   &EntryCache<SymbolId, SortAnnotation>,
        _side:    &()
    ) -> Vec<crate::cache::events::Event> {
        use crate::cache::events::Event;
        // Must clear wholesale, not `evict_keys(syms)`: a changed symbol can
        // invalidate entries keyed by other symbols this cache cannot name from
        // the event and has no reverse index to find. No `PureAddition` fast path
        // either — a pure taxonomy-edge addition can still flip an existing
        // class's numeric membership.
        let tax = events.iter().any(|e| matches!(e, Event::TaxonomyChanged { .. }));
        let dr  = events.iter().any(|e| matches!(e, Event::DomainRangeChanged { .. }));
        if tax || dr {
            store.clear();
        }
        Vec::new()
    }
}

/// Candidate sorts a single argument position can take, for a polymorphic
/// relation's signature enumeration.
///
/// * `DomainSubclass` / `Unknown` → the argument is class-valued (or untyped),
///   so it is an individual (`$i`).
/// * `Domain(cls)` where `cls` is itself numeric-sorted → fixed to that sort.
/// * `Domain(cls)` where `cls` is a numeric *ancestor* (superclass of a numeric
///   root but not numeric itself) → the position can be an individual or any
///   numeric sort: `{Individual, Real, Rational, Integer}`.
/// * `Domain(cls)` otherwise → individual.
fn position_sort_options(parent: &TranslationLayer, d: &RelationDomain) -> Vec<Sort> {
    let cls = match d {
        RelationDomain::Domain(cls) => *cls,
        RelationDomain::DomainSubclass(_) | RelationDomain::Unknown => {
            return vec![Sort::Individual];
        }
    };
    if let Some(s) = parent.numeric_sorts.get(&cls) {
        return vec![s];
    }
    let is_numeric_ancestor = parent
        .numeric_ancestor_set
        .with_ref(|set| set.map(|set| set.contains(&cls)).unwrap_or(false));
    if is_numeric_ancestor {
        vec![Sort::Individual, Sort::Real, Sort::Rational, Sort::Integer]
    } else {
        vec![Sort::Individual]
    }
}

/// Cartesian product of per-position candidate sorts: one output vector per
/// distinct signature.  An empty `per_pos` yields a single empty signature
/// (a 0-ary relation), and every position contributes at least one option so the
/// result is never empty.
fn cartesian_sorts(per_pos: &[Vec<Sort>]) -> Vec<Vec<Sort>> {
    let mut acc: Vec<Vec<Sort>> = vec![Vec::new()];
    for opts in per_pos {
        let mut next = Vec::with_capacity(acc.len() * opts.len().max(1));
        for prefix in &acc {
            for s in opts {
                let mut v = prefix.clone();
                v.push(*s);
                next.push(v);
            }
        }
        acc = next;
    }
    acc
}

impl TranslationLayer {
    /// The typed sort annotations, building them on first access.
    pub(crate) fn sort_annotation(&self, sym: SymbolId) -> SortAnnotation {
        self.sort_annotations.get(self, sym)
    }
}
