//! `textDocument/completion` handler.
//!
//! Context-aware completion over the shared KB. Three cases are handled by
//! tracking the paren stack up to the cursor:
//!
//!   * `(<CURSOR>` — sentence-head position: suggest operators plus every
//!     relation that appears as a sentence head.
//!   * `(head <args> <CURSOR>` — argument position: when `head`'s domain for
//!     this arg is declared, filter symbols to instances/members of that
//!     class; otherwise offer every symbol.
//!   * Anywhere else (between forms, whitespace, inside a string) — return
//!     an empty response.

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse,
    Documentation, MarkupContent, MarkupKind,
};

use sigmakee_rs_core::{KnowledgeBase, TokenKind};

use crate::conv::{position_to_offset, uri_to_tag};
use crate::state::GlobalState;

// -- Public entry point ------------------------------------------------------

/// Handle a `textDocument/completion` request, returning context-aware
/// completion items or `None` when the document or KB is unavailable.
pub fn handle_completion(
    state:  &GlobalState,
    params: CompletionParams,
) -> Option<CompletionResponse> {
    let uri      = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;

    let docs   = state.docs.read().ok()?;
    let doc    = docs.get(&uri)?;
    let offset = position_to_offset(&doc.rope, position);
    let tag    = uri_to_tag(&uri);

    let kb = state.kb.read().ok()?;

    let text         = String::from(&doc.rope);
    let (tokens, _e) = sigmakee_rs_core::tokenize_kif(&text, &tag);

    let ctx = classify_cursor_context(&tokens, offset);
    let items = match ctx {
        CompletionCtx::SentenceHead  => suggest_heads(&kb),
        CompletionCtx::ArgPosition { head, arg_idx } =>
            suggest_args(&kb, &head, arg_idx),
        CompletionCtx::Free          => Vec::new(),
    };

    Some(CompletionResponse::Array(items))
}

// -- Context classification --------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum CompletionCtx {
    /// Cursor sits inside an opening paren with no content yet
    /// (whitespace allowed): `(<|>` or `( <|>`.
    SentenceHead,
    /// Inside a list whose head is already determined, at argument
    /// position `arg_idx` (1-based element index: head is 0,
    /// first arg is 1, etc.).
    ArgPosition {
        head:    String,
        arg_idx: usize,
    },
    /// Top-level (between forms), inside a string, or any other
    /// non-completable position.
    Free,
}

/// Walk `tokens` up to `cursor_offset` and classify the completion context.
///
/// Tracks a paren stack where each frame records the head name (`None` until
/// the first non-paren token is consumed) and the argument count seen so far;
/// the topmost frame at the cursor determines the context.
fn classify_cursor_context(tokens: &[sigmakee_rs_core::Token], cursor_offset: usize) -> CompletionCtx {
    #[derive(Default)]
    struct Frame {
        head:     Option<String>,
        arg_count: usize,
    }
    let mut stack: Vec<Frame> = Vec::new();

    for tok in tokens {
        // A token starting at the cursor offset is "at" the cursor and excluded.
        if tok.span.offset >= cursor_offset { break; }

        match &tok.kind {
            TokenKind::LParen => stack.push(Frame::default()),
            TokenKind::RParen => { stack.pop(); }
            TokenKind::Operator(op) => {
                if let Some(top) = stack.last_mut() {
                    if top.head.is_none() {
                        top.head = Some(op.name().to_string());
                    } else {
                        top.arg_count += 1;
                    }
                }
            }
            TokenKind::Symbol(name) => {
                if let Some(top) = stack.last_mut() {
                    if top.head.is_none() {
                        top.head = Some(name.clone());
                    } else {
                        top.arg_count += 1;
                    }
                }
            }
            TokenKind::Variable(_)
            | TokenKind::RowVariable(_)
            | TokenKind::Str(_)
            | TokenKind::Number(_) => {
                if let Some(top) = stack.last_mut() {
                    if top.head.is_none() {
                        // A variable or literal in head position is a parse
                        // error; the empty-string sentinel keeps subsequent
                        // tokens counting as args.
                        top.head = Some(String::new());
                    } else {
                        top.arg_count += 1;
                    }
                }
            }
        }
    }

    match stack.last() {
        None => CompletionCtx::Free,
        Some(f) if f.head.is_none() => CompletionCtx::SentenceHead,
        Some(f) => CompletionCtx::ArgPosition {
            head:    f.head.clone().unwrap_or_default(),
            arg_idx: f.arg_count + 1,
        },
    }
}

// -- Head suggestions --------------------------------------------------------

