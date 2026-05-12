//! Position-based queries over the KB: given a byte offset in a source file,
//! which token is there? Drives LSP hover / goto-definition / rename, CLI
//! tools, and REPL inspection.
//!
//! These queries run against the source AST (`SourceStore`), not the interned
//! `Sentence`s, which are content-addressed and carry no spans.

use crate::AstNode;
use crate::parse::Span;
use super::SyntacticLayer;

/// One token hit from a position query, in source terms.
#[derive(Debug, Clone)]
pub struct ElementHit {
    /// Content fingerprint of the source formula the offset falls in.
    pub fingerprint: u64,
    /// Source range of the matched node.
    pub span:        Span,
    /// Symbol / variable name (no `?`/`@` sigil), if the node names something.
    pub name:        Option<String>,
    /// Whether the matched node is a variable (vs a ground symbol / other).
    pub is_variable: bool,
    /// For variables, whether it is a row variable (`@`).
    pub is_row:      bool,
}

/// Find the token at byte `offset` in `file`.
///
/// Locates the source formula covering the offset and descends into it,
/// returning the innermost non-synthetic node whose span contains the offset.
/// Falls back to the whole formula when the offset lands inside its parens but
/// on no inner token.  `None` when no formula covers the offset.
pub(crate) fn element_at_offset(store: &SyntacticLayer, file: &str, offset: usize) -> Option<ElementHit> {
    let root = store.source_node_at(file, offset)?;
    let fp = root.fingerprint();
    let target = node_at(&root, offset).unwrap_or(&root);
    Some(node_hit(target, fp))
}

/// True if `span` covers `offset` and is not a synthetic sentinel.
fn span_contains(span: &Span, offset: usize) -> bool {
    !span.is_synthetic() && offset >= span.offset && offset < span.end_offset
}

/// Descend into `node` (a `List`) for the innermost child whose span contains
/// `offset`.  Returns `None` when `node` isn't a list or no child matches.
fn node_at(node: &AstNode, offset: usize) -> Option<&AstNode> {
    let AstNode::List { elements, .. } = node else { return None };
    for el in elements {
        if !span_contains(el.span(), offset) { continue; }
        if matches!(el, AstNode::List { .. }) {
            return Some(node_at(el, offset).unwrap_or(el));
        }
        return Some(el);
    }
    None
}

/// Build an [`ElementHit`] from a matched source node.
fn node_hit(node: &AstNode, fingerprint: u64) -> ElementHit {
    let (name, is_variable, is_row) = match node {
        AstNode::Symbol      { name, .. } => (Some(name.clone()), false, false),
        AstNode::Variable    { name, .. } => (Some(name.clone()), true,  false),
        AstNode::RowVariable { name, .. } => (Some(name.clone()), true,  true),
        _                                 => (None, false, false),
    };
    ElementHit { fingerprint, span: node.span().clone(), name, is_variable, is_row }
}

// -- Symbol resolution --------------------------------------------------------

/// If the token at `offset` is a ground symbol, return its name. Variables are
/// excluded.
pub(crate) fn symbol_at_offset(store: &SyntacticLayer, file: &str, offset: usize) -> Option<String> {
    let hit = element_at_offset(store, file, offset)?;
    if hit.is_variable { return None; }
    hit.name
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(text: &str, file: &str) -> SyntacticLayer {
        let mut store = SyntacticLayer::default();
        let errs = store.load_kif(text, file);
        assert!(errs.is_empty(), "load errors: {:?}", errs);
        store
    }

    #[test]
    fn offset_on_head_symbol_returns_symbol() {
        //                     0123456789012345678901234
        let src   = "(subclass Human Animal)";
        let store = store_with(src, "t.kif");
        let hit = element_at_offset(&store, "t.kif", 3).expect("hit");
        assert_eq!(hit.name.as_deref(), Some("subclass"));
        assert_eq!(symbol_at_offset(&store, "t.kif", 3).as_deref(), Some("subclass"));
    }

    #[test]
    fn offset_on_second_symbol_returns_that_symbol() {
        let src   = "(subclass Human Animal)";
        let store = store_with(src, "t.kif");
        assert_eq!(symbol_at_offset(&store, "t.kif", 12).as_deref(), Some("Human"));
        assert_eq!(symbol_at_offset(&store, "t.kif", 17).as_deref(), Some("Animal"));
    }

    #[test]
    fn offset_in_nested_sub_sentence_finds_inner_symbol() {
        //          0         1
        //          012345678901234567
        let src   = "(=> (P ?X) (Q ?X))";
        let store = store_with(src, "t.kif");
        assert_eq!(symbol_at_offset(&store, "t.kif", 12).as_deref(), Some("Q"));
        assert_eq!(symbol_at_offset(&store, "t.kif", 5).as_deref(),  Some("P"));
    }

    #[test]
    fn offset_on_variable_is_not_a_symbol() {
        let src   = "(=> (P ?X) (Q ?X))";
        let store = store_with(src, "t.kif");
        // `?X` at offset 7 is a variable — symbol_at_offset excludes it, but
        // element_at_offset still reports it as a variable.
        assert!(symbol_at_offset(&store, "t.kif", 7).is_none());
        let hit = element_at_offset(&store, "t.kif", 7).expect("hit");
        assert!(hit.is_variable);
        assert_eq!(hit.name.as_deref(), Some("X"));
    }

    #[test]
    fn offset_outside_any_sentence_returns_none() {
        let src   = "(subclass Human Animal)";
        let store = store_with(src, "t.kif");
        assert!(element_at_offset(&store, "t.kif", 50).is_none());
    }

    #[test]
    fn offset_on_whitespace_between_elements_falls_back_to_formula() {
        // Offset 9 is the space between `subclass` and `Human`: no inner token,
        // so the hit is the whole formula (no name).
        let src   = "(subclass Human Animal)";
        let store = store_with(src, "t.kif");
        let hit = element_at_offset(&store, "t.kif", 9).expect("hit");
        assert!(hit.name.is_none());
    }

    #[test]
    fn unknown_file_returns_none() {
        let src   = "(subclass Human Animal)";
        let store = store_with(src, "t.kif");
        assert!(element_at_offset(&store, "nope.kif", 3).is_none());
    }
}
