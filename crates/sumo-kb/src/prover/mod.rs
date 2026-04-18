// crates/sumo-kb/src/prover/mod.rs
//
// Prover API: shared types + sub-prover implementations.
// Gated: #[cfg(feature = "ask")] in lib.rs.

#[cfg(all(feature = "ask", target_arch = "wasm32"))]
compile_error!("sumo-kb: the `ask` feature is not available on wasm32 targets");

use std::{fmt, time::Duration};

// -- Sub-prover modules --------------------------------------------------------

pub mod subprocess;

pub use subprocess::VampireRunner;

use serde::{Serialize, Deserialize};

// -- Shared types --------------------------------------------------------------

pub trait ProverRunner: Send + Sync {
    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult;
    /// The timeout this runner will apply to the prover, in seconds.
    /// Returns 0 if the runner manages its own timeout independently.
    fn timeout_secs(&self) -> u32 { 0 }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProverOpts {
    pub timeout_secs: u32,
    pub mode: ProverMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProverMode {
    Prove,
    CheckConsistency,
}

/// Per-query timing breakdown, populated on every call.
#[derive(Debug, Clone, Default)]
pub struct ProverTimings {
    /// Time spent building the theorem-prover input (TPTP string or native Problem).
    pub input_gen:    Duration,
    /// Time spent inside the theorem prover itself.
    pub prover_run:   Duration,
    /// Time spent parsing the prover output / extracting bindings.
    pub output_parse: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProverResult {
    pub status:     ProverStatus,
    pub raw_output: String,
    pub bindings:   Vec<Binding>,
    /// Proof steps converted to SUO-KIF, populated when a proof is found.
    pub proof_kif:  Vec<crate::tptp::kif::KifProofStep>,
    /// Per-phase timing breakdown (not serialized).
    #[serde(skip)]
    pub timings:    ProverTimings,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProverStatus {
    Proved,
    Disproved,
    Consistent,
    Inconsistent,
    Timeout,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Binding {
    pub variable: String,
    pub value:    String,
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} = {}", self.variable, self.value)
    }
}
