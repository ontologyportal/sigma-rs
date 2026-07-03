// crates/sumo-lsp/tests/phase4_handlers.rs
//
// End-to-end scripted coverage of the Phase 4 handlers:
// references, rename, and workspace/symbol.

use std::thread;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    notification::{DidOpenTextDocument, Initialized, Notification as _, PublishDiagnostics},
    request::{
        Initialize, References, Rename, Shutdown, WorkspaceSymbolRequest,
    },
    DidOpenTextDocumentParams, InitializeParams, InitializedParams,
    Location, PartialResultParams, Position, ReferenceContext, ReferenceParams,
    RenameParams, SymbolKind, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Url, WorkDoneProgressParams,
    WorkspaceSymbolParams, WorkspaceSymbolResponse,
};

// -- Helpers ------------------------------------------------------------------

fn spawn_server(connection: Connection) -> thread::JoinHandle<()> {
    thread::spawn(move || { let _ = sumo_lsp::server::run(connection); })
}

fn send_request<R: lsp_types::request::Request>(
    client: &Connection,
    id:     impl Into<RequestId>,
    params: R::Params,
) -> RequestId {
    let id: RequestId = id.into();
    let req = Request {
        id: id.clone(),
        method: R::METHOD.to_string(),
        params: serde_json::to_value(&params).expect("serialisable"),
    };
    client.sender.send(Message::Request(req)).expect("send");
    id
}

fn send_notification<N: lsp_types::notification::Notification>(
    client: &Connection,
    params: N::Params,
) {
    let not = Notification {
        method: N::METHOD.to_string(),
        params: serde_json::to_value(&params).expect("serialisable"),
    };
    client.sender.send(Message::Notification(not)).expect("send");
}

fn recv_response(client: &Connection) -> lsp_server::Response {
    loop {
        let m = client.receiver.recv_timeout(Duration::from_secs(5))
            .expect("response within 5s");
        if let Message::Response(r) = m { return r; }
    }
}

fn drain_publish_diagnostics_for(client: &Connection, uri: &Url) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        match client.receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(Message::Notification(not)) if not.method == PublishDiagnostics::METHOD => {
                if let Ok(p) = serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(not.params) {
                    if &p.uri == uri { return; }
                }
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
}

fn initialize(client: &Connection) {
    send_request::<Initialize>(client, 1, InitializeParams::default());
    let r = recv_response(client);
    assert!(r.error.is_none(), "initialize: {:?}", r.error);
    send_notification::<Initialized>(client, InitializedParams {});
}

fn shutdown(client: &Connection) {
    send_request::<Shutdown>(client, 999, ());
    recv_response(client);
    client.sender.send(Message::Notification(Notification {
        method: lsp_types::notification::Exit::METHOD.to_string(),
        params: serde_json::Value::Null,
    })).expect("exit");
}

fn open(client: &Connection, uri: &Url, text: &str) {
    send_notification::<DidOpenTextDocument>(client, DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri:         uri.clone(),
            language_id: "kif".to_string(),
            version:     1,
            text:        text.to_string(),
        },
    });
    drain_publish_diagnostics_for(client, uri);
}

fn references_at(
    client: &Connection,
    uri: &Url,
    line: u32, ch: u32,
    include_decl: bool,
    id: i32,
) -> Option<Vec<Location>> {
    send_request::<References>(client, id, ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position:      Position { line, character: ch },
        },
        context: ReferenceContext { include_declaration: include_decl },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params:     PartialResultParams::default(),
    });
    let r = recv_response(client);
    assert!(r.error.is_none(), "references: {:?}", r.error);
    serde_json::from_value(r.result.unwrap_or(serde_json::Value::Null)).ok()
}

fn rename_at(
    client: &Connection,
    uri:      &Url,
    line: u32, ch: u32,
    new_name: &str,
    id: i32,
) -> Option<lsp_types::WorkspaceEdit> {
    send_request::<Rename>(client, id, RenameParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position:      Position { line, character: ch },
        },
        new_name: new_name.to_string(),
        work_done_progress_params: WorkDoneProgressParams::default(),
    });
    let r = recv_response(client);
    assert!(r.error.is_none(), "rename: {:?}", r.error);
    serde_json::from_value(r.result.unwrap_or(serde_json::Value::Null)).ok()
}

