// crates/core/src/parse/tptp/mod.rs
//
// TPTP (Thousand Problems) parsing submodule.

pub(crate) mod dis;
pub mod error;
pub mod parser;
pub mod syntax;
pub(crate) mod tokenizer;

pub use parser::parse;
pub(crate) use tokenizer::{
    comment_blocks, tokenize, tokenize_with_meta, tokenize_without_comments,
};
