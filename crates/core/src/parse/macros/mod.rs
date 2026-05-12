/// macros.rs
/// 
/// This module provides parse time macros or functions run on post-parsed nodes
/// but pre-symbol resolved statements

mod row_vars;
mod errors;
mod quantifiers;
mod arith;
mod literals;
mod caf;

pub(crate) use row_vars::expand_row_vars;
pub(crate) use quantifiers::collapse_quantifiers;
pub(crate) use quantifiers::strip_top_level_forall;
pub(crate) use literals::decode_tptp_literals;
pub(crate) use arith::fold_arithmetic;
pub(crate) use caf::{normalize_ast, flatten_connectives, split_top_level_and, push_negation_inward};

use crate::AstNode;

/// Run the generic, parser-free macro transforms on one parsed root node and
/// return the resulting set of formulas.  Row-variable expansion fans one input
/// out to several (`MAX_ARITY^N`), so this returns a `Vec`.
///
/// Called at the ingest/normalization stage — *not* in `Parser::parse`.  The
/// only macro that stays in the parse stage is the TPTP-specific
/// [`decode_tptp_literals`], which needs the parser kind.
pub(crate) fn expand_node(node: AstNode) -> Vec<AstNode> {
    // AXIOMS: strip the top-level `(forall …)` — SUMO's implicit-universal
    // convention.  Harmless (the clausifier closes free vars universally
    // anyway) and it keeps stored axioms compact, which matters for the
    // forall-heavy TPTP axiom sets.
    expand_node_inner(node, true)
}

/// Like [`expand_node`] but PRESERVES a leading `(forall …)`.  Used for
/// the CONJECTURE, where the universal quantifier is load-bearing: a
/// genuinely universal conjecture `∀X. φ` must stay distinguishable from
/// SUMO's implicit-EXISTENTIAL free-variable query.  Stripping it made
/// the two identical, so the refutation negation universalized
/// (`∀X. ¬φ`, unsound — `¬(X=Y)` collapses to FALSE) instead of
/// skolemizing (`∃X. ¬φ`).  Keeping the quantifier lets
/// `lift_form`→`nnf` flip ∀→∃ and skolemize correctly; genuinely free
/// SUMO query variables (no wrapper) are untouched either way.
pub(crate) fn expand_node_conjecture(node: AstNode) -> Vec<AstNode> {
    expand_node_inner(node, false)
}

fn expand_node_inner(mut node: AstNode, strip_forall: bool) -> Vec<AstNode> {
    fold_arithmetic(&mut node);
    collapse_quantifiers(&mut node);
    push_negation_inward(&mut node);
    if strip_forall {
        strip_top_level_forall(&mut node);
    }
    flatten_connectives(&mut node);
    let expanded = match expand_row_vars(&mut node) {
        Some(expanded) => expanded,
        None           => vec![node],
    };
    // Split top-level conjunctions into independent roots (after row-var fan-out,
    // before CAF) so each conjunct is its own assertion.
    expanded.into_iter().flat_map(split_top_level_and).collect()
}