use thiserror::Error;

use crate::Span;
use crate::parse::ParseError;

#[derive(Debug, Error)]
pub enum MacroExpansionError {
    #[error("{span}")]
    Other { span: Span },
}

impl ParseError for MacroExpansionError {
    fn get_span(&self) -> Span {
        match self {
            MacroExpansionError::Other { span } => span.clone(),
        }
    }
}