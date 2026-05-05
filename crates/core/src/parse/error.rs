use std::error::Error;
use crate::parse::ast::Span;

pub trait ParseError: Error {
    fn get_span(&self) -> Span;
}

impl std::error::Error for Box<dyn ParseError> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        (**self).source()
    }
}