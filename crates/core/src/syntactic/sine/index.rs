//! The [`SineIndex`] data structure and its incremental maintenance (add /
//! remove / rebuild). Store-agnostic: operates on `SentenceId` / `SymbolId`
//! only.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::types::{SentenceId, SymbolId};

use super::params::default_tolerance;

// -- SineIndex ---------------------------------------------------------------

/// Eagerly-maintained SInE index.
///
/// Owned by [`SyntacticLayer`] as the `sine` field. Use the
/// `SyntacticLayer::sine_*` wrapper methods for all mutation — they handle
/// symbol extraction before forwarding to the index.
///
/// The D-relation is tolerance-independent, so queries at different
/// tolerances can be interleaved freely without any index rebuild; the
/// `tolerance` field only records the last value passed to [`Self::select`]
/// for introspection.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SineIndex {
    /// Last-used tolerance factor.  Initialised to [`default_tolerance()`]
    /// and updated every time [`Self::select`] is called.
    #[serde(default = "default_tolerance")]
    pub(crate) tolerance: f32,

    /// Per-axiom symbol set.  Tolerance-independent.
    pub(in crate::syntactic::sine) axiom_syms: HashMap<SentenceId, HashSet<SymbolId>>,

    /// Per-symbol generality: `occ(s)` = how many axioms contain `s`.
    ///
    /// Kept as a plain running count because the incremental `g_min`
    /// computation needs `occ(s)` **before** the new axiom's `sym_to_axioms`
    /// entry exists, so it cannot read the count off that map during an `add`.
    pub(in crate::syntactic::sine) sym_occ: HashMap<SymbolId, usize>,

    /// Cached minimum generality per axiom: `min{ occ(s) : s ∈ syms(A) }`.
    pub(in crate::syntactic::sine) axiom_g_min: HashMap<SentenceId, usize>,

    /// Per-symbol sorted trigger list for select.
    ///
    /// `sym_to_axioms[s]` is a `Vec<(g_min, axiom_id)>` sorted **descending**
    /// by `g_min`.  During selection for symbol `s` with generality `occ`:
    ///   - An axiom with entry `(gm, aid)` is triggered iff `occ/gm ≤ tolerance`.
    ///   - Because the list is descending, the loop breaks as soon as
    ///     `gm < occ/tolerance` — all remaining entries cannot be triggered.
    pub(in crate::syntactic::sine) sym_to_axioms: HashMap<SymbolId, Vec<(usize, SentenceId)>>,

    /// Per-symbol ownership index.
    ///
    /// `sym_to_owned[s]` = set of axioms where `s` currently achieves the
    /// minimum generality.  When `occ(s)` increases only these axioms'
    /// g_min values can change.
    pub(in crate::syntactic::sine) sym_to_owned: HashMap<SymbolId, HashSet<SentenceId>>,

    /// Which-code-path counters, used by unit tests to verify that the bulk
    /// rebuild and incremental paths fire as expected.
    #[serde(skip)]
    stats: AddAxiomStats,

    /// Promotions deferred by the `AxiomsPromoted` reactor, folded in by
    /// [`Self::flush_pending`] at the next read. Serialized like the rest of
    /// the index: a snapshot taken mid-load carries the queue and flushes at
    /// the first read after thaw.
    #[serde(default)]
    pub(in crate::syntactic::sine) pending: Vec<(SentenceId, HashSet<SymbolId>)>,
}

impl Default for SineIndex {
    fn default() -> Self {
        Self {
            tolerance:    default_tolerance(),
            axiom_syms:   HashMap::new(),
            sym_occ:      HashMap::new(),
            axiom_g_min:  HashMap::new(),
            sym_to_axioms: HashMap::new(),
            sym_to_owned: HashMap::new(),
            stats:        AddAxiomStats::default(),
            pending:      Vec::new(),
        }
    }
}

/// Counters for which code paths `SineIndex` took since the last
/// [`SineIndex::take_stats`] call. Used only by unit-test assertions.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct AddAxiomStats {
    /// Number of `add_axiom` calls.
    pub calls:           usize,
    /// Number of full threshold-recompute calls (entry insertions / updates).
    pub recompute_calls: usize,
    /// Number of times `rebuild_from` fired (at most once per
    /// `sine_add_axioms` call, and only when the batch is large).
    pub bulk_rebuilds:   usize,
    /// Number of `remove_axiom` calls.
    pub removes:         usize,
}

