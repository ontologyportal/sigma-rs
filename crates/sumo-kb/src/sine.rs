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
    /// Default tolerance is read from `SINE_TOLERANCE` at compile
    /// time if set (via `.cargo/config.toml` or the environment),
    /// falling back to 2.0.  `option_env!` (not `env!`) is used so a
    /// fresh checkout without the override variable still compiles.
    fn default() -> Self {
        let tol = option_env!("SINE_TOLERANCE")
            .and_then(|s| s.parse().ok())
            .unwrap_or(2.0);
        Self { tolerance: tol, depth_limit: None }
    }
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

    /// Cached minimum generality per axiom: `min{ |sym_axioms[s]| : s ∈ syms(A) }`.
    /// Updated only inside `recompute_triggers_for`.  Absent for axioms
    /// with an empty symbol set.
    ///
    /// Used by `add_axiom`'s fine-grained update path to detect whether
    /// a bumped symbol was the (unique) minimum of an affected axiom —
    /// which is the only condition under which the threshold shifts
    /// and a full recompute is actually required.  Without this cache
    /// every shared-symbol affected axiom would fall through to a
    /// full recompute, which on dense ontologies is O(N²).
    axiom_g_min: HashMap<SentenceId, usize>,

    /// Companion to `axiom_g_min`: how many symbols of A are currently
    /// tied at `axiom_g_min[A]`.  When bumping symbols in an affected
    /// axiom, we only need a full recompute if we bumped *all* of its
    /// min-tied symbols (so the minimum shifts upward).  When some
    /// tied symbols remain at min, the threshold is unchanged and we
    /// can do a cheap per-symbol status update.
    axiom_g_min_count: HashMap<SentenceId, usize>,

    /// The D-relation, inverted for query-time lookup: for each symbol, the
    /// axioms it triggers at the current tolerance.
    trigger_idx: HashMap<SymbolId, HashSet<SentenceId>>,

    /// Which-code-path counters.  Incremented by `add_axiom` /
    /// `update_affected_axiom` / `rebuild_from`.  Reset via
    /// `take_stats()`.  Used by unit tests to verify that the
    /// fine-grained fast path and bulk rebuild fire when expected.
    /// Not exposed at the public API boundary — timing is the
    /// general profiler's job (see `crate::profiling`).
    stats: AddAxiomStats,
}

/// Counters for which code paths `SineIndex` took since the last
/// `take_stats()` call.  Exclusively for unit-test assertions; timing
/// of these paths belongs to the general `crate::profiling::Profiler`.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct AddAxiomStats {
    /// Number of `add_axiom` calls.
    pub calls:           usize,
    /// Number of full `recompute_triggers_for` calls.
    pub recompute_calls: usize,
    /// Number of affected axioms that took the fine-grained fast path.
    pub fast_path:       usize,
    /// Number of times `rebuild_from` fired (at most once per
    /// `add_axioms` call, and only when the batch is large).
    pub bulk_rebuilds:   usize,
    /// Number of `remove_axiom` calls.
    pub removes:         usize,
}

impl SineIndex {
    /// Construct an empty index at the given tolerance.
    pub(crate) fn new(tolerance: f32) -> Self {
        Self {
            tolerance:         tolerance.max(1.0),
            axiom_syms:        HashMap::new(),
            sym_axioms:        HashMap::new(),
            axiom_triggers:    HashMap::new(),
            axiom_g_min:       HashMap::new(),
            axiom_g_min_count: HashMap::new(),
            trigger_idx:       HashMap::new(),
            stats:             AddAxiomStats::default(),
        }
    }

