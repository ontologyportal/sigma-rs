// crates/sumo-lsp/src/handlers/format.rs
//
// `textDocument/formatting` and `textDocument/rangeFormatting`
// handlers.  Both delegate to `AstNode::pretty_print(indent)`
// on the retained `ParsedDocument.ast` -- no new formatting
// machinery introduced in the LSP crate.
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

use sumo_kb::AstNode;

use crate::conv::{offset_to_position, position_to_offset};
use crate::state::GlobalState;

// -- Full document -----------------------------------------------------------

pub fn handle_formatting(
    state:  &GlobalState,
    params: DocumentFormattingParams,
) -> Option<Vec<TextEdit>> {
    let uri  = params.text_document.uri;
    let docs = state.docs.read().ok()?;
    let doc  = docs.get(&uri)?;
    let parsed = doc.parsed.as_ref()?;

    if parsed.has_errors() { return Some(Vec::new()); }
    if parsed.ast.is_empty() { return Some(Vec::new()); }

    let formatted = render_forms(&parsed.ast);

    // Replace the entire document -- one TextEdit covering
    // [0, end_of_buffer).  LSP clients accept this shape; they
    // compute the diff client-side to preserve the user's
    // selection.
    let end = rope_end_position(&doc.rope);
    Some(vec![TextEdit {
        range: Range { start: Position { line: 0, character: 0 }, end },
        new_text: formatted,
    }])
}

// -- Range formatting --------------------------------------------------------

pub fn handle_range_formatting(
    state:  &GlobalState,
    params: DocumentRangeFormattingParams,
) -> Option<Vec<TextEdit>> {
    let uri   = params.text_document.uri;
    let range = params.range;
    let docs  = state.docs.read().ok()?;
    let doc   = docs.get(&uri)?;
    let parsed = doc.parsed.as_ref()?;

    if parsed.has_errors() { return Some(Vec::new()); }

    let start_off = position_to_offset(&doc.rope, range.start);
    let end_off   = position_to_offset(&doc.rope, range.end);

    // Pick the root AST nodes whose span intersects the requested
    // range.  A node is "in range" if its span overlaps [start, end)
    // at all -- partial overlap pulls the whole node in so we don't
    // emit mid-sentence edits.
    let nodes: Vec<&AstNode> = parsed.ast.iter()
        .filter(|n| {
            let s = n.span();
            !(s.end_offset <= start_off || s.offset >= end_off)
        })
        .collect();
    if nodes.is_empty() { return Some(Vec::new()); }

    // Edit range = union of selected-node spans, snapped to whole
    // lines at the start (so leading indentation disappears) and
    // through the end of the last selected node.
    let first = nodes.first().expect("non-empty").span();
    let last  = nodes.last() .expect("non-empty").span();
    let union_start = offset_to_position(&doc.rope, first.offset);
    let union_end   = offset_to_position(&doc.rope, last.end_offset);

    let formatted = render_forms(
        &nodes.iter().map(|n| (*n).clone()).collect::<Vec<_>>()
    );

    Some(vec![TextEdit {
        range: Range { start: union_start, end: union_end },
        new_text: formatted,
    }])
}

// -- Shared rendering --------------------------------------------------------

/// Pretty-print every node in `nodes` using the plain-text
/// formatter (no ANSI colour) and join with blank-line separators.
fn render_forms(nodes: &[AstNode]) -> String {
    let mut out = String::new();
    for (i, node) in nodes.iter().enumerate() {
        if i > 0 { out.push_str("\n\n"); }
        out.push_str(&node.format_plain(0));
    }
    out
}

/// End-of-buffer position, used for full-document formatting.
fn rope_end_position(rope: &Rope) -> Position {
    if rope.len_bytes() == 0 {
        return Position { line: 0, character: 0 };
    }
    offset_to_position(rope, rope.len_bytes())
}

