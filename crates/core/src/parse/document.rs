//! Pure-function document parsing: parse text without touching any KB.
//!
//! Everything returned is owned and self-contained: the AST, per-sentence span
//! and fingerprint vectors, and a diagnostic list. `source` is the file-tag
//! string used by `Sentence.file` throughout the crate.

use std::sync::Arc;

use crate::diagnostic::{Diagnostic, ToDiagnostic};
use crate::parse::ParseError;
use crate::parse::doc::DocItem;
use super::{Span};
use super::Parser;

/// Result of parsing one document.  All fields are owned and
/// self-contained; the document can be passed around freely without
/// a reference back to the source buffer or a KB.
#[derive(Debug)]
pub struct ParsedDocument {
    /// File-tag string (matches `Sentence.file`).
    pub source:       String,
    /// Original text, shared cheaply when the document is cloned.
    pub text:         Arc<str>,
    /// Top-level AST nodes, in source order.
    pub ast:          Vec<DocItem>,
    /// Hard parse errors collected during this pass (tokenizer + parser).
    /// Positionally independent of `ast` — the recovered AST nodes are
    /// returned regardless of whether errors are present.
    pub parse_errors: Vec<(Span, Box<dyn ParseError>)>,
    /// Per-root-sentence fingerprint, positionally aligned with `ast`.
    /// Used by file-level diff protocols to detect which root sentences
    /// are unchanged across an edit.
    pub root_hashes:  Vec<u64>,
    /// Per-root-sentence span, positionally aligned with `ast` and
    /// `root_hashes`.  Carries the `(` through `)` range for each root.
    pub root_spans:   Vec<Span>,
}

impl ParsedDocument {
    /// True when the document has at least one hard parse error.
    pub fn has_errors(&self) -> bool {
        !self.parse_errors.is_empty()
    }

    /// Convert `parse_errors` to [`Diagnostic`] form for LSP / display consumers.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.parse_errors.iter()
            .map(|(_, e)| e.to_diagnostic())
            .collect()
    }
}

/// Parse `text` tagged as `source` into a [`ParsedDocument`].
///
/// Runs the full KIF pipeline (tokenise -> parse -> macro-expand) and
/// collects every diagnostic encountered. Does not run semantic validation —
/// that requires a `KnowledgeBase`. Even when diagnostics are non-empty, the
/// returned `ast` contains whatever well-formed sentences were recoverable.
pub fn parse_document(source: impl Into<String>, text: impl Into<Arc<str>>, doc_type: Parser) -> ParsedDocument {
    let source: String   = source.into();
    let text:   Arc<str> = text.into();

    let (ast, parse_errors) = doc_type.parse(&text, &source);

    let root_hashes: Vec<u64>  = ast.iter().filter_map(|node| {
        match node {
            DocItem::Stmt(node) => Some(node.fingerprint()),
            _ => None,
        }
    }).collect();
    let root_spans:  Vec<Span> = ast.iter().filter_map(|n| {
        match n {
            DocItem::Stmt(n) => Some(n.span().clone()),
            _ => None
        }
    }).collect();

    ParsedDocument {
        source,
        text,
        ast,
        parse_errors,
        root_hashes,
        root_spans,
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::AstNode;

    #[test]
    fn pure_parse_returns_owned_ast() {
        let doc = parse_document("t", "(subclass Human Animal)", Parser::Kif);
        assert_eq!(doc.ast.len(), 1);
        assert_eq!(doc.root_hashes.len(), 1);
        assert_eq!(doc.root_spans.len(),  1);
        assert!(doc.parse_errors.is_empty());
        assert!(!doc.has_errors());
    }

    #[test]
    fn malformed_file_preserves_valid_sentences() {
        // `(` alone is malformed; the second sentence is well-formed.
        // The AST should still contain the valid sentence; diagnostics
        // should capture the bad one.
        let doc = parse_document("t", "(\n(subclass Human Animal)", Parser::Kif);
        assert!(doc.has_errors(), "expected error diagnostic");
        assert!(!doc.ast.is_empty(), "valid sentence must survive");
        assert!(doc.ast.iter().any(|n| matches!(n.as_stmt(), Some(AstNode::List { .. }))));
    }

    #[test]
    fn parse_errors_carry_spans() {
        let doc = parse_document("t", "(", Parser::Kif);
        assert!(!doc.parse_errors.is_empty());
        let d = doc.diagnostics();
        assert_eq!(d[0].kind, "parse");
        assert_eq!(d[0].range.file, "t");
    }

    #[test]
    fn root_hashes_align_with_ast() {
        let doc = parse_document("t",
            "(instance A B) (instance A B) (instance C D)", Parser::Kif);
        assert_eq!(doc.ast.len(),          3);
        assert_eq!(doc.root_hashes.len(),  3);
        // Identical sentences -> identical hashes.
        assert_eq!(doc.root_hashes[0], doc.root_hashes[1]);
        assert_ne!(doc.root_hashes[0], doc.root_hashes[2]);
    }

    #[test]
    fn root_span_covers_full_sentence() {
        let src = "(subclass Human Animal)";
        let doc = parse_document("t", src, Parser::Kif);
        let sp  = &doc.root_spans[0];
        assert_eq!(sp.offset,     0);
        assert_eq!(sp.end_offset, src.len());
    }

    #[test]
    fn text_is_shared_cheaply() {
        let doc    = parse_document("t", "(P)", Parser::Kif);
        let text2  = Arc::clone(&doc.text);
        assert!(Arc::ptr_eq(&doc.text, &text2));
    }
}