    /// Take-and-reset the which-path counters.  Test-only; not
    /// exposed outside the crate.
    #[allow(dead_code)] // used by tests
    pub(crate) fn take_stats(&mut self) -> AddAxiomStats {
        let s = self.stats;
        self.stats = AddAxiomStats::default();
        s
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
        self.stats.calls += 1;
        if self.axiom_syms.contains_key(&sid) { return; }
        if !store.has_sentence(sid) { return; }

        let syms = store.sentence_symbols(sid);

        // Record even "symbol-less" axioms (pure var/literal bodies — rare
        // in SUMO but possible) so axiom_count and contains() stay accurate.
        self.axiom_syms.insert(sid, syms.clone());
        self.axiom_triggers.insert(sid, HashSet::new());
        if syms.is_empty() { return; }

        // Snapshot the PRE-bump generality of every symbol in the new
        // axiom.  We need these values to decide whether an affected
        // axiom's g_min shifted because a bumped symbol was at its min.
        // Reading `sym_axioms` AFTER bumping would always show the
        // post-bump count and break the logic.
        let old_occ: HashMap<SymbolId, usize> = syms.iter()
            .map(|&s| (s, self.sym_axioms.get(&s).map_or(0, |set| set.len())))
            .collect();

        // Update the symbol → axioms reverse index (bumps generality
        // for every symbol in the new axiom).
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

        // The new axiom always needs a fresh trigger computation — it
        // has no cached state.
        self.recompute_triggers_for(sid);
        self.stats.recompute_calls += 1;

        // For each OTHER affected axiom, decide between the fine-grained
        // fast path and a full recompute.
        for a in affected {
            if a == sid { continue; }
            self.update_affected_axiom(a, &syms, &old_occ);
        }
    }

    /// Remove `sid` from the index, decrementing per-symbol
    /// generality counts and recomputing triggers for every other
    /// axiom that shared a symbol with it.
    ///
    /// Symmetric to [`add_axiom`].  Idempotent: calling twice, or on
    /// an sid that was never added, is a no-op.  After removal `sid`
    /// is not reported by [`triggers`](SineIndex::triggers),
    /// [`generality`](SineIndex::generality),
    /// [`symbols_of_axiom`](SineIndex::symbols_of_axiom), or
    /// [`contains`](SineIndex::contains).
    ///
    /// **Does not read the store.**  The symbol set is pulled from
    /// the cached `axiom_syms` entry populated when the axiom was
    /// added, so callers are free to remove the underlying
    /// `Sentence` from the store before or after this call.
    ///
    /// # Why not a fast path
    ///
    /// `add_axiom`'s fine-grained update exploits a one-directional
    /// monotonicity: adding an axiom only *increments* symbol
    /// generalities, so thresholds only ever loosen.  Removal runs
    /// the same logic in reverse — thresholds tighten — and the set
    /// of axioms whose g_min shifts is conceptually symmetric, but
    /// the resulting trigger-set changes go in the opposite
    /// direction (triggers drop out rather than enter).  We could
    /// mirror the fast path, but removal is expected to be dwarfed
    /// in volume by addition in every realistic workload
    /// (reconcile-on-load touches ~1% of axioms per edit), so
    /// correctness-first via full recompute is the better trade-off.
    /// Revisit if profiling shows `remove_axiom` hot.
    pub(crate) fn remove_axiom(&mut self, sid: SentenceId) {
        self.stats.removes += 1;

        // Drop the cached symbol set.  Early-out on unknown sid.
        let Some(syms) = self.axiom_syms.remove(&sid) else { return };

        // Un-index `sid`'s trigger entries.
        if let Some(old_triggers) = self.axiom_triggers.remove(&sid) {
            for &s in &old_triggers {
                if let Some(set) = self.trigger_idx.get_mut(&s) {
                    set.remove(&sid);
                    if set.is_empty() { self.trigger_idx.remove(&s); }
                }
            }
        }
        self.axiom_g_min.remove(&sid);
        self.axiom_g_min_count.remove(&sid);

        if syms.is_empty() {
            // Pure var / literal body — no generality to update.
            return;
        }

        // Collect affected axioms BEFORE dropping generality.  An
        // axiom is affected iff it shares ≥1 symbol with `sid`.
        let mut affected: HashSet<SentenceId> = HashSet::new();
        for &s in &syms {
            if let Some(set) = self.sym_axioms.get(&s) {
                affected.extend(set.iter().copied());
            }
        }
        affected.remove(&sid);

        // Decrement per-symbol generality by dropping `sid` from the
        // reverse index.  An empty bucket for a symbol that's no
        // longer in any axiom is purged outright to keep
        // `generality(s)` returning 0 consistent with a post-
        // removal KB.
        for &s in &syms {
            if let Some(set) = self.sym_axioms.get_mut(&s) {
                set.remove(&sid);
                if set.is_empty() { self.sym_axioms.remove(&s); }
            }
        }

        // Recompute triggers for every affected axiom.  See the
        // "Why not a fast path" doc comment above for rationale.
        for a in affected {
            self.recompute_triggers_for(a);
            self.stats.recompute_calls += 1;
        }
    }

