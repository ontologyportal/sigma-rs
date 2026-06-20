// crates/core/src/prover/eprover/mod.rs
//
// E (eprover) backend.
//
// Generic prover types (ProverRunner, ProverResult, ProverStatus, …) live in
// the parent `crate::prover` module; the backend-agnostic TSTP proof parsing
// lives in `crate::prover::tptp_proof`.  Everything in here is E-specific:
// command-line construction, E's `#`-prefixed SZS / `# Failure:` marker
// dialect, and E's `c_0_N` / `i_0_N` proof-step naming.
//
// Unlike Vampire, E exposes no embeddable C library, so there is no FFI
// ("integrated") peer — the subprocess runner is the entire backend.
//
// Sub-modules
// -----------
//   subprocess — EproverRunner: spawn `eprover` as a child process

pub mod subprocess;

pub use subprocess::EproverRunner;
