// crates/core/src/syntactic/caches/term_facts.rs
//
// `syntactic::term_facts` — KB-lifetime structural facts of a stored
// sentence/term: groundness, leaf count, depth, and a 64-bit Bloom
// signature of its symbol/operator content.  Content-addressed like the
// store itself (`SentenceId` = content hash), so a fact can never go
// STALE — a removed sid's entry is evicted for memory hygiene only, and
// an identical re-add recomputes the identical value.
//
// This is tier 1 of the two-tier ground-term identity design: KB-stable
// term facts ride the sentence store's hash-cons.  The prover-side peer
// (`saturate::terms::TermFactsTable`) lives in the SAME 64-bit keyspace
// (the `AtomTable` interns with the identical content hash), so facts
// computed here apply directly to KB-origin subtrees the prover lifts.
// KBO weight is deliberately NOT here — it is prover-strategy-adjacent
// and lives in the prover-side table.
//
// Lazy compute-on-miss (zero eager cost), memoized per sid, recursing
// through `Element::Sub` via the cache itself so every sub-sentence's
// facts memoize under their own id on the way up.  Invalidation:
// `RootRemoved` carries the removed bodies (root + orphaned subs), and
// each body's content hash is evicted.
//
// DESIGN DELTA (documented): the work order suggested also consuming
// `FormulasRecycled`, "copying the sibling caches' consumes list".
// Verified against the seams: `FormulasRecycled { nodes }` carries
// SOURCE-node fingerprints (not sentence ids) and no sid-keyed sibling
// consumes it; the sid-carrying removal event every sid-keyed sibling
// (axiom_index, sine, session) consumes is `RootRemoved`, whose payload
// (`sentences`) is exactly the removed bodies.  `RootRemoved` alone is
// therefore the correct (and sufficient) invalidation seam here.

use crate::cache::events::{Event, EventKind};
use crate::cache::{CacheBehavior, EntryCache};
use crate::parse::OpKind;
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, SentenceId, SymbolId};

/// Structural facts of one stored sentence/term (see module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TermFacts {
    /// No variable anywhere in the tree.
    pub(crate) ground: bool,
    /// Leaf count (saturating) — matches `AtomInfo.size` semantics.
    pub(crate) size: u16,
    /// Term depth (saturating): leaf = 0, so a flat sentence is 1 —
    /// matches `AtomInfo.depth` / the prover's `term_depth` semantics.
    pub(crate) depth: u8,
    /// OR of `1 << (symbol_key % 64)` over every symbol AND operator in
    /// the tree ([`bloom_bit_symbol`] / [`bloom_bit_op`]).  A bit ABSENT
    /// here is a true absence of that key from the whole subtree — the
    /// superset property whole-subtree pruning stands on.  Variables and
    /// string/numeric literals contribute nothing (neither can head a
    /// demodulator left side, so they never key a redex bucket).
    pub(crate) sym_bloom: u64,
}

impl TermFacts {
    /// The "unresolvable sid" husk: never ground (nobody treats it as a
    /// prunable ground subtree) and an all-ones bloom (never prunes).
    pub(crate) fn unknown() -> Self {
        TermFacts { ground: false, size: 0, depth: 0, sym_bloom: !0u64 }
    }
}

/// The Bloom bit of a symbol key — shared by tier 1 (here), the prover's
/// `PTermFacts.sym_bloom`, and `DemodIndex`'s registered head-bit mask,
/// so an intersection test across tiers is meaningful.
#[inline]
pub(crate) fn bloom_bit_symbol(id: SymbolId) -> u64 {
    1u64 << (id & 63)
}

/// The Bloom bit of an operator head.  Key derivation mirrors the
/// prover's `units::op_tag` (`u64::from(op byte)`) — the same key the
/// demodulator index buckets op-headed left sides under.
#[inline]
pub(crate) fn bloom_bit_op(op: &OpKind) -> u64 {
    1u64 << (op_bloom_key(op) & 63)
}

/// The 64-bit key of an operator in the bloom/head-bit keyspace — MUST
/// stay byte-for-byte equal to `saturate::units::op_tag` (asserted by
/// the prover-side tests).
#[inline]
pub(crate) fn op_bloom_key(op: &OpKind) -> u64 {
    u64::from(match op {
        OpKind::And => b'a', OpKind::Or => b'o', OpKind::Not => b'n',
        OpKind::Implies => b'i', OpKind::Iff => b'f', OpKind::Equal => b'e',
        OpKind::ForAll => b'A', OpKind::Exists => b'E',
    })
}

/// Behavior for the `syntactic::term_facts` cache (enabled by default;
/// lazy, so an unconsulted KB pays nothing).
#[derive(Debug, Default)]
pub(crate) struct TermFactsCache;

