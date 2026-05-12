//! Row variable expansion for SUMO/KIF formulas.
//!
//! ## What are row variables?
//!
//! KIF supports two kinds of variables:
//!   - `?VAR` — a regular variable, bound to exactly one term.
//!   - `@VAR` — a *row variable*, bound to a sequence of zero or more terms.
//!
//! Row variables model variadicity. A SUMO predicate like `ListFn` can take
//! any number of arguments, and the axiom describing it is written once with a
//! row variable rather than once per arity. Neither FOF nor TFF can express
//! variadic quantification directly, so row variables require *expansion*: the
//! single formula is unrolled into one concrete formula per possible arity.
//!
//! ## Expansion algorithm
//!
//! For each row variable `@VAR` in a formula, the variable is replaced with
//! successively longer sequences of regular variables (index starting at 2):
//!
//!   j = 1  ->  ?VAR2
//!   j = 2  ->  ?VAR2 ?VAR3
//!   ...
//!   j = MAX_ARITY  ->  ?VAR2 ?VAR3 ... ?VAR(MAX_ARITY+1)
//!
//! The resulting names (`?ROW2`, `?ROW3`, …) become ordinary KIF variables
//! handled by the regular variable-binding machinery. When a formula contains
//! *multiple* row variables they are expanded independently and the results
//! are **cross-producted**: two row variables each expanded to MAX_ARITY
//! arities produce MAX_ARITY^2 formulas.
//!
//! Example — single row variable:
//!
//!   (P @ROW)  ->  (P ?ROW2)
//!                (P ?ROW2 ?ROW3)
//!                (P ?ROW2 ?ROW3 ?ROW4)
//!                (P ?ROW2 ?ROW3 ?ROW4 ?ROW5)
//!                (P ?ROW2 ?ROW3 ?ROW4 ?ROW5 ?ROW6)
//!
//! Example — row variable in quantifier list and body (both occurrences expand):
//!
//!   (forall (@ROW) (P @ROW))
//!     ->  (forall (?ROW2) (P ?ROW2))
//!        (forall (?ROW2 ?ROW3) (P ?ROW2 ?ROW3))
//!        ...
//!
//! Expansion operates on the parsed AST inside `syntactic::load_kif`,
//! immediately after parsing; by the time a formula enters a later layer it
//! contains no row variables.
//!
//! ## Relationship to VariableArityRelation in TFF
//!
//! SUMO marks variadic predicates with `(instance P VariableArityRelation)`.
//! After expansion the predicate appears with different argument counts across
//! the expanded formulas. TFF is monomorphic, so every argument count needs
//! its own type declaration: `tff::ensure_declared` detects `arity == -1` and
//! emits `s__P__1`, `s__P__2`, … `s__P__MAX_ARITY`, and the call-site in
//! `translate::translate_sentence` appends `__N` to the predicate name to match.

use std::collections::HashSet;

use crate::parse::AstNode;

/// Maximum number of argument positions a row variable is expanded to.
///
/// A row variable `@VAR` is replaced by sequences of length 1 through
/// `MAX_ARITY`, producing `MAX_ARITY` concrete formulas per row variable
/// (and `MAX_ARITY^N` for N row variables).
///
/// TODO: add ability to modify this via CLI arg
pub const MAX_ARITY: usize = 5;

/// Return `true` if an [`AstNode`] tree contains any row variable (`@`-prefixed).
pub(crate) fn contains_row_var(node: &AstNode) -> bool {
    match node {
        AstNode::RowVariable { .. } => true,
        AstNode::List { elements, .. } => elements.iter().any(contains_row_var),
        _ => false,
    }
}

