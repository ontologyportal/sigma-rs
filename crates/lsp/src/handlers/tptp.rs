//! Custom TPTP-preview requests.
//!
//! `sumo/toTptp` renders the whole KB to TPTP and remembers which output line
//! each sentence landed on; `sumo/tptpLineForPosition` maps a document
//! position (the editor cursor) to the line its enclosing sentence rendered
//! on, for scroll-syncing a read-only TPTP preview pane.  Mirrors the
//! `toTptpIndexed` / `tptpLineForPosition` pair on the wasm SDK facade -- the
//! export is a whole-KB re-translation (an occasional explicit refresh, not a
//! per-keystroke call); the line lookup is cheap and fine on every cursor
//! move.

use lsp_types::TextDocumentPositionParams;
use serde::{Deserialize, Serialize};

use sigmakee_rs_sdk::{HasTranslation, TopLayer, TptpLang, TptpOptions};

use crate::conv::{position_to_offset, uri_to_tag};
use crate::state::GlobalState;

/// Method name for the whole-KB TPTP export request.
pub const TPTP_EXPORT_METHOD: &str = "sumo/toTptp";
/// Method name for the cursor-position -> TPTP-line lookup request.
pub const TPTP_LINE_METHOD: &str = "sumo/tptpLineForPosition";

/// `sumo/toTptp` -- render the KB to TPTP and (re)build the line index.
pub enum TptpExportRequest {}

impl lsp_types::request::Request for TptpExportRequest {
    type Params = TptpExportParams;
    type Result = TptpExportResponse;
    const METHOD: &'static str = TPTP_EXPORT_METHOD;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TptpExportParams {
    /// Target dialect: `"tff"`, or anything else (and absent) for FOF.
    pub lang: Option<String>,
    /// Suppress numeric-literal axioms.  Defaults to `true`, matching the
    /// wasm facade's `toTptpIndexed`.
    pub hide_numbers: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TptpExportResponse {
    /// The rendered TPTP text.
    pub tptp: String,
}

/// `sumo/tptpLineForPosition` -- standard `TextDocumentPositionParams` in,
/// 0-based line of the last export out.
pub enum TptpLineRequest {}

impl lsp_types::request::Request for TptpLineRequest {
    type Params = TextDocumentPositionParams;
    type Result = TptpLineResponse;
    const METHOD: &'static str = TPTP_LINE_METHOD;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TptpLineResponse {
    /// 0-based line in the last `sumo/toTptp` output rendering the sentence
    /// that encloses the requested position.  `None` when the document isn't
    /// open, the position falls outside every sentence, or that sentence
    /// wasn't part of the last export (stale index -- the KB changed since;
    /// re-export first).
    pub line: Option<u32>,
}

/// Render the KB to TPTP, replacing the server's sentence -> line index.
pub fn handle_tptp_export<L: TopLayer + HasTranslation>(
    state: &GlobalState<L>,
    params: TptpExportParams,
) -> TptpExportResponse {
    let opts = TptpOptions {
        lang: match params.lang.as_deref() {
            Some("tff") => TptpLang::Tff,
            _ => TptpLang::Fof,
        },
        hide_numbers: params.hide_numbers.unwrap_or(true),
        ..TptpOptions::default()
    };
    // Lock order (session, then tptp_lines) matches `handle_tptp_line`.
    let mut session = state.session.write().expect("kb lock not poisoned");
    let mut lines = state
        .tptp_lines
        .write()
        .expect("tptp_lines lock not poisoned");
    lines.clear();
    let tptp = session
        .kb_mut()
        .to_tptp_indexed(&opts, None, Some(&mut lines));
    TptpExportResponse { tptp }
}

/// Look up the export line for the sentence enclosing `params.position`.
pub fn handle_tptp_line<L: TopLayer>(
    state: &GlobalState<L>,
    params: TextDocumentPositionParams,
) -> TptpLineResponse {
    let uri = params.text_document.uri;
    let tag = uri_to_tag(&uri);
    let docs = state.docs.read().expect("docs lock not poisoned");
    let Some(doc) = docs.get(&uri) else {
        return TptpLineResponse { line: None };
    };
    let offset = position_to_offset(&doc.rope, params.position);
    let session = state.session.read().expect("kb lock not poisoned");
    let lines = state
        .tptp_lines
        .read()
        .expect("tptp_lines lock not poisoned");
    let line = session
        .kb()
        .sentence_at(&tag, offset)
        .and_then(|sid| lines.get(&sid).copied());
    TptpLineResponse { line }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DocState;
    use lsp_types::{Position, TextDocumentIdentifier, Url};
    use sigmakee_rs_sdk::{Parser, SourceFile};

    const KIF: &str = "(subclass Dog Mammal)\n(subclass Cat Mammal)\n";
    const TAG: &str = "/test.kif";

    fn load() -> (GlobalState, Url) {
        let state = GlobalState::new();
        {
            let mut session = state.session.write().expect("kb not poisoned");
            let kb = session.kb_mut();
            let _ = kb.load(
                SourceFile::kif(std::path::PathBuf::from(TAG), KIF.to_string()),
                TAG,
            );
            let _ = kb.make_session_axiomatic(TAG);
        }
        let uri = Url::from_file_path(TAG).expect("file url");
        let parsed =
            sigmakee_rs_sdk::parse_document(TAG.to_string(), KIF, Parser::Kif { options: None });
        let mut ds = DocState::new(KIF, 1);
        ds.parsed = Some(parsed);
        state
            .docs
            .write()
            .expect("docs not poisoned")
            .insert(uri.clone(), ds);
        (state, uri)
    }

    fn line_at(state: &GlobalState, uri: &Url, line: u32, character: u32) -> Option<u32> {
        handle_tptp_line(
            state,
            TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: Position { line, character },
            },
        )
        .line
    }

    #[test]
    fn export_then_cursor_lookup_round_trips() {
        let (state, uri) = load();
        let export = handle_tptp_export(&state, TptpExportParams::default());
        assert!(export.tptp.contains("fof("), "export renders FOF");

        let dog = line_at(&state, &uri, 0, 5).expect("line for the Dog sentence");
        let cat = line_at(&state, &uri, 1, 5).expect("line for the Cat sentence");
        assert_ne!(dog, cat, "distinct sentences map to distinct lines");

        let dog_line = export
            .tptp
            .lines()
            .nth(dog as usize)
            .expect("line in range");
        assert!(
            dog_line.contains("Dog"),
            "cursor in the Dog sentence lands on its axiom: {dog_line}"
        );
    }

    #[test]
    fn lookup_without_export_or_document_is_none() {
        let (state, uri) = load();
        // No export yet: the index is empty.
        assert_eq!(line_at(&state, &uri, 0, 5), None);

        handle_tptp_export(&state, TptpExportParams::default());
        // A document that was never opened has no rope to resolve against.
        let other = Url::from_file_path("/absent.kif").expect("file url");
        assert_eq!(line_at(&state, &other, 0, 0), None);
    }
}