impl CacheBehavior for TermFactsCache {
    type Parent = SyntacticLayer;
    type Key    = SentenceId;
    type Value  = TermFacts;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "syntactic::term_facts";

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::RootRemoved]
    }

    fn generate(&self, parent: &SyntacticLayer, &sid: &SentenceId) -> TermFacts {
        let Some(sent) = parent.sentence(sid) else {
            return TermFacts::unknown();
        };
        let mut ground = true;
        let mut size: u16 = 0;
        let mut depth: u8 = 0;
        let mut sym_bloom = 0u64;
        for el in sent.elements.iter() {
            match el {
                Element::Variable { .. } => {
                    ground = false;
                    size = size.saturating_add(1);
                }
                Element::Symbol(s) => {
                    sym_bloom |= bloom_bit_symbol(s.id());
                    size = size.saturating_add(1);
                }
                Element::Literal(_) => {
                    size = size.saturating_add(1);
                }
                Element::Op(op) => {
                    sym_bloom |= bloom_bit_op(op);
                    size = size.saturating_add(1);
                }
                Element::Sub(sub) => {
                    // Recurse through the cache itself: the sub-sentence's
                    // facts memoize under its own sid on the way up.
                    let f = parent.term_facts(*sub);
                    ground &= f.ground;
                    size = size.saturating_add(f.size);
                    depth = depth.max(f.depth);
                    sym_bloom |= f.sym_bloom;
                }
            }
        }
        TermFacts { ground, size, depth: depth.saturating_add(1), sym_bloom }
    }

    /// Evict the removed root and every removed body it carried (the
    /// orphaned sub-sentences ride along in the event payload, so no
    /// store read-back is needed).  Content addressing makes this pure
    /// memory hygiene — a surviving shared sub-sentence keeps its entry,
    /// and a re-added identical sentence recomputes the identical facts.
    fn react(
        &self,
        _parent: &SyntacticLayer,
        events: &[&Event],
        store:  &EntryCache<SentenceId, TermFacts>,
        _side:  &(),
    ) -> Vec<Event> {
        let mut evict: Vec<SentenceId> = Vec::new();
        for ev in events {
            if let Event::RootRemoved { sid, sentences } = ev {
                evict.push(*sid);
                evict.extend(sentences.iter().map(|s| s.hash()));
            }
        }
        if !evict.is_empty() {
            store.evict_keys(&evict);
        }
        Vec::new()
    }
}

impl SyntacticLayer {
    /// The memoized structural facts of `sid` (see [`TermFacts`]).
    pub(crate) fn term_facts(&self, sid: SentenceId) -> TermFacts {
        self.term_facts.get(self, sid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntactic::SyntacticLayer;
    use crate::types::SentenceId;

    fn roots(layer: &SyntacticLayer) -> Vec<SentenceId> {
        layer.root_sids()
    }

    /// The sid of the first `Element::Sub` child of `sid`'s sentence.
    fn first_sub(layer: &SyntacticLayer, sid: SentenceId) -> SentenceId {
        let sent = layer.sentence(sid).expect("sentence resolves");
        sent.elements.iter().find_map(|el| match el {
            Element::Sub(s) => Some(*s),
            _ => None,
        }).expect("a Sub child exists")
    }

    #[test]
    fn facts_fold_ground_size_depth_and_bloom_through_subs() {
        let mut layer = SyntacticLayer::default();
        layer.load_kif_assert("(instance (MealFn Breakfast) Meal)", "a.kif");
        let sid = roots(&layer)[0];
        let f = layer.term_facts(sid);
        assert!(f.ground, "no variables anywhere");
        assert_eq!(f.size, 4, "instance, MealFn, Breakfast, Meal");
        assert_eq!(f.depth, 2, "one nested compound under the root");
        for name in ["instance", "MealFn", "Breakfast", "Meal"] {
            let bit = bloom_bit_symbol(crate::types::Symbol::hash_name(name));
            assert_ne!(f.sym_bloom & bit, 0, "bloom carries {name}");
        }
        // The nested compound memoized under its own sid, with its own facts.
        let sub = first_sub(&layer, sid);
        let fs = layer.term_facts(sub);
        assert!(fs.ground);
        assert_eq!(fs.size, 2);
        assert_eq!(fs.depth, 1);

        // An open formula is non-ground (and its bloom still carries its
        // symbols — content, not groundness).
        let mut layer2 = SyntacticLayer::default();
        layer2.load_kif_assert("(=> (instance ?X Dog) (mammal ?X))", "b.kif");
        let open = roots(&layer2)[0];
        let fo = layer2.term_facts(open);
        assert!(!fo.ground);
        let dog = bloom_bit_symbol(crate::types::Symbol::hash_name("Dog"));
        assert_ne!(fo.sym_bloom & dog, 0);
    }

    // The Part-1 gate test: a removal event evicts the root's entry AND
    // the orphaned sub-sentence entries carried on the event.
    #[test]
    fn term_facts_invalidated_on_root_removed() {
        let mut layer = SyntacticLayer::default();
        layer.load_kif_assert("(instance (MealFn Breakfast) Meal)", "a.kif");
        let sid = roots(&layer)[0];
        let sub = first_sub(&layer, sid);

        // Warm both entries (the root recurses through the sub).
        let _ = layer.term_facts(sid);
        assert!(layer.term_facts.peek(&sid).is_some(), "root memoized");
        assert!(layer.term_facts.peek(&sub).is_some(), "sub memoized on the way up");

        // Empty re-ingest of the file retracts the root (RootRemoved,
        // carrying the root + orphaned sub bodies).
        layer.load_kif_assert("", "a.kif");
        assert!(layer.term_facts.peek(&sid).is_none(), "root entry evicted");
        assert!(layer.term_facts.peek(&sub).is_none(), "orphaned sub entry evicted");
    }
}
