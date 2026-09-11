// crates/core/src/prover/mod.rs
//
// Prover API: shared types + sub-prover implementations.
// Gated: #[cfg(feature = "external-prover")] in `prover/mod.rs`; the shipped
// subprocess runners below additionally need `ask` (banned on wasm32).

// -- Prover backends -----------------------------------------------------------

/// Vampire-specific backend: subprocess runner, embedded FFI runner,
/// lowering, proof translation, axiom cache, and binding extraction.
/// Additional backends (e.g. E, Z3) would each get a peer submodule here.
#[cfg(feature = "ask")]
pub mod vampire;

/// E (eprover) backend: subprocess runner that drives the `eprover` binary
/// over TPTP/SZS.  E ships no embeddable library, so there is no FFI peer to
/// Vampire's `integrated` runner — the subprocess path is the whole backend.
#[cfg(feature = "ask")]
pub mod eprover;

#[cfg(feature = "ask")]
pub use eprover::EproverRunner;
#[cfg(feature = "integrated-prover")]
pub use vampire::IntegratedVampireRunner;
#[cfg(feature = "ask")]
pub use vampire::VampireRunner;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::super::result::ProverResult;

// -- Shared types --------------------------------------------------------------

pub trait ProverRunner: Send + Sync {
    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult;

    /// Prove a structured [`ir::Problem`](crate::trans::ir::Problem) directly.
    ///
    /// Backends that can consume the IR override this to skip TPTP
    /// serialisation entirely (the embedded FFI prover lowers the `Problem`
    /// straight into the solver).  The default serialises with the standard
    /// `kb_<sid>` axiom naming and delegates to [`Self::prove`] — correct for
    /// every text-driven subprocess backend, including its `tptp_dump_path`
    /// (`--keep`) behavior.
    fn prove_ir(
        &self,
        problem: &crate::trans::ir::Problem,
        sid_map: &[crate::types::SentenceId],
        conjecture_name: &str,
        opts: &ProverOpts,
    ) -> ProverResult {
        let tptp = crate::kb::assemble::assemble_tptp_indexed(
            problem,
            sid_map,
            &crate::kb::assemble::AssemblyOpts {
                conjecture_name,
                ..Default::default()
            },
            None,
        );
        self.prove(&tptp, opts)
    }

    /// Prove a structured [`HoProblem`](crate::trans::ir::HoProblem) (THF).
    ///
    /// The default serialises the 1-to-1 THF text and delegates to
    /// [`Self::prove`] — correct for every text-driven subprocess backend.
    /// The embedded backend overrides this to lower the HO IR straight into
    /// the FFI solver's native structures (no text round-trip), mirroring
    /// [`Self::prove_ir`].
    fn prove_ho(
        &self,
        problem: &crate::trans::ir::HoProblem,
        sid_map: &[crate::types::SentenceId],
        conjecture_name: &str,
        opts: &ProverOpts,
    ) -> ProverResult {
        let text = problem.to_thf(sid_map, conjecture_name);
        self.prove(&text, opts)
    }

    /// The timeout this runner will apply to the prover, in seconds.
    /// Returns 0 if the runner manages its own timeout independently.
    fn timeout_secs(&self) -> u32 {
        0
    }

    /// A short label for logs and `Debug` output (`Prover::Custom` has no
    /// other way to say what it wraps).
    fn name(&self) -> &str {
        "custom"
    }
}

// `ProverMode` lives in `prover::result` (ungated) — the wasm-safe
// `prover::vampire_proof::determine_status` needs it without pulling in the
// whole `ask`-gated `external` module. Re-exported here for the existing
// `external::backends::ProverMode` path.
pub use crate::prover::result::ProverMode;

/// The runner ABI: timeout + [`ProverMode`] instruction handed to
/// [`ProverRunner::prove`].  Built locally per attempt by the external layer —
/// distinct from [`ExternalOpts`](crate::ExternalOpts), the external prover
/// layer's consolidated params struct.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ProverOpts {
    pub timeout_secs: u64,
    pub mode: ProverMode,
}

impl ProverOpts {
    /// The per-attempt wall-clock budget in seconds the runner should apply.
    #[inline]
    pub fn timeout(&self) -> u64 {
        self.timeout_secs
    }
}

