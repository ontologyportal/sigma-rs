/// macros.rs
/// 
/// This module provides parse time macros or functions run on post-parsed nodes
/// but pre-symbol resolved statements

mod row_vars;
mod errors;

pub use row_vars::{expand_row_vars, MAX_ARITY as MAX_ROW_ARITY};
pub use errors::MacroExpansionError;
use crate::{AstNode, Span, parse::{ParseError, Parser}};

pub fn expand(ast: Vec<AstNode>, parser: &Parser) -> (Vec<AstNode>, Vec<(Span, impl ParseError)>) {
     // Expand row variables: a formula with @VAR becomes MAX_ARITY concrete formulas.
    let expanded: Vec<_> = ast.into_iter().flat_map(|node| {
        expand_row_vars(&node, parser)
            .into_iter()
            .collect::<Vec<_>>()
    }).collect();
    let err : Vec<(Span, MacroExpansionError)> = Vec::new();
    (expanded, err)
}