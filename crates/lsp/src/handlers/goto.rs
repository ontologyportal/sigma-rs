//! `textDocument/definition` handler. Resolves the cursor offset to a symbol,
//! asks the KB for the symbol's defining sentence, and returns its `Location`.

use lsp_types::{GotoDefinitionParams, GotoDefinitionResponse, Location};

use crate::conv::{position_to_offset, span_to_range_with_fallback, tag_to_uri, uri_to_tag};
use crate::state::GlobalState;

/// Handle a `textDocument/definition` request. Returns `None` when the cursor
/// isn't on a known symbol, or the symbol has no defining sentence in the
/// workspace.
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

    let session = state.session.read().ok()?;
    let kb = session.kb();
    let sym_name             = kb.symbol_at_offset(&tag, offset)?;
    let (_defining_sid, span) = kb.defining_sentence(&sym_name)?;

    // The span carries its own `file` tag; if that file isn't in the per-doc
    // rope table, `span_to_range_with_fallback` builds a temporary rope from disk.
    let target_uri   = tag_to_uri(&span.file)?;
    let target_range = span_to_range_with_fallback(&docs, &target_uri, &span);

    Some(GotoDefinitionResponse::Scalar(Location {
        uri:   target_uri,
        range: target_range,
    }))
}
