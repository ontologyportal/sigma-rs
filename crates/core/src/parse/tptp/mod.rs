// crates/core/src/parse/tptp/mod.rs
//
// TPTP (Thousand Problems) parsing submodule.

pub mod error;
pub(crate) mod tokenizer;
pub mod parser;
pub mod syntax;
pub(crate) mod dis;

pub(crate) use tokenizer::{tokenize};
pub use parser::parse;