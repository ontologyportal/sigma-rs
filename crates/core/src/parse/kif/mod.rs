// crates/core/src/parse/kif/mod.rs
//
// KIF (Knowledge Interchange Format) parsing submodule.

pub mod error;
pub mod tokenizer;
pub mod parser;
pub(crate) mod dis;

pub use tokenizer::{tokenize, Token, TokenKind};
pub use parser::parse;