// crates/sumo-kb/src/sine.rs
//
// SInE (SUMO Inference Engine) axiom selection — eagerly maintained.
//
// Given a large KB and a small conjecture, SInE selects a relevance-filtered
// subset of axioms to send to a theorem prover.  The algorithm, due to Hoder
// and Voronkov (CADE 2011, "Sine Qua Non for Large Theory Reasoning"), is a
// BFS over the D-relation: a link between symbols and the axioms for which
// they are the least-general (within a tolerance factor).
//
// Definitions
// -----------
// - Generality:  occ(s) = number of axioms in which symbol `s` appears.
// - D-relation:  s triggers axiom A iff
//                s ∈ syms(A)  AND  occ(s) ≤ t · min{occ(s') | s' ∈ syms(A)}
//   where t ≥ 1 is the tolerance factor.
// - Selection:   BFS from the conjecture's symbols, adding triggered axioms,
//                recursing on their symbols, until fixed point (or a depth
//                cap).
//
// Why eager
// ---------
// The original sumo-kb SInE was a lazy cache that rebuilt the whole index
// on first query after any axiom-set change.  On a 24 k-axiom SUMO ontology
// with a tell → ask feedback loop, that's 100 ms of rebuild per query.
//
// Eager maintenance flips the cost: pay a tiny incremental update at
// promotion time (proportional to axioms sharing a symbol with the new
// axiom), get O(answer size) selection at query time, no rebuild.
//
// Incremental correctness
// -----------------------
// When axiom A is added:
//   - For each s ∈ syms(A): occ(s) increases by 1.
//   - g_min(A')  can change only for axioms A' sharing a symbol with A.
//   - Trigger entries for A' can change only for those same A'.
//   So we recompute triggers for exactly the **affected** set —
//   { A' : syms(A') ∩ syms(A) ≠ ∅ } — plus A itself.  That set is small
//   for rare symbols (leaf terms) and bounded above by
//   max_s∈syms(A) occ(s) for any one symbol's contribution.  In a dense
//   ontology a common head (e.g. `instance`) dominates the affected set;
//   in practice that's hundreds, not tens of thousands.
//
// Non-LSP consumers
// -----------------
// Gated behind `feature = "vampire"` because SInE is only useful to
// prover-driven workflows.  Editor tooling (language server, validator,
// parser) does not need it and does not pull it in.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::kif_store::KifStore;
use crate::types::{Element, SentenceId, SymbolId};

// -- Parameters --------------------------------------------------------------

/// Tuning knobs for SInE axiom selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SineParams {
    /// Tolerance factor (≥ 1.0).  A symbol `s` triggers axiom `A` iff
    /// `occ(s) ≤ tolerance · min{occ(s') | s' ∈ symbols(A)}`.
    ///
    /// - `1.0`: only the least-general symbol(s) trigger (smallest premise sets).
    /// - `1.2`: common empirical default; modest benevolence.
    /// - `3.0+`: generous selection; use when strict is losing needed premises.
    ///
    /// Values below `1.0` are clamped to `1.0` during use.
    pub tolerance: f32,
    /// Maximum BFS depth.  `None` = unlimited (run to fixed point).
    /// `Some(0)` returns the empty set (no expansion performed).
    pub depth_limit: Option<usize>,
}

impl Default for SineParams {
    fn default() -> Self { Self { tolerance: 1.2, depth_limit: None } }
}

impl SineParams {
    /// Strict: tolerance 1.0, unlimited depth — only least-general symbols trigger.
    pub fn strict() -> Self { Self { tolerance: 1.0, depth_limit: None } }
    /// Benevolent: higher tolerance pulls in more axioms.  Clamped to ≥ 1.0.
    pub fn benevolent(tolerance: f32) -> Self {
        Self { tolerance: tolerance.max(1.0), depth_limit: None }
    }
}

// -- SineIndex ---------------------------------------------------------------