fn workspace_symbols(client: &Connection, query: &str, id: i32) -> Option<WorkspaceSymbolResponse> {
    send_request::<WorkspaceSymbolRequest>(client, id, WorkspaceSymbolParams {
        query: query.to_string(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params:     PartialResultParams::default(),
    });
    let r = recv_response(client);
    assert!(r.error.is_none(), "workspaceSymbol: {:?}", r.error);
    serde_json::from_value(r.result.unwrap_or(serde_json::Value::Null)).ok()
}

// -- References ---------------------------------------------------------------

#[test]
fn references_returns_every_occurrence_of_symbol() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri  = Url::parse("file:///tmp/refs.kif").expect("url");
    //            0         1         2
    //            0123456789012345678901234567890
    let text = "(subclass Human Hominid)\n\
                (instance Fido Mammal)\n\
                (format EnglishLanguage Human \"%1 is human\")";
    open(&client, &uri, text);

    // Cursor on `Human` in the first line (col 10..15).
    let refs = references_at(&client, &uri, 0, 11, true, 10)
        .expect("references found");
    // Human appears at: line 0 col 10 (decl arg 1) and line 2 col 24.
    assert_eq!(refs.len(), 2, "got {:?}", refs);
    assert!(refs.iter().all(|l| l.uri == uri));
    let lines: Vec<u32> = refs.iter().map(|l| l.range.start.line).collect();
    assert!(lines.contains(&0));
    assert!(lines.contains(&2));

    shutdown(&client);
    server.join().expect("join");
}

#[test]
fn references_excludes_declaration_when_asked() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri  = Url::parse("file:///tmp/refs-nodecl.kif").expect("url");
    let text = "(subclass Human Hominid)\n\
                (format EnglishLanguage Human \"%1\")\n\
                (documentation Human EnglishLanguage \"hi\")";
    open(&client, &uri, text);

    let with_decl = references_at(&client, &uri, 0, 11, true, 11)
        .expect("with decl");
    let no_decl   = references_at(&client, &uri, 0, 11, false, 12)
        .expect("no decl");
    assert!(with_decl.len() > no_decl.len(),
        "expected fewer refs without declaration; got {} vs {}",
        with_decl.len(), no_decl.len());

    shutdown(&client);
    server.join().expect("join");
}

// -- Rename -------------------------------------------------------------------

#[test]
fn rename_symbol_replaces_every_occurrence() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri  = Url::parse("file:///tmp/rename.kif").expect("url");
    let text = "(subclass Human Hominid)\n\
                (instance Fido Mammal)\n\
                (format EnglishLanguage Human \"%1\")";
    open(&client, &uri, text);

    let edit = rename_at(&client, &uri, 0, 11, "Person", 20)
        .expect("rename produced an edit");
    let changes = edit.changes.expect("workspace edit has changes");
    let edits   = changes.get(&uri).expect("edits for this URI");
    // Two Human occurrences -> two text edits.
    assert_eq!(edits.len(), 2);
    assert!(edits.iter().all(|e| e.new_text == "Person"));

    shutdown(&client);
    server.join().expect("join");
}

#[test]
fn rename_variable_preserves_sigil() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri  = Url::parse("file:///tmp/varename.kif").expect("url");
    let text = "(forall (?X) (instance ?X Human))";
    open(&client, &uri, text);

    // Cursor on the first `?X` at col 9.
    let edit = rename_at(&client, &uri, 0, 9, "Y", 30)
        .expect("variable rename");
    let changes = edit.changes.expect("changes");
    let edits   = changes.get(&uri).expect("this URI");
    // Two `?X` occurrences in the sentence (quantifier binder + body).
    assert_eq!(edits.len(), 2);
    // All new_text values must start with `?`, even though the user
    // typed `Y`.
    for e in edits {
        assert!(e.new_text.starts_with('?'),
            "expected sigil-preserved rename, got '{}'", e.new_text);
        assert_eq!(e.new_text, "?Y");
    }

    shutdown(&client);
    server.join().expect("join");
}

