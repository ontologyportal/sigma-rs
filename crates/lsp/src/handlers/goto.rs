// crates/sumo-lsp/src/handlers/goto.rs
//
// `textDocument/definition` handler.  Resolves the cursor offset
// to a symbol, then asks the KB for the symbol's defining
// sentence (first-declaration heuristic — see
// `KnowledgeBase::defining_sentence`) and returns its `Location`.

use lsp_types::{GotoDefinitionParams, GotoDefinitionResponse, Location};

use crate::conv::{position_to_offset, span_to_range_with_fallback, tag_to_uri, uri_to_tag};
use crate::state::GlobalState;

/// Handle a `textDocument/definition` request.  Returns `None`
/// when the cursor isn't on a known symbol, or when the symbol
/// has no defining sentence anywhere in the workspace.
pub fn handle_goto_definition(
    state:  &GlobalState,
    params: GotoDefinitionParams,
) -> Option<GotoDefinitionResponse> {
    let uri      = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    let docs = state.docs.read().ok()?;
    let doc  = docs.get(&uri)?;
    let offset = position_to_offset(&doc.rope, position);
    let tag    = uri_to_tag(&uri);

    let kb = state.kb.read().ok()?;
    let sym_name             = kb.symbol_at_offset(&tag, offset)?;
    let (_defining_sid, span) = kb.defining_sentence(&sym_name)?;

    // Convert the defining sentence's span into a `Location`.
    // The span carries its own `file` tag; map that back to a
    // URL.  If the file isn't in our per-doc rope table, build a
    // temporary rope from disk (best-effort).  `span_to_range`
    // only needs byte positions and the source text layout.
    let target_uri   = tag_to_uri(&span.file)?;
    let target_range = span_to_range_with_fallback(&docs, &target_uri, &span);

    Some(GotoDefinitionResponse::Scalar(Location {
        uri:   target_uri,
        range: target_range,
    }))
}
