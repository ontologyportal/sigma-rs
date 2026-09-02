// crates/sumo-lsp/tests/phase5_handlers.rs
//
// Scripted LSP integration tests for Phase 5:
// semantic tokens, formatting, completion.

use std::thread;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    notification::{DidOpenTextDocument, Initialized, Notification as _, PublishDiagnostics},
    request::{
        Completion, Formatting, Initialize, RangeFormatting, SemanticTokensFullRequest, Shutdown,
    },
    CompletionContext, CompletionParams, CompletionResponse, DidOpenTextDocumentParams,
    DocumentFormattingParams, DocumentRangeFormattingParams, FormattingOptions, InitializeParams,
    InitializedParams, PartialResultParams, Position, Range, SemanticTokens, SemanticTokensParams,
    SemanticTokensResult, TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams,
    Url, WorkDoneProgressParams,
};

// -- Helpers (shared shape with earlier phase integration tests) -------------

fn spawn_server(connection: Connection) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let _ = sumo_lsp::server::run(connection);
    })
}

fn send_request<R: lsp_types::request::Request>(
    client: &Connection,
    id: impl Into<RequestId>,
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
    client
        .sender
        .send(Message::Notification(not))
        .expect("send");
}

fn recv_response(client: &Connection) -> lsp_server::Response {
    loop {
        let m = client
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("response within 5s");
        if let Message::Response(r) = m {
            return r;
        }
    }
}

fn drain_publish_diagnostics_for(client: &Connection, uri: &Url) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        match client.receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(Message::Notification(not)) if not.method == PublishDiagnostics::METHOD => {
                if let Ok(p) =
                    serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(not.params)
                {
                    if &p.uri == uri {
                        return;
                    }
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
    client
        .sender
        .send(Message::Notification(Notification {
            method: lsp_types::notification::Exit::METHOD.to_string(),
            params: serde_json::Value::Null,
        }))
        .expect("exit");
}

/// Full-document `didChange` — the realistic route into a mid-typing
/// (syntactically broken) buffer: the server refuses to load broken text
/// into the KB, so tests must open valid content first and then edit.
fn change(client: &Connection, uri: &Url, version: i32, text: &str) {
    send_notification::<lsp_types::notification::DidChangeTextDocument>(
        client,
        lsp_types::DidChangeTextDocumentParams {
            text_document: lsp_types::VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version,
            },
            content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.to_string(),
            }],
        },
    );
    drain_publish_diagnostics_for(client, uri);
}

fn open(client: &Connection, uri: &Url, text: &str) {
    send_notification::<DidOpenTextDocument>(
        client,
        DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "kif".to_string(),
                version: 1,
                text: text.to_string(),
            },
        },
    );
    drain_publish_diagnostics_for(client, uri);
}

// -- Semantic tokens ---------------------------------------------------------

fn semantic_tokens(client: &Connection, uri: &Url, id: i32) -> Option<SemanticTokens> {
    send_request::<SemanticTokensFullRequest>(
        client,
        id,
        SemanticTokensParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        },
    );
    let r = recv_response(client);
    assert!(r.error.is_none(), "semanticTokens: {:?}", r.error);
    match serde_json::from_value::<Option<SemanticTokensResult>>(
        r.result.unwrap_or(serde_json::Value::Null),
    )
    .ok()
    .flatten()
    {
        Some(SemanticTokensResult::Tokens(t)) => Some(t),
        _ => None,
    }
}

#[test]
fn semantic_tokens_emit_one_tuple_per_non_paren_token() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri = Url::parse("file:///tmp/st.kif").expect("url");
    let text = "(subclass Human Animal)";
    open(&client, &uri, text);

    let toks = semantic_tokens(&client, &uri, 10).expect("tokens");
    // Expect 3 tokens: subclass, Human, Animal (parens skipped).
    assert_eq!(toks.data.len(), 3);

    // First token starts at line 0, col 1 (after opening paren).
    assert_eq!(toks.data[0].delta_line, 0);
    assert_eq!(toks.data[0].delta_start, 1);
    assert_eq!(toks.data[0].length, "subclass".len() as u32);

    shutdown(&client);
    server.join().expect("join");
}

