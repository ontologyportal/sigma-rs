// crates/core/src/parse/kif/error.rs
//
// Span and ParseError -- source location and tokenizer/parser hard errors.
use thiserror::Error;
use crate::diagnostic::{ToDiagnostic, Diagnostic, Severity};
use super::super::{Span, error::ParseError};

/// Hard tokenizer / parser / syntax errors that prevent sentence acceptance.
#[derive(Debug, Clone, Error)]
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

    #[error("quantifier operators' first argument must be a sentence comprised only of variables")]
    QuantifierArg { span: Span },

    #[error("the first term of a sentence must be an operator, symbol, or non-row variable")]
    FirstTerm { span: Span },

    #[error("operator '{op}' cannot appear in argument position")]
    OperatorOutOfPosition { op: String, span: Span },

    #[error("sentence has only a head with no arguments; SUMO requires at least one argument")]
    SingleTermSentence { span: Span },

    #[error("operator '{op}' expects {expected} argument(s) but got {actual}")]
    OperatorArityMismatch {
        /// KIF spelling of the operator (`and`, `not`, `=>`, …)
        op:       String,
        /// Human-readable expected-arity string (`"exactly 1"`, `"at least 2"`, …)
        expected: String,
        /// Actual argument count supplied
        actual:   usize,
        span:     Span,
    },

    #[allow(dead_code)]
    #[error("{msg}")]
    Other { msg: String, span: Span },
}

impl ToDiagnostic for KifParseError {
    fn to_diagnostic(&self) -> Diagnostic {
        let code: &'static str = match self {
            KifParseError::UnterminatedString { .. }    => "kif/unterminated-string",
            KifParseError::UnexpectedChar      { .. }    => "kif/unexpected-char",
            KifParseError::EmptySentence       { .. }    => "kif/empty-sentence",
            KifParseError::UnexpectedEof       { .. }    => "kif/unexpected-eof",
            KifParseError::UnbalancedParens    { .. }    => "kif/unbalanced-parens",
            KifParseError::QuantifierArg           { .. } => "kif/quantifier-arg",
            KifParseError::FirstTerm               { .. } => "kif/first-term",
            KifParseError::OperatorOutOfPosition   { .. } => "kif/operator-out-of-position",
            KifParseError::SingleTermSentence      { .. } => "kif/single-term-sentence",
            KifParseError::OperatorArityMismatch   { .. } => "kif/operator-arity-mismatch",
            KifParseError::Other                   { .. } => "kif/other",
        };
        Diagnostic {
            kind:          "parse",
            range:         self.get_span(),
            severity:      Severity::Error,
            code,
            message:       self.to_string(),
            related:       Vec::new(),
            sids:          Vec::new(),
            highlight_arg: -1,
            highlight_var: None,
        }
    }
}

impl ParseError for KifParseError {
    fn get_span(&self) -> Span {
        match self {
            KifParseError::UnterminatedString { span }
            | KifParseError::UnexpectedChar { span, .. }
            | KifParseError::EmptySentence { span }
            | KifParseError::UnexpectedEof { span }
            | KifParseError::UnbalancedParens { span }
            | KifParseError::QuantifierArg           { span }
            | KifParseError::FirstTerm             { span }
            | KifParseError::OperatorOutOfPosition  { span, .. }
            | KifParseError::SingleTermSentence     { span }
            | KifParseError::OperatorArityMismatch  { span, .. }
            | KifParseError::Other                  { span, .. } => span.clone(),
        }
    }
}
