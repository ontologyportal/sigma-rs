// crates/sumo-lsp/src/handlers/format.rs
//
// `textDocument/formatting` and `textDocument/rangeFormatting`
// handlers.  Both delegate to the core's comment-preserving
// document formatter (`format_document` / `format_forms`) over
// the retained `ParsedDocument` -- a full round trip through the
// parsed representation: the AST is re-emitted in canonical
// layout and the document's comment blocks are re-interleaved by
// source position, so formatting never deletes a comment.
//
// Safety rail: if the document has any error-severity
// diagnostics we decline to format.  Pretty-printing a
// parse-error-riddled document would replace broken input with a
// partial re-emission and quietly drop the user's malformed
// fragments.  Clients show the "formatting failed" message,
// fix the syntax, and retry.

use lsp_types::{
    DocumentFormattingParams, DocumentRangeFormattingParams, Position, Range, TextEdit,
};
use ropey::Rope;

use sigmakee_rs_sdk::DocItem;
use sigmakee_rs_sdk::TopLayer;
use sigmakee_rs_sdk::{format_document, format_forms};

use crate::conv::{offset_to_position, position_to_offset};
use crate::state::GlobalState;

// -- Full document -----------------------------------------------------------

pub fn handle_formatting<L: TopLayer>(
    state: &GlobalState<L>,
    params: DocumentFormattingParams,
) -> Option<Vec<TextEdit>> {
    let uri = params.text_document.uri;
    let docs = state.docs.read().ok()?;
    let doc = docs.get(&uri)?;
    let parsed = doc.parsed.as_ref()?;

    if parsed.has_errors() {
        return Some(Vec::new());
    }
    if parsed.ast.is_empty() {
        return Some(Vec::new());
    }

    let formatted = format_document(parsed)?;

    // Replace the entire document -- one TextEdit covering
    // [0, end_of_buffer).  LSP clients accept this shape; they
    // compute the diff client-side to preserve the user's
    // selection.
    let end = rope_end_position(&doc.rope);
    Some(vec![TextEdit {
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end,
        },
        new_text: formatted,
    }])
}

// -- Range formatting --------------------------------------------------------

pub fn handle_range_formatting<L: TopLayer>(
    state: &GlobalState<L>,
    params: DocumentRangeFormattingParams,
) -> Option<Vec<TextEdit>> {
    let uri = params.text_document.uri;
    let range = params.range;
    let docs = state.docs.read().ok()?;
    let doc = docs.get(&uri)?;
    let parsed = doc.parsed.as_ref()?;

    if parsed.has_errors() {
        return Some(Vec::new());
    }

    let start_off = position_to_offset(&doc.rope, range.start);
    let end_off = position_to_offset(&doc.rope, range.end);

    // Pick the top-level items (statements AND directives) whose span
    // intersects the requested range.  An item is "in range" if its span
    // overlaps [start, end) at all -- partial overlap pulls the whole item
    // in so we don't emit mid-sentence edits.
    let items: Vec<&DocItem> = parsed
        .ast
        .iter()
        .filter(|i| {
            let s = i.span();
            !(s.end_offset <= start_off || s.offset >= end_off)
        })
        .collect();
    if items.is_empty() {
        return Some(Vec::new());
    }

    // Edit range = union of selected-item spans, snapped to whole
    // lines at the start (so leading indentation disappears) and
    // through the end of the last selected item.
    let first = items.first().expect("non-empty").span();
    let last = items.last().expect("non-empty").span();
    let union_start = offset_to_position(&doc.rope, first.offset);
    let union_end = offset_to_position(&doc.rope, last.end_offset);

    // Comment blocks inside the edited window ride along; anything outside
    // it (including a trailing comment after the last item's `)`) is not
    // touched by the edit and needs no re-emission.
    let comments: Vec<sigmakee_rs_sdk::CommentBlock> = parsed
        .comments
        .iter()
        .filter(|c| c.span.offset >= first.offset && c.span.offset < last.end_offset)
        .cloned()
        .collect();
    let formatted = format_forms(&parsed.text, &items, &comments);

    Some(vec![TextEdit {
        range: Range {
            start: union_start,
            end: union_end,
        },
        new_text: formatted,
    }])
}

// -- Shared rendering --------------------------------------------------------

/// End-of-buffer position, used for full-document formatting.
fn rope_end_position(rope: &Rope) -> Position {
    if rope.len_bytes() == 0 {
        return Position {
            line: 0,
            character: 0,
        };
    }
    offset_to_position(rope, rope.len_bytes())
}
