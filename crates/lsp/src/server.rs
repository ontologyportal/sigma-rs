//! LSP message loop, split into a transport-free core and a stdio shell.
//!
//! [`dispatch_message`] is the core: feed it one client->server `Message`, get
//! back every server->client message it produces (the response, plus any
//! `publishDiagnostics` notifications).  It has no I/O and no threads, so any
//! transport can drive it -- the wasm build feeds it JSON strings from
//! `postMessage`.
//!
//! [`run`] is the native shell: the lsp-server `Connection` loop (stdio or
//! in-memory), the `initialize` handshake, and the initial workspace sweep.
//! Handlers run inline on the event-loop thread.

use anyhow::Result;
use lsp_server::{Connection, ExtractError, Message, Notification, Request, Response};
use lsp_types::{
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    },
    request::{
        Completion, DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest,
        RangeFormatting, References, Rename, Request as _, SemanticTokensFullRequest,
        WorkspaceSymbolRequest,
    },
    CompletionOptions, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, InitializeParams, InitializeResult, OneOf, PositionEncodingKind,
    RenameOptions, SemanticTokensFullOptions, SemanticTokensOptions,
    SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions, Url, WorkDoneProgressOptions, WorkspaceFolder,
};
use ropey::Rope;
use serde::de::DeserializeOwned;

use sigmakee_rs_sdk::{parse_document, HasTranslation, Parser, SourceFile, TopLayer};

use crate::conv::uri_to_tag;
use crate::handlers::{
    handle_completion, handle_document_symbol, handle_formatting, handle_goto_definition,
    handle_hover, handle_range_formatting, handle_references, handle_rename,
    handle_semantic_tokens_full, handle_set_active_files, handle_set_ignored_diagnostics,
    handle_taxonomy, handle_tptp_export, handle_tptp_line, handle_workspace_symbols,
    publish_diagnostics, semantic_tokens_legend, SetActiveFilesParams, SetIgnoredDiagnosticsParams,
    TaxonomyRequest, TptpExportRequest, TptpLineRequest, SET_ACTIVE_FILES_METHOD,
    SET_IGNORED_DIAGNOSTICS_METHOD, TPTP_EXPORT_METHOD, TPTP_LINE_METHOD,
};
use crate::state::{DocState, GlobalState};

/// Run the server against a `Connection`.  Returns on clean shutdown, or
/// propagates a transport error.
pub fn run(connection: Connection) -> Result<()> {
    let (id, params) = connection.initialize_start()?;
    let init_params: InitializeParams = serde_json::from_value(params)?;
    connection.initialize_finish(id, serde_json::to_value(initialize_result())?)?;

    log::info!(target: "sumo_lsp", "initialised");

    let state = GlobalState::new();

    // Clients that own KB membership via `sumo/setActiveFiles` can advertise
    // `initializationOptions: { "clientManagesFiles": true }` to suppress the
    // initial workspace sweep, whose un-loading is quadratic on large
    // workspaces.  Headless clients still get the sweep.
    let client_manages_files = init_params
        .initialization_options
        .as_ref()
        .and_then(|v| v.get("clientManagesFiles"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if client_manages_files {
        use std::sync::atomic::Ordering;
        state.client_manages_files.store(true, Ordering::SeqCst);
        log::info!(target: "sumo_lsp",
            "clientManagesFiles=true in init options; skipping workspace sweep");
    } else {
        for out in initial_workspace_sweep(&state, &init_params) {
            let _ = connection.sender.send(out);
        }
    }

    for msg in &connection.receiver {
        if let Message::Request(req) = &msg {
            if connection.handle_shutdown(req)? {
                log::info!(target: "sumo_lsp", "shutdown requested");
                return Ok(());
            }
        }
        for out in dispatch_message(&state, msg) {
            let _ = connection.sender.send(out);
        }
    }

    Ok(())
}

// -- Transport-free dispatch --------------------------------------------------

/// Dispatch one client->server message and return every server->client message
/// it produces -- the response (for requests) plus any notifications
/// (`publishDiagnostics`) the handling emitted.
///
/// This is the whole server minus the transport: no I/O, no threads, no
/// blocking.  Lifecycle messages are the shell's job and are NOT handled
/// here -- the native [`run`] loop answers `initialize`/`shutdown`/`exit` via
/// lsp-server's `Connection`, and an embedding transport (e.g. the wasm
/// bridge) must answer them itself, using [`initialize_result`] for the
/// handshake.
///
/// The `HasTranslation` bound comes from the `sumo/toTptp` preview export;
/// both real layer stacks (the standalone binary's `TranslationLayer`, the
/// wasm build's `ProverLayer<TranslationLayer>`) satisfy it.
pub fn dispatch_message<L: TopLayer + HasTranslation>(
    state: &GlobalState<L>,
    msg: Message,
) -> Vec<Message> {
    let mut out = Vec::new();
    match msg {
        Message::Request(req) => handle_request(state, req, &mut out),
        Message::Notification(not) => {
            if let Err(e) = handle_notification(state, not, &mut out) {
                log::warn!(target: "sumo_lsp", "notification handler error: {:?}", e);
            }
        }
        Message::Response(_) => {}
    }
    out
}

/// The `InitializeResult` the server answers the `initialize` handshake with.
/// Public so an embedding transport can perform the same handshake [`run`]
/// does.
pub fn initialize_result() -> InitializeResult {
    InitializeResult {
        capabilities: server_capabilities(),
        server_info: Some(ServerInfo {
            name: "sumo-lsp".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    }
}

// -- Capabilities -------------------------------------------------------------

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                save: None,
                will_save: None,
                will_save_wait_until: None,
            },
        )),
        definition_provider: Some(OneOf::Left(true)),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(false),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                work_done_progress_options: WorkDoneProgressOptions::default(),
                legend: semantic_tokens_legend(),
                range: Some(false),
                full: Some(SemanticTokensFullOptions::Bool(true)),
            },
        )),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            // Firing on `(` gives sentence-head completion; space advances to
            // arg-position completion.
            trigger_characters: Some(vec![
                "(".to_string(),
                " ".to_string(),
                "?".to_string(),
                "@".to_string(),
            ]),
            resolve_provider: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    }
}

