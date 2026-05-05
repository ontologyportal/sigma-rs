// crates/sumo-lsp/src/handlers/rename.rs
//
// `textDocument/rename` handler.
//
// Two modes, chosen by the kind of element under the cursor:
//
//   * Symbol rename -- every `Element::Symbol { id }` occurrence
//     across every file gets a text replacement.
//
//   * Variable rename -- same, but the SymbolId is already
//     scope-qualified (`X__42` in the intern table), so occurrences
//     naturally restrict to the same `(forall/exists (?X) ...)`
//     body.  The surface text replacement uses the leading sigil
//     (`?` / `@`) the user had under the cursor so the renamed form
//     stays syntactically valid.
//
// The returned `WorkspaceEdit` uses the `changes` map (simplest
// shape most clients accept); no document-version constraints are
// emitted, so clients that applied edits in order during the
// refactor are free to accept or reject.

use std::collections::HashMap;

use lsp_types::{RenameParams, TextEdit, Url, WorkspaceEdit};
// `Url` is used as the key type in `HashMap<Url, Vec<TextEdit>>`.

use sigmakee_rs_core::types::Element;

use crate::conv::{position_to_offset, span_to_range_with_fallback, tag_to_uri, uri_to_tag};
use crate::state::GlobalState;

pub fn handle_rename(state: &GlobalState, params: RenameParams) -> Option<WorkspaceEdit> {
    let uri       = params.text_document_position.text_document.uri;
    let position  = params.text_document_position.position;
    let new_name  = params.new_name;

    let docs = state.docs.read().ok()?;
    let doc  = docs.get(&uri)?;
    let offset = position_to_offset(&doc.rope, position);
    let tag    = uri_to_tag(&uri);

    let kb = state.kb.read().ok()?;
    let (sym_id, _old_display) = kb.id_at_offset(&tag, offset)?;

    // Determine the replacement-text shape based on the kind of the
    // element under the cursor.  Variables need their sigil
    // preserved (or supplied by the user if `new_name` already
    // carries one); plain symbols go in verbatim.
    let hit = kb.element_at_offset(&tag, offset)?;
    let sent = kb.sentence(hit.sid)?;
    let replacement = match sent.elements.get(hit.idx)? {
        Element::Symbol { .. } => new_name.clone(),
        Element::Variable { is_row, .. } => {
            let stripped = new_name.trim_start_matches('?').trim_start_matches('@');
            if *is_row { format!("@{}", stripped) } else { format!("?{}", stripped) }
        }
        _ => return None,
    };

    // Build `changes: HashMap<Url, Vec<TextEdit>>` from the
    // occurrence index.  Each edit replaces the stored span with
    // the new text; no diff logic needed because every occurrence
    // carries a precise range.
    let occurrences = kb.occurrences_of(sym_id);
    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();

    for occ in occurrences {
        let Some(occ_uri) = tag_to_uri(&occ.span.file) else { continue; };
        let range = span_to_range_with_fallback(&docs, &occ_uri, &occ.span);
        changes.entry(occ_uri).or_default().push(TextEdit {
            range,
            new_text: replacement.clone(),
        });
    }

    // LSP convention: WorkspaceEdit with an empty `changes` map
    // still counts as a successful rename of a symbol that happens
    // to have zero references.  Return Some(empty) rather than None
    // so the client doesn't report "cannot rename".
    Some(WorkspaceEdit {
        changes:           Some(changes),
        document_changes:  None,
        change_annotations: None,
    })
}
