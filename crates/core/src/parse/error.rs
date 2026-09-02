use std::error::Error;

use super::Span;
use crate::{Diagnostic, Severity, ToDiagnostic};

pub trait ParseError: Error + ToDiagnostic + Send + Sync {
    fn get_span(&self) -> Span;
}

impl std::error::Error for Box<dyn ParseError> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        (**self).source()
    }
}

// `Box<dyn ParseError>` is the concrete parse error carrier; the
// concrete types today are `KifParseError` and `TptpParseError`.
// `ParseError: ToDiagnostic` is a supertrait bound, so we simply
// forward to the inner type's `to_diagnostic` to pick up its proper
// per-variant `code` (e.g. `kif/unbalanced-parens`) rather than the
// previous catch-all `"error"`.
impl ToDiagnostic for Box<dyn ParseError> {
    fn to_diagnostic(&self) -> Diagnostic {
        (**self).to_diagnostic()
    }
}

// Keep a Severity import path so callers that referenced `Severity`
// through this module continue to compile.
use thiserror::Error;
#[allow(unused_imports)]
use Severity as _UnusedSeverity;

#[derive(Debug, Clone, Error)]
#[allow(dead_code)]
pub enum GenericParseError {
    #[error("duplicate formula")]
    DuplicateNode { span: Span },
    #[error(
        "unknown file type ({filename}): file could not be matched to a parser by its extension"
    )]
    UnknownFileType { filename: String },
}

impl ParseError for GenericParseError {
    fn get_span(&self) -> Span {
        match self {
            GenericParseError::DuplicateNode { span } => span.clone(),
            GenericParseError::UnknownFileType { filename } => {
                Span::whole_file(filename.to_owned())
            }
        }
    }
}

impl ToDiagnostic for GenericParseError {
    fn to_diagnostic(&self) -> Diagnostic {
        let code: &'static str = match self {
            GenericParseError::DuplicateNode { .. } => "duplicate-node",
            GenericParseError::UnknownFileType { .. } => "unknown-filetype",
        };
        Diagnostic {
            kind: "parse",
            range: self.get_span(),
            severity: Severity::Error,
            code,
            message: self.to_string(),
            related: Vec::new(),
            sids: Vec::new(),
            highlight_arg: -1,
            highlight_var: None,
        }
    }
}
