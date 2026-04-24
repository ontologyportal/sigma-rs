// crates/sumo-lsp/src/server.rs
//
// LSP message loop.  Receives `Message`s from the lsp-server
// `Connection`, dispatches each to a handler, and publishes
// notifications back on the same connection.
//
// Single-threaded MVP: handlers run inline on the event-loop
// thread.  When contention becomes measurable we'll move heavy
// handlers to a worker pool.

use anyhow::Result;
use lsp_server::{Connection, ExtractError, Message, Notification, Request, Response};
use lsp_types::{
    notification::{DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _},
    request::{
        Completion, DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest,
        RangeFormatting, References, Rename, Request as _, SemanticTokensFullRequest,
        WorkspaceSymbolRequest,
    },
    CompletionOptions, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, InitializeParams, InitializeResult, OneOf,
    PositionEncodingKind, RenameOptions, SemanticTokensFullOptions, SemanticTokensOptions,
    SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions, Url,
    WorkDoneProgressOptions, WorkspaceFolder,
};
use ropey::Rope;
use serde::de::DeserializeOwned;

use sumo_kb::parse_document;

use crate::conv::uri_to_tag;
use crate::handlers::{
    handle_completion, handle_document_symbol, handle_formatting, handle_goto_definition,
    handle_hover, handle_range_formatting, handle_references, handle_rename,
    handle_semantic_tokens_full, handle_set_active_files, handle_set_ignored_diagnostics,
    handle_taxonomy, handle_workspace_symbols, publish_diagnostics, semantic_tokens_legend,
    TaxonomyRequest, SET_ACTIVE_FILES_METHOD, SET_IGNORED_DIAGNOSTICS_METHOD,
    SetActiveFilesParams, SetIgnoredDiagnosticsParams,
};
use crate::state::{DocState, GlobalState};

/// Run the server against a `Connection`.  Returns on clean
/// shutdown (or propagates a transport error).
pub fn run(connection: Connection) -> Result<()> {
    // LSP initialize handshake.
    let (id, params) = connection.initialize_start()?;
    let init_params: InitializeParams = serde_json::from_value(params)?;
    let result = InitializeResult {
        capabilities: server_capabilities(),
        server_info:  Some(ServerInfo {
            name:    "sumo-lsp".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    };
    connection.initialize_finish(id, serde_json::to_value(result)?)?;

    log::info!(target: "sumo_lsp", "initialised");

    // Event state.
    let state = GlobalState::new();

    // Clients that plan to own KB membership via `sumo/setActiveFiles`
    // (notably the VSCode extension) can advertise
    // `initializationOptions: { "clientManagesFiles": true }` to
    // suppress the server's initial workspace sweep.  The sweep
    // otherwise loads every .kif under the workspace roots, which
    // `setActiveFiles` then has to partially un-load — `remove_file`
    // is O(total occurrences in KB) per call, so the dance is
    // quadratic on larger workspaces and hangs the event loop long
    // enough to starve every subsequent request.  Headless clients
    // that never send `setActiveFiles` still get the sweep.
    let client_manages_files = init_params.initialization_options
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
        // Best-effort workspace index: load every `.kif` / `.kif.tq`
        // under each workspaceFolder into the shared KB, then publish
        // a first-pass diagnostics sweep for each.  Failures are logged
        // and ignored -- missing perms or a non-file root shouldn't
        // kill the server.
        initial_workspace_sweep(&connection, &state, &init_params);
    }

    // Main message loop.
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    // `handle_shutdown` sent the response internally;
                    // we just need to exit the loop.
                    log::info!(target: "sumo_lsp", "shutdown requested");
                    return Ok(());
                }
                handle_request(&connection, &state, req);
            }
            Message::Notification(not) => {
                if let Err(e) = handle_notification(&connection, &state, not) {
                    log::warn!(target: "sumo_lsp", "notification handler error: {:?}", e);
                }
            }
            Message::Response(_) => {
                // We issue no requests of our own in MVP; responses
                // are dropped.  (Phase 4 adds client-initiated
                // workDoneProgress which would land here.)
            }
        }
    }

    Ok(())
}

