// crates/core/src/parse/mod.rs
//
// Parse submodule -- extensible for multiple input formats.
// Currently only KIF is supported.

pub mod kif;
pub mod ast;
pub mod macros;
pub mod error;
pub mod fingerprint;
pub mod document;

pub use ast::*;
pub use error::*;
pub use fingerprint::sentence_fingerprint;
pub use document::{parse_document, ParsedDocument};

use crate::parse::kif::tokenizer::Token;

pub enum Parser {
    Kif
}

impl Parser {
    pub fn parse(&self, inp: &str, file: &str) -> (Vec<AstNode>, Vec<(Span, Box<dyn ParseError>)>) {
        // Parse
        let (ast, parse_err) = match self {
            Parser::Kif => {
                let (tokens, tok_err) = kif::tokenize(&inp, file);
                let (ast, parse_err) = kif::parse(tokens, file);
                let mut errors = tok_err;
                errors.extend(parse_err);
                (ast, errors)
            }
        };
        // Macros
        let (expanded, mac_err) = macros::expand(ast, self);

        let mut errors = parse_err.into_iter()
            .map(|(span, e)| { 
                (span, Box::new(e) as Box<dyn ParseError>) 
            }).collect::<Vec<(Span, Box<dyn ParseError>)>>();

        errors.extend(mac_err.into_iter().map(|(span, e)| { (span, Box::new(e) as Box<dyn ParseError>) } ));

        (expanded, errors)
    }

    pub fn tokenize(&self, inp: &str, file: &str) -> (Vec<Token>, Vec<(Span, Box<dyn ParseError>)>) {
        match self {
            Parser::Kif => {
                let (tokens, err) = kif::tokenize(inp, file);
                let errors = err.into_iter().map(| (span, e) | { (span, Box::new(e) as Box<dyn ParseError>) }).collect::<Vec<(Span, Box<dyn ParseError>)>>();
                (tokens, errors)
            }
        }
    }
}