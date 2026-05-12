//! `SyntacticLayer` integration for SInE: symbol extraction, forwarding to the
//! index, and the public selection entry points.

use std::collections::HashSet;
use std::sync::Arc;

use crate::types::{SentenceId, SymbolId};

use super::super::SyntacticLayer;

// -- SyntacticLayer wrapper methods ------------------------------------------

impl SyntacticLayer {
    /// Read the SInE index with any deferred promotions folded in first.
    ///
    /// Every selection / introspection read goes through here to observe
    /// deferred promotions; a cheap no-op when nothing is pending.
    pub(crate) fn sine_current<R>(&self, f: impl FnOnce(&super::SineIndex) -> R) -> R {
        if self.sine.with_ref(|idx| idx.has_pending()) {
            self.sine.update_with(|idx| idx.flush_pending());
        }
        self.sine.with_ref(f)
    }

    /// Add `sid` to the SInE index and register its symbols in
    /// `Symbol.all_sentences`. Both updates happen atomically from the
    /// caller's perspective. Idempotent.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn sine_add_axiom(&mut self, sid: SentenceId) {
        if !self.has_sentence(sid) { return; }
        let syms = self.sentence_symbols(sid);
        for &s in &syms {
            self.axiom_index.modify_entry(s, |set| { Arc::make_mut(set).insert(sid); });
        }
        self.sine.modify(|idx| idx.add_axiom(sid, syms));
    }

    /// Remove `sid` from the SInE index and unregister its symbols from
    /// `Symbol.all_sentences`. Both updates happen atomically. Idempotent.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn sine_remove_axiom(&mut self, sid: SentenceId) {
        // Sentence body must still be intact for symbol extraction.
        let syms = self.sentence_symbols(sid);
        for &s in &syms {
            self.axiom_index.modify_entry(s, |set| { Arc::make_mut(set).remove(&sid); });
        }
        self.sine.modify(|idx| idx.remove_axiom(sid));
    }

    /// Bulk-add many axioms, choosing between incremental and bulk-rebuild
    /// paths based on the batch size relative to the current index size.
    ///
    /// When the batch is a non-trivial fraction of the final axiom count the
    /// incremental path goes quadratic; a from-scratch rebuild is
    /// O(N · avg_syms).  Heuristic: if batch ≥ `max(current/10, 50)`
    /// (clamped to 500), do a bulk rebuild.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn sine_add_axioms<I>(&mut self, sids: I)
    where
        I: IntoIterator<Item = SentenceId>,
    {
        let sids: Vec<SentenceId> = sids.into_iter().collect();
        if sids.is_empty() { return; }

        // The size heuristic and the `axiom_sids` collection below must see the
        // full index, so fold in any deferred promotions first.
        self.sine.modify(|idx| idx.flush_pending());
        let threshold = self.sine.with_ref(|idx| idx.bulk_threshold());
        if sids.len() >= threshold {
            let mut all: Vec<SentenceId> = self.sine.with_ref(|idx| idx.axiom_sids());
            all.extend(sids.iter().copied());
            self.sine_rebuild_from(all);
        } else {
            for sid in sids {
                self.sine_add_axiom(sid);
            }
        }
    }

    /// Rebuild the SInE index from scratch using the given axiom sid list.
    ///
    /// Also registers any sids that are not yet in `Symbol.all_sentences`.
    /// Safe to call with a mix of existing and new sids — `HashSet::insert`
    /// is idempotent.
    fn sine_rebuild_from<I>(&mut self, sids: I)
    where
        I: IntoIterator<Item = SentenceId>,
    {
        let pairs: Vec<(SentenceId, HashSet<SymbolId>)> = {
            // Collect unique sids first to avoid duplicate symbol extraction.
            let mut seen = HashSet::new();
            sids.into_iter()
                .filter(|sid| seen.insert(*sid) && self.has_sentence(*sid))
                .map(|sid| (sid, self.sentence_symbols(sid)))
                .collect()
        };
        // Keep the axiom-occurrence index in sync for every sid in this rebuild.
        for (sid, syms) in &pairs {
            for &s in syms {
                self.axiom_index.modify_entry(s, |set| { Arc::make_mut(set).insert(*sid); });
            }
        }
        self.sine.modify(|idx| idx.rebuild_from(pairs));
    }

    /// Rebuild the SInE index from scratch over all current root sentences.
    #[allow(dead_code)]
    pub(crate) fn sine_rebuild(&mut self) {
        let sids: Vec<SentenceId> = self.root_sids();
        self.sine_rebuild_from(sids);
    }

    /// Run SInE axiom selection: BFS from `seed_syms`, returning every
    /// triggered axiom SentenceId.  Returns an empty set when the SInE
    /// index is disabled.
    ///
    /// Records `tolerance` in [`SineIndex::tolerance`] so that
    /// [`KnowledgeBase::sine_tolerance`] reflects the last-used value.
    pub fn select_axioms(
        &self,
        seed_syms:   &HashSet<SymbolId>,
        tolerance:   f32,
        depth_limit: Option<usize>,
    ) -> HashSet<SentenceId> {
        let result = self.sine_current(|idx| idx.select(seed_syms, tolerance, depth_limit));
        self.sine.modify(|idx| idx.tolerance = tolerance.max(1.0));
        result
    }

    /// Auto-tolerance SInE selection: pick the largest tolerance whose
    /// selected set stays within `budget` axioms (see
    /// [`SineIndex::select_within_budget`]) and return the selected sids.
    ///
    /// Records the chosen tolerance in [`SineIndex::tolerance`] so that
    /// [`KnowledgeBase::sine_tolerance`] reflects the value actually used.
    /// Returns an empty set when the SInE index is disabled.
    pub fn select_axioms_within_budget(
        &self,
        seed_syms:   &HashSet<SymbolId>,
        budget:      usize,
        depth_limit: Option<usize>,
    ) -> (f32, HashSet<SentenceId>) {
        let (chosen_t, result) = self.sine_current(|idx|
            idx.select_within_budget(seed_syms, budget, depth_limit));
        self.sine.modify(|idx| idx.tolerance = chosen_t.max(1.0));
        (chosen_t, result)
    }
}
