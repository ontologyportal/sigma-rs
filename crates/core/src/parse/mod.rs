// crates/core/src/parse/mod.rs
//
// Parse submodule -- extensible for multiple input formats.
// Currently only KIF is supported.

pub mod kif;
pub mod tptp;
pub mod tq;
pub mod doc;
pub mod ast;
pub mod span;
pub mod macros;
pub mod error;
pub mod fingerprint;
pub mod document;
pub mod dialect;

pub use ast::*;
pub use span::*;
pub use fingerprint::sentence_fingerprint;
pub use error::*;
pub use document::{parse_document, ParsedDocument};

pub use crate::parse::tptp::parser::TptpParseOptions;
use crate::{parse::{doc::DocItem, tq::parse_tq}};

#[derive(Debug, Default, Clone)]
pub enum Parser {
    #[default]
    Kif,
    Tptp {
        options: Option<TptpParseOptions>
    },
    Tq
}

impl Parser {
    /// Perform full parsing on a file input
    pub fn parse(&self, inp: &str, file: &str) -> (Vec<DocItem>, Vec<(Span, Box<dyn ParseError>)>) {
        // Parse
        let (ast, errors) = match self {
            Parser::Kif => {
                let (tokens, tok_err) = kif::tokenize(&inp, file);
                let (ast, parse_err) = kif::parse(tokens, file);
                let mut errors = tok_err;
                errors.extend(parse_err);
                let doc: Vec<DocItem> = ast.into_iter().map(|ast| {
                    DocItem::Stmt(ast)
                }).collect();
                (doc, wrap_error(errors))
            },
            Parser::Tptp { options} => {
                let (tokens, tok_err) = tptp::tokenize(&inp, file);
                let (mut ast, parse_err) = tptp::parse(tokens, file, options.clone());
                let mut errors = tok_err;
                errors.extend(parse_err);
                // Only TPTP-specific literal decoding stays in the parse stage.  The
                // generic macros (quantifier collapse, top-level-`forall` strip, row-var
                // expansion) moved to the ingest/normalization stage and run there,
                // parser-free, so `SourceStore` keeps the raw parsed AST.
                for node in &mut ast {
                    macros::decode_tptp_literals(node, self);
                }
                let doc: Vec<DocItem> = ast.into_iter().map(|ast| {
                    DocItem::Stmt(ast)
                }).collect();
                (doc, wrap_error(errors))
            },
            Parser::Tq => {
                let (doc, errors) = parse_tq(inp, file);
                (doc, wrap_error(errors))
            },
        };
        (ast, errors)
    }

    /// Perform tokenization ONLY on file contents
    pub fn tokenize(&self, inp: &str, file: &str) -> (Vec<String>, Vec<(Span, Box<dyn ParseError>)>) {
        match self {
            Parser::Kif | Parser::Tq => {
                let (tokens, err) = kif::tokenize(inp, file);
                let errors = err.into_iter().map(| (span, e) | { (span, Box::new(e) as Box<dyn ParseError>) }).collect::<Vec<(Span, Box<dyn ParseError>)>>();
                (tokens.iter().map(|t| format!("{}", t).to_uppercase()).collect(), errors)
            },
            Parser::Tptp { .. } => {
                let (tokens, err) = tptp::tokenize(inp, file);
                let errors = err.into_iter().map(| (span, e) | { (span, Box::new(e) as Box<dyn ParseError>) }).collect::<Vec<(Span, Box<dyn ParseError>)>>();
                (tokens.iter().map(|t| format!("{}", t).to_uppercase()).collect(), errors)
            }
        }
    }

    /// Determine if the parser is for a test file
    pub fn is_test(&self) -> bool {
        match self {
            Parser::Kif => false,
            Parser::Tptp { options } => options.as_ref().map_or(false, |o| o.keep_conjectures),
            Parser::Tq => true,
        }
    }

    /// Create a parser from the file's extension. Returns `None` when nothing
    /// matches
    pub fn from_filename(filename: &str) -> Option<Self> {
        let ext = filename.split(".").last()?;
        let p = match ext {
            "kif" => Parser::Kif,
            // A `.p` / `.tptp` file is a theorem-proving *problem*: keep its
            // conjecture so it is recognized as a test (`is_test`) and its goal
            // surfaces as the `TestCase` query.
            "p" | "tptp" => Parser::Tptp { options: Some(TptpParseOptions {
                formulas_only: false, keep_conjectures: true, ..TptpParseOptions::default()
            }) },
            "ax" => Parser::Tptp { options: Some(TptpParseOptions { 
                formulas_only: true, ..TptpParseOptions::default() 
            }) },
            "tq" => Parser::Tq,
            _ => { return None }
        };
        Some(p)
    }

    /// Best-effort parser selection for a source. Returns `None` when nothing matches
    pub fn from_contents(contents: &str) -> Option<Parser> {
        // Content sniff over a bounded prefix (skip line/block comments cheaply
        // by just scanning for the annotated-formula keyword anywhere early).
        let head: String = contents.chars().take(4096).collect();
        if ["fof(", "cnf(", "tff(", "thf(", "tcf(", "include("]
            .iter().any(|kw| head.contains(kw))
        {
            return Some(Parser::Tptp { options: None });
        }
        if head.trim_start().starts_with('(') {
            return Some(Parser::Kif);
        }
        None
    }
}

fn wrap_error<E: ParseError + 'static>(err: Vec<(Span, E)>) -> Vec<(Span, Box<dyn ParseError>)> {
    err.into_iter()
        .map(|(span, e)| { 
            (span, Box::new(e) as Box<dyn ParseError>) 
        }).collect::<Vec<(Span, Box<dyn ParseError>)>>()
}