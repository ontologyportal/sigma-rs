// crates/sumo-lsp/src/handlers/symbols.rs
//
// `textDocument/documentSymbol` handler.  Emits one
// `DocumentSymbol` per root sentence -- the flat outline view
// VSCode / Neovim / Helix all render in their symbol-navigator
// panels.
//
// Driven by the SOURCE AST: stored sentences are content-addressed
// and carry no spans (provenance lives in side tables), but the
// outline needs precise source ranges — so we parse the live
// document text (cheap; the LSP already holds the rope) and walk
// the root nodes, classifying heads through the KB.

use lsp_types::{
    DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, SymbolKind,
};
use sigmakee_rs_sdk::{parse_document, AstNode, Parser};
use sigmakee_rs_sdk::AstKif;

use crate::conv::{span_to_range, uri_to_tag};
use crate::state::GlobalState;

pub fn handle_document_symbol(
    state:  &GlobalState,
    params: DocumentSymbolParams,
) -> Option<DocumentSymbolResponse> {
    let uri = params.text_document.uri;
    let tag = uri_to_tag(&uri);

    let docs = state.docs.read().ok()?;
    let doc  = docs.get(&uri)?;

    let text = doc.rope.to_string();
    let parsed = parse_document(tag, text, Parser::Kif);
    if parsed.ast.is_empty() {
        return Some(DocumentSymbolResponse::Nested(Vec::new()));
    }

    let session = state.session.read().ok()?;
    let kb = session.kb();
    let mut symbols: Vec<DocumentSymbol> = Vec::with_capacity(parsed.ast.len());
    for item in &parsed.ast {
        // Only logical statements make the outline; non-logical `Meta`
        // directives (TQ harness keys etc.) have no symbol to show.
        let Some(node) = item.as_stmt() else { continue };
        let span = node.span();
        if span.is_synthetic() { continue; }
        let range = span_to_range(&doc.rope, span);
        // Selection range: the head token's span when contained in the
        // sentence range (VSCode rejects the response otherwise).
        let sel_range = head_node(node)
            .map(AstNode::span)
            .filter(|sp| !sp.is_synthetic()
                      && sp.offset     >= span.offset
                      && sp.end_offset <= span.end_offset)
            .map(|sp| span_to_range(&doc.rope, sp))
            .unwrap_or(range);

        let (name, detail, kind) = describe_node(kb, node);

        #[allow(deprecated)]  // `deprecated` field deprecated; must still be passed
        symbols.push(DocumentSymbol {
            name,
            detail,
            kind,
            tags:            None,
            deprecated:      None,
            range,
            selection_range: sel_range,
            children:        None,
        });
    }

    Some(DocumentSymbolResponse::Nested(symbols))
}

fn head_node(node: &AstNode) -> Option<&AstNode> {
    match node {
        AstNode::List { elements, .. } => elements.first(),
        _ => None,
    }
}

/// Pull a (name, detail, kind) triple from a root AST node.
///
/// - Name: the head symbol or operator keyword.
/// - Detail: a short preview of the rest of the sentence.
/// - Kind: maps the head through the KB's classification caches.
fn describe_node(
    kb:   &sigmakee_rs_sdk::KnowledgeBase,
    node: &AstNode,
) -> (String, Option<String>, SymbolKind) {
    let AstNode::List { elements, .. } = node else {
        return (node.flat(), None, SymbolKind::NULL);
    };

    let (name, kind) = match elements.first() {
        Some(AstNode::Symbol { name, .. }) => {
            let kind = match kb.symbol_id(name) {
                Some(id) if kb.is_class(id)    => SymbolKind::CLASS,
                Some(id) if kb.is_function(id) => SymbolKind::FUNCTION,
                Some(id) if kb.is_relation(id) => SymbolKind::INTERFACE,
                _                              => SymbolKind::VARIABLE,
            };
            (name.clone(), kind)
        }
        Some(AstNode::Operator { op, .. }) => (op.name().to_string(), SymbolKind::OPERATOR),
        _ => ("<anon>".to_string(), SymbolKind::NULL),
    };

    // Detail: render args 1.. truncated.
    let detail = if elements.len() > 1 {
        let mut parts = Vec::new();
        for el in &elements[1..] {
            parts.push(el.flat());
            if parts.join(" ").len() > 40 { break; }
        }
        let joined = parts.join(" ");
        let trunc = if joined.chars().count() > 60 {
            let mut t: String = joined.chars().take(57).collect();
            t.push_str("...");
            t
        } else {
            joined
        };
        if trunc.is_empty() { None } else { Some(trunc) }
    } else {
        None
    };

    (name, detail, kind)
}
