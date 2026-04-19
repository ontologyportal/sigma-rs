// crates/sumo-lsp/src/handlers/diagnostics.rs
//
// Publish `textDocument/publishDiagnostics` notifications.
//
// The server runs two diagnostic passes for each document:
//   1. `ParsedDocument.diagnostics`  -- the parse / tokenize errors
//      surfaced by `parse_document` (pure, per-document).
//   2. `KnowledgeBase::validate_session` -- semantic errors that can
//      only be detected once the sentences are interned (arity,
//      head-not-relation, etc.) keyed on the per-file session that
//      `didOpen` / `didChange` used.
//
// Both are collected into a single `Vec<lsp_types::Diagnostic>` and
// published.  Semantic-error spans fall back to the sentence span
// when the error variant itself doesn't carry one.

use crossbeam_channel::Sender;
use lsp_server::{Message, Notification};
use lsp_types::{PublishDiagnosticsParams, Url};
use ropey::Rope;

use sumo_kb::{KnowledgeBase, ParsedDocument, ToDiagnostic};

use crate::conv::{kb_diagnostic_to_lsp, uri_to_tag};

/// Collect parse + semantic diagnostics for `uri` and emit a
/// `publishDiagnostics` notification on `sender`.
///
/// `rope` is the current text buffer (used for byte-offset -> LSP
/// Position conversion).  `parsed` carries the per-document parse
/// diagnostics; `kb` provides the semantic checker.  `version` is
/// the document version the diagnostics correspond to, echoed back
/// so the client can match against its local state.
pub fn publish_diagnostics(
    sender:  &Sender<Message>,
    uri:     &Url,
    rope:    &Rope,
    parsed:  &ParsedDocument,
    kb:      &KnowledgeBase,
    version: Option<i32>,
) {
    let mut diagnostics = Vec::new();

    // (1) Parse / tokenize diagnostics -- already in sumo-kb's
    // Diagnostic form.  Convert each to LSP directly.
    for d in &parsed.diagnostics {
        diagnostics.push(kb_diagnostic_to_lsp(rope, d));
    }

    // (2) Semantic diagnostics -- generated per-sentence for the
    // file's session, then the span is filled in from the
    // corresponding sentence.  Build a file-tag-keyed lookup first
    // so we only scan the store once.
    let file_tag = uri_to_tag(uri);
    let roots    = kb.file_roots(&file_tag);
    if !roots.is_empty() {
        for &sid in roots {
            if let Err(err) = kb.validate_sentence(sid) {
                let mut d = err.to_diagnostic();
                if let Some(sent) = kb.sentence(sid) {
                    d.range = sent.span.clone();
                }
                diagnostics.push(kb_diagnostic_to_lsp(rope, &d));
            }
        }
    }

    let params = PublishDiagnosticsParams {
        uri:     uri.clone(),
        diagnostics,
        version,
    };
    let not = Notification {
        method: "textDocument/publishDiagnostics".to_string(),
        params: serde_json::to_value(&params).expect("serialisable"),
    };
    if let Err(e) = sender.send(Message::Notification(not)) {
        log::warn!(target: "sumo_lsp", "publishDiagnostics send failed: {}", e);
    }
}