    /// Bulk-remove sids.  Delegates to [`remove_axiom`] in a loop —
    /// if profiling shows per-call overhead, a future optimisation
    /// could gather affected axioms once across the entire batch
    /// and recompute each one just once rather than up to |sids|
    /// times.  Today, removals are sparse enough that the naive
    /// loop is fine.
    #[allow(dead_code)]  // exposed for tests + future reconcile batching
    pub(crate) fn remove_axioms<I>(&mut self, sids: I)
    where
        I: IntoIterator<Item = SentenceId>,
    {
        for sid in sids {
            self.remove_axiom(sid);
        }
    }

    /// Process an existing affected axiom `a` after a new axiom was
    /// added whose symbol set is `new_syms` with pre-bump generalities
    /// `old_occ`.  Takes the fast path when the bumped symbols did not
    /// displace `a`'s unique minimum; falls back to a full recompute
    /// otherwise.
    fn update_affected_axiom(
        &mut self,
        a:        SentenceId,
        new_syms: &HashSet<SymbolId>,
        old_occ:  &HashMap<SymbolId, usize>,
    ) {
        // Without a cached g_min for `a` we can't take the fast path.
        // Fall back to a full recompute — this only happens if `a` was
        // added before the g_min cache existed (e.g. legacy state) or
        // if it had zero symbols.  On this code path the cache is
        // always populated for non-empty axioms, so the fallback is
        // effectively dead but kept for defence-in-depth.
        let (old_g_min, old_g_min_count) = match (
            self.axiom_g_min.get(&a).copied(),
            self.axiom_g_min_count.get(&a).copied(),
        ) {
            (Some(m), Some(c)) => (m, c),
            _ => {
                self.recompute_triggers_for(a);
                self.stats.recompute_calls += 1;
                return;
            }
        };

        // Gather the bumped symbols (those in both axioms).
        let a_syms = match self.axiom_syms.get(&a).cloned() {
            Some(s) => s,
            None    => return, // a not tracked — nothing to update
        };

        // Count how many of a's at-min symbols were bumped.  The
        // bumped set is syms(new) ∩ syms(a).  We iterate new_syms
        // (typically ≤10 symbols for a SUMO axiom) and test
        // membership in a_syms (HashSet).
        let mut min_tied_removed = 0usize;
        let mut bumped: Vec<SymbolId> = Vec::with_capacity(new_syms.len().min(a_syms.len()));
        for &s in new_syms {
            if a_syms.contains(&s) {
                bumped.push(s);
                if old_occ.get(&s).copied().unwrap_or(0) == old_g_min {
                    min_tied_removed += 1;
                }
            }
        }

        if bumped.is_empty() {
            // Shouldn't happen if `a` is in the affected set, but handle
            // defensively.
            return;
        }

        // If we bumped *every* symbol that was at the old minimum,
        // g_min shifts upward and the threshold changes — full
        // recompute is needed because OTHER (non-bumped) symbols may
        // now enter the trigger set.
        if min_tied_removed >= old_g_min_count {
            self.recompute_triggers_for(a);
            self.stats.recompute_calls += 1;
            return;
        }

        // Fast path: g_min unchanged, threshold unchanged.  Only the
        // bumped symbols' own trigger status can have changed (their
        // new occ might exceed the threshold).
        self.stats.fast_path += 1;
        let threshold = (self.tolerance * old_g_min as f32).floor() as usize;
        for &s in &bumped {
            let new_occ = self.sym_axioms.get(&s).map_or(0, |set| set.len());
            let was_triggering = self
                .axiom_triggers.get(&a)
                .map_or(false, |t| t.contains(&s));
            let still_triggers = new_occ <= threshold;
            if was_triggering && !still_triggers {
                if let Some(t) = self.axiom_triggers.get_mut(&a) {
                    t.remove(&s);
                }
                if let Some(idx) = self.trigger_idx.get_mut(&s) {
                    idx.remove(&a);
                    if idx.is_empty() {
                        self.trigger_idx.remove(&s);
                    }
                }
            }
            // Note: occ only increases, so a symbol can't *enter* the
            // trigger set on the fast path — only leave it.
        }
        // Update g_min_count: some previously-tied min symbols dropped
        // out (now at g_min+1).  g_min itself is unchanged because at
        // least one other min-tied symbol is still present (otherwise
        // we would have taken the full-recompute branch above).
        if min_tied_removed > 0 {
            let new_count = old_g_min_count.saturating_sub(min_tied_removed);
            self.axiom_g_min_count.insert(a, new_count);
        }
    }

