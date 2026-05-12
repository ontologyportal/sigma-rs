//! `semantic::trans_reach` cache: ground-fact reachability for one
//! (relation, start) pair, with parent pointers and per-hop fact sids.
//!
//! An "edge of R" is any stored fact `(r x y)` with `r ∈ below(R)` (the
//! subrelation lattice, mined rule-edges included). The cache memoizes the
//! whole reachable set from each queried start node; the parent pointers
//! reconstruct the witness chain `a → … → c` with the fact sid for every hop.
//!
//! Reachability is computed on request and is meaningful only when the caller
//! has established that R is transitive.

use std::collections::HashMap;
use std::sync::Arc;

use crate::types::{Element, SentenceId, SymbolId};
use crate::cache::{CacheBehavior, EntryCache};
use crate::cache::events::{Event, EventKind};
use crate::semantics::SemanticLayer;
use crate::semantics::types::{Scope, Scoped};

/// `reached node` → (previous node on the path, sid of the direct fact
/// that made the hop).  The start node itself is absent — only proper
/// destinations appear.
pub(crate) type ReachMap = HashMap<SymbolId, (SymbolId, SentenceId)>;

/// Behavior for the `semantic::trans_reach` cache.
#[derive(Debug, Default)]
pub(crate) struct TransReach;

impl CacheBehavior for TransReach {
    type Parent = SemanticLayer;
    type Key    = Scoped<(SymbolId, SymbolId)>; // (relation, start)
    type Value  = Arc<ReachMap>;
    type Side   = ();
    type SideSnapshot = ();

    const NAME: &'static str = "semantic::trans_reach";

    fn generate(
        &self,
        parent: &SemanticLayer,
        &Scoped { scope, key: (rel, start) }: &Scoped<(SymbolId, SymbolId)>,
    ) -> Arc<ReachMap> {
        let below = parent.subrel_below(rel, scope);
        let mut reach: ReachMap = HashMap::new();
        let mut frontier = vec![start];
        while let Some(node) = frontier.pop() {
            for r in below.keys() {
                for (obj, sid) in parent.ground_binary_objects(*r, node, scope) {
                    if obj != start && !reach.contains_key(&obj) {
                        reach.insert(obj, (node, sid));
                        frontier.push(obj);
                    }
                }
            }
        }
        Arc::new(reach)
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::RelationAdded, EventKind::RelationRemoved,
          EventKind::TaxonomyChanged,
          EventKind::SessionReferenced, EventKind::SessionRetracted]
    }

    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences", "syntactic::residue_index",
          "syntactic::sessions", "semantic::subrel_lattice"]
    }

    fn react(
        &self,
        _parent: &SemanticLayer,
        events:  &[&Event],
        store:   &EntryCache<Scoped<(SymbolId, SymbolId)>, Arc<ReachMap>>,
        _side:   &Self::Side,
    ) -> Vec<Event> {
        // Any fact/lattice/scope mutation can extend or cut a path, and the
        // events carry no relation identity to target, so clear wholesale.
        if events.iter().any(|e| matches!(e,
            Event::RelationAdded { .. } | Event::RelationRemoved { .. }
            | Event::TaxonomyChanged { .. }
            | Event::SessionReferenced { .. } | Event::SessionRetracted { .. }))
        {
            store.clear();
        }
        Vec::new()
    }
}

impl SemanticLayer {
    /// Everything reachable from `start` over the ground edges of `rel`
    /// (subrelation-inherited edges included), with witness pointers.
    pub(crate) fn trans_reach(
        &self,
        rel:   SymbolId,
        start: SymbolId,
        scope: Scope,
    ) -> Arc<ReachMap> {
        self.trans_reach.get(self, Scoped { scope, key: (rel, start) })
    }

    /// The ground binary facts `(head subject OBJ)` visible in `scope`, as
    /// `(OBJ, sid)` pairs. Symbol-argument facts only; compound-argument facts
    /// are not enumerated here.
    pub(crate) fn ground_binary_objects(
        &self,
        head:    SymbolId,
        subject: SymbolId,
        scope:   Scope,
    ) -> Vec<(SymbolId, SentenceId)> {
        self.subject_sids_scoped(head, subject, scope)
            .into_iter()
            .filter_map(|sid| {
                let s = self.syntactic.sentence(sid)?;
                if s.elements.len() != 3 { return None; }
                match s.elements.get(2) {
                    Some(Element::Symbol(obj)) => Some((obj.id(), sid)),
                    _ => None,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::kif_layer;
    use crate::semantics::types::Scope;

    #[test]
    fn reachability_with_parent_pointers() {
        let layer = kif_layer("
            (located a b)
            (located b c)
            (located c d)
            (located x y)
        ");
        let rel = layer.syntactic.sym_id("located").unwrap();
        let a = layer.syntactic.sym_id("a").unwrap();
        let b = layer.syntactic.sym_id("b").unwrap();
        let c = layer.syntactic.sym_id("c").unwrap();
        let d = layer.syntactic.sym_id("d").unwrap();
        let y = layer.syntactic.sym_id("y").unwrap();

        let reach = layer.trans_reach(rel, a, Scope::Base);
        assert_eq!(reach.len(), 3, "b, c, d reachable; y not");
        assert!(reach.contains_key(&d) && !reach.contains_key(&y));
        // Walk the parent pointers d -> c -> b -> a.
        assert_eq!(reach[&d].0, c);
        assert_eq!(reach[&c].0, b);
        assert_eq!(reach[&b].0, a);
    }

    #[test]
    fn reach_inherits_subrelation_edges() {
        let layer = kif_layer("
            (subrelation properlyLocated located)
            (located a b)
            (properlyLocated b c)
        ");
        let rel = layer.syntactic.sym_id("located").unwrap();
        let a = layer.syntactic.sym_id("a").unwrap();
        let c = layer.syntactic.sym_id("c").unwrap();
        let reach = layer.trans_reach(rel, a, Scope::Base);
        assert!(reach.contains_key(&c),
            "the properlyLocated edge flows up into located's graph");
    }

    #[test]
    fn hop_sids_cite_the_direct_facts() {
        let layer = kif_layer("
            (located a b)
            (located b c)
        ");
        let rel = layer.syntactic.sym_id("located").unwrap();
        let a = layer.syntactic.sym_id("a").unwrap();
        let c = layer.syntactic.sym_id("c").unwrap();
        let reach = layer.trans_reach(rel, a, Scope::Base);
        let (_, hop_sid) = reach[&c];
        let s = layer.syntactic.sentence(hop_sid).expect("hop sid resolves");
        assert_eq!(s.elements.len(), 3, "hop cites the (located b c) fact");
    }
}
