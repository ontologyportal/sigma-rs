// crates/sumo-lsp/src/handlers/references.rs
//
// `textDocument/references` handler.  Resolves the cursor to a
// SymbolId (variables picked up by scope-qualified id, so rename
// of `?X` inside one `(forall (?X) ...)` doesn't touch `?X` in
// another) and returns every non-synthetic occurrence as a
// `Location`.

use lsp_types::{Location, ReferenceParams};
use ropey::Rope;

use crate::conv::{position_to_offset, span_to_range, tag_to_uri, uri_to_tag};
use crate::state::GlobalState;

pub fn handle_references(state: &GlobalState, params: ReferenceParams) -> Option<Vec<Location>> {
    let uri      = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let include_decl = params.context.include_declaration;

    let docs = state.docs.read().ok()?;
    let doc  = docs.get(&uri)?;
    let offset = position_to_offset(&doc.rope, position);
    let tag    = uri_to_tag(&uri);

    let kb = state.kb.read().ok()?;
    let (sym_id, _) = kb.id_at_offset(&tag, offset)?;
    let occurrences = kb.occurrences_of(sym_id);

    // Defining sentence -- used for include_declaration=false
    // filtering.  `occurrences` entries are symbol-reference spans,
    // not sentence spans; a "declaration" for our purposes means
    // the occurrence is at position 1 inside a subclass / instance /
    // subrelation / subAttribute / documentation sentence.  That
    // lets editors that want "refs minus the declaration site"
    // get a clean list.
    let decl_sid = kb.defining_sentence(
        kb.sym_name(sym_id).as_deref().unwrap_or("")
    ).map(|(sid, _)| sid);

    let mut locations: Vec<Location> = Vec::with_capacity(occurrences.len());
    for occ in occurrences {
        if !include_decl && Some(occ.sid) == decl_sid && occ.idx == 1 {
            continue;
        }
        let Some(occ_uri) = tag_to_uri(&occ.span.file) else { continue; };
        let range = if occ_uri == uri {
            span_to_range(&doc.rope, &occ.span)
        } else if let Some(td) = docs.get(&occ_uri) {
            span_to_range(&td.rope, &occ.span)
        } else {
            let text = occ_uri.to_file_path().ok()
                .and_then(|p| std::fs::read_to_string(&p).ok())
                .unwrap_or_default();
            let rope = Rope::from_str(&text);
            span_to_range(&rope, &occ.span)
        };
        locations.push(Location { uri: occ_uri, range });
    }

    Some(locations)
}
