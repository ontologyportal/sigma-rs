//! `semantic::subrel_lattice` cache: a relation's below-set — every
//! relation whose tuples flow up into it — with parent pointers for
//! witness extraction.
//!
//! Two edge sources feed the lattice:
//!   * declared `(subrelation r R)` edges, held in `tax_edges`
//!     (`TaxRelation::Subrelation`), scoped via the overlay rules;
//!   * mined rule-edges: implications of the exact shape
//!     `(=> (R1 ?x ?y) (R2 ?x ?y))`, which behave as subrelation edges.
//!     A relation's incoming rule-edges are found by shape-filtering its
//!     axiom-occurrence set (`axiom_index`) plus, in a session scope, the
//!     session's own roots. The rule's sid is kept in the hop for witness
//!     citation.

use std::collections::HashMap;
use std::sync::Arc;

use crate::types::{Element, SentenceId, SymbolId, TaxRelation};
use crate::cache::{CacheBehavior, EntryCache};
use crate::cache::events::{Event, EventKind};
use crate::semantics::SemanticLayer;
use crate::semantics::types::{Scope, Scoped};

/// One hop up the lattice: the next relation toward the queried target,
/// plus the sid justifying the hop — `Some(rule sid)` for mined edges,
/// `None` for declared `subrelation` edges (whose fact sid is resolved
/// on demand).
pub(crate) type SubrelHop = (SymbolId, Option<SentenceId>);

/// `relation at-or-below target` → its hop toward the target
/// (`None` for the target itself).  Walking the pointers reconstructs
/// the full `r → … → target` chain.
pub(crate) type BelowMap = HashMap<SymbolId, Option<SubrelHop>>;

/// Behavior for the `semantic::subrel_lattice` cache.
#[derive(Debug, Default)]
pub(crate) struct SubrelLattice;

impl CacheBehavior for SubrelLattice {
    type Parent = SemanticLayer;
    type Key    = Scoped<SymbolId>;
    type Value  = Arc<BelowMap>;
    type Side   = ();
    type SideSnapshot = ();

    const NAME: &'static str = "semantic::subrel_lattice";

    fn generate(
        &self,
        parent: &SemanticLayer,
        &Scoped { scope, key: rel }: &Scoped<SymbolId>,
    ) -> Arc<BelowMap> {
        let mut below: BelowMap = HashMap::new();
        below.insert(rel, None);
        let mut frontier = vec![rel];
        while let Some(r) = frontier.pop() {
            for (child, tax) in parent.children_of_scoped(r, scope) {
                if !matches!(tax, TaxRelation::Subrelation) { continue; }
                below.entry(child).or_insert_with(|| {
                    frontier.push(child);
                    Some((r, None))
                });
            }
            for (child, sid) in mined_subs_of(parent, r, scope) {
                below.entry(child).or_insert_with(|| {
                    frontier.push(child);
                    Some((r, Some(sid)))
                });
            }
        }
        Arc::new(below)
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::RootAdded, EventKind::RootRemoved,
          EventKind::TaxonomyChanged,
          EventKind::SessionReferenced, EventKind::SessionRetracted]
    }

    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences", "semantic::tax_edges",
          "syntactic::axiom_index", "syntactic::sessions"]
    }

    fn react(
        &self,
        parent: &SemanticLayer,
        events: &[&Event],
        store:  &EntryCache<Scoped<SymbolId>, Arc<BelowMap>>,
        _side:  &Self::Side,
    ) -> Vec<Event> {
        let dirty = events.iter().any(|e| match e {
            Event::RootAdded { sid } => mined_edge_of(parent, *sid).is_some(),
            Event::RootRemoved { .. }
            | Event::TaxonomyChanged { .. }
            | Event::SessionReferenced { .. }
            | Event::SessionRetracted { .. } => true,
            _ => false,
        });
        if dirty { store.clear(); }
        Vec::new()
    }
}

/// The mined sub-relations of `sup` visible in `scope`, as `(sub, rule sid)`
/// pairs. Shape-filters `sup`'s axiom occurrences plus, for session scopes,
/// the session's own roots.
fn mined_subs_of(
    parent: &SemanticLayer,
    sup:    SymbolId,
    scope:  Scope,
) -> Vec<(SymbolId, SentenceId)> {
    let mut candidates: Vec<SentenceId> =
        parent.syntactic.axiom_sentences_of(sup).iter().copied().collect();
    if let Scope::Session(s) = scope {
        candidates.extend(parent.syntactic.sessions.session_sentences_by_id(s));
    }
    parent
        .scope_filter_sids(candidates, scope)
        .into_iter()
        .filter_map(|sid| match mined_edge_of(parent, sid) {
            Some((sub, s2)) if s2 == sup => Some((sub, sid)),
            _ => None,
        })
        .collect()
}

