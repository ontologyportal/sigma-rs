// crates/core/src/parse/kif/mod.rs
//
// KIF (Knowledge Interchange Format) parsing submodule.

pub(crate) mod dis;
pub mod error;
pub mod parser;
pub mod tokenizer;

pub use parser::parse;
pub use tokenizer::{comment_blocks, tokenize, Token, TokenKind};
