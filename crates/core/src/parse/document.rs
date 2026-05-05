// crates/core/src/parse/document.rs
//
// Pure-function document parsing -- the "parse this text without
// touching any KB" primitive.
//
// Non-LSP uses: CLI "validate --dry-run" that wants diagnostics
// without mutating state, REPL one-shot parsing, test fixtures that
// assemble synthetic ASTs, any consumer that wants the token stream
// and per-root fingerprints for content-addressed workflows.
//
// The function is free-standing -- it does not take a `&mut SyntacticLayer`
// or a `&mut KnowledgeBase`.  Everything it returns is owned and
// self-contained: the AST, the token stream (retained for
// semantic-highlighting and other offset-based tooling), per-sentence
// span + fingerprint vectors, and a pre-normalised diagnostic list.
//
// `source` is the same file-tag string already used by `Sentence.file`
// throughout the crate -- no LSP-specific URI type leaks into this
// surface.

use std::sync::Arc;

use crate::diagnostic::{Diagnostic, ToDiagnostic};
use crate::parse::ast::{AstNode, Span};
use crate::parse::fingerprint::sentence_fingerprint;
use crate::parse::kif::{parse, tokenize, KifParseError, Token};

/// Result of parsing one document.  All fields are owned and
/// self-contained; the document can be passed around freely without
/// a reference back to the source buffer or a KB.
#[derive(Debug, Clone)]
pub struct ParsedDocument {
    /// File-tag string (matches `Sentence.file`).
    pub source:       String,
    /// Original text, shared cheaply when the document is cloned.
    pub text:         Arc<str>,
    /// Full token stream, with spans.  Retained so downstream tooling
    /// (semantic highlighting, format-on-type) can avoid re-tokenising.
    pub tokens:       Vec<Token>,
    /// Top-level AST nodes, in source order.
    pub ast:          Vec<AstNode>,
    /// Parse and semantic diagnostics collected during this pass.
    /// Diagnostics surfaced later (e.g. by `KnowledgeBase::validate_all`)
    /// are appended by the caller and aren't included here.
    pub diagnostics:  Vec<Diagnostic>,
    /// Per-root-sentence fingerprint, positionally aligned with `ast`.
    /// Used by file-level diff protocols to detect which root sentences
    /// are unchanged across an edit.
    pub root_hashes:  Vec<u64>,
    /// Per-root-sentence span, positionally aligned with `ast` and
    /// `root_hashes`.  Carries the `(` through `)` range for each root.
    pub root_spans:   Vec<Span>,
}

impl ParsedDocument {
    /// True when `diagnostics` contains at least one entry at
    /// error severity (as opposed to warning / info / hint).
    pub fn has_errors(&self) -> bool {
        use crate::diagnostic::Severity;
        self.diagnostics.iter().any(|d| d.severity == Severity::Error)
    }
}

/// Parse `text` tagged as `source` into a [`ParsedDocument`].
///
/// Runs the full KIF pipeline (tokenise -> parse -> macro-expand) and
/// collects every diagnostic encountered.  Does **not** run semantic
/// validation -- that requires a `KnowledgeBase` and is the caller's
/// decision.  Even when diagnostics are non-empty, the returned
/// `ast` contains whatever well-formed sentences were recoverable;
/// partial parses are a feature, not a bug.
pub fn parse_document(source: impl Into<String>, text: impl Into<Arc<str>>) -> ParsedDocument {
    let source: String   = source.into();
    let text:   Arc<str> = text.into();

    let (tokens, tok_errs) = tokenize(&text, &source);
    let (ast,    parse_errs) = parse(tokens.clone(), &source);

    // Surface tokenizer + parser errors as diagnostics.  Both are
    // `KifParseError` variants already.
    let mut diagnostics: Vec<Diagnostic> = Vec::with_capacity(tok_errs.len() + parse_errs.len());
    for (_, e) in &tok_errs   { diagnostics.push((e as &KifParseError).to_diagnostic()); }
    for (_, e) in &parse_errs { diagnostics.push((e as &KifParseError).to_diagnostic()); }

    // Root-level fingerprints + spans.  Non-list roots are rare
    // (the parser usually rejects them) but keep the vectors aligned
    // with `ast` positionally for robust zip use downstream.
    let root_hashes: Vec<u64>  = ast.iter().map(sentence_fingerprint).collect();
    let root_spans:  Vec<Span> = ast.iter().map(|n| n.span().clone()).collect();

    ParsedDocument {
        source,
        text,
        tokens,
        ast,
        diagnostics,
        root_hashes,
        root_spans,
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_parse_returns_owned_ast() {
        let doc = parse_document("t", "(subclass Human Animal)");
        assert_eq!(doc.ast.len(), 1);
        assert_eq!(doc.root_hashes.len(), 1);
        assert_eq!(doc.root_spans.len(),  1);
        assert!(doc.diagnostics.is_empty());
        assert!(!doc.has_errors());
    }

    #[test]
    fn malformed_file_preserves_valid_sentences() {
        // `(` alone is malformed; the second sentence is well-formed.
        // The AST should still contain the valid sentence; diagnostics
        // should capture the bad one.
        let doc = parse_document("t", "(\n(subclass Human Animal)");
        assert!(doc.has_errors(), "expected error diagnostic");
        assert!(!doc.ast.is_empty(), "valid sentence must survive");
        assert!(doc.ast.iter().any(|n| matches!(n, AstNode::List { .. })));
    }

    #[test]
    fn diagnostics_carry_parse_spans() {
        let doc = parse_document("t", "(");
        assert!(!doc.diagnostics.is_empty());
        let d = &doc.diagnostics[0];
        assert!(d.code.starts_with("parse/"));
        assert_eq!(d.range.file, "t");
    }

    #[test]
    fn root_hashes_align_with_ast() {
        let doc = parse_document("t",
            "(instance A B) (instance A B) (instance C D)");
        assert_eq!(doc.ast.len(),          3);
        assert_eq!(doc.root_hashes.len(),  3);
        // Identical sentences -> identical hashes.
        assert_eq!(doc.root_hashes[0], doc.root_hashes[1]);
        assert_ne!(doc.root_hashes[0], doc.root_hashes[2]);
    }

    #[test]
    fn token_stream_is_retained() {
        let doc = parse_document("t", "(subclass Human Animal)");
        // (  subclass  Human  Animal  )  = 5 tokens
        assert_eq!(doc.tokens.len(), 5);
    }

    #[test]
    fn root_span_covers_full_sentence() {
        let src = "(subclass Human Animal)";
        let doc = parse_document("t", src);
        let sp  = &doc.root_spans[0];
        assert_eq!(sp.offset,     0);
        assert_eq!(sp.end_offset, src.len());
    }

    #[test]
    fn text_is_shared_cheaply() {
        let doc    = parse_document("t", "(P)");
        let text2  = Arc::clone(&doc.text);
        assert!(Arc::ptr_eq(&doc.text, &text2));
    }
}