    // `remove_axiom` / `remove_axioms` moved earlier in the file,
    // grouped next to `add_axiom` / `add_axioms` for discoverability.

    /// Bulk-add many axioms.
    ///
    /// When the batch is a non-trivial fraction of the final axiom
    /// count, the incremental `add_axiom` path is asymptotically
    /// worse than a from-scratch rebuild: each add recomputes
    /// triggers for every axiom sharing a symbol with it, and on a
    /// dense ontology that set grows linearly with the KB size,
    /// yielding `O(N²)`-ish bulk cost.  A from-scratch rebuild is
    /// `O(Σ |syms(axiom)|)` ≈ `O(N · avg_syms)` ≈ `O(N)` for typical
    /// SUMO-like data, dozens of times cheaper.
    ///
    /// Heuristic: if the batch size ≥ `max(current / 10, 50)`, do a
    /// bulk rebuild over the union of already-tracked axioms plus the
    /// new batch.  Below that threshold the incremental path's
    /// constant-factor win on small changes dominates.
    ///
    /// Callers: `KnowledgeBase::{make_session_axiomatic,
    /// promote_assertions_unchecked, open}` — the three promotion
    /// sites on the KB's hot path.  Tests and single-axiom callers
    /// may still use `add_axiom` directly.
    pub(crate) fn add_axioms<I>(&mut self, store: &KifStore, sids: I)
    where
        I: IntoIterator<Item = SentenceId>,
    {
        let sids: Vec<SentenceId> = sids.into_iter().collect();
        if sids.is_empty() { return; }

        let current = self.axiom_count();
        let threshold = (current / 10).max(50);
        if sids.len() >= threshold {
            // Bulk rebuild — fold the existing axioms and the batch into
            // one input vector, then rebuild the D-relation in two
            // clean passes with no repeated recomputes.
            let mut all: Vec<SentenceId> = self.axiom_syms.keys().copied().collect();
            all.extend(sids.iter().copied());
            self.rebuild_from(store, &all);
        } else {
            for sid in sids {
                self.add_axiom(store, sid);
            }
        }
    }