#[test]
fn semantic_tokens_operator_classified_as_keyword_index_zero() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri = Url::parse("file:///tmp/st-op.kif").expect("url");
    let text = "(=> (P) (Q))";
    open(&client, &uri, text);

    let toks = semantic_tokens(&client, &uri, 20).expect("tokens");
    // First token is `=>`.  In our legend, keyword is type-idx 0.
    assert_eq!(
        toks.data[0].token_type, 0,
        "operator should classify as keyword (idx 0), got {:?}",
        toks.data
    );

    shutdown(&client);
    server.join().expect("join");
}

// -- Formatting --------------------------------------------------------------

fn formatting(client: &Connection, uri: &Url, id: i32) -> Option<Vec<lsp_types::TextEdit>> {
    send_request::<Formatting>(
        client,
        id,
        DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            options: FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        },
    );
    let r = recv_response(client);
    assert!(r.error.is_none(), "formatting: {:?}", r.error);
    serde_json::from_value(r.result.unwrap_or(serde_json::Value::Null)).ok()
}

fn range_formatting(
    client: &Connection,
    uri: &Url,
    range: Range,
    id: i32,
) -> Option<Vec<lsp_types::TextEdit>> {
    send_request::<RangeFormatting>(
        client,
        id,
        DocumentRangeFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range,
            options: FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        },
    );
    let r = recv_response(client);
    assert!(r.error.is_none(), "rangeFormatting: {:?}", r.error);
    serde_json::from_value(r.result.unwrap_or(serde_json::Value::Null)).ok()
}

#[test]
fn formatting_clean_document_emits_single_replacement_edit() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri = Url::parse("file:///tmp/fmt.kif").expect("url");
    //                    messy whitespace:
    let text = "(subclass    Human    Animal)   \n\n(subclass Dog Mammal)";
    open(&client, &uri, text);

    let edits = formatting(&client, &uri, 30).expect("formatting edits");
    assert_eq!(edits.len(), 1, "should be one whole-document edit");
    let e = &edits[0];
    // Starts at (0,0); new text should contain both sentences in
    // pretty-printed form with a blank line between.
    assert_eq!(
        e.range.start,
        Position {
            line: 0,
            character: 0
        }
    );
    assert!(
        e.new_text.contains("(subclass Human Animal)"),
        "new text: {}",
        e.new_text
    );
    assert!(e.new_text.contains("(subclass Dog Mammal)"));
    // Two sentences joined by a blank line.
    assert!(e.new_text.contains("\n\n"), "expected blank line separator");

    shutdown(&client);
    server.join().expect("join");
}

#[test]
fn formatting_preserves_comments() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri = Url::parse("file:///tmp/fmt-comments.kif").expect("url");
    let text =
        "; header\n(subclass   Dog   Mammal) ; trailing\n\n; between\n\n(subclass Cat Mammal)";
    open(&client, &uri, text);

    let edits = formatting(&client, &uri, 32).expect("formatting edits");
    assert_eq!(edits.len(), 1);
    let new_text = &edits[0].new_text;
    // The round-trip formatter re-interleaves every comment: the header stays
    // above its sentence, the trailing comment stays on its line, and the
    // standalone block keeps its blank-line separation.
    assert!(new_text.starts_with("; header\n"), "got: {new_text}");
    assert!(
        new_text.contains("(subclass Dog Mammal) ; trailing"),
        "got: {new_text}"
    );
    assert!(new_text.contains("\n\n; between\n\n"), "got: {new_text}");

    shutdown(&client);
    server.join().expect("join");
}

#[test]
fn formatting_document_with_errors_returns_empty_edits() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri = Url::parse("file:///tmp/fmt-err.kif").expect("url");
    let text = "(subclass Human\n"; // unbalanced paren
    open(&client, &uri, text);

    let edits = formatting(&client, &uri, 31).expect("edits");
    assert!(
        edits.is_empty(),
        "malformed docs must not be formatted (would drop user input)"
    );

    shutdown(&client);
    server.join().expect("join");
}

