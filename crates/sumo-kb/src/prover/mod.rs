// crates/sumo-kb/src/prover/mod.rs
//
// Prover API: shared types + sub-prover implementations.
// Gated: #[cfg(feature = "ask")] in lib.rs.

#[cfg(all(feature = "ask", target_arch = "wasm32"))]
compile_error!("sumo-kb: the `ask` feature is not available on wasm32 targets");

use std::fmt;

// ── Sub-prover modules ────────────────────────────────────────────────────────

pub mod subprocess;

#[cfg(feature = "integrated-prover")]
pub mod embedded;

pub use subprocess::VampireRunner;

#[cfg(feature = "integrated-prover")]
pub use embedded::EmbeddedProverRunner;

// ── Shared types ──────────────────────────────────────────────────────────────

pub trait ProverRunner: Send + Sync {
    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult;
}

pub struct ProverOpts {
    pub timeout_secs: u32,
    pub mode: ProverMode,
}

pub enum ProverMode {
    Prove,
    CheckConsistency,
}

pub struct ProverResult {
    pub status:     ProverStatus,
    pub raw_output: String,
    pub bindings:   Vec<Binding>,
    /// Proof steps converted to SUO-KIF, populated when a proof is found.
    pub proof_kif:  Vec<crate::tptp::kif::KifProofStep>,
}

pub enum ProverStatus {
    Proved,
    Disproved,
    Consistent,
    Inconsistent,
    Timeout,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub variable: String,
    pub value:    String,
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} = {}", self.variable, self.value)
    }
}
