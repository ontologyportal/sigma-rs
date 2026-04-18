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

use converter::{Mode, NativeConverter};

/// Pre-built axiom data shared by both prover backends.
pub(crate) struct VampireAxiomCache {
    /// Fully-typed TFF problem containing the axiom set and its sort /
    /// function / predicate declarations.  No conjecture.
    pub problem: IrProblem,

    /// Parallel to `problem.axioms()`: `sid_map[i]` is the KIF SentenceId
    /// that produced `problem.axioms()[i]`.  Callers use this to re-link
    /// proof steps back to KIF sentences.
    pub sid_map: Vec<SentenceId>,
}

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
}