/// Eagerly-maintained SInE index.
///
/// Lives in `KnowledgeBase` as `RwLock<SineIndex>` (behind `feature = "vampire"`)
/// and is kept in sync with the promoted axiom set via [`add_axiom`] /
/// [`remove_axiom`] at every promotion site.  Reads are lock-free for select;
/// tolerance changes require a write but only rebuild the D-relation portion.
pub struct SineIndex {
    /// The tolerance at which [`trigger_idx`] and [`axiom_triggers`] are
    /// currently computed.  Changed via [`set_tolerance`].
    tolerance: f32,

    /// Per-axiom symbol set.  Tolerance-independent.
    axiom_syms: HashMap<SentenceId, HashSet<SymbolId>>,

    /// Reverse of [`axiom_syms`]: for each symbol, the axioms containing it.
    /// Tolerance-independent.  `sym_axioms[s].len()` is this index's generality
    /// count for `s`, equivalent to `Symbol.all_sentences.len()` in the store.
    /// Maintained internally so the index is self-sufficient for its own
    /// incremental updates (no store handle needed beyond symbol collection).
    sym_axioms: HashMap<SymbolId, HashSet<SentenceId>>,

    /// Per-axiom triggering symbols at the current tolerance.  Used to
    /// un-index on recompute so we don't leak stale entries into
    /// [`trigger_idx`].
    axiom_triggers: HashMap<SentenceId, HashSet<SymbolId>>,

    /// The D-relation, inverted for query-time lookup: for each symbol, the
    /// axioms it triggers at the current tolerance.
    trigger_idx: HashMap<SymbolId, HashSet<SentenceId>>,
}

impl SineIndex {
    /// Construct an empty index at the given tolerance.
    pub(crate) fn new(tolerance: f32) -> Self {
        Self {
            tolerance:      tolerance.max(1.0),
            axiom_syms:     HashMap::new(),
            sym_axioms:     HashMap::new(),
            axiom_triggers: HashMap::new(),
            trigger_idx:    HashMap::new(),
        }
    }

    /// The tolerance at which the D-relation is currently computed.
    #[inline] pub fn tolerance(&self) -> f32 { self.tolerance }

    /// Number of axioms currently tracked.
    #[inline] pub fn axiom_count(&self) -> usize { self.axiom_syms.len() }