/// High-level backend selector.
///
/// Implements [`ProverRunner`] so callers can treat both variants uniformly.
/// Select the backend at construction time and pass the enum wherever a
/// `&dyn ProverRunner` is expected, or call [`ProverRunner::prove`] directly.
///
/// # Examples
///
/// ```no_run
/// use std::path::PathBuf;
/// use sigmakee_rs_core::prover::{Prover, ProverOpts, ProverMode, ProverRunner};
/// use sigmakee_rs_core::prover::external::backends::VampireRunner;
///
/// let prover = Prover::VampireSubprocess(VampireRunner {
///     vampire_path: PathBuf::from("/usr/bin/vampire"),
///     tptp_dump_path: None,
/// });
/// let opts = ProverOpts { timeout_secs: 5, mode: ProverMode::Prove };
/// let result = prover.prove("fof(a, conjecture, p).\n", &opts);
/// ```
#[derive(Clone, Default)]
pub enum Prover {
    /// Spawn `vampire` as a child process; communicate via TPTP stdin/stdout.
    #[cfg(feature = "ask")]
    VampireSubprocess(VampireRunner),
    /// Spawn `eprover` as a child process; communicate via TPTP stdin/stdout.
    #[cfg(feature = "ask")]
    Eprover(EproverRunner),
    /// Any caller-supplied [`ProverRunner`] -- an embedder's own transport to
    /// a TPTP prover (the browser's bridge to the Emscripten Vampire, a
    /// remote prover service, a test double).  Shared so the enum stays
    /// `Clone` for `fresh_config_clone`.
    Custom(Arc<dyn ProverRunner>),
    /// Use the embedded Vampire library via FFI.
    ///
    /// Requires the `integrated-prover` feature.
    #[cfg(feature = "integrated-prover")]
    VampireIntegrated(IntegratedVampireRunner),
    /// Default option, error on ask
    #[default]
    None,
}

impl std::fmt::Debug for Prover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "ask")]
            Prover::VampireSubprocess(r) => f.debug_tuple("VampireSubprocess").field(r).finish(),
            #[cfg(feature = "ask")]
            Prover::Eprover(r) => f.debug_tuple("Eprover").field(r).finish(),
            #[cfg(feature = "integrated-prover")]
            Prover::VampireIntegrated(r) => f.debug_tuple("VampireIntegrated").field(r).finish(),
            Prover::Custom(r) => f.debug_tuple("Custom").field(&r.name()).finish(),
            Prover::None => f.write_str("None"),
        }
    }
}

impl ProverRunner for Prover {
    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
        match self {
            #[cfg(feature = "ask")]
            Prover::VampireSubprocess(r) => r.prove(tptp, opts),
            #[cfg(feature = "ask")]
            Prover::Eprover(r) => r.prove(tptp, opts),
            Prover::Custom(r) => r.prove(tptp, opts),
            #[cfg(feature = "integrated-prover")]
            Prover::VampireIntegrated(r) => r.prove(tptp, opts),
            Prover::None => ProverResult::default(),
        }
    }

    // Delegate — the enum must forward to each variant's own `prove_ir` (the
    // trait default would re-serialise, costing the embedded backend its
    // direct-IR path).
    fn prove_ir(
        &self,
        problem: &crate::trans::ir::Problem,
        sid_map: &[crate::types::SentenceId],
        conjecture_name: &str,
        opts: &ProverOpts,
    ) -> ProverResult {
        match self {
            #[cfg(feature = "ask")]
            Prover::VampireSubprocess(r) => r.prove_ir(problem, sid_map, conjecture_name, opts),
            #[cfg(feature = "ask")]
            Prover::Eprover(r) => r.prove_ir(problem, sid_map, conjecture_name, opts),
            Prover::Custom(r) => r.prove_ir(problem, sid_map, conjecture_name, opts),
            #[cfg(feature = "integrated-prover")]
            Prover::VampireIntegrated(r) => r.prove_ir(problem, sid_map, conjecture_name, opts),
            Prover::None => ProverResult::default(),
        }
    }

    fn prove_ho(
        &self,
        problem: &crate::trans::ir::HoProblem,
        sid_map: &[crate::types::SentenceId],
        conjecture_name: &str,
        opts: &ProverOpts,
    ) -> ProverResult {
        match self {
            #[cfg(feature = "ask")]
            Prover::VampireSubprocess(r) => r.prove_ho(problem, sid_map, conjecture_name, opts),
            #[cfg(feature = "ask")]
            Prover::Eprover(r) => r.prove_ho(problem, sid_map, conjecture_name, opts),
            Prover::Custom(r) => r.prove_ho(problem, sid_map, conjecture_name, opts),
            #[cfg(feature = "integrated-prover")]
            Prover::VampireIntegrated(r) => r.prove_ho(problem, sid_map, conjecture_name, opts),
            Prover::None => ProverResult::default(),
        }
    }

    fn timeout_secs(&self) -> u32 {
        match self {
            #[cfg(feature = "ask")]
            Prover::VampireSubprocess(r) => r.timeout_secs(),
            #[cfg(feature = "ask")]
            Prover::Eprover(r) => r.timeout_secs(),
            Prover::Custom(r) => r.timeout_secs(),
            #[cfg(feature = "integrated-prover")]
            Prover::VampireIntegrated(r) => r.timeout_secs(),
            Prover::None => 0,
        }
    }
}