// -- Workspace sweep ----------------------------------------------------------

fn initial_workspace_sweep<L: TopLayer>(
    state: &GlobalState<L>,
    init: &InitializeParams,
) -> Vec<Message> {
    let mut out = Vec::new();
    // Prefer `workspace_folders`; fall back to the legacy `root_uri`.
    let folders: Vec<WorkspaceFolder> = match &init.workspace_folders {
        Some(fs) if !fs.is_empty() => fs.clone(),
        _ =>
        {
            #[allow(deprecated)]
            if let Some(root) = init.root_uri.clone() {
                vec![WorkspaceFolder {
                    uri: root,
                    name: "root".to_string(),
                }]
            } else {
                return out;
            }
        }
    };

    for folder in &folders {
        let Some(dir) = crate::conv::url_to_file_path(&folder.uri) else {
            continue;
        };
        let kif_files = collect_kif_files(&dir);
        log::info!(target: "sumo_lsp",
            "workspace sweep: {} KIF files in '{}'", kif_files.len(), dir.display());
        for path in kif_files {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let Some(uri) = crate::conv::url_from_file_path(&path) else {
                    continue;
                };
                let tag = uri_to_tag(&uri);
                // Parse errors reject the file entirely so the rest of the
                // workspace stays healthy; the bad file still publishes
                // diagnostics below via `parse_document`.
                let load_report = {
                    let mut session = state.session.write().expect("kb not poisoned");
                    let kb = session.kb_mut();
                    let report = kb.load(
                        SourceFile::kif(std::path::PathBuf::from(&tag), text.to_string()),
                        &tag,
                    );
                    // Man-page / documentation introspection reads the Base
                    // scope; a loaded file sits in its own session until
                    // promoted.
                    if report.ok {
                        let _ = kb.make_session_axiomatic(&tag);
                    }
                    report
                };
                if !load_report.ok {
                    log::warn!(target: "sumo_lsp",
                        "workspace sweep: skipped '{}' ({} parse error(s)); \
                         LSP features on this file will be unavailable until it parses cleanly",
                        tag, load_report.errors().count());
                }
                let parsed = parse_document(tag.clone(), text.as_str(), Parser::Kif);
                let rope = Rope::from_str(&text);
                // Publish diagnostics before moving `parsed` into the doc state
                // (ParsedDocument is not Clone).
                {
                    let session = state.session.read().expect("kb not poisoned");
                    publish_diagnostics(&mut out, &uri, &rope, &parsed, state, session.kb(), None);
                }
                {
                    let mut docs = state.docs.write().expect("docs not poisoned");
                    let mut ds = DocState::new(&text, 0);
                    ds.parsed = Some(parsed);
                    docs.insert(uri.clone(), ds);
                }
            }
        }
    }
    out
}

