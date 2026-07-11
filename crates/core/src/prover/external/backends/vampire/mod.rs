// crates/core/src/prover/vampire/mod.rs
//
// Vampire backend — both the subprocess runner and the embedded FFI runner,
// plus all the supporting infrastructure that is Vampire-specific.
//
// Generic prover types (ProverRunner, ProverResult, ProverStatus, …) live in
// the parent `crate::prover` module.  Everything in here is either:
//   a) a runner that speaks to Vampire specifically, or
//   b) infrastructure for translating between the Vampire wire format / FFI
//      types and the generic `trans::ir` / `prover::proof` representations.
//
// Sub-modules
// -----------
//   subprocess   — VampireRunner: spawn `vampire` as a child process
//   integrated   — IntegratedVampireRunner: call embedded Vampire via FFI
//   lower        — lower trans::ir::Problem → SysProblem (FFI)
//   native_proof — walk a native Vampire Proof into KifProofStep / IrProofStep
//   axiom_cache  — lazy whole-KB IR cache, shared by both runners
//   axiom_source — AxiomSourceIndex: map proof steps back to source axioms
//   bindings     — extract variable bindings from a native Vampire Proof

pub mod subprocess;
// Moved up to `prover::axiom_source` (no vampire coupling); path shim:
pub use super::super::super::axiom_source;

#[cfg(feature = "integrated-prover")]
pub(crate) mod lower;
#[cfg(feature = "integrated-prover")]
pub(crate) mod lower_ho;
#[cfg(feature = "integrated-prover")]
pub mod integrated;
#[cfg(feature = "integrated-prover")]
pub(crate) mod native_proof;
// The whole proof-binding extractor is parked: it is the "walk the native
// Proof for ground-term bindings" implementation that `integrated.rs`'s
// TODO calls for, fully tested but not yet wired into the integrated
// backend (which currently returns empty bindings).
crate::prover::parked! {
    #[cfg(feature = "integrated-prover")]
    pub(crate) mod bindings;
}

pub use subprocess::VampireRunner;
#[cfg(feature = "integrated-prover")]
pub use integrated::IntegratedVampireRunner;
