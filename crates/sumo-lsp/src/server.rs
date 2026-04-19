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
    request::{DocumentSymbolRequest, GotoDefinition, HoverRequest, Request as _},
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, InitializeResult, OneOf, PositionEncodingKind, ServerCapabilities,
    ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    Url, WorkspaceFolder,
};
use ropey::Rope;
use serde::de::DeserializeOwned;

use sumo_kb::parse_document;

use crate::conv::uri_to_tag;
use crate::handlers::{
    handle_document_symbol, handle_goto_definition, handle_hover, publish_diagnostics,
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

    // Best-effort workspace index: load every `.kif` / `.kif.tq`
    // under each workspaceFolder into the shared KB, then publish
    // a first-pass diagnostics sweep for each.  Failures are logged
    // and ignored -- missing perms or a non-file root shouldn't
    // kill the server.
    initial_workspace_sweep(&connection, &state, &init_params);

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
        definition_provider:      Some(OneOf::Left(true)),
        hover_provider:           Some(lsp_types::HoverProviderCapability::Simple(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        // Phase 4+ capabilities: references, rename, completion,
        // semantic tokens -- flipped on as the handlers land.
        references_provider:      None,
        rename_provider:          None,
        completion_provider:      None,
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
                // Load into the shared KB.  Errors get published as
                // diagnostics along with the parse pass.
                {
                    let mut kb = state.kb.write().expect("kb not poisoned");
                    let _ = kb.load_kif(&text, &tag, None);
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
                publish_diagnostics(&connection.sender, &uri, &rope, &parsed, &kb, None);
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
    let uri      = params.text_document.uri;
    let text     = params.text_document.text;
    let version  = params.text_document.version;
    let tag      = uri_to_tag(&uri);

    log::debug!(target: "sumo_lsp", "didOpen '{}' v{}", tag, version);

    // If the workspace sweep already loaded this file, skip the
    // re-load (the KB's state is already canonical).  Otherwise
    // ingest this text as a fresh file in the KB.
    let already_loaded = {
        let kb = state.kb.read().expect("kb not poisoned");
        !kb.file_roots(&tag).is_empty()
    };
    if !already_loaded {
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
    publish_diagnostics(&connection.sender, &uri, &rope, &parsed, &kb, Some(version));
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
    publish_diagnostics(&connection.sender, &uri, &rope, &parsed, &kb, Some(version));
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