fn collect_kif_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(collect_kif_files(&path));
        } else if is_kif_file(&path) {
            out.push(path);
        }
    }
    out.sort();
    out
}

fn is_kif_file(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy();
    s.ends_with(".kif") || s.ends_with(".kif.tq")
}

// -- Request dispatch ---------------------------------------------------------

fn handle_request<L: TopLayer + HasTranslation>(
    state: &GlobalState<L>,
    req: Request,
    out: &mut Vec<Message>,
) {
    let resp = match req.method.as_str() {
        HoverRequest::METHOD => dispatch::<HoverRequest, _>(req, |p| Some(handle_hover(state, p))),
        GotoDefinition::METHOD => {
            dispatch::<GotoDefinition, _>(req, |p| Some(handle_goto_definition(state, p)))
        }
        DocumentSymbolRequest::METHOD => {
            dispatch::<DocumentSymbolRequest, _>(req, |p| Some(handle_document_symbol(state, p)))
        }
        References::METHOD => dispatch::<References, _>(req, |p| Some(handle_references(state, p))),
        Rename::METHOD => dispatch::<Rename, _>(req, |p| Some(handle_rename(state, p))),
        WorkspaceSymbolRequest::METHOD => {
            dispatch::<WorkspaceSymbolRequest, _>(req, |p| Some(handle_workspace_symbols(state, p)))
        }
        SemanticTokensFullRequest::METHOD => dispatch::<SemanticTokensFullRequest, _>(req, |p| {
            Some(handle_semantic_tokens_full(state, p))
        }),
        Formatting::METHOD => dispatch::<Formatting, _>(req, |p| Some(handle_formatting(state, p))),
        RangeFormatting::METHOD => {
            dispatch::<RangeFormatting, _>(req, |p| Some(handle_range_formatting(state, p)))
        }
        Completion::METHOD => dispatch::<Completion, _>(req, |p| Some(handle_completion(state, p))),
        // Custom extension request: taxonomy graph for a symbol.
        m if m == <TaxonomyRequest as lsp_types::request::Request>::METHOD => {
            dispatch::<TaxonomyRequest, _>(req, |p| Some(handle_taxonomy(state, p)))
        }
        // Custom extension requests: TPTP preview export + cursor line sync.
        m if m == TPTP_EXPORT_METHOD => {
            dispatch::<TptpExportRequest, _>(req, |p| Some(handle_tptp_export(state, p)))
        }
        m if m == TPTP_LINE_METHOD => {
            dispatch::<TptpLineRequest, _>(req, |p| Some(handle_tptp_line(state, p)))
        }
        _ => Response {
            id: req.id,
            result: None,
            error: Some(lsp_server::ResponseError {
                code: lsp_server::ErrorCode::MethodNotFound as i32,
                message: format!("sumo-lsp: method '{}' not implemented", req.method),
                data: None,
            }),
        },
    };
    out.push(Message::Response(resp));
}

/// Extract the typed `Params` from a `Request`, run the handler, and re-wrap
/// the `Result` into an `lsp_server::Response`.  A `None` from `handler`
/// encodes "no result" (empty response body, not an error).
fn dispatch<R, F>(req: Request, handler: F) -> Response
where
    R: lsp_types::request::Request,
    R::Params: DeserializeOwned,
    R::Result: serde::Serialize,
    F: FnOnce(R::Params) -> Option<R::Result>,
{
    match req.extract::<R::Params>(R::METHOD) {
        Ok((id, params)) => match handler(params) {
            Some(result) => Response {
                id,
                result: Some(serde_json::to_value(&result).expect("serialisable")),
                error: None,
            },
            None => Response {
                id,
                result: Some(serde_json::Value::Null),
                error: None,
            },
        },
        Err(ExtractError::MethodMismatch(r)) => Response {
            id: r.id,
            result: None,
            error: Some(lsp_server::ResponseError {
                code: lsp_server::ErrorCode::MethodNotFound as i32,
                message: format!("method mismatch for {}", R::METHOD),
                data: None,
            }),
        },
        Err(ExtractError::JsonError { method: _, error }) => Response {
            id: lsp_server::RequestId::from(0),
            result: None,
            error: Some(lsp_server::ResponseError {
                code: lsp_server::ErrorCode::InvalidParams as i32,
                message: format!("parse error: {}", error),
                data: None,
            }),
        },
    }
}

