// crates/sumo-lsp/tests/phase3_handlers.rs
//
// End-to-end Phase-3 handler coverage against an in-process
// `Connection::memory()` pair: hover, goto-definition,
// documentSymbol.  Shares the test-driver helpers with the Phase-2
// init_flow suite via duplication (we keep test helpers local to
// each integration-test binary to avoid cross-test coupling).

use std::thread;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    notification::{DidOpenTextDocument, Initialized, Notification as _, PublishDiagnostics},
    request::{DocumentSymbolRequest, GotoDefinition, HoverRequest,
              Initialize, Shutdown},
    DidOpenTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
    InitializeParams, InitializedParams, MarkupContent, MarkupKind, PartialResultParams,
    Position, TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams,
    Url, WorkDoneProgressParams,
};

// -- Helpers ------------------------------------------------------------------

fn spawn_server(connection: Connection) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let _ = sumo_lsp::server::run(connection);
    })
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
    client.sender.send(Message::Request(req)).expect("request sent");
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
    client.sender.send(Message::Notification(not)).expect("notification sent");
}

fn recv_response(client: &Connection) -> lsp_server::Response {
    loop {
        let m = client.receiver.recv_timeout(Duration::from_secs(5))
            .expect("response within 5s");
        if let Message::Response(r) = m { return r; }
    }
}

fn drain_publish_diagnostics_for(client: &Connection, uri: &Url) {
    // Drain notifications until we see publishDiagnostics for `uri`.
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
    assert!(r.error.is_none(), "initialize error: {:?}", r.error);
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

// -- Tests --------------------------------------------------------------------

/// Send a hover request at `(line, character)` and return the response body.
fn hover_at(client: &Connection, uri: &Url, line: u32, ch: u32, id: i32) -> Option<Hover> {
    send_request::<HoverRequest>(client, id, HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position:      Position { line, character: ch },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    });
    let r = recv_response(client);
    assert!(r.error.is_none(), "hover error: {:?}", r.error);
    serde_json::from_value::<Option<Hover>>(r.result.unwrap_or(serde_json::Value::Null)).ok().flatten()
}

fn goto_at(client: &Connection, uri: &Url, line: u32, ch: u32, id: i32) -> Option<GotoDefinitionResponse> {
    send_request::<GotoDefinition>(client, id, GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position:      Position { line, character: ch },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params:     PartialResultParams::default(),
    });
    let r = recv_response(client);
    assert!(r.error.is_none(), "goto error: {:?}", r.error);
    serde_json::from_value::<Option<GotoDefinitionResponse>>(r.result.unwrap_or(serde_json::Value::Null))
        .ok().flatten()
}

fn document_symbols(client: &Connection, uri: &Url, id: i32) -> Option<DocumentSymbolResponse> {
    send_request::<DocumentSymbolRequest>(client, id, DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params:     PartialResultParams::default(),
    });
    let r = recv_response(client);
    assert!(r.error.is_none(), "documentSymbol error: {:?}", r.error);
    serde_json::from_value::<Option<DocumentSymbolResponse>>(r.result.unwrap_or(serde_json::Value::Null))
        .ok().flatten()
}

#[test]
fn hover_on_symbol_returns_manpage_markdown() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri  = Url::parse("file:///tmp/hover.kif").expect("url");
    // (subclass Human Animal) -- `Human` starts at byte 10.
    // On line 0 in KIF, byte == UTF-16 column for ASCII-only files.
    let text = "(subclass Human Animal)\n\
                (documentation Human EnglishLanguage \"A &%Human being.\")";
    open(&client, &uri, text);

    // Hover over `Human` in the first sentence.
    let hover = hover_at(&client, &uri, 0, 12, 10)
        .expect("hover result at (0, 12) where `Human` lives");
    match hover.contents {
        HoverContents::Markup(MarkupContent { kind, value }) => {
            assert_eq!(kind, MarkupKind::Markdown);
            assert!(value.contains("### Human"), "markdown missing heading: {}", value);
            assert!(value.contains("Human being"),
                "markdown missing documentation text: {}", value);
        }
        other => panic!("expected markup hover contents, got {:?}", other),
    }

    shutdown(&client);
    server.join().expect("server joins");
}

#[test]
fn hover_outside_symbol_returns_null() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri  = Url::parse("file:///tmp/hover2.kif").expect("url");
    let text = "(subclass Human Animal)";
    open(&client, &uri, text);

    // Position far past the closing paren -- no element there.
    let hover = hover_at(&client, &uri, 0, 100, 11);
    assert!(hover.is_none(), "expected null hover, got {:?}", hover);

    shutdown(&client);
    server.join().expect("server joins");
}

#[test]
fn goto_definition_jumps_to_defining_subclass_sentence() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri  = Url::parse("file:///tmp/goto.kif").expect("url");
    // Human is used in sentence 2 but defined (via subclass) in
    // sentence 1.  Jump from sentence 2 should land at sentence 1.
    let text = "(subclass Human Hominid)\n\
                (instance Fido Mammal)\n\
                (format EnglishLanguage Human \"%1 is human\")";
    open(&client, &uri, text);

    // `Human` on line 2 is at column 24 (after "(format EnglishLanguage ").
    let response = goto_at(&client, &uri, 2, 24, 20)
        .expect("goto should resolve Human");
    match response {
        GotoDefinitionResponse::Scalar(loc) => {
            assert_eq!(loc.uri, uri);
            // Defining sentence is line 0 (first subclass).
            assert_eq!(loc.range.start.line, 0);
        }
        other => panic!("expected Scalar location, got {:?}", other),
    }

    shutdown(&client);
    server.join().expect("server joins");
}

#[test]
fn document_symbols_list_each_root_sentence() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri  = Url::parse("file:///tmp/symbols.kif").expect("url");
    let text = "(subclass Human Animal)\n\
                (subclass Dog Animal)\n\
                (instance Fido Dog)";
    open(&client, &uri, text);

    let response = document_symbols(&client, &uri, 30)
        .expect("documentSymbol response");
    match response {
        DocumentSymbolResponse::Nested(symbols) => {
            assert_eq!(symbols.len(), 3, "expected one entry per root sentence");
            assert_eq!(symbols[0].name, "subclass");
            assert_eq!(symbols[1].name, "subclass");
            assert_eq!(symbols[2].name, "instance");
            // Each symbol's range should start on a distinct line.
            assert_eq!(symbols[0].range.start.line, 0);
            assert_eq!(symbols[1].range.start.line, 1);
            assert_eq!(symbols[2].range.start.line, 2);
        }
        other => panic!("expected nested document symbols, got {:?}", other),
    }

    shutdown(&client);
    server.join().expect("server joins");
}

#[test]
fn document_symbols_on_empty_file_returns_empty_list() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri = Url::parse("file:///tmp/empty.kif").expect("url");
    open(&client, &uri, "");

    let response = document_symbols(&client, &uri, 40)
        .expect("documentSymbol response");
    // An empty list serialises ambiguously between Nested([]) and
    // Flat([]) on the wire; accept either.
    match response {
        DocumentSymbolResponse::Nested(symbols) => assert!(symbols.is_empty()),
        DocumentSymbolResponse::Flat(symbols)   => assert!(symbols.is_empty()),
    }

    shutdown(&client);
    server.join().expect("server joins");
}
