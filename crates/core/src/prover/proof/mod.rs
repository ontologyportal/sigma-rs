// crates/core/src/prover/proof/mod.rs
//
// Proof representation, emission, and parsing — the backend-agnostic proof
// vocabulary shared by the native (`saturate`) and external (subprocess)
// provers.  Submodules:
//
//   * `model` — the proof step types (`KifProofStep`, `IrProofStep`) and the
//     `kb_<sid>` source-name decoder;
//   * `emit`  — TPTP-formula → KIF/AST conversion and dialect emission
//     (`formula_to_ast`/`formula_to_kif`, `proof_to_ast`/`emit_proof`,
//     `proof_steps_to_kif`);
//   * `tstp`  — TSTP transcript parsing + SUO-KIF binding extraction, consumed
//     by the subprocess backends (previously `prover::tptp_proof`).
//   * `graphviz` — `render_graphviz`, proof → Graphviz DOT digraph.
//
// Everything is re-exported flat at `crate::prover::proof::*`, so the module
// split is internal — callers' existing `crate::prover::proof::{…}` paths are
// unchanged.

mod model;
mod emit;
mod graphviz;
// TSTP transcript parsing (subprocess-backend only) pulls in `regex`, an
// `ask`-only dep; the native prover's proof vocabulary lives in `model`/`emit`.
#[cfg(feature = "ask")]
pub(crate) mod tstp;

pub use model::{IrProofStep, KifProofStep};
pub(crate) use model::parse_kb_axiom_name;
pub use emit::{emit_proof, formula_to_ast, formula_to_kif, proof_steps_to_kif, proof_to_ast};
pub use graphviz::render_graphviz;
