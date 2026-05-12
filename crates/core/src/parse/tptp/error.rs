// crates/core/src/parse/tptp/error.rs
//
// Span and ParseError -- source location and tokenizer/parser hard errors.
use thiserror::Error;
use crate::diagnostic::{ToDiagnostic, Diagnostic, Severity};
use crate::parse::error::ParseError;
use super::tokenizer::TokenKind;
use super::super::Span;

/// Hard tokenizer / parser / syntax errors that prevent sentence acceptance.
#[derive(Debug, Clone, Error)]
pub enum TptpParseError {
    #[error("unterminated string literal")]
    UnterminatedString { span: Span },

    #[error("unexpected character '{ch}'")]
    UnexpectedChar { ch: char, span: Span },

    #[error("empty quantifier variable list encountered")]
    EmptyQuantifierList { span: Span },

    #[error("unexpected end of input")]
    UnexpectedEof { span: Span },

    #[error("unexpected token: {found} ")]
    UnexpectedToken { span: Span, found: TokenKind },

    #[error("Block comment was unterminated")]
    UnterminatedBlockComment { span: Span },

    #[error("Invalid escape sequence: {ch}")]
    InvalidEscape { span: Span, ch: char },

    #[error("The TPTP include keyword is unsupported at this time, consolodate your TPTP into a single file then reparse.")]
    UnsupportedInclude { span: Span },

    #[error("unsupported TPTP language '{lang}': only fof, cnf, and tff are accepted (tff parses as untyped fof)")]
    UnsupportedLanguage { span: Span, lang: String },

    #[allow(dead_code)]
    #[error("{msg}")]
    Other { msg: String, span: Span },
}

impl ToDiagnostic for TptpParseError {
    fn to_diagnostic(&self) -> Diagnostic {
        let code: &'static str = match self {
            TptpParseError::UnterminatedString { .. }     => "tptp/unterminated-string",
            TptpParseError::UnexpectedChar      { .. }    => "tptp/unexpected-char",
            TptpParseError::UnexpectedEof       { .. }    => "tptp/unexpected-eof",
            TptpParseError::Other               { .. }    => "tptp/other",
            TptpParseError::EmptyQuantifierList { .. }    => "tptp/empty-quantifier-list",
            TptpParseError::UnexpectedToken { ..}         => "tptp/unexpected-token",
            TptpParseError::UnterminatedBlockComment { .. } => "tptp/unterminated-block-comment",
            TptpParseError::InvalidEscape { .. }          => "tptp/invalid-escape",
            TptpParseError::UnsupportedInclude { .. }     => "tptp/unsupported-include",
            TptpParseError::UnsupportedLanguage { .. }    => "tptp/unsupported-language",
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

impl ParseError for TptpParseError {
    fn get_span(&self) -> Span {
        match self {
            TptpParseError::UnterminatedString { span }
            | TptpParseError::UnexpectedChar { span, .. }
            | TptpParseError::UnexpectedEof { span }
            | TptpParseError::UnterminatedBlockComment { span }
            | TptpParseError::InvalidEscape { span, .. }
            | TptpParseError::EmptyQuantifierList { span, .. }
            | TptpParseError::UnexpectedToken { span, .. }
            | TptpParseError::UnsupportedInclude { span, .. }
            | TptpParseError::UnsupportedLanguage { span, .. }
            | TptpParseError::Other { span, .. } => span.clone(),
        }
    }
}
