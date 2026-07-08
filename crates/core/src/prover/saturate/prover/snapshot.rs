// crates/core/src/prover/saturate/prover/snapshot.rs
//
// Frozen-background snapshot cache: everything a run computes BEFORE
// support/conjecture loading (clause arena, indexes, dedup set, oracle
// products of the input pre-pass), detached from the layer borrow so
// it can live in `ProverLayer::bg_snapshots` and be reused across runs
// over an identical problem base.  `NativeProver::new`/`from_snapshot`
// (the constructors) stay in `mod.rs`; this file holds the snapshot
// struct itself plus `freeze`/`retain_background`.

use crate::types::{SentenceId, SymbolId};

use super::super::clause::{AtomId, ClauseKey, Term};
use super::super::hash64::{Map64, Set64};
use super::super::index::{EntryRef, LiteralIndex};
use super::super::oracle::OracleSnapshot;
use super::super::theory::TheoryOracle;
use super::super::units::UnitStores;
use super::{ClauseRec, NativeProver};

/// A frozen background problem base: everything `ask_native_once`
/// computes BEFORE support/conjecture loading, detached from the
/// layer borrow so it can live in `ProverLayer::bg_snapshots`.
/// Rehydration is a deep clone — a few ms against the ~60 ms+ of
/// pre-pass + clause-pipeline + indexing it replaces.
#[derive(Debug, Clone)]
pub(crate) struct ProverSnapshot {
    /// Roots whose pre-pass + clauses are IN THE ARENA (indexes may
    /// cover a subset after a narrowed rehydration — `retain_background`
    /// rebuilds them from the arena for any subset of these).
    pub(crate) loaded_roots: std::collections::HashSet<SentenceId>,
    pub(super) clauses: Vec<ClauseRec>,
    pub(super) seen: Set64<ClauseKey>,
    pub(super) idx: LiteralIndex,
    pub(super) units: UnitStores,
    pub(super) support_seeds: Vec<(AtomId, u32)>,
    pub(super) eq_terms: Map64<u64, Term>,
    pub(super) lists_done: Set64<u64>,
    pub(super) pending_list_units: Vec<Term>,
    pub(super) has_compound_eqs: bool,
    pub(super) antisym_mined: Map64<SymbolId, Option<SentenceId>>,
    pub(super) irrefl_mined: Map64<SymbolId, Option<SentenceId>>,
    pub(super) inverse_mined: Vec<(SymbolId, SymbolId, Option<SentenceId>)>,
    pub(super) sym_swap_memo: Map64<AtomId, (u64, Option<AtomId>)>,
    pub(super) seq: u64,
    pub(super) tick: u64,
    pub(super) oracle: OracleSnapshot,
}

impl<'a> NativeProver<'a> {
    /// Capture the prover's owned state after BACKGROUND loading —
    /// clause arena, indexes, dedup set, oracle products of the input
    /// pre-pass — for reuse by later runs over an identical problem
    /// base (see `ProverLayer::bg_snapshots`).  Must be taken BEFORE
    /// support/conjecture loading: the queue is asserted empty (the
    /// background tier is pre-activated, never queued).
    pub(crate) fn freeze(&self) -> ProverSnapshot {
        debug_assert!(
            self.h_weight.is_empty() && self.h_age.is_empty(),
            "freeze must precede support/conjecture loading"
        );
        ProverSnapshot {
            loaded_roots: self.bg_roots.clone(),
            clauses: self.clauses.clone(),
            seen: self.seen.clone(),
            idx: self.idx.clone(),
            units: self.units.clone(),
            support_seeds: self.support_seeds.clone(),
            eq_terms: self.eq_terms.clone(),
            lists_done: self.lists_done.clone(),
            pending_list_units: self.pending_list_units.clone(),
            has_compound_eqs: self.has_compound_eqs,
            antisym_mined: self.antisym_mined.clone(),
            irrefl_mined: self.irrefl_mined.clone(),
            inverse_mined: self.inverse_mined.clone(),
            sym_swap_memo: self.sym_swap_memo.clone(),
            seq: self.seq,
            tick: self.tick,
            oracle: self.oracle.snapshot(),
        }
    }

    /// Re-derive the retrieval surfaces (literal index, unit stores,
    /// dedup set) for a SUBSET of the frozen background — the
    /// contraction half of cross-slice reuse.  Clauses from roots
    /// outside `keep` stay in the arena (ids are stable, so parent /
    /// proof references keep working) but vanish from every probe, so
    /// they can never be resolution partners — exactly a narrower
    /// slice's search space.  The ORACLE deliberately keeps the
    /// superset's theory (equalities / FD / closures contributed by
    /// masked axioms): every discharge still cites real KB axioms, so
    /// narrowing stays sound — it is a search heuristic, not a
    /// semantic restriction.  Synthesized theory clauses (subrel
    /// schema, `source == None`) are always kept.
    pub(crate) fn retain_background(&mut self, keep: &std::collections::HashSet<SentenceId>) {
        self.idx = LiteralIndex::default();
        self.units = UnitStores::default();
        self.demods.clear();
        self.seen = Set64::default();
        self.support_seeds.clear();
        let n = self.clauses.len() as u32;
        let layer = self.layer;
        let src = move |a| layer.atom_info(a);
        for id in 0..n {
            let (kept, key) = {
                let c = &self.clauses[id as usize];
                let kept = c.activated
                    && match c.source {
                        Some(sid) => keep.contains(&sid),
                        None => true,
                    };
                (kept, c.key)
            };
            if !kept {
                continue;
            }
            self.seen.insert(key);
            let lits = self.clauses[id as usize].lits.clone();
            for (i, l) in lits.iter().enumerate() {
                self.idx.add(EntryRef { clause: id, lit: i as u8 }, l.pos, l.atom, &src);
            }
            if lits.len() == 1 {
                let nv = self.clauses[id as usize].nvars;
                self.units.add_unit(
                    id, lits[0].pos, lits[0].atom, nv,
                    &layer.atom_infos, &layer.atoms, &layer.semantic.syntactic);
                self.index_demodulator(id);
            }
        }
        if self.opts.strategy.superposition {
            self.rebuild_superposition_index();
        }
    }
}
