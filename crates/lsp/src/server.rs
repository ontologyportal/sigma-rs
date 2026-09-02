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

use sigmakee_rs_sdk::{
    parse_document, tokenize_kif, FileOrigin, HasTranslation, KnowledgeBase, LocalProvenance,
    ParsedDocument, Parser, SourceFile, TellResult, TestCase, TopLayer,
};

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

/// The dialect a buffer parses with, from its file tag: `.kif.tq` test files
/// use the TQ classifier; everything else is plain KIF.  TPTP dialects are
/// deliberately NOT selected -- this server's editor features (semantic
/// tokens, formatting) are KIF-tokenizer-based.
pub(crate) fn parser_for(tag: &str) -> Parser {
    match Parser::from_filename(tag) {
        Some(Parser::Tq) => Parser::Tq,
        _ => Parser::default(),
    }
}

/// True when `tag` names a `.kif.tq` test file.
pub(crate) fn is_tq(tag: &str) -> bool {
    matches!(Parser::from_filename(tag), Some(Parser::Tq))
}

/// Load a buffer into the shared KB under its own file tag.
///
/// A `.kif` constituent loads and is promoted, so Base-scope introspection
/// (man pages, taxonomy) sees it.  A `.kif.tq` test file is a session, not a
/// constituent: only its hypotheses are staged as session support under the
/// file's own tag (no promotion). The query and harness directives stay out
/// of the KB entirely.  Closing the document truncates the session
/// (see `on_did_close`).
fn load_buffer<L: TopLayer>(
    kb: &mut KnowledgeBase<L>,
    tag: &str,
    text: &str,
    parsed: &ParsedDocument,
) -> Option<TellResult> {
    // A syntactically broken buffer never touches the KB. The source
    // cache treats broken buffers as "the file now holds only these"
    // and retracts everything else the file contributed. The problem is
    // that mid-typing this is the NORMAL state, so reconciling it would
    // retract the whole constituent: semantic diagnostics vanish and
    // completion loses its KB context until the syntax is correct again.
    // This function simply skips the ingest of the buffer in those cases.
    // It still publishes the parse errors but skips ingesting the file so
    // that other artifacts (e.g. symbol info, semantic errors, etc) remain
    // in the KB
    if parsed.has_errors() {
        return None;
    }
    Some(if is_tq(tag) {
        let (tc, _background) = TestCase::from_doc_items(&parsed.ast, tag);
        kb.load(
            SourceFile {
                parser: Parser::Tq,
                name: tag.to_string(),
                path: std::path::PathBuf::from(tag),
                origin: FileOrigin::Local(LocalProvenance::UNKNOWN),
                contents: String::new(),
                prebuilt: Some(tc.axioms),
            },
            tag,
        )
    } else {
        let report = kb.load(
            SourceFile::kif(std::path::PathBuf::from(tag), text.to_string()),
            tag,
        );
        // Promote so man-page introspection (Base scope) sees the file.
        if report.ok {
            let _ = kb.make_session_axiomatic(tag);
        }
        report
    })
}

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

    // A short poll interval (well under `RELOAD_DEBOUNCE`) rather than a
    // blocking `for msg in &connection.receiver`, so a debounced reload
    // still flushes -- and diagnostics still refresh -- once its deadline
    // passes even if the client sends no further message after the last
    // keystroke (e.g. the user stops typing and does nothing else).
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
    loop {
        match connection.receiver.recv_timeout(POLL_INTERVAL) {
            Ok(msg) => {
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
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                let mut out = Vec::new();
                flush_due_reloads(&state, &mut out);
                for msg in out {
                    let _ = connection.sender.send(msg);
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
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
pub fn dispatch_message<L: TopLayer + HasTranslation>(
    state: &GlobalState<L>,
    msg: Message,
) -> Vec<Message> {
    let mut out = Vec::new();
    match msg {
        // Requests (completion, hover, ...) never flush a due reload inline:
        // doing so would make an interactive request pay for a full KB
        // reload (retract + reingest + cache cascade) synchronously before
        // it can answer, landing as a stutter right when the client is
        // waiting on a response. They answer against whatever KB state is
        // currently loaded -- fresh, or briefly stale (bounded by
        // `RELOAD_DEBOUNCE` plus however soon the next notification or the
        // native `run` loop's idle poll flushes it).
        Message::Request(req) => handle_request(state, req, &mut out),
        // Notifications carry no response the client is blocked on, so
        // flushing here is not user-visible the way it would be on a
        // request: a settled prior edit's reload runs before this one is
        // processed, keeping KB state reasonably fresh without stalling
        // interactive requests.
        Message::Notification(not) => {
            flush_due_reloads(state, &mut out);
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
                // During the initial sweep, documents with parse errors are skipped entirely
                let parsed = parse_document(tag.clone(), text.as_str(), parser_for(&tag));
                let load_report = {
                    let mut session = state.session.write().expect("kb not poisoned");
                    load_buffer(session.kb_mut(), &tag, &text, &parsed)
                };
                if !load_report.is_some_and(|r| r.ok) {
                    log::warn!(target: "sumo_lsp",
                        "workspace sweep: skipped '{}' ({} parse error(s)); \
                         LSP features on this file will be unavailable until it parses cleanly",
                        tag, parsed.parse_errors.len());
                }
                let rope = Rope::from_str(&text);
                // Publish diagnostics before moving `parsed` into the doc state
                // (ParsedDocument is not Clone).
                {
                    let session = state.session.read().expect("kb not poisoned");
                    publish_diagnostics(&mut out, &uri, &rope, &parsed, state, session.kb(), None);
                }
                {
                    let (tokens, _tok_err) = tokenize_kif(&text, &tag);
                    let mut docs = state.docs.write().expect("docs not poisoned");
                    let mut ds = DocState::new(&text, 0);
                    ds.parsed = Some(parsed);
                    ds.tokens = tokens;
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
            on_did_change(state, params);
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
    let parsed = parse_document(tag.clone(), text.as_str(), parser_for(&tag));

    let client_managed = state.client_manages_files.load(Ordering::SeqCst);
    if !already_loaded && !client_managed {
        let mut session = state.session.write().expect("kb not poisoned");
        // Ingestion into the KB is guarded by load_buffer here
        let _ = load_buffer(session.kb_mut(), &tag, &text, &parsed);
    }
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
        let (tokens, _tok_err) = tokenize_kif(&text, &tag);
        let mut docs = state.docs.write().expect("docs not poisoned");
        let mut ds = DocState::new(&text, version);
        ds.parsed = Some(parsed);
        ds.tokens = tokens;
        docs.insert(uri.clone(), ds);
    }
}

// -- didChange ----------------------------------------------------------------

/// How long to wait after the last edit to a document before reconciling it
/// into the KB (retract + reingest, firing the full reactive cache cascade)
/// and re-publishing diagnostics. Reparsing/retokenizing for completion and
/// other syntax-only handlers is NOT debounced -- see [`on_did_change`].
const RELOAD_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

fn on_did_change<L: TopLayer>(state: &GlobalState<L>, params: DidChangeTextDocumentParams) {
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

    // Cheap and synchronous on every keystroke: pure parsing/tokenizing, no
    // KB interaction. Completion's cursor-context classification and other
    // syntax-only handlers (hover, goto-definition, document-symbols) read
    // `DocState.parsed`/`tokens` directly, so this keeps them working
    // against the just-typed text with no debounce latency.
    let parsed = parse_document(tag.clone(), new_text.as_str(), parser_for(&tag));
    let (tokens, _tok_err) = tokenize_kif(&new_text, &tag);
    {
        let mut docs = state.docs.write().expect("docs not poisoned");
        let mut ds = DocState::new(&new_text, version);
        ds.parsed = Some(parsed);
        ds.tokens = tokens;
        docs.insert(uri.clone(), ds);
    }

    // Expensive, and debounced: reconciling the buffer into the KB and
    // semantic validation. Overwriting any existing pending entry pushes its
    // deadline forward, so a burst of keystrokes reloads once after typing
    // settles rather than once per keystroke. `flush_due_reloads` (called at
    // the top of every `dispatch_message`) performs it once due.
    let mut pending = state.pending_reloads.write().expect("pending not poisoned");
    pending.insert(
        uri,
        crate::state::PendingReload {
            text: new_text,
            version,
            due: sigmakee_rs_sdk::Instant::now() + RELOAD_DEBOUNCE,
        },
    );
}

/// Perform any debounced KB reload + diagnostics publish (see
/// [`PendingReload`](crate::state::PendingReload)) whose deadline has
/// elapsed, appending the resulting `publishDiagnostics` notifications to
/// `out`.
///
/// Called from [`dispatch_message`]'s notification branch (never its
/// request branch -- see that function's comment: flushing inline with a
/// request would make an interactive completion/hover/etc. pay for a full
/// KB reload before it can answer), so pending reloads flush on the natural
/// cadence of edit-shaped client traffic without a background timer thread
/// -- the same mechanism drives both the native binary and an embedding
/// transport (e.g. wasm). [`run`]'s native loop additionally polls via
/// `recv_timeout` so a typing pause still flushes even if the client sends
/// no further notification before the next keystroke.
fn flush_due_reloads<L: TopLayer>(state: &GlobalState<L>, out: &mut Vec<Message>) {
    let now = sigmakee_rs_sdk::Instant::now();
    let due: Vec<(Url, crate::state::PendingReload)> = {
        let mut pending = state.pending_reloads.write().expect("pending not poisoned");
        let ready: Vec<Url> = pending
            .iter()
            .filter(|(_, p)| p.due <= now)
            .map(|(u, _)| u.clone())
            .collect();
        ready
            .into_iter()
            .filter_map(|u| pending.remove(&u).map(|p| (u, p)))
            .collect()
    };
    for (uri, reload) in due {
        let tag = uri_to_tag(&uri);
        let parsed = parse_document(tag.clone(), reload.text.as_str(), parser_for(&tag));
        {
            let mut session = state.session.write().expect("kb not poisoned");
            let _ = load_buffer(session.kb_mut(), &tag, &reload.text, &parsed);
        }
        let rope = Rope::from_str(&reload.text);
        let session = state.session.read().expect("kb not poisoned");
        publish_diagnostics(
            out,
            &uri,
            &rope,
            &parsed,
            state,
            session.kb(),
            Some(reload.version),
        );
    }
}

// -- didClose -----------------------------------------------------------------

fn on_did_close<L: TopLayer>(state: &GlobalState<L>, params: DidCloseTextDocumentParams) {
    let uri = params.text_document.uri;
    let tag = uri_to_tag(&uri);
    log::debug!(target: "sumo_lsp", "didClose '{}'", tag);
    {
        let mut docs = state.docs.write().expect("docs not poisoned");
        docs.remove(&uri);
    }
    if is_tq(&tag) {
        // A test file's hypotheses are ephemeral session support scoped to
        // the open document: reconcile the source to empty (the same
        // rollback shape `KnowledgeBase::ask` uses) so nothing leaks into
        // later queries.  `.kif` constituents, by contrast, stay loaded so
        // other open documents that cross-reference them still resolve.
        let mut session = state.session.write().expect("kb not poisoned");
        let _ = session
            .kb_mut()
            .load(SourceFile::truncate(std::path::PathBuf::from(&tag)), &tag);
    }
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
                    let p =
                        parse_document(tag.clone(), text.as_str(), Parser::Kif { options: None });
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

#[cfg(test)]
mod tq_session_tests {
    use super::*;
    use lsp_types::notification::{DidCloseTextDocument, DidOpenTextDocument};

    fn notify<N: lsp_types::notification::Notification>(params: N::Params) -> Message {
        Message::Notification(Notification {
            method: N::METHOD.to_string(),
            params: serde_json::to_value(&params).expect("serialisable"),
        })
    }

    fn open(state: &GlobalState, uri: &Url, text: &str) -> Vec<Message> {
        dispatch_message(
            state,
            notify::<DidOpenTextDocument>(lsp_types::DidOpenTextDocumentParams {
                text_document: lsp_types::TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "kif".into(),
                    version: 1,
                    text: text.into(),
                },
            }),
        )
    }

    fn close(state: &GlobalState, uri: &Url) {
        dispatch_message(
            state,
            notify::<DidCloseTextDocument>(lsp_types::DidCloseTextDocumentParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            }),
        );
    }

    fn change(state: &GlobalState, uri: &Url, version: i32, text: &str) -> Vec<Message> {
        dispatch_message(
            state,
            notify::<DidChangeTextDocument>(lsp_types::DidChangeTextDocumentParams {
                text_document: lsp_types::VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version,
                },
                content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: text.into(),
                }],
            }),
        )
    }

    #[test]
    fn did_change_defers_kb_reload_until_the_debounce_deadline() {
        let state = GlobalState::new();
        let uri = Url::parse("file:///tmp/debounce.kif").expect("url");
        open(&state, &uri, "(subclass Dog Mammal)");

        // The change adds a brand-new symbol not yet in the KB.
        change(
            &state,
            &uri,
            2,
            "(subclass Dog Mammal)\n(subclass Cat Mammal)",
        );

        // Immediately after: the reload is pending, not yet applied -- the
        // KB still only reflects the original content.
        {
            let session = state.session.read().expect("kb");
            assert!(
                session.kb().symbol_id("Cat").is_none(),
                "reload must not run synchronously with didChange"
            );
        }
        assert!(
            state
                .pending_reloads
                .read()
                .expect("pending")
                .contains_key(&uri),
            "a pending reload must be scheduled"
        );

        // Force the deadline into the past (no real sleep) and flush.
        {
            let mut pending = state.pending_reloads.write().expect("pending");
            let p = pending.get_mut(&uri).expect("pending entry");
            p.due = sigmakee_rs_sdk::Instant::now() - std::time::Duration::from_millis(1);
        }
        let mut out = Vec::new();
        flush_due_reloads(&state, &mut out);

        let session = state.session.read().expect("kb");
        assert!(
            session.kb().symbol_id("Cat").is_some(),
            "reload must apply once its deadline has passed"
        );
        assert!(
            !state
                .pending_reloads
                .read()
                .expect("pending")
                .contains_key(&uri),
            "pending entry consumed after flush"
        );
    }

    #[test]
    fn a_later_edit_before_the_deadline_pushes_it_forward() {
        let state = GlobalState::new();
        let uri = Url::parse("file:///tmp/debounce-coalesce.kif").expect("url");
        open(&state, &uri, "(subclass Dog Mammal)");

        change(
            &state,
            &uri,
            2,
            "(subclass Dog Mammal)\n(subclass Cat Mammal)",
        );
        let due1 = state
            .pending_reloads
            .read()
            .expect("pending")
            .get(&uri)
            .expect("pending entry")
            .due;

        change(
            &state,
            &uri,
            3,
            "(subclass Dog Mammal)\n(subclass Cat Mammal)\n(subclass Bird Animal)",
        );
        let due2 = state
            .pending_reloads
            .read()
            .expect("pending")
            .get(&uri)
            .expect("pending entry")
            .due;
        assert!(
            due2 > due1,
            "a further edit must push the debounce deadline forward, not queue a second reload"
        );
        assert_eq!(
            state.pending_reloads.read().expect("pending").len(),
            1,
            "edits to the same doc coalesce into one pending reload"
        );

        // Flushing now applies the LATEST text, not the first change's.
        {
            let mut pending = state.pending_reloads.write().expect("pending");
            let p = pending.get_mut(&uri).expect("pending entry");
            p.due = sigmakee_rs_sdk::Instant::now() - std::time::Duration::from_millis(1);
        }
        let mut out = Vec::new();
        flush_due_reloads(&state, &mut out);

        let session = state.session.read().expect("kb");
        assert!(
            session.kb().symbol_id("Bird").is_some(),
            "the coalesced reload must reflect the latest edit"
        );
    }

    #[test]
    fn a_request_never_flushes_a_due_reload() {
        // The exact bug this guards against: an interactive request (hover,
        // completion, ...) arriving right after a pending reload's deadline
        // has passed must NOT pay for that reload inline -- doing so stalls
        // the request behind a full KB retract+reingest+cache-cascade,
        // landing as a stutter right when the client is waiting on a
        // response. Only a notification (didChange/didOpen/didClose) or the
        // native `run` loop's idle poll may flush it.
        let state = GlobalState::new();
        let uri = Url::parse("file:///tmp/debounce-no-request-flush.kif").expect("url");
        open(&state, &uri, "(subclass Dog Mammal)");
        change(
            &state,
            &uri,
            2,
            "(subclass Dog Mammal)\n(subclass Cat Mammal)",
        );

        // Force the deadline into the past, as if the client had paused.
        {
            let mut pending = state.pending_reloads.write().expect("pending");
            let p = pending.get_mut(&uri).expect("pending entry");
            p.due = sigmakee_rs_sdk::Instant::now() - std::time::Duration::from_millis(1);
        }

        // A request (hover -- any request handler exercises the same
        // dispatch_message::Request branch) must leave the overdue reload
        // untouched.
        dispatch_message(
            &state,
            Message::Request(Request {
                id: 1.into(),
                method: lsp_types::request::HoverRequest::METHOD.to_string(),
                params: serde_json::to_value(lsp_types::HoverParams {
                    text_document_position_params: lsp_types::TextDocumentPositionParams {
                        text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                        position: lsp_types::Position {
                            line: 0,
                            character: 0,
                        },
                    },
                    work_done_progress_params: Default::default(),
                })
                .expect("serialisable"),
            }),
        );
        assert!(
            state
                .pending_reloads
                .read()
                .expect("pending")
                .contains_key(&uri),
            "a request must not flush an overdue pending reload"
        );

        // A notification (any of them) does flush it.
        close(
            &state,
            &Url::parse("file:///tmp/unrelated.kif").expect("url"),
        );
        assert!(
            !state
                .pending_reloads
                .read()
                .expect("pending")
                .contains_key(&uri),
            "a subsequent notification must flush the overdue reload"
        );
    }

    #[test]
    fn tq_open_stages_hypotheses_as_an_unpromoted_session() {
        let state = GlobalState::new();
        let uri = Url::parse("file:///tmp/t1.kif.tq").expect("url");
        let tag = uri_to_tag(&uri);
        open(
            &state,
            &uri,
            "(note \"a test\")\n(instance Rex Dog)\n(query (instance Rex Mammal))\n(answer yes)",
        );

        let session = state.session.read().expect("kb");
        let kb = session.kb();
        // Exactly the hypothesis entered the store -- not the query, not the
        // directives.
        assert_eq!(kb.file_roots(&tag).len(), 1, "hypothesis only");
        assert_eq!(kb.session_sids(&tag).len(), 1, "staged as session support");
        // Never promoted: the axiom base stays empty.
        assert_eq!(kb.sine_axiom_count(), 0, "test files must not promote");
    }

    #[test]
    fn kif_open_still_promotes() {
        let state = GlobalState::new();
        let uri = Url::parse("file:///tmp/base.kif").expect("url");
        open(&state, &uri, "(subclass Dog Mammal)");
        let session = state.session.read().expect("kb");
        assert!(
            session.kb().sine_axiom_count() > 0,
            "constituents still promote"
        );
    }

    #[test]
    fn tq_close_truncates_the_session() {
        let state = GlobalState::new();
        let uri = Url::parse("file:///tmp/t2.kif.tq").expect("url");
        let tag = uri_to_tag(&uri);
        open(
            &state,
            &uri,
            "(instance Rex Dog)\n(query (instance Rex Dog))",
        );
        close(&state, &uri);

        let session = state.session.read().expect("kb");
        let kb = session.kb();
        assert!(
            kb.file_roots(&tag).is_empty(),
            "hypotheses removed on close"
        );
        assert!(kb.session_sids(&tag).is_empty(), "session emptied on close");
    }

    #[test]
    fn tq_formatting_roundtrips_directives_query_and_comments() {
        let state = GlobalState::new();
        let uri = Url::parse("file:///tmp/t4.kif.tq").expect("url");
        open(
            &state,
            &uri,
            "; suite header\n(note \"a test\")\n(instance   Rex   Dog) ; the pet\n(query (instance Rex Dog))\n(answer yes)",
        );

        let req = Message::Request(Request {
            id: 7.into(),
            method: lsp_types::request::Formatting::METHOD.to_string(),
            params: serde_json::to_value(lsp_types::DocumentFormattingParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                options: lsp_types::FormattingOptions {
                    tab_size: 2,
                    insert_spaces: true,
                    ..Default::default()
                },
                work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
            })
            .expect("serialisable"),
        });
        let out = dispatch_message(&state, req);
        let edits: Vec<lsp_types::TextEdit> = out
            .iter()
            .find_map(|m| match m {
                Message::Response(r) => serde_json::from_value(r.result.clone()?).ok(),
                _ => None,
            })
            .expect("formatting response");
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].new_text,
            "; suite header\n(note \"a test\")\n(instance Rex Dog) ; the pet\n(query (instance Rex Dog))\n(answer yes)"
        );
    }

    /// Count semantic tokens of type-index 0 (KEYWORD) for an open document.
    fn keyword_token_count(state: &GlobalState, uri: &Url, id: i32) -> usize {
        let req = Message::Request(Request {
            id: id.into(),
            method: lsp_types::request::SemanticTokensFullRequest::METHOD.to_string(),
            params: serde_json::to_value(lsp_types::SemanticTokensParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
                partial_result_params: lsp_types::PartialResultParams::default(),
            })
            .expect("serialisable"),
        });
        let out = dispatch_message(state, req);
        let toks: lsp_types::SemanticTokensResult = out
            .iter()
            .find_map(|m| match m {
                Message::Response(r) => serde_json::from_value(r.result.clone()?).ok(),
                _ => None,
            })
            .expect("semantic tokens response");
        match toks {
            lsp_types::SemanticTokensResult::Tokens(t) => {
                t.data.iter().filter(|d| d.token_type == 0).count()
            }
            _ => 0,
        }
    }

    #[test]
    fn tq_directive_heads_highlight_as_keywords() {
        let state = GlobalState::new();
        let src = "(note \"n\")\n(instance Rex Dog)\n(query (instance Rex Dog))\n(answer yes)";

        // In a test file, `note` / `query` / `answer` heads are keywords.
        let tq = Url::parse("file:///tmp/hl.kif.tq").expect("url");
        open(&state, &tq, src);
        assert_eq!(keyword_token_count(&state, &tq, 20), 3);

        // The same text as plain KIF gets no directive treatment.
        let kif = Url::parse("file:///tmp/hl.kif").expect("url");
        open(&state, &kif, src);
        assert_eq!(keyword_token_count(&state, &kif, 21), 0);
    }

    #[test]
    fn completion_is_prefix_filtered_and_capped_server_side() {
        // An untyped argument position against a large symbol table must NOT
        // return the whole table (it wedges a single-threaded host behind
        // item construction + serialization): capped list, isIncomplete set.
        let state = GlobalState::new();
        let uri = Url::parse("file:///tmp/comp.kif").expect("url");
        let mut text = String::new();
        for i in 0..300 {
            text.push_str(&format!("(subclass Class{i} Entity)\n"));
        }
        text.push_str("(instance Rex Entity)");
        open(&state, &uri, &text);

        let complete = |id: i32, character: u32| -> serde_json::Value {
            let out = dispatch_message(
                &state,
                Message::Request(Request {
                    id: id.into(),
                    method: lsp_types::request::Completion::METHOD.to_string(),
                    params: serde_json::to_value(lsp_types::CompletionParams {
                        text_document_position: lsp_types::TextDocumentPositionParams {
                            text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                            position: lsp_types::Position {
                                line: 300,
                                character,
                            },
                        },
                        work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
                        partial_result_params: lsp_types::PartialResultParams::default(),
                        context: None,
                    })
                    .expect("serialisable"),
                }),
            );
            out.iter()
                .find_map(|m| match m {
                    Message::Response(r) => r.result.clone(),
                    _ => None,
                })
                .expect("completion response")
        };

        // Empty prefix at "(instance Rex |": nothing is offered until the
        // user types -- `instance`'s 2nd-arg domain is the `Class` metaclass,
        // which can't be pushed into a search taxonomy constraint (see
        // `suggest_args`'s `class_type_fallback`), so an empty query with no
        // constraint returns nothing rather than the whole (capped) table.
        let r = complete(50, 14);
        assert_eq!(r["isIncomplete"], false, "nothing to cut when empty");
        assert_eq!(r["items"].as_array().expect("items").len(), 0);

        // A common, still-large-matching prefix is capped and incomplete:
        // "(instance Rex Class|" matches all 300 `ClassN` symbols.
        let state1b = GlobalState::new();
        let uri1b = Url::parse("file:///tmp/comp1b.kif").expect("url");
        let text1b = text.replace("(instance Rex Entity)", "(instance Rex Class)");
        open(&state1b, &uri1b, &text1b);
        let out1b = dispatch_message(
            &state1b,
            Message::Request(Request {
                id: 52.into(),
                method: lsp_types::request::Completion::METHOD.to_string(),
                params: serde_json::to_value(lsp_types::CompletionParams {
                    text_document_position: lsp_types::TextDocumentPositionParams {
                        text_document: lsp_types::TextDocumentIdentifier { uri: uri1b.clone() },
                        position: lsp_types::Position {
                            line: 300,
                            character: 19,
                        },
                    },
                    work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
                    partial_result_params: lsp_types::PartialResultParams::default(),
                    context: None,
                })
                .expect("serialisable"),
            }),
        );
        let r1b = out1b
            .iter()
            .find_map(|m| match m {
                Message::Response(resp) => resp.result.clone(),
                _ => None,
            })
            .expect("completion response");
        assert_eq!(r1b["isIncomplete"], true, "large candidate set must be cut");
        assert_eq!(r1b["items"].as_array().expect("items").len(), 50);

        // A narrowing prefix filters server-side: "(instance Rex Class29|".
        let state2 = GlobalState::new();
        let uri2 = Url::parse("file:///tmp/comp2.kif").expect("url");
        let text2 = text.replace("(instance Rex Entity)", "(instance Rex Class29X)");
        open(&state2, &uri2, &text2);
        let out = dispatch_message(
            &state2,
            Message::Request(Request {
                id: 51.into(),
                method: lsp_types::request::Completion::METHOD.to_string(),
                params: serde_json::to_value(lsp_types::CompletionParams {
                    text_document_position: lsp_types::TextDocumentPositionParams {
                        text_document: lsp_types::TextDocumentIdentifier { uri: uri2.clone() },
                        position: lsp_types::Position {
                            line: 300,
                            character: 21,
                        },
                    },
                    work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
                    partial_result_params: lsp_types::PartialResultParams::default(),
                    context: None,
                })
                .expect("serialisable"),
            }),
        );
        let r2 = out
            .iter()
            .find_map(|m| match m {
                Message::Response(resp) => resp.result.clone(),
                _ => None,
            })
            .expect("completion response");
        let items = r2["items"].as_array().expect("items");
        assert_eq!(r2["isIncomplete"], false, "narrow prefix fits the cap");
        // Class29, Class290..Class299, plus the buffer's own Class29X = 12.
        assert_eq!(items.len(), 12, "prefix must filter server-side");
        assert!(items
            .iter()
            .all(|i| i["label"].as_str().unwrap().starts_with("Class29")));
    }

    #[test]
    fn tq_file_directive_warns_when_constituent_is_missing() {
        let state = GlobalState::new();
        let uri = Url::parse("file:///tmp/t3.kif.tq").expect("url");
        let out = open(
            &state,
            &uri,
            "(file \"Merge.kif\")\n(instance Rex Dog)\n(query (instance Rex Dog))",
        );
        let diags = published_messages(&out);
        assert!(
            diags.iter().any(|m| m.contains("file-not-loaded")),
            "expected a file-not-loaded warning, got: {diags:?}"
        );

        // Load a constituent with that basename; re-opening publishes clean.
        let base = Url::parse("file:///tmp/Merge.kif").expect("url");
        open(&state, &base, "(subclass Dog Mammal)");
        let out2 = open(
            &state,
            &uri,
            "(file \"Merge.kif\")\n(instance Rex Dog)\n(query (instance Rex Dog))",
        );
        let diags2 = published_messages(&out2);
        assert!(
            !diags2.iter().any(|m| m.contains("file-not-loaded")),
            "warning must clear once the constituent is loaded: {diags2:?}"
        );
    }

    /// Render each publishDiagnostics notification in `out` as its JSON text.
    fn published_messages(out: &[Message]) -> Vec<String> {
        out.iter()
            .filter_map(|m| match m {
                Message::Notification(n) if n.method == "textDocument/publishDiagnostics" => {
                    Some(n.params.to_string())
                }
                _ => None,
            })
            .collect()
    }
}
