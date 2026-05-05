// crates/sumo-lsp/src/conv.rs
//
// Conversion layer between LSP protocol types (`Url`, `Position`,
// `Range`, `Diagnostic`) and the KB's native types (`String` file
// tag, byte offset, `Span`, `Diagnostic`).  Everything LSP-specific
// lives here; every other module in the crate speaks sigmakee-rs-core types.
//
// LSP positions are UTF-16 code units by default.  Most editors
// (VSCode, Neovim, Helix) use UTF-16 for historical reasons.  Some
// newer clients negotiate UTF-8 via `PositionEncodingKind::UTF8`;
// the server advertises support but defaults to UTF-16 for the
// widest compatibility.  `ropey::Rope` gives us cheap UTF-16 ↔ byte
// conversion on large files.

use std::collections::HashMap;

use lsp_types::{Diagnostic as LspDiagnostic, DiagnosticSeverity, Position, Range, Url};
use ropey::Rope;

use sigmakee_rs_core::{Diagnostic as KbDiagnostic, Severity, Span};

use crate::state::DocState;

// -- URI ↔ file tag -----------------------------------------------------------

/// Canonical file-tag form for a URL.
///
/// The tag string is what sigmakee-rs-core stores as `Sentence.file` and what
/// every mutation path keys on.  A single canonical mapping here
/// prevents drift between `didOpen` and `didChange` (especially on
/// Windows, where clients may send paths with differing drive-letter
/// casing).
///
/// Non-file URIs and URIs whose path can't be parsed fall back to
/// the raw URL string -- good enough for logging and for ad-hoc
/// single-file opens, which aren't expected to cross-reference.
pub fn uri_to_tag(uri: &Url) -> String {
    match uri.to_file_path() {
        Ok(path) => {
            let s = path.to_string_lossy().to_string();
            // On Windows, lower-case the drive letter so `C:\foo` and
            // `c:\foo` map to the same KB tag.
            if cfg!(windows) && s.len() >= 2 && s.as_bytes()[1] == b':' {
                let mut chars: Vec<char> = s.chars().collect();
                chars[0] = chars[0].to_ascii_lowercase();
                chars.into_iter().collect()
            } else {
                s
            }
        }
        Err(_) => uri.to_string(),
    }
}

/// Inverse of [`uri_to_tag`].  Returns `None` when the tag isn't
/// a parseable file path (e.g. fallback ad-hoc strings).
pub fn tag_to_uri(tag: &str) -> Option<Url> {
    Url::from_file_path(tag).ok()
}

/// Convert a `Span` to an LSP `Range` against whichever buffer
/// can be located for `uri`:
///
/// 1. The open-document table (`docs`) if the URI is currently
///    open in the client.
/// 2. An on-demand disk read as a fallback for cross-file
///    references into files not yet opened.
/// 3. An empty rope as a last resort (malformed URI, unreadable
///    file, permissions error).  A `debug`-level log line is
///    emitted so misconfigurations surface without cluttering
///    normal operation.
///
/// Centralising this logic avoids drift across handlers that all
/// need the same "give me a range for this span, wherever the
/// source text lives" primitive.
pub fn span_to_range_with_fallback(
    docs: &HashMap<Url, DocState>,
    uri:  &Url,
    span: &Span,
) -> Range {
    if let Some(doc) = docs.get(uri) {
        return span_to_range(&doc.rope, span);
    }
    let rope = match read_rope_from_disk(uri) {
        Some(r) => r,
        None    => Rope::new(),
    };
    span_to_range(&rope, span)
}

/// Best-effort disk read of a `file://` URL into a `Rope`.  Emits
/// a debug log when the read fails -- silent would let bad paths
/// silently misreport ranges -- then lets the caller decide
/// whether to use an empty rope or bail.
fn read_rope_from_disk(uri: &Url) -> Option<Rope> {
    let path = match uri.to_file_path() {
        Ok(p)  => p,
        Err(_) => {
            log::debug!(target: "sumo_lsp::conv",
                "non-file URI '{}' in cross-file range lookup; using empty rope", uri);
            return None;
        }
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(Rope::from_str(&text)),
        Err(e)   => {
            log::debug!(target: "sumo_lsp::conv",
                "cross-file read of '{}' failed ({}); using empty rope",
                path.display(), e);
            None
        }
    }
}

// -- Position ↔ byte offset ---------------------------------------------------

/// Convert a byte offset into an LSP `Position` (line + UTF-16 col).
///
/// LSP positions are 0-based.  We use the Rope to find the line, then
/// compute the UTF-16 column by counting the UTF-16 units from the
/// line start up to the byte offset.  Both directions are O(log N)
/// given ropey's cached line index.
pub fn offset_to_position(rope: &Rope, byte_offset: usize) -> Position {
    // Clamp to buffer -- defensive against stale spans after an edit.
    let byte_offset = byte_offset.min(rope.len_bytes());
    let line_idx    = rope.byte_to_line(byte_offset);
    let line_start  = rope.line_to_byte(line_idx);
    let line_prefix = rope.byte_slice(line_start..byte_offset);
    let utf16_col   = line_prefix.chars().map(|c| c.len_utf16() as u32).sum();
    Position {
        line:      line_idx as u32,
        character: utf16_col,
    }
}

