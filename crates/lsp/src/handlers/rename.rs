// crates/sumo-lsp/src/handlers/rename.rs
//
// `textDocument/rename` handler.
//
// Two modes, chosen by the kind of element under the cursor:
//
//   * Symbol rename -- every `Element::Symbol { id }` occurrence
//     across every file gets a text replacement.
//
//   * Variable rename -- resolved from the DOCUMENT, not the KB: the
//     content-addressed sentence store no longer carries scope-
//     qualified variable ids (`id_at_offset` refuses variables), so
//     the handler re-tokenizes the live buffer and renames every
//     same-named variable token inside the top-level form under the
//     cursor.  Variables never leak across root sentences in KIF, so
//     the containing form is the correct scope boundary; intra-form
//     quantifier shadowing is not distinguished.  The replacement
//     preserves the leading sigil (`?` / `@`) so the renamed form
//     stays syntactically valid.
//
// The returned `WorkspaceEdit` uses the `changes` map (simplest
// shape most clients accept); no document-version constraints are
// emitted, so clients that applied edits in order during the
// refactor are free to accept or reject.

use std::collections::HashMap;

use lsp_types::{RenameParams, TextEdit, Url, WorkspaceEdit};
// `Url` is used as the key type in `HashMap<Url, Vec<TextEdit>>`.

use crate::conv::{offset_to_position, position_to_offset, span_to_range_with_fallback, tag_to_uri, uri_to_tag};
use crate::state::GlobalState;

pub fn handle_rename(state: &GlobalState, params: RenameParams) -> Option<WorkspaceEdit> {
    let uri       = params.text_document_position.text_document.uri;
    let position  = params.text_document_position.position;
    let new_name  = params.new_name;

    let docs = state.docs.read().ok()?;
    let doc  = docs.get(&uri)?;
    let offset = position_to_offset(&doc.rope, position);
    let tag    = uri_to_tag(&uri);

    let session = state.session.read().ok()?;
    let kb = session.kb();

    // Variables are renamed from the live document (see module doc); ground
    // symbols go through the KB's occurrence index below.
    let hit = kb.element_at_offset(&tag, offset)?;
    if hit.is_variable {
        let name = hit.name?;
        return rename_variable_in_document(&doc.rope, &tag, &uri, offset, &name, hit.is_row, &new_name);
    }

    let (sym_id, _old_display) = kb.id_at_offset(&tag, offset)?;
    let replacement = new_name.clone();

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

/// Rename every occurrence of the variable `name` (sigil-less, as reported by
/// `element_at_offset`) inside the top-level form containing `cursor_offset`.
///
/// Re-tokenizes the buffer, walks paren depth to find the boundaries of the
/// root form under the cursor, and emits one `TextEdit` per matching
/// `Variable` / `RowVariable` token in that range.  Row and plain variables
/// are distinct namespaces (`@X` vs `?X`), so only the cursor's own kind is
/// touched.
fn rename_variable_in_document(
    rope:          &ropey::Rope,
    tag:           &str,
    uri:           &Url,
    cursor_offset: usize,
    name:          &str,
    is_row:        bool,
    new_name:      &str,
) -> Option<WorkspaceEdit> {
    let stripped    = new_name.trim_start_matches('?').trim_start_matches('@');
    let replacement = if is_row { format!("@{}", stripped) } else { format!("?{}", stripped) };
    let old_token   = if is_row { format!("@{}", name) }     else { format!("?{}", name) };

    let text = String::from(rope);
    let (tokens, _errs) = sigmakee_rs_sdk::tokenize_kif(&text, tag);

    // Locate the top-level form [start, end] whose span covers the cursor.
    let mut depth = 0usize;
    let mut form_start = 0usize;
    let mut form: Option<(usize, usize)> = None;
    for tok in &tokens {
        match tok.kind {
            sigmakee_rs_sdk::TokenKind::LParen => {
                if depth == 0 { form_start = tok.span.offset; }
                depth += 1;
            }
            sigmakee_rs_sdk::TokenKind::RParen => {
                depth = depth.saturating_sub(1);
                if depth == 0 && form_start <= cursor_offset && cursor_offset < tok.span.end_offset {
                    form = Some((form_start, tok.span.end_offset));
                    break;
                }
            }
            _ => {}
        }
    }
    let (start, end) = form?;

    let mut edits: Vec<TextEdit> = Vec::new();
    for tok in &tokens {
        if tok.span.offset < start || tok.span.end_offset > end { continue; }
        let matches = match &tok.kind {
            sigmakee_rs_sdk::TokenKind::Variable(v)    if !is_row => v == &old_token,
            sigmakee_rs_sdk::TokenKind::RowVariable(v) if  is_row => v == &old_token,
            _ => false,
        };
        if !matches { continue; }
        edits.push(TextEdit {
            range: lsp_types::Range {
                start: offset_to_position(rope, tok.span.offset),
                end:   offset_to_position(rope, tok.span.end_offset),
            },
            new_text: replacement.clone(),
        });
    }

    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    changes.insert(uri.clone(), edits);
    Some(WorkspaceEdit {
        changes:           Some(changes),
        document_changes:  None,
        change_annotations: None,
    })
}