impl SineIndex {
    /// Construct an empty index. `tolerance` is stored as the initial
    /// last-used value; pass `default_tolerance()` if no specific value
    /// is needed.
    #[allow(dead_code)]
    pub(crate) fn new(tolerance: f32) -> Self {
        Self { tolerance: tolerance.max(1.0), ..Self::default() }
    }

    /// Take-and-reset the which-path counters.  Test-only.
    #[allow(dead_code)]
    pub(crate) fn take_stats(&mut self) -> AddAxiomStats {
        let s = self.stats;
        self.stats = AddAxiomStats::default();
        s
    }

    /// The last tolerance factor used in a [`Self::select`] call, or
    /// `default_tolerance()` if no query has been run yet.
    #[inline]
    pub fn tolerance(&self) -> f32 {
        self.tolerance
    }

    /// Number of axioms currently tracked.
    #[inline] pub fn axiom_count(&self) -> usize { self.axiom_syms.len() }

    /// The sids of every axiom currently tracked (for bulk-rebuild planning).
    pub(crate) fn axiom_sids(&self) -> Vec<SentenceId> {
        self.axiom_syms.keys().copied().collect()
    }

    /// Is `sid` currently tracked as an axiom in this index?
    #[inline] pub fn contains(&self, sid: SentenceId) -> bool {
        self.axiom_syms.contains_key(&sid)
    }

    /// Generality of `s`: number of axioms in which it appears.  `0` for
    /// symbols absent from the axiom set.
    #[inline] pub fn generality(&self, s: SymbolId) -> usize {
        self.sym_occ.get(&s).copied().unwrap_or(0)
    }

    /// Every tracked axiom containing symbol `s`, with the axiom's cached
    /// g_min, in descending g_min order. Empty slice for unknown symbols.
    #[inline]
    pub(crate) fn axioms_of_symbol(&self, s: SymbolId) -> &[(usize, SentenceId)] {
        self.sym_to_axioms.get(&s).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The symbol set of an indexed axiom. `None` if `sid` is not tracked.
    pub fn symbols_of_axiom(&self, sid: SentenceId) -> Option<&HashSet<SymbolId>> {
        self.axiom_syms.get(&sid)
    }

    // -- Sorted-Vec helpers --------------------------------------------------

    fn sta_insert(&mut self, s: SymbolId, g_min: usize, aid: SentenceId) {
        let v = self.sym_to_axioms.entry(s).or_default();
        let pos = v.partition_point(|&(gm, _)| gm > g_min);
        v.insert(pos, (g_min, aid));
    }

    fn sta_remove(&mut self, s: SymbolId, g_min: usize, aid: SentenceId) {
        let Some(v) = self.sym_to_axioms.get_mut(&s) else { return };
        let lo = v.partition_point(|&(gm, _)| gm > g_min);
        let hi = v.partition_point(|&(gm, _)| gm >= g_min);
        if let Some(i) = v[lo..hi].iter().position(|&(_, a)| a == aid) {
            v.remove(lo + i);
        }
        if v.is_empty() { self.sym_to_axioms.remove(&s); }
    }

    fn sta_reposition(&mut self, s: SymbolId, old_g_min: usize, new_g_min: usize, aid: SentenceId) {
        self.sta_remove(s, old_g_min, aid);
        self.sta_insert(s, new_g_min, aid);
    }

    // -- Mutation ------------------------------------------------------------

    /// Are there deferred promotions not yet folded into the index?
    #[inline]
    pub(crate) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The batch size at or above which an axiom addition takes the two-pass
    /// bulk rebuild instead of per-axiom incremental g_min updates.
    ///
    /// This is the single definition of the heuristic: the promotion reactor,
    /// [`Self::flush_pending`], and `sine_add_axioms` must all consult it, or a
    /// batch deferred by one is mis-sized by another.
    #[inline]
    pub(crate) fn bulk_threshold(&self) -> usize {
        (self.axiom_count() / 10).clamp(50, 500)
    }

    /// Defer axioms for indexing at the next [`Self::flush_pending`].
    ///
    /// Readers must go through a flushing accessor
    /// (`SyntacticLayer::sine_current`) to observe these.
    pub(crate) fn defer_axioms<I>(&mut self, pairs: I)
    where
        I: IntoIterator<Item = (SentenceId, HashSet<SymbolId>)>,
    {
        self.pending.extend(pairs);
    }

    /// Fold any deferred promotions into the index.  No-op when nothing is
    /// pending.  Applies the same batch heuristic as the reactor: a large
    /// pending set takes the two-pass bulk rebuild, a small one the
    /// incremental per-axiom path.
    pub(crate) fn flush_pending(&mut self) {
        if self.pending.is_empty() { return; }
        let pending = std::mem::take(&mut self.pending);
        if pending.len() >= self.bulk_threshold() {
            // Existing entries must precede pending ones so rebuild_from's
            // first-wins dedup matches the incremental path's idempotence.
            let mut pairs: Vec<(SentenceId, HashSet<SymbolId>)> =
                std::mem::take(&mut self.axiom_syms).into_iter().collect();
            pairs.extend(pending);
            self.rebuild_from(pairs);
        } else {
            for (sid, syms) in pending {
                self.add_axiom(sid, syms);
            }
        }
    }

    /// Incrementally register an axiom with pre-computed symbol set.
    ///
    /// Idempotent: re-adding an already-tracked sid is a no-op.
    /// Callers should use [`SyntacticLayer::sine_add_axiom`] instead,
    /// which handles symbol extraction from the store.
    pub(crate) fn add_axiom(&mut self, sid: SentenceId, syms: HashSet<SymbolId>) {
        self.stats.calls += 1;
        if self.axiom_syms.contains_key(&sid) { return; }
        if syms.is_empty() {
            self.axiom_syms.insert(sid, syms);
            return;
        }

        for &s in &syms {
            *self.sym_occ.entry(s).or_insert(0) += 1;
        }

        // Recompute g_min for existing axioms whose current g_min owner had
        // its occ bumped.
        let mut updated: HashSet<SentenceId> = HashSet::new();
        updated.insert(sid);

        for &s in &syms {
            let owned: Vec<SentenceId> = self.sym_to_owned
                .get(&s).map(|set| set.iter().copied().collect())
                .unwrap_or_default();
            for a in owned {
                if updated.insert(a) {
                    self.update_entry_g_min(a);
                }
            }
        }

        self.axiom_syms.insert(sid, syms.clone());
        self.insert_entry(sid, &syms);
        self.stats.recompute_calls += 1;
    }

    /// Remove `sid` from the index.  Idempotent.
    pub(crate) fn remove_axiom(&mut self, sid: SentenceId) {
        self.stats.removes += 1;
        // A retraction can race a deferred promotion, so drop sid from the
        // pending queue as well as the live index.
        if !self.pending.is_empty() {
            self.pending.retain(|(p, _)| *p != sid);
        }
        let Some(syms) = self.axiom_syms.remove(&sid) else { return };

        let old_g_min = self.axiom_g_min.remove(&sid).unwrap_or(0);
        for &s in &syms {
            if old_g_min > 0 {
                self.sta_remove(s, old_g_min, sid);
            }
            if let Some(set) = self.sym_to_owned.get_mut(&s) {
                set.remove(&sid);
                if set.is_empty() { self.sym_to_owned.remove(&s); }
            }
        }

        if syms.is_empty() { return; }

        // Other axioms whose g_min may drop now that `sid` is gone: those
        // sharing a symbol with it. The `sta_remove` loop above already
        // dropped `sid`'s own entries, so `sym_to_axioms[s]` now lists exactly
        // the survivors containing `s`.
        let mut to_update: HashSet<SentenceId> = HashSet::new();
        for &s in &syms {
            if let Some(entries) = self.sym_to_axioms.get(&s) {
                to_update.extend(entries.iter().map(|&(_, a)| a));
            }
        }
        to_update.remove(&sid);

        for &s in &syms {
            if let Some(c) = self.sym_occ.get_mut(&s) {
                *c -= 1;
                if *c == 0 { self.sym_occ.remove(&s); }
            }
        }

        for a in to_update {
            self.update_entry_g_min(a);
            self.stats.recompute_calls += 1;
        }
    }

    /// Bulk-remove sids.  Delegates to [`Self::remove_axiom`] in a loop.
    #[allow(dead_code)]
    pub(crate) fn remove_axioms<I>(&mut self, sids: I)
    where
        I: IntoIterator<Item = SentenceId>,
    {
        for sid in sids {
            self.remove_axiom(sid);
        }
    }

    /// Clear the index and rebuild from pre-computed `(SentenceId, symbols)` pairs.
    ///
    /// Two-pass algorithm:
    /// Pass 1 — collect per-axiom symbol sets and the `sym_occ` generality
    /// counts.
    /// Pass 2 — compute `g_min`, `sym_to_axioms`, and `sym_to_owned`.
    pub(crate) fn rebuild_from<I>(&mut self, pairs: I)
    where
        I: IntoIterator<Item = (SentenceId, HashSet<SymbolId>)>,
    {
        self.stats.bulk_rebuilds += 1;
        // The index becomes exactly `pairs`; deferred promotions from before
        // the rebuild must not resurface.
        self.pending.clear();
        self.axiom_syms.clear();
        self.sym_occ.clear();
        self.axiom_g_min.clear();
        self.sym_to_axioms.clear();
        self.sym_to_owned.clear();

        // Pass 1: per-axiom symbols + generality counts.
        for (sid, syms) in pairs {
            if self.axiom_syms.contains_key(&sid) { continue; } // dedup
            for &s in &syms {
                *self.sym_occ.entry(s).or_insert(0) += 1;
            }
            self.axiom_syms.insert(sid, syms);
        }

        // Pass 2: compute g_min and populate sym_to_axioms / sym_to_owned.
        let axiom_sids: Vec<SentenceId> = self.axiom_syms.keys().copied().collect();
        for a in axiom_sids {
            let syms = self.axiom_syms[&a].clone();
            self.insert_entry(a, &syms);
        }

        crate::emit_event!(crate::progress::ProgressEvent::Log {
            level:   crate::progress::LogLevel::Debug,
            target:  "sigmakee_rs_core::sine",
            message: format!(
                "SineIndex::rebuild_from: {} axioms, {} symbols",
                self.axiom_count(),
                self.sym_occ.len(),
            ),
        });
    }

    /// Drop all state.
    #[allow(dead_code)]
    pub(crate) fn clear(&mut self) {
        self.axiom_syms.clear();
        self.sym_occ.clear();
        self.axiom_g_min.clear();
        self.sym_to_axioms.clear();
        self.sym_to_owned.clear();
    }

    // -- Internal helpers ----------------------------------------------------

    fn insert_entry(&mut self, sid: SentenceId, syms: &HashSet<SymbolId>) {
        if syms.is_empty() { return; }

        let g_min = syms.iter()
            .map(|&s| self.sym_occ.get(&s).copied().unwrap_or(0))
            .min()
            .unwrap_or(0);
        if g_min == 0 { return; }

        self.axiom_g_min.insert(sid, g_min);

        for &s in syms {
            self.sta_insert(s, g_min, sid);
            let occ_s = self.sym_occ.get(&s).copied().unwrap_or(0);
            if occ_s == g_min {
                self.sym_to_owned.entry(s).or_default().insert(sid);
            }
        }
    }

    fn update_entry_g_min(&mut self, a: SentenceId) {
        let Some(a_syms) = self.axiom_syms.get(&a).cloned() else { return };
        if a_syms.is_empty() { return; }

        let new_g_min = a_syms.iter()
            .map(|&s| self.sym_occ.get(&s).copied().unwrap_or(0))
            .min()
            .unwrap_or(0);

        let old_g_min = self.axiom_g_min.get(&a).copied().unwrap_or(0);

        if new_g_min == 0 {
            if old_g_min > 0 {
                for &s in &a_syms {
                    self.sta_remove(s, old_g_min, a);
                    if let Some(set) = self.sym_to_owned.get_mut(&s) {
                        set.remove(&a);
                        if set.is_empty() { self.sym_to_owned.remove(&s); }
                    }
                }
                self.axiom_g_min.remove(&a);
            }
            return;
        }

        if new_g_min != old_g_min {
            self.axiom_g_min.insert(a, new_g_min);
            for &s in &a_syms {
                if old_g_min > 0 {
                    self.sta_reposition(s, old_g_min, new_g_min, a);
                } else {
                    self.sta_insert(s, new_g_min, a);
                }
            }
        }

        for &s in &a_syms {
            let occ_s = self.sym_occ.get(&s).copied().unwrap_or(0);
            let should_own = occ_s == new_g_min;
            let currently_owns = self.sym_to_owned.get(&s)
                .map_or(false, |set| set.contains(&a));
            match (currently_owns, should_own) {
                (false, true)  => { self.sym_to_owned.entry(s).or_default().insert(a); }
                (true,  false) => {
                    if let Some(set) = self.sym_to_owned.get_mut(&s) {
                        set.remove(&a);
                        if set.is_empty() { self.sym_to_owned.remove(&s); }
                    }
                }
                _ => {}
            }
        }
    }
}