// -- Capabilities -------------------------------------------------------------

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change:     Some(TextDocumentSyncKind::FULL),
                save:       None,
                will_save:  None,
                will_save_wait_until: None,
            },
        )),
        definition_provider:              Some(OneOf::Left(true)),
        hover_provider:                   Some(lsp_types::HoverProviderCapability::Simple(true)),
        document_symbol_provider:         Some(OneOf::Left(true)),
        references_provider:              Some(OneOf::Left(true)),
        rename_provider:                  Some(OneOf::Right(RenameOptions {
            prepare_provider:                    Some(false),
            work_done_progress_options:          WorkDoneProgressOptions::default(),
        })),
        workspace_symbol_provider:        Some(OneOf::Left(true)),
        // Phase 5: semantic highlighting, formatting, completion.
        semantic_tokens_provider:         Some(
            SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                work_done_progress_options:       WorkDoneProgressOptions::default(),
                legend:                           semantic_tokens_legend(),
                range:                            Some(false),
                full:                             Some(SemanticTokensFullOptions::Bool(true)),
            })
        ),
        document_formatting_provider:     Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        completion_provider:              Some(CompletionOptions {
            // Firing on `(` gives sentence-head completion; space
            // advances to arg-position completion.  Clients that
            // invoke completion on Ctrl-Space still work -- these
            // are purely user-convenience auto-triggers.
            trigger_characters:             Some(vec![
                "(".to_string(), " ".to_string(), "?".to_string(), "@".to_string(),
            ]),
            resolve_provider:               Some(false),
            ..Default::default()
        }),
        ..Default::default()
    }
}

// -- Workspace sweep ----------------------------------------------------------

fn initial_workspace_sweep(connection: &Connection, state: &GlobalState, init: &InitializeParams) {
    // Prefer `workspace_folders` (LSP >= 3.6); fall back to the
    // legacy `root_uri` for older clients.
    let folders: Vec<WorkspaceFolder> = match &init.workspace_folders {
        Some(fs) if !fs.is_empty() => fs.clone(),
        _ => {
            #[allow(deprecated)]
            if let Some(root) = init.root_uri.clone() {
                vec![WorkspaceFolder { uri: root, name: "root".to_string() }]
            } else {
                return;
            }
        }
    };

    for folder in &folders {
        let Ok(dir) = folder.uri.to_file_path() else { continue; };
        let kif_files = collect_kif_files(&dir);
        log::info!(target: "sumo_lsp",
            "workspace sweep: {} KIF files in '{}'", kif_files.len(), dir.display());
        for path in kif_files {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let Ok(uri) = Url::from_file_path(&path) else { continue; };
                let tag      = uri_to_tag(&uri);
                // Load into the shared KB.  Parse errors reject the
                // file entirely (see `sumo_kb::kif_store::load_kif`)
                // so the rest of the workspace stays healthy; the
                // bad file still publishes diagnostics below via
                // `parse_document`, which the client surfaces in
                // the Problems panel.
                let load_report = {
                    let mut kb = state.kb.write().expect("kb not poisoned");
                    kb.load_kif(&text, &tag, None)
                };
                if !load_report.ok {
                    log::warn!(target: "sumo_lsp",
                        "workspace sweep: skipped '{}' ({} parse error(s)); \
                         LSP features on this file will be unavailable until it parses cleanly",
                        tag, load_report.errors.len());
                }
                // Build the per-doc state so subsequent didChanges
                // can diff.  parse_document gives us the token
                // stream + per-root hashes cheaply.
                let parsed = parse_document(tag.clone(), text.as_str());
                let rope   = Rope::from_str(&text);
                {
                    let mut docs = state.docs.write().expect("docs not poisoned");
                    let mut ds   = DocState::new(&text, 0);
                    ds.parsed    = Some(parsed.clone());
                    docs.insert(uri.clone(), ds);
                }
                // Publish initial diagnostics for this file.
                let kb = state.kb.read().expect("kb not poisoned");
                publish_diagnostics(&connection.sender, &uri, &rope, &parsed, state, &kb, None);
            }
        }
    }
}

fn collect_kif_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out; };
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