// -- Notification dispatch ----------------------------------------------------

fn handle_notification<L: TopLayer>(
    state: &GlobalState<L>,
    not: Notification,
    out: &mut Vec<Message>,
) -> Result<()> {
    match not.method.as_str() {
        DidOpenTextDocument::METHOD => {
            let params = cast_notification::<DidOpenTextDocument>(not)?;
            on_did_open(state, params, out);
        }
        DidChangeTextDocument::METHOD => {
            let params = cast_notification::<DidChangeTextDocument>(not)?;
            on_did_change(state, params, out);
        }
        DidCloseTextDocument::METHOD => {
            let params = cast_notification::<DidCloseTextDocument>(not)?;
            on_did_close(state, params);
        }
        m if m == SET_ACTIVE_FILES_METHOD => {
            on_set_active_files(state, not, out)?;
        }
        m if m == SET_IGNORED_DIAGNOSTICS_METHOD => {
            on_set_ignored_diagnostics(state, not, out)?;
        }
        _ => {
            log::trace!(target: "sumo_lsp", "ignored notification '{}'", not.method);
        }
    }
    Ok(())
}

fn cast_notification<N: lsp_types::notification::Notification>(
    not: Notification,
) -> Result<N::Params, ExtractError<Notification>> {
    not.extract::<N::Params>(N::METHOD)
}

// -- didOpen ------------------------------------------------------------------

fn on_did_open<L: TopLayer>(
    state: &GlobalState<L>,
    params: DidOpenTextDocumentParams,
    out: &mut Vec<Message>,
) {
    use std::sync::atomic::Ordering;

    let uri = params.text_document.uri;
    let text = params.text_document.text;
    let version = params.text_document.version;
    let tag = uri_to_tag(&uri);

    log::debug!(target: "sumo_lsp", "didOpen '{}' v{}", tag, version);

    // Skip the re-load if the workspace sweep already loaded this file, or if
    // the client owns KB membership via `sumo/setActiveFiles`.
    let already_loaded = {
        let session = state.session.read().expect("kb not poisoned");
        !session.kb().file_roots(&tag).is_empty()
    };
    let client_managed = state.client_manages_files.load(Ordering::SeqCst);
    if !already_loaded && !client_managed {
        let mut session = state.session.write().expect("kb not poisoned");
        let kb = session.kb_mut();
        let report = kb.load(
            SourceFile::kif(std::path::PathBuf::from(&tag), text.to_string()),
            &tag,
        );
        // Promote so man-page introspection (Base scope) sees the file.
        if report.ok {
            let _ = kb.make_session_axiomatic(&tag);
        }
    }

    let parsed = parse_document(tag.clone(), text.as_str(), Parser::Kif);
    let rope = Rope::from_str(&text);
    // Publish diagnostics before moving `parsed` into the per-doc state
    // (ParsedDocument is not Clone).
    {
        let session = state.session.read().expect("kb not poisoned");
        publish_diagnostics(
            out,
            &uri,
            &rope,
            &parsed,
            state,
            session.kb(),
            Some(version),
        );
    }
    {
        let mut docs = state.docs.write().expect("docs not poisoned");
        let mut ds = DocState::new(&text, version);
        ds.parsed = Some(parsed);
        docs.insert(uri.clone(), ds);
    }
}

// -- didChange ----------------------------------------------------------------

fn on_did_change<L: TopLayer>(
    state: &GlobalState<L>,
    params: DidChangeTextDocumentParams,
    out: &mut Vec<Message>,
) {
    let uri = params.text_document.uri;
    let version = params.text_document.version;
    let tag = uri_to_tag(&uri);

    log::debug!(target: "sumo_lsp", "didChange '{}' v{}", tag, version);

    // Full-document sync only (advertised in ServerCapabilities): each
    // `content_changes` entry's `text` replaces the full buffer.
    let new_text = match params.content_changes.last() {
        Some(change) => change.text.clone(),
        None => return,
    };

    {
        let mut session = state.session.write().expect("kb not poisoned");
        let kb = session.kb_mut();
        let report = kb.load(
            SourceFile::kif(std::path::PathBuf::from(&tag), new_text.to_string()),
            &tag,
        );
        // Re-promote the reconciled delta so man-page introspection
        // (Base scope) keeps seeing the file's current contents.
        if report.ok {
            let _ = kb.make_session_axiomatic(&tag);
        }
    }

    let parsed = parse_document(tag.clone(), new_text.as_str(), Parser::Kif);
    let rope = Rope::from_str(&new_text);
    {
        let session = state.session.read().expect("kb not poisoned");
        publish_diagnostics(
            out,
            &uri,
            &rope,
            &parsed,
            state,
            session.kb(),
            Some(version),
        );
    }
    {
        let mut docs = state.docs.write().expect("docs not poisoned");
        let mut ds = DocState::new(&new_text, version);
        ds.parsed = Some(parsed);
        docs.insert(uri.clone(), ds);
    }
}

