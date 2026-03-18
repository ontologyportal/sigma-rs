// crates/sumo-kb/src/parse/kif/error.rs
//
// Span and ParseError — source location and tokenizer/parser hard errors.

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Span ──────────────────────────────────────────────────────────────────────

/// Source location (1-based line and column).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub file:   String,
    pub line:   u32,
    pub col:    u32,
    pub offset: usize,
}

impl std::fmt::Display for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.col)
    }
}

// ── ParseError ────────────────────────────────────────────────────────────────

/// Hard tokenizer / parser / syntax errors that prevent sentence acceptance.
#[derive(Debug, Clone, Error)]
pub enum ParseError {
    #[error("unterminated string literal")]
    UnterminatedString { span: Span },

    #[error("unexpected character '{ch}'")]
    UnexpectedChar { ch: char, span: Span },

    #[error("sentence with no terms encountered")]
    EmptySentence { span: Span },

    #[error("unexpected end of input")]
    UnexpectedEof { span: Span },

    #[error("unbalanced parentheses")]
    UnbalancedParens { span: Span },

    #[error("operator '{op}' outside first-term position")]
    OperatorOutOfPosition { op: String, span: Span },

    #[error("quantifier operators' first argument must be a sentence comprised only of variables")]
    QuantifierArg { span: Span },

    #[error("the first term of a sentence must be an operator, symbol, or non-row variable")]
    FirstTerm { span: Span },

    #[error("{msg}")]
    Syntax { msg: String, span: Span },

    #[error("{msg}")]
    Other { msg: String },
}