#[test]
fn rename_variable_respects_scope() {
    // Two quantifier bodies with independent `?X` scopes.  Rename
    // from the first body must NOT touch the second body's `?X`.
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri = Url::parse("file:///tmp/varscope.kif").expect("url");
    let text = "(forall (?X) (P ?X))\n\
                (forall (?X) (Q ?X))";
    open(&client, &uri, text);

    // Rename the `?X` on line 0.
    let edit = rename_at(&client, &uri, 0, 9, "Y", 40)
        .expect("scoped rename");
    let changes = edit.changes.expect("changes");
    let edits   = changes.get(&uri).expect("this URI");
    // Only line-0 `?X`s (2 of them) should be touched.  Line-1 left alone.
    assert_eq!(edits.len(), 2, "got {:?}", edits);
    for e in edits {
        assert_eq!(e.range.start.line, 0,
            "scope leak: rename touched line {}", e.range.start.line);
    }

    shutdown(&client);
    server.join().expect("join");
}

// -- Workspace symbols -------------------------------------------------------

#[test]
fn workspace_symbols_filters_by_substring() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri  = Url::parse("file:///tmp/ws.kif").expect("url");
    let text = "(subclass Human Animal)\n\
                (subclass Hominid Primate)\n\
                (instance Fido Mammal)";
    open(&client, &uri, text);

    // Query "om" should match Hominid, Primate (case-insensitive, by
    // substring).  Mammal doesn't contain "om".
    let resp = workspace_symbols(&client, "om", 50).expect("workspace symbol response");
    let flat = match resp {
        WorkspaceSymbolResponse::Flat(v) => v,
        _ => panic!("expected Flat response"),
    };
    let names: Vec<&str> = flat.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"Hominid"), "missing Hominid in {:?}", names);
    assert!(!names.contains(&"Mammal"), "unexpected Mammal in {:?}", names);
    assert!(!names.contains(&"Fido"),   "unexpected Fido in {:?}", names);

    shutdown(&client);
    server.join().expect("join");
}

#[test]
fn workspace_symbols_empty_query_returns_everything_up_to_cap() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri  = Url::parse("file:///tmp/ws-all.kif").expect("url");
    let text = "(subclass A B)\n(subclass C D)\n(instance X Y)";
    open(&client, &uri, text);

    let resp = workspace_symbols(&client, "", 60).expect("response");
    let flat = match resp {
        WorkspaceSymbolResponse::Flat(v) => v,
        _ => panic!("Flat"),
    };
    // Every symbol with a defining sentence gets a slot.  Defining-
    // sentence heuristic covers subclass/instance arg 1: A, C, X.
    // Without a (subclass X _) or (instance X _) declaration, B/D/Y
    // are still included as heads of their own roots? -- no, `B` is
    // never a head; it's arg 2 of subclass.  So we expect at least
    // A, C, X.
    let names: Vec<&str> = flat.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"A"), "missing A in {:?}", names);
    assert!(names.contains(&"C"), "missing C in {:?}", names);
    assert!(names.contains(&"X"), "missing X in {:?}", names);

    shutdown(&client);
    server.join().expect("join");
}

#[test]
fn workspace_symbols_classifies_kinds() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri  = Url::parse("file:///tmp/ws-kinds.kif").expect("url");
    let text = "(subclass Human Animal)";
    open(&client, &uri, text);

    let resp = workspace_symbols(&client, "Human", 70).expect("response");
    let flat = match resp {
        WorkspaceSymbolResponse::Flat(v) => v,
        _ => panic!("Flat"),
    };
    let human = flat.iter().find(|s| s.name == "Human").expect("found Human");
    // Human is a (subclass _ _) -> treated as a class.
    assert_eq!(human.kind, SymbolKind::CLASS);

    shutdown(&client);
    server.join().expect("join");
}
