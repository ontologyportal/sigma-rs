// crates/core/src/prover/mod.rs
//
// Prover API: shared types + sub-prover implementations.
// Gated: #[cfg(feature = "ask")] in lib.rs.

#[cfg(all(feature = "ask", target_arch = "wasm32"))]
compile_error!("sigmakee-rs-core: the `ask` feature is not available on wasm32 targets");

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
pub use vampire::VampireRunner;
#[cfg(feature = "ask")]
pub use eprover::EproverRunner;
#[cfg(feature = "integrated-prover")]
pub use vampire::IntegratedVampireRunner;

use serde::{Serialize, Deserialize};

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
        problem:         &crate::trans::ir::Problem,
        sid_map:         &[crate::types::SentenceId],
        conjecture_name: &str,
        opts:            &ProverOpts,
    ) -> ProverResult {
        let tptp = crate::kb::assemble::assemble_tptp(
            problem,
            sid_map,
            &crate::kb::assemble::AssemblyOpts {
                conjecture_name,
                ..Default::default()
            },
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
        problem:         &crate::trans::ir::HoProblem,
        sid_map:         &[crate::types::SentenceId],
        conjecture_name: &str,
        opts:            &ProverOpts,
    ) -> ProverResult {
        let text = problem.to_thf(sid_map, conjecture_name);
        self.prove(&text, opts)
    }

    /// The timeout this runner will apply to the prover, in seconds.
    /// Returns 0 if the runner manages its own timeout independently.
    fn timeout_secs(&self) -> u32 { 0 }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ProverMode {
    #[default]
    Prove,
    CheckConsistency,
}

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
    pub fn timeout(&self) -> u64 { self.timeout_secs }
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
/// use sigmakee_rs_core::prover::{Prover, ProverOpts, ProverMode, ProverRunner};
///
/// let prover = Prover::VampireSubprocess(
///     sigmakee_rs_core::prover::VampireRunner::new("/usr/bin/vampire"),
/// );
/// let opts = ProverOpts { timeout_secs: 5, mode: ProverMode::Prove };
/// let result = prover.prove("fof(a, conjecture, p).\n", &opts);
/// ```
#[cfg(feature = "ask")]
#[derive(Debug, Clone, Default)]
pub enum Prover {
    /// Spawn `vampire` as a child process; communicate via TPTP stdin/stdout.
    VampireSubprocess(VampireRunner),
    /// Spawn `eprover` as a child process; communicate via TPTP stdin/stdout.
    Eprover(EproverRunner),
    /// Use the embedded Vampire library via FFI.
    ///
    /// Requires the `integrated-prover` feature.
    #[cfg(feature = "integrated-prover")]
    VampireIntegrated(IntegratedVampireRunner),
    /// Default option, error on ask
    #[default]
    None
}

#[cfg(feature = "ask")]
impl ProverRunner for Prover {
    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
        match self {
            Prover::VampireSubprocess(r)  => r.prove(tptp, opts),
            Prover::Eprover(r)            => r.prove(tptp, opts),
            #[cfg(feature = "integrated-prover")]
            Prover::VampireIntegrated(r)  => r.prove(tptp, opts),
            Prover::None => ProverResult::default()
        }
    }

    // Delegate — the enum must forward to each variant's own `prove_ir` (the
    // trait default would re-serialise, costing the embedded backend its
    // direct-IR path).
    fn prove_ir(
        &self,
        problem:         &crate::trans::ir::Problem,
        sid_map:         &[crate::types::SentenceId],
        conjecture_name: &str,
        opts:            &ProverOpts,
    ) -> ProverResult {
        match self {
            Prover::VampireSubprocess(r)  => r.prove_ir(problem, sid_map, conjecture_name, opts),
            Prover::Eprover(r)            => r.prove_ir(problem, sid_map, conjecture_name, opts),
            #[cfg(feature = "integrated-prover")]
            Prover::VampireIntegrated(r)  => r.prove_ir(problem, sid_map, conjecture_name, opts),
            Prover::None => ProverResult::default()
        }
    }

    fn prove_ho(
        &self,
        problem:         &crate::trans::ir::HoProblem,
        sid_map:         &[crate::types::SentenceId],
        conjecture_name: &str,
        opts:            &ProverOpts,
    ) -> ProverResult {
        match self {
            Prover::VampireSubprocess(r)  => r.prove_ho(problem, sid_map, conjecture_name, opts),
            Prover::Eprover(r)            => r.prove_ho(problem, sid_map, conjecture_name, opts),
            #[cfg(feature = "integrated-prover")]
            Prover::VampireIntegrated(r)  => r.prove_ho(problem, sid_map, conjecture_name, opts),
            Prover::None => ProverResult::default()
        }
    }

    fn timeout_secs(&self) -> u32 {
        match self {
            Prover::VampireSubprocess(r)  => r.timeout_secs(),
            Prover::Eprover(r)            => r.timeout_secs(),
            #[cfg(feature = "integrated-prover")]
            Prover::VampireIntegrated(r)  => r.timeout_secs(),
            Prover::None => 0
        }
    }
}
