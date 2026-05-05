// crates/core/src/vampire/converter/mod.rs
//
// Native converter: KIF sentence -> vampire_prover::ir::Formula.
//
// Produces pure-Rust IR values that can be consumed by either the embedded
// solver (`lower_problem(...).solve()`) or the subprocess solver
// (`problem.to_tptp()` piped to vampire stdin).  Declarations for typed
// sorts, functions, and predicates are registered on the Problem as the
// conversion proceeds, so the resulting Problem can be serialised directly
// without a separate preamble pass.
//
// Two modes are supported:
//
//   Mode::Tff: direct typed-predicate encoding
//     `(instance A Entity)` -> `instance(A, Entity)` with
//     `Predicate::typed("instance", &[$i, $i])` declared once.
//
//   Mode::Fof: holds-reification encoding
//     `(instance A Entity)` -> `s__holds(s__instance__m, A, Entity)` with
//     `Predicate::new("s__holds", 3)`.
//
// Module layout:
//   common.rs  -- struct, lifecycle, state management, dispatchers, helpers
//                 used by both modes; free-standing variable collectors.
//   fof.rs     -- FOF-only `impl NativeConverter` methods.
//   tff.rs     -- TFF-only `impl NativeConverter` methods plus declaration
//                 registration (sorts/funcs/preds) which only lives in TFF.
//
// Gated: requires the `vampire` feature.

mod common;
mod fof;
mod tff;
pub(crate) mod layer;
pub(crate) mod sort;

pub use common::{Mode, NativeConverter};
// `QueryVarMap` is consumed by feature-gated callers (`vampire/bindings.rs`
// under `integrated-prover`, `kb/prove.rs` under `ask`).  Allow the
// "unused" lint so default-feature builds compile cleanly.
#[allow(unused_imports)]
pub use common::QueryVarMap;