fn handle_request(connection: &Connection, state: &GlobalState, req: Request) {
    // `R::Result` for hover/definition/documentSymbol is itself
    // `Option<_>`; our inner handler already returns that shape, so
    // we wrap the whole thing in `Some` for `dispatch`.
    let resp = match req.method.as_str() {
        HoverRequest::METHOD => {
            dispatch::<HoverRequest, _>(req, |p| Some(handle_hover(state, p)))
        }
        GotoDefinition::METHOD => {
            dispatch::<GotoDefinition, _>(req, |p| Some(handle_goto_definition(state, p)))
        }
        DocumentSymbolRequest::METHOD => {
            dispatch::<DocumentSymbolRequest, _>(req, |p| Some(handle_document_symbol(state, p)))
        }
        References::METHOD => {
            dispatch::<References, _>(req, |p| Some(handle_references(state, p)))
        }
        Rename::METHOD => {
            dispatch::<Rename, _>(req, |p| Some(handle_rename(state, p)))
        }
        WorkspaceSymbolRequest::METHOD => {
            dispatch::<WorkspaceSymbolRequest, _>(req, |p| Some(handle_workspace_symbols(state, p)))
        }
        SemanticTokensFullRequest::METHOD => {
            dispatch::<SemanticTokensFullRequest, _>(req, |p| Some(handle_semantic_tokens_full(state, p)))
        }
        Formatting::METHOD => {
            dispatch::<Formatting, _>(req, |p| Some(handle_formatting(state, p)))
        }
        RangeFormatting::METHOD => {
            dispatch::<RangeFormatting, _>(req, |p| Some(handle_range_formatting(state, p)))
        }
        Completion::METHOD => {
            dispatch::<Completion, _>(req, |p| Some(handle_completion(state, p)))
        }
        // Custom extension request: taxonomy graph for a symbol.
        // See `handlers::taxonomy` for the wire format.
        m if m == <TaxonomyRequest as lsp_types::request::Request>::METHOD => {
            dispatch::<TaxonomyRequest, _>(req, |p| Some(handle_taxonomy(state, p)))
        }
        _ => Response {
            id:     req.id,
            result: None,
            error:  Some(lsp_server::ResponseError {
                code:    lsp_server::ErrorCode::MethodNotFound as i32,
                message: format!("sumo-lsp: method '{}' not implemented", req.method),
                data:    None,
            }),
        },
    };
    let _ = connection.sender.send(Message::Response(resp));
}

/// Extract the typed `Params` from a `Request`, run the handler,
/// and re-wrap the `Result` into an `lsp_server::Response`.
/// `handler` returns an `Option` -- `None` encodes "no result"
/// (empty response body, not an error), which is the LSP
/// convention for hover/definition when the cursor isn't on a
/// recognisable element.
fn dispatch<R, F>(req: Request, handler: F) -> Response
where
    R:            lsp_types::request::Request,
    R::Params:    DeserializeOwned,
    R::Result:    serde::Serialize,
    F: FnOnce(R::Params) -> Option<R::Result>,
{
    match req.extract::<R::Params>(R::METHOD) {
        Ok((id, params)) => match handler(params) {
            Some(result) => Response {
                id,
                result: Some(serde_json::to_value(&result).expect("serialisable")),
                error:  None,
            },
            None => Response {
                id,
                result: Some(serde_json::Value::Null),
                error:  None,
            },
        },
        Err(ExtractError::MethodMismatch(r)) => Response {
            id:     r.id,
            result: None,
            error:  Some(lsp_server::ResponseError {
                code:    lsp_server::ErrorCode::MethodNotFound as i32,
                message: format!("method mismatch for {}", R::METHOD),
                data:    None,
            }),
        },
        Err(ExtractError::JsonError { method: _, error }) => Response {
            id:     lsp_server::RequestId::from(0),
            result: None,
            error:  Some(lsp_server::ResponseError {
                code:    lsp_server::ErrorCode::InvalidParams as i32,
                message: format!("parse error: {}", error),
                data:    None,
            }),
        },
    }
}

// -- Notification dispatch ----------------------------------------------------