#[test]
fn range_formatting_selects_sentences_in_range() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri = Url::parse("file:///tmp/fmt-range.kif").expect("url");
    //                    0         1         2
    //                    0123456789012345678901234
    let text = "(subclass Human Animal)\n(subclass Dog Animal)\n(subclass Cat Animal)";
    open(&client, &uri, text);

    // Range covers only line 1 (the Dog sentence).
    let range = Range {
        start: Position {
            line: 1,
            character: 0,
        },
        end: Position {
            line: 1,
            character: 21,
        },
    };
    let edits = range_formatting(&client, &uri, range, 40).expect("edits");
    assert_eq!(edits.len(), 1);
    let e = &edits[0];
    assert!(e.new_text.contains("(subclass Dog Animal)"));
    // Must NOT contain the cat sentence.
    assert!(!e.new_text.contains("Cat"), "range leak: {}", e.new_text);

    shutdown(&client);
    server.join().expect("join");
}

// -- Completion --------------------------------------------------------------

fn completion(
    client: &Connection,
    uri: &Url,
    line: u32,
    ch: u32,
    id: i32,
) -> Option<CompletionResponse> {
    send_request::<Completion>(
        client,
        id,
        CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: Position {
                    line,
                    character: ch,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: Some(CompletionContext {
                trigger_kind: lsp_types::CompletionTriggerKind::INVOKED,
                trigger_character: None,
            }),
        },
    );
    let r = recv_response(client);
    assert!(r.error.is_none(), "completion: {:?}", r.error);
    serde_json::from_value(r.result.unwrap_or(serde_json::Value::Null)).ok()
}

#[test]
fn completion_after_open_paren_offers_operators_and_heads() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri = Url::parse("file:///tmp/comp.kif").expect("url");
    // Seed the KB with a few sentences so head_names() has content to
    // offer, then TYPE the `(` via didChange -- an open buffer with a
    // dangling paren never loads (the mid-typing guard), so the seed must
    // land from a syntactically-valid open first.  `subclass`/`instance`
    // need their own `Predicate` declaration (as real SUMO content
    // has) for `is_predicate` to recognize them as formula heads --
    // top-level completion only offers predicates, since a well-formed KIF
    // assertion's head always is one.
    let text = "(instance subclass Predicate)\n(instance instance Predicate)\n(subclass Human Animal)\n(instance Fido Dog)\n";
    open(&client, &uri, text);
    change(
        &client,
        &uri,
        4,
        "(instance subclass Predicate)\n(instance instance Predicate)\n(subclass Human Animal)\n(instance Fido Dog)\n(",
    );

    // Cursor on line 4, just after `(`, nothing typed yet: operators only --
    // no relation-name enumeration until the user starts typing (search's
    // empty-query-and-no-taxonomy guard naturally suppresses it).
    let resp = completion(&client, &uri, 4, 1, 50).expect("completion response");
    let items = match resp {
        CompletionResponse::Array(v) => v,
        CompletionResponse::List(list) => list.items,
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();

    assert!(labels.contains(&"and"), "missing 'and' in {:?}", labels);
    assert!(
        labels.contains(&"forall"),
        "missing 'forall' in {:?}",
        labels
    );
    assert!(
        !labels.contains(&"subclass"),
        "no relation names before typing starts, got {:?}",
        labels
    );

    // Typing "s" shunts to the search path: relation names matching the
    // prefix now appear.
    change(
        &client,
        &uri,
        5,
        "(instance subclass Predicate)\n(instance instance Predicate)\n(subclass Human Animal)\n(instance Fido Dog)\n(s",
    );
    let resp = completion(&client, &uri, 4, 2, 50).expect("completion response");
    let items = match resp {
        CompletionResponse::Array(v) => v,
        CompletionResponse::List(list) => list.items,
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"subclass"),
        "missing 'subclass' in {:?}",
        labels
    );

    shutdown(&client);
    server.join().expect("join");
}