/// Convert an LSP `Position` into a byte offset.
///
/// Inverse of [`offset_to_position`].  Returns a clamped offset when
/// the position is past the end of its line; returns `rope.len_bytes()`
/// when the line is out of range.  This resilience matches what
/// editors expect -- stale positions should saturate rather than
/// error.
pub fn position_to_offset(rope: &Rope, pos: Position) -> usize {
    let line = pos.line as usize;
    if line >= rope.len_lines() { return rope.len_bytes(); }
    let line_start = rope.line_to_byte(line);
    let line_len   = rope.line(line).len_bytes();
    let line_end   = line_start + line_len;

    // Walk the line char-by-char until we've consumed `pos.character`
    // UTF-16 units.  Most lines are short; no optimisation needed.
    let line_slice = rope.byte_slice(line_start..line_end);
    let mut remaining = pos.character as usize;
    let mut byte_off  = line_start;
    for c in line_slice.chars() {
        if remaining == 0 { break; }
        let u16_len = c.len_utf16();
        if remaining < u16_len {
            // Split inside a surrogate pair -- round up to the
            // character boundary.
            break;
        }
        remaining -= u16_len;
        byte_off  += c.len_utf8();
    }
    byte_off
}

/// Convert a sigmakee-rs-core [`Span`] into an LSP [`Range`] relative to `rope`.
pub fn span_to_range(rope: &Rope, span: &Span) -> Range {
    Range {
        start: offset_to_position(rope, span.offset),
        end:   offset_to_position(rope, span.end_offset),
    }
}

// -- Diagnostic conversion ----------------------------------------------------

pub fn kb_diagnostic_to_lsp(rope: &Rope, d: &KbDiagnostic) -> LspDiagnostic {
    LspDiagnostic {
        range:    span_to_range(rope, &d.range),
        severity: Some(severity_to_lsp(d.severity)),
        code:     Some(lsp_types::NumberOrString::String(d.code.to_string())),
        code_description: None,
        source:   Some("sumo-lsp".to_string()),
        message:  d.message.clone(),
        related_information: if d.related.is_empty() {
            None
        } else {
            Some(d.related.iter().map(|r| lsp_types::DiagnosticRelatedInformation {
                location: lsp_types::Location {
                    uri:   tag_to_uri(&r.range.file).unwrap_or_else(placeholder_url),
                    range: span_to_range(rope, &r.range),
                },
                message: r.message.clone(),
            }).collect())
        },
        tags: None,
        data: None,
    }
}

fn severity_to_lsp(s: Severity) -> DiagnosticSeverity {
    match s {
        Severity::Error   => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Info    => DiagnosticSeverity::INFORMATION,
        Severity::Hint    => DiagnosticSeverity::HINT,
    }
}

/// Placeholder URL used only when a related-info span has no valid
/// file tag.  Should never be observed in practice -- sigmakee-rs-core produces
/// `related_info` spans for in-store sentences, all of which have
/// real file tags.
fn placeholder_url() -> Url {
    // `parse` on a hardcoded valid URL is infallible in practice.
    Url::parse("file:///unknown").expect("hardcoded URL")
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_tag_round_trip_file() {
        // A file:// URL should round-trip.  We don't compare to the
        // original string (platform-dependent) but the round-trip
        // must be stable.
        let url = Url::from_file_path("/tmp/foo.kif")
            .expect("constructible file url");
        let tag = uri_to_tag(&url);
        let back = tag_to_uri(&tag).expect("tag parseable");
        assert_eq!(back, url);
    }

    #[test]
    fn offset_to_position_ascii() {
        let rope = Rope::from_str("abc\ndef\n");
        assert_eq!(offset_to_position(&rope, 0), Position { line: 0, character: 0 });
        assert_eq!(offset_to_position(&rope, 3), Position { line: 0, character: 3 });
        assert_eq!(offset_to_position(&rope, 4), Position { line: 1, character: 0 });
        assert_eq!(offset_to_position(&rope, 7), Position { line: 1, character: 3 });
    }

    #[test]
    fn offset_to_position_utf16_counts_surrogate_pairs() {
        // 😀 is U+1F600, represented as a surrogate pair in UTF-16
        // (2 code units).  Byte length is 4 in UTF-8.
        let rope = Rope::from_str("a😀b");
        // Byte positions: a=0, 😀=1..5, b=5
        assert_eq!(offset_to_position(&rope, 0), Position { line: 0, character: 0 });
        assert_eq!(offset_to_position(&rope, 1), Position { line: 0, character: 1 });
        assert_eq!(offset_to_position(&rope, 5), Position { line: 0, character: 3 });
        assert_eq!(offset_to_position(&rope, 6), Position { line: 0, character: 4 });
    }

    #[test]
    fn position_to_offset_inverse_ascii() {
        let rope = Rope::from_str("abc\ndef\n");
        let p    = Position { line: 1, character: 2 };
        assert_eq!(position_to_offset(&rope, p), 6);
    }

    #[test]
    fn position_to_offset_beyond_end_clamps() {
        let rope = Rope::from_str("abc");
        let p    = Position { line: 9, character: 9 };
        assert_eq!(position_to_offset(&rope, p), rope.len_bytes());
    }

    #[test]
    fn span_to_range_covers_full_span() {
        let rope = Rope::from_str("(subclass Human Animal)");
        let span = Span {
            file: "t".into(),
            line: 1, col: 1, offset: 0,
            end_line: 1, end_col: 24, end_offset: 23,
        };
        let r = span_to_range(&rope, &span);
        assert_eq!(r.start, Position { line: 0, character: 0 });
        assert_eq!(r.end,   Position { line: 0, character: 23 });
    }
}
