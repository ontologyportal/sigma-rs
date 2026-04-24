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

use std::collections::HashSet;

use crossbeam_channel::Sender;
use lsp_server::{Message, Notification};
use lsp_types::{PublishDiagnosticsParams, Url};
use ropey::Rope;

use sumo_kb::{KnowledgeBase, ParsedDocument, Severity, ToDiagnostic};

use crate::conv::{kb_diagnostic_to_lsp, uri_to_tag};
use crate::state::GlobalState;

/// Collect parse + semantic diagnostics for `uri` and emit a
/// `publishDiagnostics` notification on `sender`.
///
/// `rope` is the current text buffer (used for byte-offset -> LSP
/// Position conversion).  `parsed` carries the per-document parse
/// diagnostics; `kb` provides the semantic checker.  `version` is
/// the document version the diagnostics correspond to, echoed back
/// so the client can match against its local state.
///
/// Semantic-diagnostic severity: the sumo-kb validator's default
/// severity for every check is `Warning` (promoted to `Error` only
/// via the CLI's `-Wall` / `--warning=<code>` flags, which the LSP
/// does not set).  We therefore force every semantic diagnostic
/// to `Warning` here -- matches the user's expectation that in a
/// KB that's mid-edit, these are advisories, not compile errors,
/// and keeps the Problems panel dominated by the yellow-triangle
/// icon.  Hard parse errors remain `Error`.
pub fn publish_diagnostics(
    sender:  &Sender<Message>,
    uri:     &Url,
    rope:    &Rope,
    parsed:  &ParsedDocument,
    state:   &GlobalState,
    kb:      &KnowledgeBase,
    version: Option<i32>,
) {
    let ignored = state.ignored_diagnostic_codes.read().ok()
        .map(|g| g.clone())
        .unwrap_or_default();
    publish_diagnostics_filtered(sender, uri, rope, parsed, kb, version, &ignored)
}

fn publish_diagnostics_filtered(
    sender:  &Sender<Message>,
    uri:     &Url,
    rope:    &Rope,
    parsed:  &ParsedDocument,
    kb:      &KnowledgeBase,
    version: Option<i32>,
    ignored: &HashSet<String>,
) {
    let mut diagnostics = Vec::new();

    // (1) Parse / tokenize diagnostics -- already in sumo-kb's
    // Diagnostic form.  Convert each to LSP directly.  These are
    // always hard errors; the ignore-list doesn't apply (parse
    // errors are never "semantic suggestions" the user can mute).
    for d in &parsed.diagnostics {
        diagnostics.push(kb_diagnostic_to_lsp(rope, d));
    }

    // (2) Semantic diagnostics -- every check the validator
    // raises on each root sentence, including the warning-level
    // ones the severity-aware `validate_sentence` would have
    // swallowed.  Severity is forced to Warning (see module
    // doc).  Codes / names in the ignore-list are dropped here
    // so the client sees an immediate change the next time
    // diagnostics are published.
    let file_tag = uri_to_tag(uri);
    let roots    = kb.file_roots(&file_tag);
    for &sid in roots {
        let sent_span = kb.sentence(sid).map(|s| s.span.clone());
        for err in kb.validate_sentence_all(sid) {
            if ignored.contains(err.code()) || ignored.contains(err.name()) {
                continue;
            }
            let mut d = err.to_diagnostic();
            d.severity = Severity::Warning;
            if let Some(ref sp) = sent_span {
                d.range = sp.clone();
            }
            diagnostics.push(kb_diagnostic_to_lsp(rope, &d));
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