#[test]
fn completion_at_top_level_is_empty() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri = Url::parse("file:///tmp/comp-top.kif").expect("url");
    let text = "(subclass Human Animal)\n";
    open(&client, &uri, text);

    // Cursor on line 1 (empty), column 0.
    let resp = completion(&client, &uri, 1, 0, 60).expect("response");
    let items = match resp {
        CompletionResponse::Array(v) => v,
        CompletionResponse::List(list) => list.items,
    };
    assert!(
        items.is_empty(),
        "top-level completion should be empty, got {} items",
        items.len()
    );

    shutdown(&client);
    server.join().expect("join");
}

#[test]
fn completion_inside_arg_position_returns_nonempty() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri = Url::parse("file:///tmp/comp-arg.kif").expect("url");
    // Valid open seeds the KB; the mid-typing `(subclass H` arrives by edit
    // (a broken buffer never loads -- see the guard in `load_buffer`). A
    // character is typed (not just `(subclass `) because arg-position
    // completion with nothing typed and no expressible taxonomy constraint
    // now returns nothing -- see `suggest_args`'s "nothing until typing"
    // rule -- so this exercises the typed-prefix search path instead.
    let text = "(subclass Human Animal)\n(instance Fido Dog)\n";
    open(&client, &uri, text);
    change(
        &client,
        &uri,
        2,
        "(subclass Human Animal)\n(instance Fido Dog)\n(subclass H",
    );

    // Cursor right after `(subclass H` on line 2.
    let resp = completion(&client, &uri, 2, 11, 70).expect("response");
    let items = match resp {
        CompletionResponse::Array(v) => v,
        CompletionResponse::List(list) => list.items,
    };
    assert!(
        !items.is_empty(),
        "arg-position completion should surface symbols"
    );
    // Symbols from prior sentences should be present somewhere.
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"Human")
            || labels.contains(&"Animal")
            || labels.contains(&"Fido")
            || labels.contains(&"Dog"),
        "expected at least one interned symbol in {:?}",
        labels
    );

    shutdown(&client);
    server.join().expect("join");
}

#[test]
fn completion_for_class_domain_includes_classes_reached_via_narrower_subclass() {
    let (server_conn, client) = Connection::memory();
    let server = spawn_server(server_conn);

    initialize(&client);

    let uri = Url::parse("file:///tmp/comp-class-domain.kif").expect("url");
    // `Elephant` is a class only via `SetOrClass` -- never declared
    // `(instance Elephant Class)` directly. `takesAClass`'s domain is the
    // bare `Class` symbol (SUMO's implicit superclass of every
    // class-denoting symbol), which must still offer `Elephant`:
    // `instances_of("Class")` alone would miss it. A prefix is typed
    // ("El") because the bare-`Class` domain can't be expressed as a search
    // taxonomy constraint (see `suggest_args`'s `class_type_fallback`), so
    // an empty prefix now returns nothing -- this exercises the typed-prefix
    // path, where the `is_class` post-filter still does its job.
    let text = "(instance takesAClass BinaryPredicate)\n\
                (domain takesAClass 1 Class)\n\
                (subclass SetOrClass Class)\n\
                (instance Elephant SetOrClass)\n";
    open(&client, &uri, text);
    change(
        &client,
        &uri,
        2,
        "(instance takesAClass BinaryPredicate)\n\
         (domain takesAClass 1 Class)\n\
         (subclass SetOrClass Class)\n\
         (instance Elephant SetOrClass)\n\
         (takesAClass El",
    );

    // Cursor right after `(takesAClass El` on line 4 (0-based).
    let resp = completion(&client, &uri, 4, 15, 70).expect("response");
    let items = match resp {
        CompletionResponse::Array(v) => v,
        CompletionResponse::List(list) => list.items,
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"Elephant"),
        "Class-domain completion should include a class reached only via a narrower subclass; got {:?}",
        labels
    );

    shutdown(&client);
    server.join().expect("join");
}
