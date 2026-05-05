// crates/sumo-lsp/src/handlers/completion.rs
//
// `textDocument/completion` handler.
//
// Context-aware completion over the shared KB.  Three cases are
// handled, detected by walking the retained token stream up to
// the cursor and tracking the paren stack:
//
//   * `(<CURSOR>`                   - sentence-head position.
//                                     Suggest operators + every
//                                     relation that actually appears
//                                     as a sentence head somewhere.
//
//   * `(head <args> <CURSOR>`       - argument position.
//                                     When `head`'s domain for this
//                                     arg is declared, filter the
//                                     symbol set to instances /
//                                     members of that class;
//                                     otherwise offer every symbol.
//
//   * Anywhere else (between forms, on whitespace, inside a
//     string) - return nothing.  LSP convention is empty response,
//     not an error.

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse,
    Documentation, MarkupContent, MarkupKind,
};

use sigmakee_rs_core::{KnowledgeBase, TokenKind};

use crate::conv::{position_to_offset, uri_to_tag};
use crate::state::GlobalState;

// -- Public entry point ------------------------------------------------------

pub fn handle_completion(
    state:  &GlobalState,
    params: CompletionParams,
) -> Option<CompletionResponse> {
    let uri      = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;

    let docs   = state.docs.read().ok()?;
    let doc    = docs.get(&uri)?;
    let parsed = doc.parsed.as_ref()?;
    let offset = position_to_offset(&doc.rope, position);
    let tag    = uri_to_tag(&uri);
    let _ = tag;  // reserved for per-file scoping in later phases

    let kb = state.kb.read().ok()?;

    let ctx = classify_cursor_context(&parsed.tokens, offset);
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
    /// Cursor sits right inside an opening paren with no content
    /// yet (whitespace allowed): `(<|>` or `( <|>`.
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

/// Walk `tokens` up to `cursor_offset` and classify the context.
///
/// The walker tracks a paren stack where each frame records the
/// head name (`None` until the first non-paren token is consumed)
/// and the count of arguments seen so far.  At the cursor the
/// topmost frame dictates the context.
fn classify_cursor_context(tokens: &[sigmakee_rs_core::Token], cursor_offset: usize) -> CompletionCtx {
    #[derive(Default)]
    struct Frame {
        head:     Option<String>,
        arg_count: usize,
    }
    let mut stack: Vec<Frame> = Vec::new();

    for tok in tokens {
        // Stop once we've crossed the cursor.  A token that starts
        // at the cursor offset is considered "at" the cursor and
        // excluded; the client normally replaces / completes it.
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
                        // Atypical -- a variable or literal in head
                        // position is a parse error.  Mark the head
                        // anyway so subsequent tokens count as args;
                        // Sentinel string leaves the completion
                        // logic unconfused.
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

/// Offer every logical operator plus every relation name that
/// appears as a sentence head somewhere in the KB.
fn suggest_heads(kb: &KnowledgeBase) -> Vec<CompletionItem> {
    let mut out: Vec<CompletionItem> = OP_KEYWORDS.iter().map(|op| CompletionItem {
        label:  op.to_string(),
        kind:   Some(CompletionItemKind::KEYWORD),
        detail: Some("logical operator".to_string()),
        ..Default::default()
    }).collect();

    for name in kb.head_names() {
        if kb.symbol_is_skolem(name) { continue; }
        out.push(item_for_symbol(kb, name));
    }

    out
}

const OP_KEYWORDS: &[&str] = &[
    "and", "or", "not", "=>", "<=>", "equal", "forall", "exists",
];

// -- Argument suggestions ----------------------------------------------------

/// Offer symbols that satisfy the declared domain at this argument
/// position.  Falls back to every interned symbol when no domain
/// is declared.  Skolem symbols are filtered out.
fn suggest_args(kb: &KnowledgeBase, head: &str, arg_idx: usize) -> Vec<CompletionItem> {
    let expected = kb.expected_arg_class(head, arg_idx);
    let mut out: Vec<CompletionItem> = Vec::new();

    for (_id, name) in kb.iter_symbols() {
        if kb.symbol_is_skolem(name) { continue; }
        if let Some(class) = &expected {
            // Keep only symbols that are either an instance of
            // `class` or have `class` in their ancestor chain.
            // Leveraging `has_ancestor` is the cheapest check
            // against the taxonomy cache.
            let Some(id) = kb.symbol_id(name) else { continue; };
            if !(kb.has_ancestor(id, class) || name == class) {
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
    use sigmakee_rs_core::parse_document;

    fn tokens_for(src: &str) -> Vec<sigmakee_rs_core::Token> {
        parse_document("t.kif", src).tokens
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
        // `(forall ` -- head is the forall keyword, arg_idx=1.
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
