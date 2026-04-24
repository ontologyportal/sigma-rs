// crates/sumo-lsp/src/handlers/symbols.rs
//
// `textDocument/documentSymbol` handler.  Emits one
// `DocumentSymbol` per root sentence -- the flat outline view
// VSCode / Neovim / Helix all render in their symbol-navigator
// panels.  A future extension can group by head predicate
// (`subclass Human Animal` → under a "subclass" fold) but the
// flat shape is easy to consume and hits the 80% use case of
// "jump to the line where Foo is declared".

use lsp_types::{
    DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, SymbolKind,
};

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

    let kb    = state.kb.read().ok()?;
    let roots = kb.file_roots(&tag);
    if roots.is_empty() { return Some(DocumentSymbolResponse::Nested(Vec::new())); }

    let mut symbols: Vec<DocumentSymbol> = Vec::with_capacity(roots.len());
    for &sid in roots {
        let Some(sent) = kb.sentence(sid) else { continue; };
        if sent.span.is_synthetic() { continue; }
        let range = span_to_range(&doc.rope, &sent.span);
        // Selection range: the head symbol's span if available and
        // real, otherwise the whole sentence.  VSCode rejects the
        // whole response if selectionRange is not contained in
        // fullRange, so drop back to `range` for synthetic/mixed
        // elements (cache rehydration leaves element spans at the
        // `<synthetic>` sentinel while the sentence may have a real
        // re-parsed span) and verify byte containment defensively.
        let sel_range = sent.elements.first()
            .map(|e| e.span())
            .filter(|sp| !sp.is_synthetic()
                      && sp.offset     >= sent.span.offset
                      && sp.end_offset <= sent.span.end_offset)
            .map(|sp| span_to_range(&doc.rope, sp))
            .unwrap_or(range);

        let (name, detail, kind) = describe_sentence(&kb, sent);

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

/// Pull a (name, detail, kind) triple from a sentence for symbol display.
///
/// - Name: the head symbol or operator keyword.
/// - Detail: a short preview of the rest of the sentence.
/// - Kind: maps the head to a conventional SymbolKind.
fn describe_sentence(
    kb:   &sumo_kb::KnowledgeBase,
    sent: &sumo_kb::types::Sentence,
) -> (String, Option<String>, SymbolKind) {
    use sumo_kb::types::Element;

    // Head name + kind.
    let (name, kind) = match sent.elements.first() {
        Some(Element::Symbol { id, .. }) => {
            let name = kb_sym_name(kb, *id);
            let kind = if kb.is_class(*id) {
                SymbolKind::CLASS
            } else if kb.is_function(*id) {
                SymbolKind::FUNCTION
            } else if kb.is_relation(*id) {
                SymbolKind::INTERFACE
            } else {
                SymbolKind::VARIABLE
            };
            (name, kind)
        }
        Some(Element::Op { op, .. }) => (op.name().to_string(), SymbolKind::OPERATOR),
        _ => ("<anon>".to_string(), SymbolKind::NULL),
    };

    // Detail: render args 1.. truncated.
    let detail = if sent.elements.len() > 1 {
        let mut parts = Vec::new();
        for el in &sent.elements[1..] {
            parts.push(element_preview(kb, el));
            if parts.join(" ").len() > 40 { break; }
        }
        let joined = parts.join(" ");
        let trunc  = if joined.chars().count() > 60 {
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

fn element_preview(kb: &sumo_kb::KnowledgeBase, el: &sumo_kb::types::Element) -> String {
    use sumo_kb::types::{Element, Literal};
    match el {
        Element::Symbol { id, .. }                          => kb_sym_name(kb, *id),
        Element::Variable { name, is_row: false, .. }       => format!("?{}", name),
        Element::Variable { name, is_row: true, .. }        => format!("@{}", name),
        Element::Literal { lit: Literal::Str(s), .. }       => s.clone(),
        Element::Literal { lit: Literal::Number(n), .. }    => n.clone(),
        Element::Op { op, .. }                              => op.name().to_string(),
        Element::Sub { .. }                                 => "(...)".to_string(),
    }
}

fn kb_sym_name(kb: &sumo_kb::KnowledgeBase, id: sumo_kb::SymbolId) -> String {
    kb.sym_name(id).unwrap_or_else(|| format!("<sid:{}>", id))
}