/// Collect all row-variable names (without the `@` prefix) from a KIF string.
///
/// Scans the raw KIF text character-by-character looking for `@` followed by
/// an alphanumeric/underscore/hyphen identifier.  Names are returned in
/// first-occurrence order and deduplicated.
///
/// Example: `"(P @ROW ?X @ROW)"` -> `["ROW"]` (one occurrence, deduped).
pub(crate) fn find_row_var_names(kif: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut chars = kif.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if ch == '@' {
            let mut name = String::new();
            while let Some(&(_, nc)) = chars.peek() {
                if nc.is_alphanumeric() || nc == '_' || nc == '-' {
                    name.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            if !name.is_empty() && seen.insert(name.clone()) {
                names.push(name);
            }
        }
    }
    names
}

/// Expand all row variables in an AST, taking the cross-product over distinct
/// row variables.
///
/// Returns `None` when the tree contains no row variables; otherwise returns
/// up to `MAX_ARITY^N` trees (for N distinct row variables). The returned
/// trees contain only regular `?VAR` variables — no `@`-prefixed variables
/// remain.
pub fn expand_row_vars(node: &mut AstNode) -> Option<Vec<AstNode>> {
    if !contains_row_var(node) {
        return None;
    }

    let row_vars = find_row_var_names(&node.to_string());

    crate::log!(Trace, "sigmakee_rs_core::parse", format!("row-var expansion of {}", node));

    let mut result: Vec<AstNode> = vec![node.clone()];
    for row_var in &row_vars {
        let mut next: Vec<AstNode> = Vec::with_capacity(result.len() * MAX_ARITY);
        for tree in &result {
            for arity in 1..=MAX_ARITY {
                let mut variant = tree.clone();
                splice_row_var(&mut variant, row_var, arity);
                next.push(variant);
            }
        }
        result = next;
    }

    crate::log!(Debug, "sigmakee_rs_core::parse", format!("row-var expansion produced {} sentences", result.len()));
    Some(result)
}

/// Replace every `@row_var` element inside `node` (recursively) with `arity`
/// regular variables `?{row_var}2 … ?{row_var}{arity+1}`, splicing them into
/// their enclosing list in place.  Variable names are bare (no `?`), matching
/// the KIF parser.  Other row variables are left untouched — each is handled in
/// its own pass of the cross-product loop.
fn splice_row_var(node: &mut AstNode, row_var: &str, arity: usize) {
    let AstNode::List { elements, .. } = node else { return };
    let mut expanded: Vec<AstNode> = Vec::with_capacity(elements.len());
    for el in std::mem::take(elements) {
        match el {
            AstNode::RowVariable { name, span } if name == row_var => {
                for n in 2..=(arity + 1) {
                    expanded.push(AstNode::Variable { name: format!("{}{}", row_var, n), span: span.clone() });
                }
            }
            mut other => {
                splice_row_var(&mut other, row_var, arity);
                expanded.push(other);
            }
        }
    }
    *elements = expanded;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kif(kif: &str) -> AstNode {
        // Raw tokenizer + parser directly: Parser::Kif.parse() would call
        // macros::expand(), consuming the row variables before the test can
        // exercise expand_row_vars itself.
        let (tokens, _) = crate::parse::kif::tokenize(kif, "__inline__");
        let (nodes, _)  = crate::parse::kif::parse(tokens, "__inline__");
        nodes.into_iter().next().unwrap()
    }

    #[test]
    fn no_row_vars_unchanged() {
        let mut node = kif("(P ?X ?Y)");
        let expanded = expand_row_vars(&mut node);
        assert!(expanded.is_none());
    }

    #[test]
    fn single_row_var_five_arities() {
        let mut node = kif("(P @ROW)");
        let expanded = expand_row_vars(&mut node);
        assert!(expanded.is_some());
        let expanded: Vec<String> = expanded.unwrap().into_iter().map(|n| { n.to_string() }).collect();
        assert_eq!(expanded.len(), 5);
        assert_eq!(expanded[0], "(P ?ROW2)");
        assert_eq!(expanded[1], "(P ?ROW2 ?ROW3)");
        assert_eq!(expanded[2], "(P ?ROW2 ?ROW3 ?ROW4)");
        assert_eq!(expanded[3], "(P ?ROW2 ?ROW3 ?ROW4 ?ROW5)");
        assert_eq!(expanded[4], "(P ?ROW2 ?ROW3 ?ROW4 ?ROW5 ?ROW6)");
    }

    #[test]
    fn row_var_in_quantifier_and_body() {
        let mut node = kif("(forall (@ROW) (P @ROW))");
        let expanded = expand_row_vars(&mut node);
        assert!(expanded.is_some());
        let expanded = expanded.unwrap();
        assert_eq!(expanded.len(), MAX_ARITY);
        assert_eq!(expanded[0].to_string(), "(forall (?ROW2) (P ?ROW2))");
        assert_eq!(expanded[1].to_string(), "(forall (?ROW2 ?ROW3) (P ?ROW2 ?ROW3))");
    }

    #[test]
    fn two_row_vars_cross_product() {
        // Two row vars -> MAX_ARITY x MAX_ARITY expansions.
        // Expansion order: the first row var (@ROW) is expanded in the outer
        // loop, the second (@ARGS) in the inner loop.  So ARGS varies fastest:
        //   index 0: ROW=1, ARGS=1  ->  (P ?ROW2)       (Q ?ARGS2)
        //   index 1: ROW=1, ARGS=2  ->  (P ?ROW2)       (Q ?ARGS2 ?ARGS3)
        //   ...
        //   index 5: ROW=2, ARGS=1  ->  (P ?ROW2 ?ROW3) (Q ?ARGS2)
        let mut node = kif("(=> (P @ROW) (Q @ARGS))");
        let expanded = expand_row_vars(&mut node);
        assert!(expanded.is_some());
        let expanded = expanded.unwrap();
        assert_eq!(expanded.len(), MAX_ARITY * MAX_ARITY);
        assert_eq!(expanded[0].to_string(), "(=> (P ?ROW2) (Q ?ARGS2))");
        assert_eq!(expanded[1].to_string(), "(=> (P ?ROW2) (Q ?ARGS2 ?ARGS3))");
        assert_eq!(expanded[MAX_ARITY].to_string(), "(=> (P ?ROW2 ?ROW3) (Q ?ARGS2))");
    }

    #[test]
    fn find_names_deduplicates() {
        let names = find_row_var_names("(P @ROW ?X @ROW)");
        assert_eq!(names, vec!["ROW"]);
    }

    #[test]
    fn contains_row_var_detects_nested() {
        let kif = "(=> (P @ROW) true)";
        let (tokens, _) = crate::parse::kif::tokenize(kif, "<test>");
        let (nodes, _)  = crate::parse::kif::parse(tokens, "<test>");
        assert!(nodes.iter().any(contains_row_var));
    }
}