use thiserror::Error;

use crate::{Span, ToDiagnostic, Diagnostic, Severity};
use crate::parse::ParseError;

#[derive(Debug, Error)]
pub enum MacroExpansionError {
    #[allow(dead_code)]
    #[error("{span}")]
    Other { span: Span },
}

impl ToDiagnostic for MacroExpansionError {
    fn to_diagnostic(&self) -> Diagnostic {
        let code: &'static str = match self {
            MacroExpansionError::Other { .. } => "macro/other"
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

impl ParseError for MacroExpansionError {
    fn get_span(&self) -> Span {
        match self {
            MacroExpansionError::Other { span } => span.clone(),
        }
    }
}