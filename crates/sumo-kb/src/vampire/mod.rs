// crates/sumo-kb/src/vampire/mod.rs
//
// Vampire axiom cache for sumo-kb.
//
// The KnowledgeBase keeps a lazily-built `VampireAxiomCache` that holds the
// pre-converted axiom set as a pure-Rust `ir::Problem` plus a parallel
// `sid_map` that records which SentenceId produced each axiom (needed for
// proof back-translation).  At ask time:
//
//   Embedded path   (integrated-prover): clone the cached Problem, add the
//                    conjecture, run `lower_problem(...).solve_and_prove()`.
//   Subprocess path (always):            clone the cached Problem, add the
//                    conjecture, call `Problem::to_tptp()` and pipe the
//                    string into vampire.
//
// Gated: requires the `vampire` feature.

use std::collections::HashSet;

use vampire_prover::ir::Problem as IrProblem;

use crate::semantic::SemanticLayer;
use crate::types::SentenceId;

pub(crate) mod converter;
pub(crate) mod assemble;

#[cfg(feature = "integrated-prover")]
pub(crate) mod bindings;
#[cfg(feature = "integrated-prover")]
pub(crate) mod native_proof;

use converter::{Mode, NativeConverter};

/// Pre-built axiom data shared by both prover backends.
///
/// Only constructed when the `integrated-prover` feature is on (the
/// embedded solver's `ask_embedded` is the sole caller).  The default
/// feature set includes `integrated-prover` via `cnf`, but a
/// `--no-default-features --features ask` build compiles this module
/// without a constructor — the `#[allow]` keeps that combo warning-free.
#[allow(dead_code)]
pub(crate) struct VampireAxiomCache {
    /// Fully-typed TFF problem containing the axiom set and its sort /
    /// function / predicate declarations.  No conjecture.
    pub problem: IrProblem,

    /// Parallel to `problem.axioms()`: `sid_map[i]` is the KIF SentenceId
    /// that produced `problem.axioms()[i]`.  Callers use this to re-link
    /// proof steps back to KIF sentences.
    pub sid_map: Vec<SentenceId>,
}

#[allow(dead_code)]
impl VampireAxiomCache {
    /// Build a fresh cache from `axiom_ids` under the requested logic mode.
    pub fn build(
        layer:     &SemanticLayer,
        axiom_ids: &HashSet<SentenceId>,
        mode:      Mode,
    ) -> Self {
        let mut conv = NativeConverter::new(&layer.store, layer, mode);
        let mut skipped = 0usize;

        // Iterate deterministically so sid_map ordering is stable.
        let mut sorted: Vec<SentenceId> = axiom_ids.iter().copied().collect();
        sorted.sort_unstable();

        for sid in sorted {
            if !conv.add_axiom(sid) {
                skipped += 1;
            }
        }
        let (problem, sid_map) = conv.finish();

        log::debug!(target: "sumo_kb::vampire",
            "axiom cache built: mode={:?}, {} axiom(s), {} skipped",
            mode, sid_map.len(), skipped);

        VampireAxiomCache { problem, sid_map }
    }

    /// Build a fresh [`IrProblem`] holding the same sort / function /
    /// predicate declarations as this cache but only the axioms whose
    /// [`SentenceId`] is in `allowed`.  The returned `sid_map` is
    /// parallel to the new problem's `axioms()`.
    ///
    /// Cheap: clones decls (which are tiny fixed-size records) and the
    /// allowed axioms' IR formulas.  Axioms not in `allowed` are
    /// dropped entirely — no token emitted.  Empty `allowed` yields a
    /// problem with only declarations.
    ///
    /// Used by the embedded-prover path (`ask_embedded`) to prune the
    /// whole-KB cached problem down to the SInE-selected subset before
    /// handing the IR to `vampire_prover::lower_problem`.  The
    /// subprocess path uses `AssemblyOpts::axiom_filter` instead
    /// (assembly-time filtering is free when we're emitting text); the
    /// embedded path can't filter at assembly time because it bypasses
    /// the TPTP serialiser entirely, so the filter has to be baked
    /// into the IR the solver receives.
    pub fn filtered_problem(
        &self,
        allowed: &HashSet<SentenceId>,
    ) -> (IrProblem, Vec<SentenceId>) {
        use vampire_prover::ir::LogicMode;

        let mut out = match self.problem.mode() {
            LogicMode::Tff => IrProblem::new_tff(),
            LogicMode::Fof => IrProblem::new(),
        };

        // Declarations are KB-wide — every filtered axiom may
        // reference any of them — so we copy them all verbatim.
        for s in self.problem.sort_decls() { out.declare_sort(s.clone()); }
        for f in self.problem.fn_decls()   { out.declare_function(f.clone()); }
        for p in self.problem.pred_decls() { out.declare_predicate(p.clone()); }

        // Walk axioms + sid_map in lockstep.  Anonymous axioms (no
        // sid_map entry) can't be classified by the filter — admit
        // them conservatively, mirroring `assemble_tptp`'s behaviour.
        let mut sid_map = Vec::with_capacity(allowed.len());
        for (i, ax) in self.problem.axioms().iter().enumerate() {
            match self.sid_map.get(i).copied() {
                Some(sid) if !allowed.contains(&sid) => continue,
                Some(sid) => {
                    out.with_axiom(ax.clone());
                    sid_map.push(sid);
                }
                None => {
                    // Anonymous — no sid to filter by, keep it.
                    out.with_axiom(ax.clone());
                }
            }
        }
        (out, sid_map)
    }
}

/// Both-mode axiom cache: holds the pre-converted axiom set in TFF
/// AND FOF shapes.  Built together so a single cache-warm pass covers
/// every backend, and either-mode `ask` can hit a warm IR without a
/// per-query rebuild.
///
/// Memory cost: ~2× the single-mode cache.  For a SUMO-sized KB this
/// is on the order of tens of MB of in-memory IR — trivial next to
/// the proving work itself.  LMDB persistence writes both keys, so
/// a cold open restores both without a recompute.
#[allow(dead_code)]
pub(crate) struct VampireAxiomCacheSet {
    pub tff: VampireAxiomCache,
    pub fof: VampireAxiomCache,
}

#[allow(dead_code)]
impl VampireAxiomCacheSet {
    /// Build both TFF and FOF caches from the same `axiom_ids`
    /// snapshot.  Runs two sequential `NativeConverter` passes —
    /// cheaper than interleaving because each converter has its
    /// own IR state and mode-specific declarations.
    pub fn build(
        layer:     &SemanticLayer,
        axiom_ids: &HashSet<SentenceId>,
    ) -> Self {
        let tff = VampireAxiomCache::build(layer, axiom_ids, Mode::Tff);
        let fof = VampireAxiomCache::build(layer, axiom_ids, Mode::Fof);
        VampireAxiomCacheSet { tff, fof }
    }

    /// Return the cache for `mode`.
    pub fn get(&self, mode: Mode) -> &VampireAxiomCache {
        match mode {
            Mode::Tff => &self.tff,
            Mode::Fof => &self.fof,
        }
    }
}
