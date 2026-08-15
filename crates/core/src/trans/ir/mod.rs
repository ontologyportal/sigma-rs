//! Pure-Rust intermediate representation for theorem-prover problems.
//!
//! This module defines [`Term`], [`Formula`], [`Sort`], [`Function`], [`Predicate`],
//! and [`Problem`] as plain Rust data structures with no dependency on any C++ prover
//! library.  It is the canonical representation for:
//!
//! - Serialising problems to TPTP (via [`Formula::to_tptp`], [`Problem::to_tptp`]).
//! - Parsing TPTP text back into structured IR (via [`parse_tptp`]).
//! - Passing problems to the embedded Vampire backend (via `crate::prover::vampire::lower`).
//! - Consumers that construct problems without solving them.
//!
//! The IR is intentionally minimal: it models the logical structure of a TPTP
//! problem and nothing else.  Proof-search options and C-ABI types stay out of
//! this module.

pub mod clause;
pub mod formula;
pub mod ho;
pub mod problem;
pub mod symbol;
pub mod term;
pub(crate) mod tptp_emit;
#[cfg(feature = "ask")]
pub mod tptp_parse;

pub use formula::Formula;
pub use symbol::{Function, Interp, Predicate, Sort};
pub use term::{Term, VarId};
// `Clause`/`Literal`/`LitKind` are the native TPTP IR clause types.
#[allow(unused_imports)]
pub use clause::{Clause, LitKind, Literal as IrLiteral};
#[cfg(feature = "ask")]
pub use ho::HoProblem;
pub use ho::{HoSort, ThfConst, ThfExpr};
pub use problem::{LogicMode, Problem};
#[cfg(feature = "ask")]
pub use tptp_parse::{ParseError as TptpParseError, TptpParser};

/// Parse a TPTP string into an [`Problem`].
///
/// This is the primary entry point for the TPTP→IR parser.  Handles both
/// FOF and TFF dialects.  Returns an error if the input is syntactically
/// invalid.
#[cfg(feature = "ask")]
pub fn parse_tptp(input: &str) -> Result<Problem, TptpParseError> {
    TptpParser::parse(input)
}