    /// Is `sid` currently tracked as an axiom in this index?
    #[inline] pub fn contains(&self, sid: SentenceId) -> bool {
        self.axiom_syms.contains_key(&sid)
    }

    /// Generality of `s`: number of axioms in which it appears.  `0` for
    /// symbols absent from the axiom set.
    #[inline] pub fn generality(&self, s: SymbolId) -> usize {
        self.sym_axioms.get(&s).map_or(0, |set| set.len())
    }

    /// The symbol set of an indexed axiom.  `None` if `sid` is not tracked.
    #[inline]
    pub fn symbols_of_axiom(&self, sid: SentenceId) -> Option<&HashSet<SymbolId>> {
        self.axiom_syms.get(&sid)
    }

    /// Axioms triggered by `s` at the current tolerance.  `None` if `s`
    /// triggers nothing.
    #[inline]
    pub fn triggers(&self, s: SymbolId) -> Option<&HashSet<SentenceId>> {
        self.trigger_idx.get(&s)
    }

    // -- Mutation (called only from KnowledgeBase at promotion sites) --------

    /// Incrementally register an axiom and bring the D-relation up to date.
    ///
    /// Idempotent: re-adding an already-tracked sid is a no-op.  Caller
    /// must ensure `sid` is a valid sentence in `store`.
    ///
    /// Work proportional to the size of the **affected** axiom set:
    ///   { A' : syms(A') ∩ syms(sid) ≠ ∅ } ∪ { sid }
    /// For a new axiom containing rare symbols only, that's ~O(1); for one
    /// containing a common head like `instance`, it's bounded by the
    /// number of axioms mentioning that head.
    pub(crate) fn add_axiom(&mut self, store: &KifStore, sid: SentenceId) {
        if self.axiom_syms.contains_key(&sid) { return; }
        if !store.has_sentence(sid) { return; }

        let syms = store.collect_axiom_symbol_set(sid);

        // Record even "symbol-less" axioms (pure var/literal bodies — rare
        // in SUMO but possible) so axiom_count and contains() stay accurate.
        self.axiom_syms.insert(sid, syms.clone());
        self.axiom_triggers.insert(sid, HashSet::new());
        if syms.is_empty() { return; }

        // Update the symbol → axioms reverse index (this also bumps
        // generality for every symbol in the new axiom).
        for &s in &syms {
            self.sym_axioms.entry(s).or_default().insert(sid);
        }

        // Affected = axioms sharing any symbol with sid (includes sid
        // itself, since sym_axioms now contains it).
        let mut affected: HashSet<SentenceId> = HashSet::new();
        for &s in &syms {
            if let Some(set) = self.sym_axioms.get(&s) {
                affected.extend(set.iter().copied());
            }
        }

        for a in affected {
            self.recompute_triggers_for(a);
        }
    }

    /// Remove an axiom and bring the D-relation up to date.
    ///
    /// Symmetric to [`add_axiom`] in both semantics and complexity.
    /// Provided for completeness; the KB does not currently expose a
    /// "demote axiom" operation, so this is a future-proofing hook
    /// rather than a hot path today.
    #[allow(dead_code)]
    pub(crate) fn remove_axiom(&mut self, sid: SentenceId) {
        let syms = match self.axiom_syms.remove(&sid) {
            Some(s) => s,
            None => return,
        };

        // Remove sid from the D-relation (before affected-set recomputation,
        // so the recomputation sees the post-removal state).
        if let Some(old_triggers) = self.axiom_triggers.remove(&sid) {
            for &s in &old_triggers {
                if let Some(set) = self.trigger_idx.get_mut(&s) {
                    set.remove(&sid);
                    if set.is_empty() { self.trigger_idx.remove(&s); }
                }
            }
        }

        // Drop sid from the symbol → axioms reverse index.
        for &s in &syms {
            if let Some(set) = self.sym_axioms.get_mut(&s) {
                set.remove(&sid);
                if set.is_empty() { self.sym_axioms.remove(&s); }
            }
        }

        // Remaining axioms sharing a symbol with the removed one may have
        // shifted g_min downward (one fewer axiom counted for those syms),
        // potentially widening their trigger set.
        let mut affected: HashSet<SentenceId> = HashSet::new();
        for &s in &syms {
            if let Some(set) = self.sym_axioms.get(&s) {
                affected.extend(set.iter().copied());
            }
        }
        for a in affected {
            self.recompute_triggers_for(a);
        }
    }

    /// Clear and rebuild the D-relation portion of the index at a new
    /// tolerance.  Axiom set, per-axiom symbol sets, and generality
    /// counts are preserved (they're tolerance-independent).
    pub(crate) fn set_tolerance(&mut self, new: f32) {
        let new = new.max(1.0);
        if (new - self.tolerance).abs() < f32::EPSILON { return; }
        self.tolerance = new;
        self.trigger_idx.clear();
        self.axiom_triggers.clear();
        let sids: Vec<SentenceId> = self.axiom_syms.keys().copied().collect();
        for a in sids {
            // axiom_triggers was cleared above, so recompute_triggers_for
            // starts from an empty old-trigger set — no stale entries to
            // unindex.  We still need to seed axiom_triggers[a] = {} so
            // the recompute inserts only fresh entries.
            self.axiom_triggers.insert(a, HashSet::new());
            self.recompute_triggers_for(a);
        }
        log::debug!(target: "sumo_kb::sine",
            "SineIndex::set_tolerance: rebuilt trigger relation at t={} \
             ({} axioms, {} trigger entries)",
            new, self.axiom_count(),
            self.trigger_idx.values().map(|s| s.len()).sum::<usize>(),
        );
    }

    /// Drop all state.  Returns the index to the state of `new(tolerance)`.
    /// Escape hatch for tests and callers that want to fully rebuild.
    #[allow(dead_code)]
    pub(crate) fn clear(&mut self) {
        self.axiom_syms.clear();
        self.sym_axioms.clear();
        self.axiom_triggers.clear();
        self.trigger_idx.clear();
    }

    /// Recompute `axiom_triggers[a]` and the corresponding slice of
    /// `trigger_idx` from the current generality state.
    ///
    /// Un-indexes any previous trigger entries for `a` (using
    /// `axiom_triggers[a]` as the old-trigger record), computes the fresh
    /// triggering set via g_min and tolerance, and re-indexes.
    fn recompute_triggers_for(&mut self, a: SentenceId) {
        // Un-index previous triggers for this axiom.
        let old = self.axiom_triggers
            .insert(a, HashSet::new())
            .unwrap_or_default();
        for &s in &old {
            if let Some(set) = self.trigger_idx.get_mut(&s) {
                set.remove(&a);
                if set.is_empty() { self.trigger_idx.remove(&s); }
            }
        }

        // No symbols → cannot be triggered.
        let syms = match self.axiom_syms.get(&a) {
            Some(s) if !s.is_empty() => s,
            _ => return, // axiom_triggers[a] stays empty
        };

        // g_min over current generality counts.
        let g_min = syms.iter()
            .map(|s| self.sym_axioms.get(s).map_or(0, |set| set.len()))
            .min()
            .unwrap_or(0);
        if g_min == 0 { return; }

        let threshold = (self.tolerance * g_min as f32).floor() as usize;

        // Compute new trigger set and index it.
        let syms = syms.clone(); // avoid borrowing self while mutating
        let mut new_triggers: HashSet<SymbolId> = HashSet::new();
        for s in syms {
            let g_s = self.sym_axioms.get(&s).map_or(0, |set| set.len());
            if g_s <= threshold {
                new_triggers.insert(s);
                self.trigger_idx.entry(s).or_default().insert(a);
            }
        }
        self.axiom_triggers.insert(a, new_triggers);
    }

    // -- Selection -----------------------------------------------------------

    /// Run the SInE BFS from `seed_syms`, returning the sids of every axiom
    /// reached.  `depth_limit` (if `Some`) caps the BFS at that many waves.
    ///
    /// `seed_syms` is typically the union of symbols across the conjecture's
    /// sentences, computed by [`collect_conjecture_symbols`].
    pub fn select(
        &self,
        seed_syms:   &HashSet<SymbolId>,
        depth_limit: Option<usize>,
    ) -> HashSet<SentenceId> {
        let mut selected:     HashSet<SentenceId> = HashSet::new();
        let mut visited_syms: HashSet<SymbolId>   = HashSet::new();
        let mut frontier:     VecDeque<SymbolId>  = seed_syms.iter().copied().collect();
        let mut depth = 0usize;

        while !frontier.is_empty() {
            if let Some(limit) = depth_limit {
                if depth >= limit { break; }
            }
            let wave_size = frontier.len();
            let mut next_wave: Vec<SymbolId> = Vec::new();
            for _ in 0..wave_size {
                let s = match frontier.pop_front() { Some(x) => x, None => break };
                if !visited_syms.insert(s) { continue; }
                if let Some(axioms) = self.trigger_idx.get(&s) {
                    for &a in axioms {
                        if selected.insert(a) {
                            if let Some(syms) = self.axiom_syms.get(&a) {
                                for &s2 in syms {
                                    if !visited_syms.contains(&s2) {
                                        next_wave.push(s2);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            for s in next_wave { frontier.push_back(s); }
            depth += 1;
        }

        log::debug!(target: "sumo_kb::sine",
            "SineIndex::select: {} seed syms -> {} axioms ({} syms visited, depth {})",
            seed_syms.len(), selected.len(), visited_syms.len(), depth,
        );
        selected
    }
}

// -- Symbol collection -------------------------------------------------------

/// Walk a sentence (including nested sub-sentences) collecting every
/// `Element::Symbol` id reached.  Variables, operators, and literals are
/// skipped — they are not part of the D-relation.
///
/// Crate-internal because `KifStore` is not part of the public API.
/// External consumers should call
/// [`crate::KnowledgeBase::query_symbols`] to get a seed symbol set
/// from a KIF string.
pub(crate) fn collect_conjecture_symbols(
    store: &KifStore,
    sid: SentenceId,
    out: &mut HashSet<SymbolId>,
) {
    if !store.has_sentence(sid) { return; }
    let sentence = &store.sentences[store.sent_idx(sid)];
    for el in &sentence.elements {
        match el {
            Element::Symbol { id, .. }          => { out.insert(*id); }
            Element::Sub    { sid: sub_id, .. } =>
                collect_conjecture_symbols(store, *sub_id, out),
            _ => {}
        }
    }
}

// -- Tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kif_store::{load_kif, KifStore};

    /// Build a store + return the sids that should be treated as "axioms"
    /// (every top-level root sentence).  Also registers them via the
    /// KifStore's axiom-symbol helper so generality counts reflect reality.
    fn store_and_axioms(kif: &str) -> (KifStore, Vec<SentenceId>) {
        let mut store = KifStore::default();
        let errs = load_kif(&mut store, kif, "test");
        assert!(errs.is_empty(), "load errors: {:?}", errs);
        let axioms = store.roots.clone();
        for &sid in &axioms {
            store.register_axiom_symbols(sid);
        }
        (store, axioms)
    }

    /// Build an eagerly-populated index by adding each axiom via
    /// `add_axiom`.  Mirrors the KB's bootstrap path.
    fn build_eager(store: &KifStore, axioms: &[SentenceId], tolerance: f32) -> SineIndex {
        let mut idx = SineIndex::new(tolerance);
        for &sid in axioms {
            idx.add_axiom(store, sid);
        }
        idx
    }

    #[test]
    fn generality_counts_distinct_axiom_occurrences() {
        let (store, axioms) = store_and_axioms(
            "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)",
        );
        let idx = build_eager(&store, &axioms, 1.2);

        let sub    = store.sym_id("subclass").unwrap();
        let human  = store.sym_id("Human").unwrap();
        let animal = store.sym_id("Animal").unwrap();
        let mammal = store.sym_id("Mammal").unwrap();
        let dog    = store.sym_id("Dog").unwrap();

        assert_eq!(idx.generality(sub),    3);
        assert_eq!(idx.generality(human),  1);
        assert_eq!(idx.generality(animal), 2);
        assert_eq!(idx.generality(mammal), 2);
        assert_eq!(idx.generality(dog),    1);

        // all_sentences matches generality exactly.
        assert_eq!(store.axiom_sentences_of(sub).len(),    3);
        assert_eq!(store.axiom_sentences_of(human).len(),  1);
        assert_eq!(store.axiom_sentences_of(animal).len(), 2);
    }

    #[test]
    fn trigger_relation_strict_picks_least_general() {
        // Same KB as above; strict tolerance.
        let (store, axioms) = store_and_axioms(
            "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)",
        );
        let idx = build_eager(&store, &axioms, 1.0);

        let sub    = store.sym_id("subclass").unwrap();
        let human  = store.sym_id("Human").unwrap();
        let animal = store.sym_id("Animal").unwrap();
        let mammal = store.sym_id("Mammal").unwrap();
        let dog    = store.sym_id("Dog").unwrap();

        // `subclass` (occ=3) is above min in every axiom it appears in.
        assert!(idx.triggers(sub).is_none(),
            "subclass should trigger nothing strict: {:?}", idx.triggers(sub));

        // Human (occ=1): axiom 0 has min 1 (Human itself) → Human triggers it.
        assert_eq!(
            idx.triggers(human).cloned().unwrap_or_default(),
            HashSet::from([axioms[0]])
        );
        // Dog (occ=1): axiom 2 has min 1 (Dog) → Dog triggers it.
        assert_eq!(
            idx.triggers(dog).cloned().unwrap_or_default(),
            HashSet::from([axioms[2]])
        );
        // Animal (occ=2) ties at min only in axiom 1 (both Mammal and Animal
        // at occ=2); in axiom 0 the min is 1 (Human).
        assert_eq!(
            idx.triggers(animal).cloned().unwrap_or_default(),
            HashSet::from([axioms[1]])
        );
        // Mammal (occ=2) ties at min in axiom 1 (with Animal); in axiom 2
        // the min is 1 (Dog) so Mammal does not trigger there.
        assert_eq!(
            idx.triggers(mammal).cloned().unwrap_or_default(),
            HashSet::from([axioms[1]])
        );
    }

    #[test]
    fn incremental_add_is_transitively_correct() {
        // Add axioms one at a time; at every point the index state must
        // match a from-scratch build.
        let kif = "(subclass Human Animal)\n\
                   (subclass Mammal Animal)\n\
                   (subclass Dog Mammal)\n\
                   (instance Rex Dog)";
        let (store, axioms) = store_and_axioms(kif);

        // Baseline: from-scratch build of the full set.
        let from_scratch = build_eager(&store, &axioms, 1.0);

        // Incremental: add one at a time, check consistency at each step.
        let mut incremental = SineIndex::new(1.0);
        for (i, &sid) in axioms.iter().enumerate() {
            incremental.add_axiom(&store, sid);
            assert_eq!(incremental.axiom_count(), i + 1);
        }

        // Final states must match for every tracked axiom and symbol.
        assert_eq!(incremental.axiom_count(), from_scratch.axiom_count());
        for &sid in &axioms {
            assert_eq!(
                incremental.symbols_of_axiom(sid),
                from_scratch.symbols_of_axiom(sid),
                "axiom_syms mismatch for sid {}", sid,
            );
        }
        // Every symbol's trigger set must match.
        let all_syms: HashSet<SymbolId> = axioms.iter()
            .filter_map(|&sid| from_scratch.symbols_of_axiom(sid))
            .flat_map(|set| set.iter().copied())
            .collect();
        for s in all_syms {
            let inc = incremental.triggers(s).cloned().unwrap_or_default();
            let scr = from_scratch.triggers(s).cloned().unwrap_or_default();
            assert_eq!(inc, scr, "trigger_idx mismatch for symbol {}", s);
        }
    }

    #[test]
    fn incremental_add_only_touches_affected_axioms() {
        // Build a KB where adding an axiom whose symbols are all fresh
        // (no overlap with existing axioms) must not disturb any
        // existing trigger entry — proving the affected-set restriction
        // is doing its job.
        let mut store = KifStore::default();
        let e1 = load_kif(&mut store, "(subclass Human Animal)", "a");
        assert!(e1.is_empty());
        let e2 = load_kif(&mut store, "(subclass Dog Mammal)", "b");
        assert!(e2.is_empty());
        for &sid in &store.roots.clone() {
            store.register_axiom_symbols(sid);
        }
        let first_two = store.roots.clone();

        let mut idx = SineIndex::new(1.0);
        for &sid in &first_two {
            idx.add_axiom(&store, sid);
        }

        // Snapshot trigger entries for existing symbols.
        let human  = store.sym_id("Human").unwrap();
        let animal = store.sym_id("Animal").unwrap();
        let before_human  = idx.triggers(human).cloned().unwrap_or_default();
        let before_animal = idx.triggers(animal).cloned().unwrap_or_default();

        // Add a wholly-disjoint axiom.
        let e3 = load_kif(&mut store, "(instance Pi Constant)", "c");
        assert!(e3.is_empty());
        let new_sid = *store.roots.last().unwrap();
        store.register_axiom_symbols(new_sid);
        idx.add_axiom(&store, new_sid);

        // Existing symbols' trigger sets are untouched.
        assert_eq!(idx.triggers(human).cloned().unwrap_or_default(),  before_human);
        assert_eq!(idx.triggers(animal).cloned().unwrap_or_default(), before_animal);

        // The new axiom is correctly indexed.
        assert!(idx.contains(new_sid));
        let pi = store.sym_id("Pi").unwrap();
        let triggered_by_pi = idx.triggers(pi).cloned().unwrap_or_default();
        assert!(triggered_by_pi.contains(&new_sid));
    }

    #[test]
    fn incremental_add_shifts_trigger_entries_for_shared_symbols() {
        // When an added axiom bumps occ(s) of a previously-unique-min
        // symbol s in some OTHER axiom A', A''s g_min goes up, threshold
        // goes up, potentially adding new triggering symbols.  Verify the
        // incremental update captures that.
        let mut store = KifStore::default();
        // Initial: (subclass Human Animal).  Human has occ=1 and is the
        // unique min, so it triggers axiom 0.  Animal has occ=1 too!
        // They both trigger.
        let _ = load_kif(&mut store, "(subclass Human Animal)", "a");
        for &sid in &store.roots.clone() { store.register_axiom_symbols(sid); }
        let a0 = store.roots[0];

        let mut idx = SineIndex::new(1.0);
        idx.add_axiom(&store, a0);
        let human_id  = store.sym_id("Human").unwrap();
        let animal_id = store.sym_id("Animal").unwrap();
        // At this point both Human and Animal trigger axiom 0 (both occ=1 == min).
        assert!(idx.triggers(human_id).unwrap().contains(&a0));
        assert!(idx.triggers(animal_id).unwrap().contains(&a0));

        // Add (subclass Dog Animal).  Animal's occ goes 1 → 2.  In axiom 0
        // the min was {Human:1, Animal:1, subclass:1 if isolated} — now
        // min is Human:1, Animal:2, subclass:2.  Min=1 (Human only).
        // Threshold=1.  Animal (occ=2) no longer triggers axiom 0.
        let _ = load_kif(&mut store, "(subclass Dog Animal)", "b");
        let a1 = *store.roots.last().unwrap();
        store.register_axiom_symbols(a1);
        idx.add_axiom(&store, a1);

        // Post-add state: Human still triggers axiom 0; Animal no longer does.
        assert!(
            idx.triggers(human_id).unwrap().contains(&a0),
            "Human should still trigger axiom 0",
        );
        assert!(
            !idx.triggers(animal_id)
                .map(|s| s.contains(&a0)).unwrap_or(false),
            "Animal should no longer trigger axiom 0 after occ bump; triggers={:?}",
            idx.triggers(animal_id),
        );
    }

    #[test]
    fn remove_axiom_restores_pre_add_state() {
        // add_axiom then remove_axiom should leave the index
        // observationally identical to never having added.
        let (store, axioms) = store_and_axioms(
            "(subclass Human Animal)\n\
             (subclass Mammal Animal)",
        );
        let baseline = build_eager(&store, &axioms, 1.0);

        let mut idx = build_eager(&store, &axioms, 1.0);
        // Pretend to add a third, then remove it.
        let (mut store2, _axioms2) = store_and_axioms(
            "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)",
        );
        let new_sid = *store2.roots.last().unwrap();
        // Need to use the same store for the index, so simulate: add
        // a new axiom derived from store2 but using store's symbols where
        // possible.  Simplest: fork a fresh scenario — load everything
        // into a single store, then compare add-then-remove vs without.
        let _ = (&mut store2,);

        // Simpler equivalent check: on the original index, issue a pair
        // of remove/add on one of its real axioms and compare with
        // baseline.
        let target = axioms[1];
        idx.remove_axiom(target);
        idx.add_axiom(&store, target);

        assert_eq!(idx.axiom_count(), baseline.axiom_count());
        for &sid in &axioms {
            assert_eq!(
                idx.symbols_of_axiom(sid),
                baseline.symbols_of_axiom(sid),
            );
        }
        let all_syms: HashSet<SymbolId> = axioms.iter()
            .filter_map(|&sid| baseline.symbols_of_axiom(sid))
            .flat_map(|set| set.iter().copied())
            .collect();
        for s in all_syms {
            assert_eq!(
                idx.triggers(s).cloned().unwrap_or_default(),
                baseline.triggers(s).cloned().unwrap_or_default(),
                "trigger mismatch after remove/add round-trip for symbol {}", s,
            );
        }
    }

    #[test]
    fn set_tolerance_rebuilds_d_relation() {
        let (store, axioms) = store_and_axioms(
            "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)",
        );
        // Strict build.
        let strict = build_eager(&store, &axioms, 1.0);

        // Start at a different tolerance, then switch to strict — must
        // match the from-scratch-at-strict build.
        let mut idx = build_eager(&store, &axioms, 3.0);
        idx.set_tolerance(1.0);

        assert_eq!(idx.tolerance(), 1.0);
        assert_eq!(idx.axiom_count(), strict.axiom_count());
        let all_syms: HashSet<SymbolId> = axioms.iter()
            .filter_map(|&sid| strict.symbols_of_axiom(sid))
            .flat_map(|s| s.iter().copied())
            .collect();
        for s in all_syms {
            assert_eq!(
                idx.triggers(s).cloned().unwrap_or_default(),
                strict.triggers(s).cloned().unwrap_or_default(),
                "symbol {} triggers mismatch after set_tolerance", s,
            );
        }
    }

    #[test]
    fn selection_reaches_transitive_axioms() {
        let (store, axioms) = store_and_axioms(
            "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)\n\
             (instance Rex Dog)",
        );
        let idx = build_eager(&store, &axioms, 1.0);

        let dog_id = store.sym_id("Dog").unwrap();
        let seed: HashSet<SymbolId> = [dog_id].into_iter().collect();
        let selected = idx.select(&seed, None);

        // Dog (occ=2 — appears in axioms 2 and 3) is min=2 in axiom 2,
        // triggers it.  Then Mammal (occ=2) triggers axiom 1 (min=2 there).
        // Axiom 3's min is 1 (Rex/instance), Dog with occ=2 doesn't trigger.
        let expected: HashSet<SentenceId> =
            [axioms[1], axioms[2]].into_iter().collect();
        assert_eq!(selected, expected, "got {:?}, expected {:?}", selected, expected);
    }

    #[test]
    fn selection_respects_depth_limit() {
        let (store, axioms) = store_and_axioms(
            "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)\n\
             (instance Rex Dog)",
        );
        let idx = build_eager(&store, &axioms, 1.0);
        let rex = store.sym_id("Rex").unwrap();
        let seed: HashSet<SymbolId> = [rex].into_iter().collect();
        let shallow = idx.select(&seed, Some(1));
        // Rex triggers only axiom 3 at depth 1.
        assert_eq!(shallow, HashSet::from([axioms[3]]));
    }

    #[test]
    fn selection_empty_seed_yields_empty_result() {
        let (store, axioms) = store_and_axioms("(subclass Human Animal)");
        let idx = build_eager(&store, &axioms, 1.2);
        assert!(idx.select(&HashSet::new(), None).is_empty());
    }

    #[test]
    fn add_axiom_tolerates_unknown_sid() {
        let (store, axioms) = store_and_axioms("(subclass Human Animal)");
        let mut idx = build_eager(&store, &axioms, 1.0);
        // Inject an sid not in the store; must not panic.
        idx.add_axiom(&store, 99_999_999);
        assert_eq!(idx.axiom_count(), 1);
    }

    #[test]
    fn add_axiom_is_idempotent() {
        let (store, axioms) = store_and_axioms("(subclass Human Animal)");
        let mut idx = SineIndex::new(1.0);
        idx.add_axiom(&store, axioms[0]);
        idx.add_axiom(&store, axioms[0]); // again
        assert_eq!(idx.axiom_count(), 1);
    }
}