fn handle_notification(connection: &Connection, state: &GlobalState, not: Notification) -> Result<()> {
    match not.method.as_str() {
        DidOpenTextDocument::METHOD => {
            let params = cast_notification::<DidOpenTextDocument>(not)?;
            on_did_open(connection, state, params);
        }
        DidChangeTextDocument::METHOD => {
            let params = cast_notification::<DidChangeTextDocument>(not)?;
            on_did_change(connection, state, params);
        }
        DidCloseTextDocument::METHOD => {
            let params = cast_notification::<DidCloseTextDocument>(not)?;
            on_did_close(state, params);
        }
        m if m == SET_ACTIVE_FILES_METHOD => {
            on_set_active_files(connection, state, not)?;
        }
        m if m == SET_IGNORED_DIAGNOSTICS_METHOD => {
            on_set_ignored_diagnostics(connection, state, not)?;
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

fn on_did_open(connection: &Connection, state: &GlobalState, params: DidOpenTextDocumentParams) {
    use std::sync::atomic::Ordering;

    let uri      = params.text_document.uri;
    let text     = params.text_document.text;
    let version  = params.text_document.version;
    let tag      = uri_to_tag(&uri);

    log::debug!(target: "sumo_lsp", "didOpen '{}' v{}", tag, version);

    // If the workspace sweep already loaded this file, skip the
    // re-load (the KB's state is already canonical).  Otherwise
    // ingest this text as a fresh file in the KB -- *unless* the
    // client has taken over KB membership via `sumo/setActiveFiles`,
    // in which case the file's inclusion is its decision alone.
    let already_loaded = {
        let kb = state.kb.read().expect("kb not poisoned");
        !kb.file_roots(&tag).is_empty()
    };
    let client_managed = state.client_manages_files.load(Ordering::SeqCst);
    if !already_loaded && !client_managed {
        let mut kb = state.kb.write().expect("kb not poisoned");
        let _ = kb.load_kif(&text, &tag, None);
    }

    let parsed = parse_document(tag.clone(), text.as_str());
    let rope   = Rope::from_str(&text);
    {
        let mut docs = state.docs.write().expect("docs not poisoned");
        let mut ds   = DocState::new(&text, version);
        ds.parsed    = Some(parsed.clone());
        docs.insert(uri.clone(), ds);
    }
    let kb = state.kb.read().expect("kb not poisoned");
    publish_diagnostics(&connection.sender, &uri, &rope, &parsed, state, &kb, Some(version));
}

// -- didChange ----------------------------------------------------------------

fn on_did_change(connection: &Connection, state: &GlobalState, params: DidChangeTextDocumentParams) {
    let uri     = params.text_document.uri;
    let version = params.text_document.version;
    let tag     = uri_to_tag(&uri);

    log::debug!(target: "sumo_lsp", "didChange '{}' v{}", tag, version);

    // MVP: full-document sync only (advertised in ServerCapabilities).
    // Each `content_changes` entry has no range, its `text` replaces
    // the full buffer.
    let new_text = match params.content_changes.last() {
        Some(change) => change.text.clone(),
        None         => return,
    };

    // Apply the incremental diff to the KB.
    let parsed = parse_document(tag.clone(), new_text.as_str());
    {
        let mut kb = state.kb.write().expect("kb not poisoned");
        let old_sids   = kb.file_roots(&tag).to_vec();
        let old_hashes = kb.file_hashes(&tag).to_vec();
        if old_sids.is_empty() {
            // Document wasn't loaded (e.g. didChange before didOpen
            // completed on a slow machine).  Fall back to a full load.
            let _ = kb.load_kif(&new_text, &tag, None);
        } else {
            let diff = sumo_kb::compute_file_diff(
                &tag, &old_sids, &old_hashes,
                &parsed.root_hashes, &parsed.ast, &parsed.root_spans,
            );
            let _ = kb.apply_file_diff(diff);
        }
    }

    // Replace the per-doc state in one go.
    let rope = Rope::from_str(&new_text);
    {
        let mut docs = state.docs.write().expect("docs not poisoned");
        let mut ds   = DocState::new(&new_text, version);
        ds.parsed    = Some(parsed.clone());
        docs.insert(uri.clone(), ds);
    }

    let kb = state.kb.read().expect("kb not poisoned");
    publish_diagnostics(&connection.sender, &uri, &rope, &parsed, state, &kb, Some(version));
}

// -- didClose -----------------------------------------------------------------

fn on_did_close(state: &GlobalState, params: DidCloseTextDocumentParams) {
    let uri = params.text_document.uri;
    log::debug!(target: "sumo_lsp", "didClose '{}'", uri_to_tag(&uri));
    let mut docs = state.docs.write().expect("docs not poisoned");
    docs.remove(&uri);
    // The KB intentionally retains the file's sentences so other open
    // documents that cross-reference them still resolve.  A separate
    // "drop workspace file" command can remove it explicitly if
    // required later.
}

// -- sumo/setActiveFiles ------------------------------------------------------

/// Client-owned KB membership control.  The extension sends the
/// authoritative file list for the union of its active KBs (one
/// or more, permanent and / or temporary).  We diff against the
/// currently-loaded set and apply the delta via the same load /
/// remove primitives the workspace sweep uses.  Diagnostics are
/// republished for every affected file so the client reconciles
/// its UI to the new state.
fn on_set_active_files(
    connection: &Connection,
    state:      &GlobalState,
    not:        Notification,
) -> Result<()> {
    use std::sync::atomic::Ordering;

    let params: SetActiveFilesParams =
        serde_json::from_value(not.params).map_err(|e| anyhow::anyhow!(e))?;

    // Flip the "client owns membership" latch before the first
    // application so subsequent didOpen calls don't race-add
    // files behind the client's back.
    state.client_manages_files.store(true, Ordering::SeqCst);

    let report = handle_set_active_files(state, params);

    // Republish diagnostics for added + removed files.  Removed
    // files that the client still has open get an empty diagnostic
    // list, clearing any leftover squiggles.
    let docs = state.docs.read().expect("docs lock not poisoned");
    let kb   = state.kb.read().expect("kb lock not poisoned");
    for tag in report.added.iter().chain(report.removed.iter()) {
        let Some(uri) = uri_from_tag(tag) else { continue; };
        let doc = docs.get(&uri);
        let rope = doc.map(|d| d.rope.clone())
            .unwrap_or_else(|| Rope::from_str(""));
        let parsed = doc.and_then(|d| d.parsed.as_ref());

        match parsed {
            Some(p) => publish_diagnostics(&connection.sender, &uri, &rope, p, state, &kb, None),
            None => {
                // No open document for this tag -- reparse from
                // disk on the fly so diagnostics reflect current
                // state.  Cheap for a one-off.
                if let Ok(text) = std::fs::read_to_string(tag) {
                    let p    = sumo_kb::parse_document(tag.clone(), text.as_str());
                    let rope = Rope::from_str(&text);
                    publish_diagnostics(&connection.sender, &uri, &rope, &p, state, &kb, None);
                }
            }
        }
    }

    Ok(())
}

/// Reverse of `uri_to_tag`: build a `file://` URL from a
/// filesystem-path tag.  Returns `None` on non-file tags (should
/// not happen in practice; setActiveFiles uses absolute paths).
fn uri_from_tag(tag: &str) -> Option<Url> {
    Url::from_file_path(tag).ok()
}

// -- sumo/setIgnoredDiagnostics ----------------------------------------------

/// Update the server's `ignored_diagnostic_codes` set from a
/// client notification and re-publish diagnostics for every
/// currently-open document so the change takes effect without a
/// restart.
fn on_set_ignored_diagnostics(
    connection: &Connection,
    state:      &GlobalState,
    not:        Notification,
) -> Result<()> {
    let params: SetIgnoredDiagnosticsParams =
        serde_json::from_value(not.params).map_err(|e| anyhow::anyhow!(e))?;

    handle_set_ignored_diagnostics(state, params);

    // Republish diagnostics for every open document.  Closed
    // documents don't need refreshing -- they have no visible
    // Problems-panel entry to update.
    let docs = state.docs.read().expect("docs lock not poisoned");
    let kb   = state.kb.read().expect("kb lock not poisoned");
    for (uri, doc) in docs.iter() {
        let rope = doc.rope.clone();
        if let Some(parsed) = doc.parsed.as_ref() {
            publish_diagnostics(&connection.sender, uri, &rope, parsed, state, &kb, Some(doc.version));
        }
    }
    Ok(())
}