/// Offer every logical operator plus every relation name that appears as a
/// sentence head in the KB (Skolem symbols excluded).
fn suggest_heads(kb: &KnowledgeBase) -> Vec<CompletionItem> {
    let mut out: Vec<CompletionItem> = OP_KEYWORDS.iter().map(|op| CompletionItem {
        label:  op.to_string(),
        kind:   Some(CompletionItemKind::KEYWORD),
        detail: Some("logical operator".to_string()),
        ..Default::default()
    }).collect();

    for name in kb.head_names() {
        if kb.symbol_is_skolem(&name) { continue; }
        out.push(item_for_symbol(kb, &name));
    }

    out
}

const OP_KEYWORDS: &[&str] = &[
    "and", "or", "not", "=>", "<=>", "equal", "forall", "exists",
];

// -- Argument suggestions ----------------------------------------------------

/// Offer symbols satisfying the declared domain at this argument position.
/// Falls back to every interned symbol when no domain is declared; Skolem
/// symbols are filtered out.
fn suggest_args(kb: &KnowledgeBase, head: &str, arg_idx: usize) -> Vec<CompletionItem> {
    let expected = kb.expected_arg_class(head, arg_idx);
    let mut out: Vec<CompletionItem> = Vec::new();

    for (_id, name) in kb.iter_symbols() {
        let name = name.as_str();
        if kb.symbol_is_skolem(name) { continue; }
        if let Some(class) = &expected {
            // Keep only symbols that are `class` itself or have `class` in
            // their ancestor chain.
            let Some(id) = kb.symbol_id(name) else { continue; };
            if !(kb.has_ancestor(id, class) || name == class.as_str()) {
                continue;
            }
        }
        out.push(item_for_symbol(kb, name));
    }
    out
}

// -- Shared helpers ----------------------------------------------------------

fn item_for_symbol(kb: &KnowledgeBase, name: &str) -> CompletionItem {
    let kind = classify_completion_kind(kb, name);
    let documentation = kb.documentation(name, Some("EnglishLanguage")).into_iter()
        .next()
        .map(|d| Documentation::MarkupContent(MarkupContent {
            kind:  MarkupKind::Markdown,
            value: d.text,
        }));
    CompletionItem {
        label:  name.to_string(),
        kind:   Some(kind),
        documentation,
        ..Default::default()
    }
}

fn classify_completion_kind(kb: &KnowledgeBase, name: &str) -> CompletionItemKind {
    let Some(id) = kb.symbol_id(name) else { return CompletionItemKind::TEXT; };
    if kb.is_class(id)      { return CompletionItemKind::CLASS; }
    if kb.is_function(id)   { return CompletionItemKind::FUNCTION; }
    if kb.is_predicate(id)  { return CompletionItemKind::INTERFACE; }
    if kb.is_relation(id)   { return CompletionItemKind::INTERFACE; }
    if kb.is_instance(id)   { return CompletionItemKind::CONSTANT; }
    CompletionItemKind::VARIABLE
}

// -- Tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sigmakee_rs_core::tokenize_kif;

    fn tokens_for(src: &str) -> Vec<sigmakee_rs_core::Token> {
        let (toks, _errs) = tokenize_kif(src, "t.kif");
        toks
    }

    #[test]
    fn cursor_right_after_open_paren_is_sentence_head() {
        let src = "(";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, 1);
        assert_eq!(ctx, CompletionCtx::SentenceHead);
    }

    #[test]
    fn cursor_after_head_and_space_is_arg_1() {
        let src = "(subclass ";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, src.len());
        match ctx {
            CompletionCtx::ArgPosition { head, arg_idx } => {
                assert_eq!(head, "subclass");
                assert_eq!(arg_idx, 1);
            }
            other => panic!("expected ArgPosition, got {:?}", other),
        }
    }

    #[test]
    fn cursor_after_two_args_is_arg_3() {
        let src = "(subclass Human Animal ";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, src.len());
        match ctx {
            CompletionCtx::ArgPosition { head, arg_idx } => {
                assert_eq!(head, "subclass");
                assert_eq!(arg_idx, 3);
            }
            other => panic!("expected ArgPosition arg 3, got {:?}", other),
        }
    }

    #[test]
    fn cursor_at_top_level_is_free() {
        let src = "(subclass Human Animal) ";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, src.len());
        assert_eq!(ctx, CompletionCtx::Free);
    }

    #[test]
    fn nested_list_picks_innermost_frame() {
        let src = "(=> (instance ?X ";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, src.len());
        match ctx {
            CompletionCtx::ArgPosition { head, arg_idx } => {
                assert_eq!(head, "instance");
                assert_eq!(arg_idx, 2);
            }
            other => panic!("expected inner ArgPosition, got {:?}", other),
        }
    }

    #[test]
    fn operator_head_recognised() {
        let src = "(forall ";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, src.len());
        match ctx {
            CompletionCtx::ArgPosition { head, arg_idx } => {
                assert_eq!(head, "forall");
                assert_eq!(arg_idx, 1);
            }
            other => panic!("expected forall ArgPosition, got {:?}", other),
        }
    }
}
