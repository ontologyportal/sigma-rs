//! Output of [`crate::AskOp::run`].

use sumo_kb::{Binding, KifProofStep, ProverStatus};
use sumo_kb::prover::ProverTimings;

/// Findings from one proof query.
#[derive(Debug, Clone)]
pub struct AskReport {
    /// SZS-style verdict from the prover.  Distinct from `Err(SdkError)`:
    /// `Unknown` / `Timeout` are *successful* runs with an undecided
    /// outcome, not infrastructure failures.
    pub status: ProverStatus,

    /// Variable bindings extracted from the proof, when one was found.
    pub bindings: Vec<Binding>,

    /// Raw stdout + stderr of the prover.  Useful for debugging or
    /// reproducing in a Vampire shell.
    pub raw_output: String,

    /// Proof steps converted to SUO-KIF.  Empty when no proof was
    /// produced (or the prover backend doesn't support extraction).
    pub proof_kif: Vec<KifProofStep>,

    /// Raw TSTP proof section as emitted by Vampire.  Empty when no
    /// proof was produced.
    pub proof_tptp: String,

    /// Per-phase timing breakdown for this single query.  KB-load
    /// timing is **not** included here — that's a one-time cost that
    /// the caller measures around `KnowledgeBase::open` / `IngestOp`.
    pub timings: ProverTimings,
}

impl AskReport {
    /// `true` iff the prover reported `ProverStatus::Proved`.
    pub fn is_proved(&self) -> bool {
        matches!(self.status, ProverStatus::Proved)
    }

    /// `true` iff the verdict is one of the "decided" outcomes
    /// (`Proved` / `Disproved` / `Consistent` / `Inconsistent`).
    pub fn is_decided(&self) -> bool {
        matches!(
            self.status,
            ProverStatus::Proved
                | ProverStatus::Disproved
                | ProverStatus::Consistent
                | ProverStatus::Inconsistent
        )
    }
}
