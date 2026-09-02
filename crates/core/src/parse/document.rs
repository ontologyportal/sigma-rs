//! Pure-function document parsing: parse text without touching any KB.
//!
//! Everything returned is owned and self-contained: the AST, per-sentence span
//! and fingerprint vectors, and a diagnostic list. `source` is the file-tag
//! string used by `Sentence.file` throughout the crate.

use std::sync::Arc;

use super::Parser;
use super::Span;
use crate::diagnostic::{Diagnostic, ToDiagnostic};
use crate::parse::doc::{CommentBlock, DocItem};
use crate::parse::ParseError;

/// Result of parsing one document.  All fields are owned and
/// self-contained; the document can be passed around freely without
/// a reference back to the source buffer or a KB.
#[derive(Debug)]
pub struct ParsedDocument {
    /// File-tag string (matches `Sentence.file`).
    pub source: String,
    /// Original text, shared cheaply when the document is cloned.
    pub text: Arc<str>,
    /// Top-level AST nodes, in source order.
    pub ast: Vec<DocItem>,
    /// Hard parse errors collected during this pass (tokenizer + parser).
    /// Positionally independent of `ast` — the recovered AST nodes are
    /// returned regardless of whether errors are present.
    pub parse_errors: Vec<(Span, Box<dyn ParseError>)>,
    /// Per-root-sentence fingerprint, positionally aligned with `ast`.
    /// Used by file-level diff protocols to detect which root sentences
    /// are unchanged across an edit.
    pub root_hashes: Vec<u64>,
    /// Per-root-sentence span, positionally aligned with `ast` and
    /// `root_hashes`.  Carries the `(` through `)` range for each root.
    pub root_spans: Vec<Span>,
    /// Consolidated `;` comment blocks in source order (KIF documents only;
    /// empty for other dialects).  Lexical trivia: comments never appear in
    /// `ast` and never affect `root_hashes` -- this side list exists for
    /// source-fidelity consumers (formatters, editors).
    pub comments: Vec<CommentBlock>,
}

impl ParsedDocument {
    /// True when the document has at least one hard parse error.
    pub fn has_errors(&self) -> bool {
        !self.parse_errors.is_empty()
    }

    /// Convert `parse_errors` to [`Diagnostic`] form for LSP / display consumers.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.parse_errors
            .iter()
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
pub fn parse_document(
    source: impl Into<String>,
    text: impl Into<Arc<str>>,
    doc_type: Parser,
) -> ParsedDocument {
    let source: String = source.into();
    let text: Arc<str> = text.into();

    let ((ast, parse_errors), comments) = doc_type.parse_full(&text, &source);

    let root_hashes: Vec<u64> = ast
        .iter()
        .filter_map(|node| match node {
            // Fingerprint the bare formula: an `Annotated` statement (a `.tq`
            // hypothesis/query, a TPTP role wrapper) panics if fingerprinted
            // directly, and the store hashes stripped formulas anyway.
            DocItem::Stmt(node) => Some(node.formula().fingerprint()),
            _ => None,
        })
        .collect();
    let root_spans: Vec<Span> = ast
        .iter()
        .filter_map(|n| match n {
            DocItem::Stmt(n) => Some(n.span().clone()),
            _ => None,
        })
        .collect();

    ParsedDocument {
        source,
        text,
        ast,
        parse_errors,
        root_hashes,
        root_spans,
        comments,
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::AstNode;
    use super::*;

    #[test]
    fn pure_parse_returns_owned_ast() {
        let doc = parse_document(
            "t",
            "(subclass Human Animal)",
            Parser::Kif { options: None },
        );
        assert_eq!(doc.ast.len(), 1);
        assert_eq!(doc.root_hashes.len(), 1);
        assert_eq!(doc.root_spans.len(), 1);
        assert!(doc.parse_errors.is_empty());
        assert!(!doc.has_errors());
    }

    #[test]
    fn comments_ride_the_side_list_and_never_touch_fingerprints() {
        let plain = parse_document("t", "(subclass Dog Mammal)", Parser::Kif { options: None });
        let commented = parse_document(
            "t",
            "; taxonomy\n; two lines\n(subclass Dog ; inline\n Mammal)\n; footer",
            Parser::Kif { options: None },
        );
        assert!(!commented.has_errors());
        // Identical logical content => identical fingerprints; the AST is
        // comment-free.
        assert_eq!(plain.root_hashes, commented.root_hashes);
        // Consolidated blocks: the two header lines merge, the inline and
        // footer comments stand alone.
        let texts: Vec<&str> = commented.comments.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["taxonomy\ntwo lines", "inline", "footer"]);
        assert!(plain.comments.is_empty());
    }

    #[test]
    fn skip_comments_option_restores_the_comment_free_parse() {
        let doc = parse_document(
            "t",
            "; header\n(subclass Dog Mammal) ; trailing",
            Parser::Kif {
                options: Some(crate::parse::KifParseOptions {
                    skip_comments: true,
                }),
            },
        );
        assert!(!doc.has_errors());
        assert_eq!(doc.ast.len(), 1, "the sentence still parses");
        assert!(
            doc.comments.is_empty(),
            "skip_comments must leave no comment blocks"
        );
    }

    #[test]
    fn tptp_comments_ride_the_side_list_and_never_touch_fingerprints() {
        let plain = parse_document(
            "t.p",
            "fof(a1, axiom, subclass(dog, mammal)).",
            Parser::Tptp { options: None },
        );
        let commented = parse_document(
            "t.p",
            "% header\n% two lines\nfof(a1, axiom, /* inline */ subclass(dog, mammal)). % footer",
            Parser::Tptp { options: None },
        );
        assert!(!commented.has_errors());
        assert_eq!(plain.root_hashes, commented.root_hashes);
        let texts: Vec<&str> = commented.comments.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["header\ntwo lines", "inline", "footer"]);
        assert!(plain.comments.is_empty());
    }