// -- didClose -----------------------------------------------------------------

fn on_did_close<L: TopLayer>(state: &GlobalState<L>, params: DidCloseTextDocumentParams) {
    let uri = params.text_document.uri;
    log::debug!(target: "sumo_lsp", "didClose '{}'", uri_to_tag(&uri));
    let mut docs = state.docs.write().expect("docs not poisoned");
    docs.remove(&uri);
    // The KB retains the file's sentences so other open documents that
    // cross-reference them still resolve.
}

// -- sumo/setActiveFiles ------------------------------------------------------

/// Client-owned KB membership control.  Diffs the client's authoritative file
/// list against the currently-loaded set, applies the delta, and republishes
/// diagnostics for every affected file.
fn on_set_active_files<L: TopLayer>(
    state: &GlobalState<L>,
    not: Notification,
    out: &mut Vec<Message>,
) -> Result<()> {
    use std::sync::atomic::Ordering;

    let params: SetActiveFilesParams =
        serde_json::from_value(not.params).map_err(|e| anyhow::anyhow!(e))?;

    // Flip the "client owns membership" latch before the first application so
    // subsequent didOpen calls don't race-add files behind the client's back.
    state.client_manages_files.store(true, Ordering::SeqCst);

    let report = handle_set_active_files(state, params);

    let docs = state.docs.read().expect("docs lock not poisoned");
    let session = state.session.read().expect("kb lock not poisoned");
    let kb = session.kb();
    for tag in report.added.iter().chain(report.removed.iter()) {
        let Some(uri) = uri_from_tag(tag) else {
            continue;
        };
        let doc = docs.get(&uri);
        let rope = doc
            .map(|d| d.rope.clone())
            .unwrap_or_else(|| Rope::from_str(""));
        let parsed = doc.and_then(|d| d.parsed.as_ref());

        match parsed {
            Some(p) => publish_diagnostics(out, &uri, &rope, p, state, kb, None),
            None => {
                // No open document for this tag: reparse from disk so
                // diagnostics reflect current state.
                if let Ok(text) = std::fs::read_to_string(tag) {
                    let p = parse_document(tag.clone(), text.as_str(), Parser::Kif);
                    let rope = Rope::from_str(&text);
                    publish_diagnostics(out, &uri, &rope, &p, state, kb, None);
                }
            }
        }
    }

    Ok(())
}

/// Reverse of `uri_to_tag`: build a `file://` URL from a filesystem-path tag.
/// Returns `None` on non-file tags.
fn uri_from_tag(tag: &str) -> Option<Url> {
    crate::conv::url_from_file_path(tag)
}

// -- sumo/setIgnoredDiagnostics ----------------------------------------------

/// Update the server's `ignored_diagnostic_codes` set from a
/// client notification and re-publish diagnostics for every
/// currently-open document so the change takes effect without a
/// restart.
fn on_set_ignored_diagnostics<L: TopLayer>(
    state: &GlobalState<L>,
    not: Notification,
    out: &mut Vec<Message>,
) -> Result<()> {
    let params: SetIgnoredDiagnosticsParams =
        serde_json::from_value(not.params).map_err(|e| anyhow::anyhow!(e))?;

    handle_set_ignored_diagnostics(state, params);

    // Republish diagnostics for every open document.
    let docs = state.docs.read().expect("docs lock not poisoned");
    let session = state.session.read().expect("kb lock not poisoned");
    for (uri, doc) in docs.iter() {
        let rope = doc.rope.clone();
        if let Some(parsed) = doc.parsed.as_ref() {
            publish_diagnostics(
                out,
                uri,
                &rope,
                parsed,
                state,
                session.kb(),
                Some(doc.version),
            );
        }
    }
    Ok(())
}
