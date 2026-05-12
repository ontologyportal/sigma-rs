use std::error::Error;

use crate::{Diagnostic, Severity, ToDiagnostic};
use super::Span;

pub trait ParseError: Error + ToDiagnostic {
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
#[allow(unused_imports)]
use Severity as _UnusedSeverity;
use thiserror::Error;

#[derive(Debug, Clone, Error)]
#[allow(dead_code)]
pub enum GenericParseError {
    #[error("duplicate formula")]
    DuplicateNode { span: Span },
}