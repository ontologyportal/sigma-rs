// crates/sumo-lsp/tests/init_flow.rs
//
// End-to-end scripted LSP exchanges against an in-process
// `lsp-server::Connection`.  Verifies the basic message loop:
// initialize → initialized → didOpen → publishDiagnostics →
// shutdown → exit.
//
// Uses `Connection::memory()` (paired senders/receivers) so no
// subprocess or stdio is involved -- the test drives both sides of
// the connection from the same Rust process.

use std::thread;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    notification::{DidChangeTextDocument, DidOpenTextDocument, Initialized,
                   Notification as _, PublishDiagnostics},
    request::{Initialize, Shutdown},
    DidChangeTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, InitializedParams,
    PublishDiagnosticsParams, TextDocumentContentChangeEvent,
    TextDocumentItem, Url,
    VersionedTextDocumentIdentifier, WorkspaceFolder,
};

// -- Helpers ------------------------------------------------------------------

fn spawn_server(connection: Connection) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // Server errors are reported via the `Result`; for tests we
        // swallow them (the test's assertions tell us if something
        // broke structurally).
        let _ = sumo_lsp::server::run(connection);
    })
}

fn send_request<R: lsp_types::request::Request>(
    client: &Connection,
    id:      impl Into<RequestId>,
    params:  R::Params,
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
        match m {
            Message::Response(r) => return r,
            other => {
                log::debug!("ignored message while awaiting response: {:?}", other);
                // Loop -- the server may emit notifications (e.g.
                // publishDiagnostics) before our response arrives.
            }
        }
    }
}

fn recv_publish_diagnostics(client: &Connection) -> PublishDiagnosticsParams {
    loop {
        let m = client.receiver.recv_timeout(Duration::from_secs(5))
            .expect("publishDiagnostics within 5s");
        if let Message::Notification(not) = m {
            if not.method == PublishDiagnostics::METHOD {
                return serde_json::from_value(not.params)
                    .expect("publishDiagnostics params parseable");
            }
        }
    }
}

fn initialize(client: &Connection, root_dir: Option<&std::path::Path>) {
    let workspace_folders = root_dir.map(|d| vec![WorkspaceFolder {
        uri:  Url::from_file_path(d).expect("dir URL"),
        name: "test".to_string(),
    }]);

    let params = InitializeParams {
        workspace_folders,
        ..Default::default()
    };
    send_request::<Initialize>(client, 1, params);
    let r = recv_response(client);
    assert!(r.error.is_none(), "initialize returned error: {:?}", r.error);
    // After initialize we must send `initialized` before any other
    // message.
    send_notification::<Initialized>(client, InitializedParams {});
}

fn shutdown(client: &Connection) {
    send_request::<Shutdown>(client, 999, ());
    let r = recv_response(client);
    assert!(r.error.is_none(), "shutdown returned error: {:?}", r.error);
    client.sender.send(Message::Notification(Notification {
        method: lsp_types::notification::Exit::METHOD.to_string(),
        params: serde_json::Value::Null,
    })).expect("exit notification sent");
}

// -- Tests --------------------------------------------------------------------

#[test]
fn initialize_shutdown_round_trip() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client, None);
    shutdown(&client);

    server.join().expect("server thread joins");
}

#[test]
fn did_open_publishes_diagnostics_for_malformed_kif() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client, None);

    // Send didOpen with a file that has both a valid sentence and
    // a malformed one.  The LSP should publish a diagnostic for
    // the malformed sentence while preserving the valid one.
    let uri = Url::parse("file:///tmp/test.kif").expect("url");
    let text = "(\n(subclass Human Animal)".to_string();
    send_notification::<DidOpenTextDocument>(&client, DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri:         uri.clone(),
            language_id: "kif".to_string(),
            version:     1,
            text,
        },
    });

    let diag = recv_publish_diagnostics(&client);
    assert_eq!(diag.uri, uri);
    assert!(
        !diag.diagnostics.is_empty(),
        "expected at least one diagnostic for malformed KIF"
    );
    // At least one diagnostic must be a parse/* code.
    assert!(
        diag.diagnostics.iter().any(|d| matches!(
            &d.code,
            Some(lsp_types::NumberOrString::String(s)) if s.starts_with("parse/")
        )),
        "expected a parse/* code in {:?}",
        diag.diagnostics.iter().map(|d| &d.code).collect::<Vec<_>>()
    );

    shutdown(&client);
    server.join().expect("server thread joins");
}

#[test]
fn did_open_then_did_change_updates_diagnostics() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client, None);

    let uri = Url::parse("file:///tmp/evolving.kif").expect("url");

    // Start with a valid file.  Expect no parse diagnostics.
    send_notification::<DidOpenTextDocument>(&client, DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri:         uri.clone(),
            language_id: "kif".to_string(),
            version:     1,
            text:        "(subclass Human Animal)".to_string(),
        },
    });
    let diag1 = recv_publish_diagnostics(&client);
    assert_eq!(diag1.version, Some(1));
    assert!(
        diag1.diagnostics.iter().all(|d| {
            !matches!(&d.code, Some(lsp_types::NumberOrString::String(s)) if s.starts_with("parse/"))
        }),
        "clean KIF should not produce parse/* diagnostics"
    );

    // didChange: introduce a syntax error.  Expect a parse
    // diagnostic on version 2.
    send_notification::<DidChangeTextDocument>(&client, DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: uri.clone(),
            version: 2,
        },
        content_changes: vec![TextDocumentContentChangeEvent {
            range:        None,
            range_length: None,
            text:         "(subclass Human\n".to_string(),
        }],
    });
    let diag2 = recv_publish_diagnostics(&client);
    assert_eq!(diag2.version, Some(2));
    assert!(
        diag2.diagnostics.iter().any(|d| matches!(
            &d.code,
            Some(lsp_types::NumberOrString::String(s)) if s.starts_with("parse/")
        )),
        "expected parse/* diagnostic after broken didChange, got: {:?}",
        diag2.diagnostics
    );

    shutdown(&client);
    server.join().expect("server thread joins");
}

#[test]
fn workspace_sweep_loads_directory_kifs() {
    // Create a temp dir with two .kif files; confirm the server
    // publishes diagnostics for both during initialize.
    let dir = tempdir_with(&[
        ("a.kif", "(subclass Human Animal)"),
        ("b.kif", "(subclass Dog Animal)"),
    ]);

    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client, Some(dir.path()));

    // Collect diagnostics for both files (initial sweep publishes
    // one per file).  We don't care about ordering -- just that
    // two arrive within timeout.
    let first  = recv_publish_diagnostics(&client);
    let second = recv_publish_diagnostics(&client);

    let uris = [first.uri.clone(), second.uri.clone()];
    assert!(uris.iter().any(|u| u.path().ends_with("a.kif")));
    assert!(uris.iter().any(|u| u.path().ends_with("b.kif")));

    shutdown(&client);
    server.join().expect("server thread joins");

    // Keep `dir` alive until after shutdown.
    drop(dir);
}

// -- tiny tempdir helper (no external crate) ---------------------------------

struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn path(&self) -> &std::path::Path { &self.path }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn tempdir_with(files: &[(&str, &str)]) -> TempDir {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path  = std::env::temp_dir().join(format!("sumo-lsp-test-{}", nanos));
    std::fs::create_dir_all(&path).expect("mkdir tempdir");
    for (name, content) in files {
        std::fs::write(path.join(name), content).expect("write file");
    }
    TempDir { path }
}