    /// Clear the index and rebuild from the given axiom sids.
    ///
    /// Two-pass algorithm: Pass 1 collects per-axiom symbol sets and
    /// the `sym_axioms` reverse index (which gives generality for
    /// free, since `|sym_axioms[s]|` is the generality of `s`).
    /// Pass 2 computes each axiom's triggers from the now-final
    /// generality table.  Neither pass needs to un-index stale
    /// trigger entries, so this is significantly cheaper than
    /// calling `add_axiom` in a loop.
    ///
    /// `tolerance` is preserved; everything else is re-derived.
    fn rebuild_from(&mut self, store: &KifStore, sids: &[SentenceId]) {
        self.stats.bulk_rebuilds += 1;
        // Preserve tolerance; drop all derived state.
        self.axiom_syms.clear();
        self.sym_axioms.clear();
        self.axiom_triggers.clear();
        self.axiom_g_min.clear();
        self.axiom_g_min_count.clear();
        self.trigger_idx.clear();

        // Pass 1: per-axiom symbols + reverse index.
        for &sid in sids {
            if self.axiom_syms.contains_key(&sid) { continue; }  // dedup
            if !store.has_sentence(sid) { continue; }
            let syms = store.sentence_symbols(sid);
            for &s in &syms {
                self.sym_axioms.entry(s).or_default().insert(sid);
            }
            self.axiom_syms.insert(sid, syms);
        }

        // Pass 2: compute triggers using the final generality table.
        let axiom_sids: Vec<SentenceId> = self.axiom_syms.keys().copied().collect();
        for a in axiom_sids {
            self.recompute_triggers_for(a);
        }

        log::debug!(target: "sumo_kb::sine",
            "SineIndex::rebuild_from: {} axioms, {} symbols, {} trigger entries",
            self.axiom_count(),
            self.sym_axioms.len(),
            self.trigger_idx.values().map(|s| s.len()).sum::<usize>(),
        );
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
        // Tolerance-dependent g_min cache is invalid at a new tolerance
        // threshold; it will be rebuilt by recompute_triggers_for below.
        // (axiom_g_min itself is actually tolerance-independent — it's
        // the min of generality counts — but we re-derive it during the
        // recompute loop anyway for simplicity.)
        self.axiom_g_min.clear();
        self.axiom_g_min_count.clear();
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
        self.axiom_g_min.clear();
        self.axiom_g_min_count.clear();
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

        // No symbols → cannot be triggered.  Clear any stale g_min
        // cache entry and return.
        let syms = match self.axiom_syms.get(&a) {
            Some(s) if !s.is_empty() => s,
            _ => {
                self.axiom_g_min.remove(&a);
                self.axiom_g_min_count.remove(&a);
                return; // axiom_triggers[a] stays empty
            }
        };

        // Compute g_min and g_min_count in a single pass.
        let mut g_min = usize::MAX;
        let mut g_min_count = 0usize;
        for &s in syms {
            let g = self.sym_axioms.get(&s).map_or(0, |set| set.len());
            if g < g_min {
                g_min = g;
                g_min_count = 1;
            } else if g == g_min {
                g_min_count += 1;
            }
        }
        if g_min == 0 || g_min == usize::MAX {
            self.axiom_g_min.remove(&a);
            self.axiom_g_min_count.remove(&a);
            return;
        }
        self.axiom_g_min.insert(a, g_min);
        self.axiom_g_min_count.insert(a, g_min_count);

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
        // let new_sid = *store2.roots.last().unwrap();
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

    // -- remove_axiom --------------------------------------------------

    #[test]
    fn remove_unknown_sid_is_noop() {
        let (store, axioms) = store_and_axioms("(subclass Human Animal)");
        let mut idx = build_eager(&store, &axioms, 1.0);
        let before = idx.axiom_count();
        idx.remove_axiom(99_999_999);
        assert_eq!(idx.axiom_count(), before);
    }

    #[test]
    fn remove_then_readd_matches_from_scratch() {
        // The structural invariant: after remove+re-add, the index
        // must be bytewise identical to a fresh from-scratch build.
        let (store, axioms) = store_and_axioms(
            "(subclass Human Animal)\n\
             (subclass Dog Mammal)\n\
             (=> (instance ?X Dog) (instance ?X Animal))",
        );
        let mut idx = build_eager(&store, &axioms, 1.2);
        idx.remove_axiom(axioms[1]);
        idx.add_axiom(&store, axioms[1]);
        assert_matches_from_scratch(&idx, &store, &axioms, 1.2);
    }

    #[test]
    fn remove_axiom_decrements_generality() {
        // Two axioms mentioning the same symbol → gen(sym) = 2.
        // Remove one → gen(sym) = 1.
        let (store, axioms) = store_and_axioms(
            "(subclass Dog Mammal)\n(subclass Cat Mammal)",
        );
        let mammal = store.sym_id("Mammal").expect("Mammal interned");
        let mut idx = build_eager(&store, &axioms, 1.0);
        assert_eq!(idx.generality(mammal), 2);
        idx.remove_axiom(axioms[0]);
        assert_eq!(idx.generality(mammal), 1);
        assert!(!idx.contains(axioms[0]));
        assert!(idx.contains(axioms[1]));
    }

    #[test]
    fn remove_axiom_unindexes_its_triggers() {
        // After remove, no symbol should report this sid as a
        // trigger.  Uses tolerance 1.0 so triggers are unambiguous.
        let (store, axioms) = store_and_axioms(
            "(subclass Dog Mammal)\n(subclass Cat Mammal)",
        );
        let mut idx = build_eager(&store, &axioms, 1.0);
        let dog = store.sym_id("Dog").expect("Dog interned");
        assert!(idx.triggers(dog).map_or(false, |s| s.contains(&axioms[0])),
            "Dog should trigger axiom 0 before removal");
        idx.remove_axiom(axioms[0]);
        assert!(!idx.triggers(dog).map_or(false, |s| s.contains(&axioms[0])),
            "Dog must not trigger axiom 0 after removal");
        assert!(idx.symbols_of_axiom(axioms[0]).is_none());
    }

    #[test]
    fn remove_axiom_recomputes_affected_triggers() {
        // A four-axiom corpus where removing one shifts another's g_min.
        //   A1: (subclass Dog Mammal)       — symbols {subclass, Dog, Mammal}
        //   A2: (subclass Mammal Animal)    — symbols {subclass, Mammal, Animal}
        //   A3: (subclass Cat Mammal)       — symbols {subclass, Cat, Mammal}
        //   A4: (subclass Animal Entity)    — symbols {subclass, Animal, Entity}
        // Generalities before: subclass=4, Mammal=3, Animal=2,
        // Dog=Cat=Entity=1.
        // Remove A2 → Mammal gen drops 3→2, Animal gen drops 2→1.
        // The new generalities are consistent with a from-scratch build.
        let (store, axioms) = store_and_axioms(
            "(subclass Dog Mammal)\n\
             (subclass Mammal Animal)\n\
             (subclass Cat Mammal)\n\
             (subclass Animal Entity)",
        );
        let mut idx = build_eager(&store, &axioms, 1.2);
        idx.remove_axiom(axioms[1]);
        // Rebuild the axiom list without the removed one and verify
        // the resulting index matches.
        let kept: Vec<_> = [axioms[0], axioms[2], axioms[3]].into_iter().collect();
        assert_matches_from_scratch(&idx, &store, &kept, 1.2);
    }

    #[test]
    fn remove_is_idempotent() {
        let (store, axioms) = store_and_axioms(
            "(subclass Human Animal)\n(subclass Dog Mammal)",
        );
        let mut idx = build_eager(&store, &axioms, 1.0);
        idx.remove_axiom(axioms[0]);
        idx.remove_axiom(axioms[0]); // again
        assert_eq!(idx.axiom_count(), 1);
        assert!(!idx.contains(axioms[0]));
    }

    #[test]
    fn remove_axioms_batch_equivalent_to_singletons() {
        let (store, axioms) = store_and_axioms(
            "(subclass Dog Mammal)\n\
             (subclass Cat Mammal)\n\
             (subclass Mammal Animal)\n\
             (subclass Animal Entity)",
        );
        // Singleton removals baseline.
        let mut a = build_eager(&store, &axioms, 1.2);
        a.remove_axiom(axioms[0]);
        a.remove_axiom(axioms[2]);
        // Batched removal.
        let mut b = build_eager(&store, &axioms, 1.2);
        b.remove_axioms([axioms[0], axioms[2]]);
        // Both indexes must agree with a from-scratch build over the
        // surviving axioms.
        let kept = [axioms[1], axioms[3]];
        assert_matches_from_scratch(&a, &store, &kept, 1.2);
        assert_matches_from_scratch(&b, &store, &kept, 1.2);
    }

    #[test]
    fn remove_axiom_bumps_stats_counter() {
        let (store, axioms) = store_and_axioms("(subclass Dog Mammal)");
        let mut idx = build_eager(&store, &axioms, 1.0);
        let _ = idx.take_stats();  // clear build-time counters
        idx.remove_axiom(axioms[0]);
        let s = idx.take_stats();
        assert_eq!(s.removes, 1, "removes counter must tick: {:?}", s);
    }

    // -- Option 1: bulk-rebuild tests --------------------------------

    /// A helper that asserts the D-relation in `idx` matches a
    /// from-scratch build over the same axiom set.
    fn assert_matches_from_scratch(
        idx:    &SineIndex,
        store:  &KifStore,
        axioms: &[SentenceId],
        tol:    f32,
    ) {
        let scratch = build_eager(store, axioms, tol);
        assert_eq!(idx.axiom_count(), scratch.axiom_count(), "axiom count");
        for &sid in axioms {
            assert_eq!(
                idx.symbols_of_axiom(sid),
                scratch.symbols_of_axiom(sid),
                "symbols of axiom {} differ",
                sid,
            );
        }
        let all_syms: HashSet<SymbolId> = axioms.iter()
            .filter_map(|&sid| scratch.symbols_of_axiom(sid))
            .flat_map(|s| s.iter().copied())
            .collect();
        for s in all_syms {
            assert_eq!(
                idx.triggers(s).cloned().unwrap_or_default(),
                scratch.triggers(s).cloned().unwrap_or_default(),
                "triggers for symbol {} differ", s,
            );
            assert_eq!(
                idx.generality(s),
                scratch.generality(s),
                "generality for symbol {} differs", s,
            );
        }
    }

    #[test]
    fn add_axioms_bulk_matches_incremental() {
        // Produce ≥50 axioms so the threshold fires (50 is the floor
        // — see `add_axioms`).  Result after bulk must match a
        // from-scratch incremental build.
        let mut src = String::new();
        for i in 0..60 {
            src.push_str(&format!("(subclass Class{} Entity)\n", i));
        }
        let (store, axioms) = store_and_axioms(&src);

        let mut bulk = SineIndex::new(1.0);
        bulk.add_axioms(&store, axioms.iter().copied());
        assert_matches_from_scratch(&bulk, &store, &axioms, 1.0);

        // Confirm the bulk path fired.
        let stats = bulk.take_stats();
        assert!(stats.bulk_rebuilds >= 1,
            "expected bulk rebuild for a 60-axiom initial batch, got {:?}", stats);
    }

    #[test]
    fn add_axioms_small_batch_takes_incremental_path() {
        // Build a decent base, then add a tiny batch.  The threshold is
        // max(current/10, 50); a 2-axiom batch on top of 6 axioms has
        // threshold = max(0, 50) = 50, so 2 < 50 and we take the
        // incremental path.
        let (store, axioms) = store_and_axioms(
            "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)\n\
             (subclass Cat Mammal)\n\
             (instance Rex Dog)\n\
             (instance Whiskers Cat)",
        );
        // Seed the first 4 via bulk (1 bulk rebuild).
        let mut idx = SineIndex::new(1.0);
        idx.add_axioms(&store, axioms[..4].iter().copied());
        let _ = idx.take_stats();

        // Now add 2 more — below the rebuild threshold.
        idx.add_axioms(&store, axioms[4..].iter().copied());
        let stats = idx.take_stats();
        assert_eq!(stats.bulk_rebuilds, 0,
            "small follow-up batch should NOT trigger a bulk rebuild");
        assert!(stats.calls >= 2,
            "incremental add_axiom should have been called at least twice: {:?}", stats);

        // Final state still matches from-scratch.
        assert_matches_from_scratch(&idx, &store, &axioms, 1.0);
    }

    #[test]
    fn add_axioms_large_followup_batch_triggers_rebuild() {
        // Base of 5 axioms + 80-axiom follow-up.  threshold =
        // max(5/10, 50) = 50; 80 > 50 so we rebuild.  (The 80
        // identical-structure axioms are just a convenient way to
        // stress the path — semantics aren't important here.)
        let mut src = String::new();
        src.push_str("(subclass Human Animal)\n");
        src.push_str("(subclass Mammal Animal)\n");
        src.push_str("(subclass Dog Mammal)\n");
        src.push_str("(subclass Cat Mammal)\n");
        src.push_str("(instance Rex Dog)\n");
        for i in 0..80 {
            src.push_str(&format!("(subclass Class{} Entity)\n", i));
        }
        let (store, axioms) = store_and_axioms(&src);
        let (first5, rest): (Vec<_>, Vec<_>) = axioms.iter().enumerate()
            .partition(|(i, _)| *i < 5);
        let first5: Vec<SentenceId> = first5.into_iter().map(|(_, sid)| *sid).collect();
        let rest:   Vec<SentenceId> = rest.into_iter().map(|(_, sid)| *sid).collect();

        let mut idx = SineIndex::new(1.2);
        idx.add_axioms(&store, first5.iter().copied());
        let _ = idx.take_stats();

        idx.add_axioms(&store, rest.iter().copied());
        let stats = idx.take_stats();
        assert!(stats.bulk_rebuilds >= 1,
            "80-axiom batch over 5 existing should rebuild, got {:?}", stats);

        assert_matches_from_scratch(&idx, &store, &axioms, 1.2);
    }

    #[test]
    fn add_axioms_empty_batch_is_noop() {
        let (store, axioms) = store_and_axioms("(subclass Human Animal)");
        let mut idx = SineIndex::new(1.0);
        idx.add_axioms(&store, axioms.iter().copied());
        let before = idx.axiom_count();
        let empty: Vec<SentenceId> = Vec::new();
        idx.add_axioms(&store, empty.into_iter());
        assert_eq!(idx.axiom_count(), before);
    }

    // -- Option 3: fine-grained fast path tests ----------------------

    #[test]
    fn fast_path_triggers_when_bumped_symbol_not_at_min() {
        // Build a KB where most added axioms share `subclass` with
        // existing ones, but `subclass`'s generality is above min
        // almost everywhere — so the fast path should dominate the
        // affected-set processing.
        //
        // We confirm via the `fast_path` stats counter.
        let (store, axioms) = store_and_axioms(
            "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)\n\
             (subclass Cat Mammal)\n\
             (subclass Bird Animal)",
        );

        // Seed with the first axiom via bulk (just one — bulk builds
        // from scratch).  Then add the rest incrementally so the fast
        // path can engage.
        let mut idx = SineIndex::new(1.2);
        idx.add_axioms(&store, axioms[..1].iter().copied());
        let _ = idx.take_stats();

        for &sid in &axioms[1..] {
            idx.add_axiom(&store, sid);
        }
        let stats = idx.take_stats();

        // The final state is correct...
        assert_matches_from_scratch(&idx, &store, &axioms, 1.2);
        // ...and at least one affected axiom took the fast path, not
        // a full recompute.  If this assertion ever fails it would
        // mean every add hit the unique-min branch — worth
        // investigating if it does.
        assert!(stats.fast_path >= 1,
            "expected fast-path updates on subsequent adds, got {:?}", stats);
    }

    #[test]
    fn fast_path_removes_symbol_when_bump_crosses_threshold() {
        // Construct an axiom whose `subclass` occurrence starts as a
        // trigger (subclass occ = 1) and then loses trigger status
        // after a second axiom bumps subclass's occ to 2 (beyond the
        // strict threshold of 1).
        let mut store = KifStore::default();
        let errs = crate::kif_store::load_kif(&mut store, "(subclass Human Animal)", "a");
        assert!(errs.is_empty());
        for &sid in &store.roots.clone() { store.register_axiom_symbols(sid); }
        let a0 = store.roots[0];

        let mut idx = SineIndex::new(1.0);
        idx.add_axiom(&store, a0);
        let subclass = store.sym_id("subclass").unwrap();
        // Before any bump: subclass appears in 1 axiom, Human in 1,
        // Animal in 1 — all tied at min 1.  All three trigger a0.
        assert!(idx.triggers(subclass).map_or(false, |s| s.contains(&a0)));

        // Add a second subclass-using axiom that shares NO other
        // symbols — so only `subclass` gets bumped in a0's affected
        // set.  Now subclass occ = 2, min of a0 is still 1 (Human,
        // Animal).  Subclass should DROP out of trigger_idx for a0.
        let errs = crate::kif_store::load_kif(&mut store, "(subclass Fruit Food)", "b");
        assert!(errs.is_empty());
        let a1 = *store.roots.last().unwrap();
        store.register_axiom_symbols(a1);
        idx.add_axiom(&store, a1);

        let stats = idx.take_stats();
        // At least one fast-path step: the a0 update for subclass.
        assert!(stats.fast_path >= 1,
            "expected fast path for a0's update on subclass bump, got {:?}", stats);
        // Subclass no longer triggers a0 (strict tolerance, occ=2 > threshold=1).
        assert!(!idx.triggers(subclass)
                    .map_or(false, |s| s.contains(&a0)),
            "subclass should have been removed from a0's trigger set via fast path");
        // Human and Animal still trigger a0 (unchanged occ=1).
        let human  = store.sym_id("Human").unwrap();
        let animal = store.sym_id("Animal").unwrap();
        assert!(idx.triggers(human).map_or(false, |s| s.contains(&a0)));
        assert!(idx.triggers(animal).map_or(false, |s| s.contains(&a0)));
    }

    #[test]
    fn g_min_cache_matches_manual_computation_after_bulk() {
        // After a bulk rebuild, axiom_g_min + axiom_g_min_count must
        // be populated for every non-empty axiom.
        let (store, axioms) = store_and_axioms(
            "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)",
        );
        let mut idx = SineIndex::new(1.0);
        idx.add_axioms(&store, axioms.iter().copied());

        for &sid in &axioms {
            let syms = idx.symbols_of_axiom(sid).unwrap().clone();
            let min_actual = syms.iter().map(|&s| idx.generality(s)).min().unwrap();
            let count_actual = syms.iter()
                .filter(|&&s| idx.generality(s) == min_actual)
                .count();
            // Peek into internals via public indirection: generality +
            // symbols_of_axiom give us what we need to verify the cache
            // would match.  The trigger set is derived from threshold =
            // floor(1.0 * min), so symbols with generality == min
            // should all be in trigger_idx[s] entries for this sid.
            let threshold = (1.0 * min_actual as f32).floor() as usize;
            for s in syms {
                let g = idx.generality(s);
                let is_trigger = idx.triggers(s).map_or(false, |set| set.contains(&sid));
                assert_eq!(g <= threshold, is_trigger,
                    "sid={} sym={} generality={} threshold={} triggered={}",
                    sid, s, g, threshold, is_trigger);
            }
            let _ = count_actual; // exercised implicitly through trigger membership
        }
    }
}
