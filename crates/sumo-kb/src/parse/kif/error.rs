// crates/sumo-kb/src/parse/kif/error.rs
//
// Span and ParseError -- source location and tokenizer/parser hard errors.
use thiserror::Error;
use crate::parse::ast::Span;
use crate::parse::error::ParseError;

/// Hard tokenizer / parser / syntax errors that prevent sentence acceptance.
#[derive(Debug, Error)]
pub enum KifParseError {
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

impl ParseError for KifParseError {
    fn get_span(&self) -> Span {
        match self {
            KifParseError::Other {..} => Span::default(),
            KifParseError::UnterminatedString { span }
            | KifParseError::UnexpectedChar { span, .. }
            | KifParseError::EmptySentence { span }
            | KifParseError::UnexpectedEof { span }
            | KifParseError::UnbalancedParens { span }
            | KifParseError::OperatorOutOfPosition { span, .. }
            | KifParseError::QuantifierArg { span }
            | KifParseError::FirstTerm { span }
            | KifParseError::Syntax { span, .. } => span.clone(),
        }
    }
}
