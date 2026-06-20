// crates/core/src/trans/caches/poly_variant_symbols.rs
//
// `translation::poly_variant_symbols` — relation SymbolIds that need polymorphic
// TFF variant declarations.  Installed by `prime_caches`; can also build lazily
// via `generate`.  Read via `with_ref`.

use std::collections::HashSet;

use crate::semantics::caches::domain::Domain;
use crate::syntactic::caches::sentences::SentenceCache;
use crate::trans::caches::numeric_ancestor_set::NumericAncestorSet;
use crate::trans::caches::numeric_sorts::NumericSorts;
use crate::types::RelationDomain;
use crate::{Element, SymbolId};
use crate::cache::{CacheBehavior, EagerMapBehavior, WholeCacheBehavior};
use crate::cache::events::{Event, EventKind};
use crate::trans::TranslationLayer;

/// Behavior for the `translation::poly_variant_symbols` cache.
#[derive(Debug, Default)]
pub(crate) struct PolyVariantSymbols;

impl WholeCacheBehavior for PolyVariantSymbols {
    type Parent = TranslationLayer;
    type Value  = HashSet<SymbolId>;

    const NAME: &'static str = "translation::poly_variant_symbols";

    fn generate(&self, parent: &TranslationLayer) -> HashSet<SymbolId> {
        let mut result: HashSet<SymbolId> = HashSet::new();
        let mut seen: HashSet<SymbolId> = HashSet::new();
        // Discover the relations that carry a `(domain …)` axiom from the
        // sentence store, then resolve each relation's positional domain through
        // the `semantic::domain` cache (which already folds in `domainSubclass`,
        // position ordering, scope, and base/session conflict rules) rather than
        // re-parsing `(domain Relation Position Class)` out of the raw sentence.
        for sid in parent.semantic.syntactic.by_head("domain").iter().copied() {
            let Some(sentence) = parent.semantic.syntactic.sentence(sid) else { continue };
            let rel_id = match sentence.elements.get(1) {
                Some(Element::Symbol(sym)) => sym.id(),
                _ => continue,
            };
            if !seen.insert(rel_id) {
                continue; // already evaluated this relation's full domain
            }
            if relation_is_poly(parent, rel_id) {
                result.insert(rel_id);
            }
        }
        result
    }

    // Discovers candidate relations from `(domain …)` roots (sentence store),
    // resolves their positional domains via the `semantic::domain` cache, and
    // flags any relation with a position whose class is a numeric ancestor but
    // not itself numeric-sorted — reading `numeric_ancestor_set` and
    // `numeric_sorts`.
    fn reads(&self) -> &'static [&'static str] {
        &[SentenceCache::NAME, Domain::NAME, NumericAncestorSet::NAME, NumericSorts::NAME]
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        &[
            EventKind::DomainRangeChanged,
            EventKind::NumericSortAdded,
            EventKind::NumericSortRemoved,
        ]
    }

    /// Targeted maintenance.  Runs only when populated (primed at construction /
    /// first read); a cold cache stays cold and rebuilds via `generate`.
    ///
    /// * `DomainRangeChanged { syms }` — a `(domain …)` axiom changed for those
    ///   relations; recompute exactly their membership in one domain scan.
    /// * `NumericSortAdded`/`NumericSortRemoved` — the numeric class set shifted
    ///   (a rare numeric-taxonomy change).  Which relations flip depends on a
    ///   class→relations lookup this cache does not index, so it recomputes the
    ///   whole set via `generate`.  (A reverse index could make this targeted.)
    fn react(
        &self,
        parent: &Self::Parent,
        events: &[&crate::cache::events::Event],
        store:  &crate::cache::LayerCache<Self::Value>,
    ) -> Vec<crate::cache::events::Event>
    {
        if !store.is_populated() {
            return Vec::new(); // cold → `generate` rebuilds on next read
        }

        // A numeric-taxonomy shift can flip relations whose domain class we do
        // not reverse-index → full recompute (rare).
        if events
            .iter()
            .any(|e| matches!(e, Event::NumericSortAdded(_) | Event::NumericSortRemoved(_)))
        {
            store.install(self.generate(parent));
            return Vec::new();
        }

        // Otherwise only domain axioms moved: recompute just the affected rels.
        let mut affected: HashSet<SymbolId> = HashSet::new();
        for ev in events {
            if let Event::DomainRangeChanged { syms } = ev {
                affected.extend(syms.iter().copied());
            }
        }
        if affected.is_empty() {
            return Vec::new();
        }

        let mut set = store.snapshot().unwrap_or_default();
        // Recompute membership for exactly the affected relations straight from
        // the `semantic::domain` cache — no sentence scan needed.
        for r in &affected {
            if relation_is_poly(parent, *r) {
                set.insert(*r);
            } else {
                set.remove(r);
            }
        }
        store.install(set);
        Vec::new()
    }
}

/// `true` iff some argument position of `rel`'s declared domain is a numeric
/// *ancestor* class (a superclass of a numeric root) that is not itself
/// numeric-sorted — the signal that `rel` needs polymorphic TFF variants.
///
/// Reads the `semantic::domain`, `numeric_ancestor_set`, and `numeric_sorts`
/// caches.  Only `Domain(_)` positions qualify: a `DomainSubclass` position is
/// class-valued (an individual), never numeric.
fn relation_is_poly(parent: &TranslationLayer, rel: SymbolId) -> bool {
    parent.semantic.domain(rel).iter().any(|d| {
        let RelationDomain::Domain(cls) = d else { return false };
        let in_ancestor = parent
            .numeric_ancestor_set
            .with_ref(|s| s.map(|s| s.contains(cls)).unwrap_or(false));
        let in_sorts = parent.numeric_sorts.get(cls).is_some();
        in_ancestor && !in_sorts
    })
}