    #[test]
    fn tptp_skip_comments_option_restores_the_comment_free_parse() {
        let doc = parse_document(
            "t.p",
            "% header\nfof(a1, axiom, subclass(dog, mammal)). % trailing",
            Parser::Tptp {
                options: Some(crate::parse::TptpParseOptions {
                    skip_comments: true,
                    ..Default::default()
                }),
            },
        );
        assert!(!doc.has_errors());
        assert_eq!(doc.ast.len(), 1, "the sentence still parses");
        assert!(
            doc.comments.is_empty(),
            "skip_comments must leave no comment blocks"
        );
    }

    #[test]
    fn malformed_file_preserves_valid_sentences() {
        // `(` alone is malformed; the second sentence is well-formed.
        // The AST should still contain the valid sentence; diagnostics
        // should capture the bad one.
        let doc = parse_document(
            "t",
            "(\n(subclass Human Animal)",
            Parser::Kif { options: None },
        );
        assert!(doc.has_errors(), "expected error diagnostic");
        assert!(!doc.ast.is_empty(), "valid sentence must survive");
        assert!(doc
            .ast
            .iter()
            .any(|n| matches!(n.as_stmt(), Some(AstNode::List { .. }))));
    }

    #[test]
    fn parse_errors_carry_spans() {
        let doc = parse_document("t", "(", Parser::Kif { options: None });
        assert!(!doc.parse_errors.is_empty());
        let d = doc.diagnostics();
        assert_eq!(d[0].kind, "parse");
        assert_eq!(d[0].range.file, "t");
    }

    #[test]
    fn root_hashes_align_with_ast() {
        let doc = parse_document(
            "t",
            "(instance A B) (instance A B) (instance C D)",
            Parser::Kif { options: None },
        );
        assert_eq!(doc.ast.len(), 3);
        assert_eq!(doc.root_hashes.len(), 3);
        // Identical sentences -> identical hashes.
        assert_eq!(doc.root_hashes[0], doc.root_hashes[1]);
        assert_ne!(doc.root_hashes[0], doc.root_hashes[2]);
    }

    #[test]
    fn root_span_covers_full_sentence() {
        let src = "(subclass Human Animal)";
        let doc = parse_document("t", src, Parser::Kif { options: None });
        let sp = &doc.root_spans[0];
        assert_eq!(sp.offset, 0);
        assert_eq!(sp.end_offset, src.len());
    }

    #[test]
    fn text_is_shared_cheaply() {
        let doc = parse_document("t", "(P)", Parser::Kif { options: None });
        let text2 = Arc::clone(&doc.text);
        assert!(Arc::ptr_eq(&doc.text, &text2));
    }
}