/// The `(sub, super)` pair iff `sid` is a rule of the exact mined shape
/// `(=> (R1 ?x ?y) (R2 ?x ?y))` — same two variables, same order, both
/// sides symbol-headed binary atoms.
fn mined_edge_of(parent: &SemanticLayer, sid: SentenceId) -> Option<(SymbolId, SymbolId)> {
    use crate::parse::OpKind;
    let s = parent.syntactic.sentence(sid)?;
    let mut els = s.elements.iter();
    if !matches!(els.next(), Some(Element::Op(OpKind::Implies))) { return None; }
    let Some(Element::Sub(ante)) = els.next() else { return None };
    let Some(Element::Sub(cons)) = els.next() else { return None };
    if els.next().is_some() { return None; }

    let binary_pred = |sid: SentenceId| -> Option<(SymbolId, SymbolId, SymbolId)> {
        let s = parent.syntactic.sentence(sid)?;
        if s.elements.len() != 3 { return None; }
        let Some(Element::Symbol(head)) = s.elements.first() else { return None };
        let Element::Variable { id: v1, .. } = s.elements[1] else { return None };
        let Element::Variable { id: v2, .. } = s.elements[2] else { return None };
        Some((head.id(), v1, v2))
    };
    let (r1, a1, a2) = binary_pred(*ante)?;
    let (r2, b1, b2) = binary_pred(*cons)?;
    // Same variables in the same seats; distinct relations.
    (a1 == b1 && a2 == b2 && a1 != a2 && r1 != r2).then_some((r1, r2))
}

impl SemanticLayer {
    /// The below-set of `rel` (relations whose tuples flow up into it,
    /// itself included) with witness parent-pointers, in `scope`.
    pub(crate) fn subrel_below(&self, rel: SymbolId, scope: Scope) -> Arc<BelowMap> {
        self.subrel_lattice.get(self, Scoped { scope, key: rel })
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::kif_layer;
    use crate::semantics::types::Scope;

    #[test]
    fn declared_subrelation_chains_into_below_set() {
        let layer = kif_layer("
            (subrelation brother sibling)
            (subrelation sibling familyRelation)
        ");
        let rel = layer.syntactic.sym_id("familyRelation").unwrap();
        let bro = layer.syntactic.sym_id("brother").unwrap();
        let sib = layer.syntactic.sym_id("sibling").unwrap();
        let below = layer.subrel_below(rel, Scope::Base);
        assert!(below.contains_key(&bro) && below.contains_key(&sib));
        // brother's hop points at sibling, sibling's at the target.
        assert_eq!(below[&bro].map(|(up, _)| up), Some(sib));
        assert_eq!(below[&sib].map(|(up, _)| up), Some(rel));
        assert_eq!(below[&rel], None);
    }

    #[test]
    fn mined_rule_edge_enters_lattice_with_sid() {
        let layer = kif_layer("
            (=> (greaterThan ?X ?Y) (greaterThanOrEqualTo ?X ?Y))
            (instance greaterThan BinaryPredicate)
        ");
        let ge = layer.syntactic.sym_id("greaterThanOrEqualTo").unwrap();
        let gt = layer.syntactic.sym_id("greaterThan").unwrap();
        let below = layer.subrel_below(ge, Scope::Base);
        let hop = below.get(&gt).expect("mined edge present").expect("not the target");
        assert_eq!(hop.0, ge);
        assert!(hop.1.is_some(), "mined hop carries the rule's sid");
    }

    #[test]
    fn wrong_shapes_are_not_mined() {
        let layer = kif_layer("
            (=> (p ?X ?Y) (q ?Y ?X))
            (=> (r ?X ?Y) (r2 ?X ?Z))
            (=> (s ?X ?X) (s2 ?X ?X))
            (=> (and (t ?X ?Y) (t ?Y ?Z)) (t ?X ?Z))
        ");
        for (sub, sup) in [("p", "q"), ("r", "r2"), ("s", "s2")] {
            let sup_id = layer.syntactic.sym_id(sup).unwrap();
            let sub_id = layer.syntactic.sym_id(sub).unwrap();
            let below = layer.subrel_below(sup_id, Scope::Base);
            assert!(!below.contains_key(&sub_id),
                "({sub} -> {sup}) must not mine: wrong variable shape");
        }
    }
}
