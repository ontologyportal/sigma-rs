use wasm_bindgen::prelude::*;

use crate::NativeStack;
use crate::Session;

// -- WasmLsp ------------------------------------------------------------------

/// A language-server facade over the SAME knowledge base as a
/// [`Session`] -- construct it from one and every document loaded,
/// edited, or removed through either side is visible to both.
///
/// Speaks LSP JSON-RPC message *bodies* (plain JSON, no `Content-Length`
/// framing): feed each client->server message string to
/// [`handleMessage`](Self::handle_message) and forward every returned string
/// back to the client.  This is `sumo-lsp`'s transport-free dispatch core
/// (`sumo_lsp::server::dispatch_message`) -- the same handlers the native
/// stdio server runs -- driven synchronously from JS.
///
/// Lifecycle: `initialize` and `shutdown` are answered here (there is no
/// lsp-server `Connection` shell in wasm); `initialized` and `exit` are
/// no-ops.
#[wasm_bindgen]
pub struct WasmLsp {
    state: sumo_lsp::state::GlobalState<NativeStack>,
}

#[wasm_bindgen]
impl WasmLsp {
    /// Build the LSP facade sharing `prover`'s knowledge base.
    #[wasm_bindgen(constructor)]
    pub fn new(prover: &Session) -> WasmLsp {
        WasmLsp {
            state: sumo_lsp::state::GlobalState::with_session(prover.session.clone()),
        }
    }

    /// Dispatch one client->server LSP message (a JSON-RPC body as a JSON
    /// string) and return the server->client messages it produced, each as a
    /// JSON string -- the response first (for requests), then any
    /// notifications (`textDocument/publishDiagnostics`).
    #[wasm_bindgen(js_name = handleMessage)]
    pub fn handle_message(&self, json: &str) -> Result<Vec<String>, JsValue> {
        let msg: lsp_server::Message = serde_json::from_str(json)
            .map_err(|e| JsValue::from_str(&format!("malformed LSP message: {e}")))?;

        // Lifecycle handling the stdio shell's `Connection` would otherwise
        // own.  `initialized` (a notification) and `exit` fall through to
        // dispatch, which ignores unknown notifications.
        if let lsp_server::Message::Request(req) = &msg {
            let lifecycle = match req.method.as_str() {
                "initialize" => {
                    if req
                        .params
                        .get("initializationOptions")
                        .and_then(|v| v.get("clientManagesFiles"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        self.state
                            .client_manages_files
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    Some(lsp_server::Response::new_ok(
                        req.id.clone(),
                        sumo_lsp::server::initialize_result(),
                    ))
                }
                "shutdown" => Some(lsp_server::Response::new_ok(
                    req.id.clone(),
                    serde_json::Value::Null,
                )),
                _ => None,
            };
            if let Some(resp) = lifecycle {
                let out = lsp_server::Message::Response(resp);
                return Ok(vec![serde_json::to_string(&out).expect("serialisable")]);
            }
        }

        Ok(sumo_lsp::server::dispatch_message(&self.state, msg)
            .into_iter()
            .map(|m| serde_json::to_string(&m).expect("serialisable"))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These run natively (the rlib target): they exercise only the
    // JsValue-free surface -- construction, the shared session, and the
    // WasmLsp JSON bridge.

    #[test]
    fn lsp_and_prover_share_one_kb() {
        let prover = Session::new();
        let lsp = WasmLsp::new(&prover);

        // Load a document through the LSP facade...
        let did_open = serde_json::json!({
            "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": "file:///shared.kif", "languageId": "kif", "version": 1,
                "text": "(subclass Dog Mammal)\n(documentation Dog EnglishLanguage \"A dog.\")\n",
            }}
        });
        let out = lsp
            .handle_message(&did_open.to_string())
            .expect("didOpen dispatches");
        assert!(
            out.iter().any(|m| m.contains("publishDiagnostics")),
            "didOpen publishes diagnostics"
        );

        // ...and observe it through the prover's session: same KB object.
        let session = prover.session.read().expect("kb lock not poisoned");
        assert!(
            session
                .kb()
                .iter_files()
                .iter()
                .any(|f| f.contains("shared.kif")),
            "file loaded via LSP is visible to the prover facade"
        );
        assert!(
            session.manpage("Dog").is_some(),
            "symbol loaded via LSP resolves through SDK introspection"
        );
    }

    #[test]
    fn lsp_tptp_preview_requests_round_trip() {
        let prover = Session::new();
        let lsp = WasmLsp::new(&prover);

        let did_open = serde_json::json!({
            "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": "file:///preview.kif", "languageId": "kif", "version": 1,
                "text": "(subclass Dog Mammal)\n",
            }}
        });
        lsp.handle_message(&did_open.to_string()).expect("didOpen");

        let export = serde_json::json!({ "id": 10, "method": "sumo/toTptp", "params": {} });
        let out = lsp.handle_message(&export.to_string()).expect("export");
        assert!(out[0].contains("fof("), "export returns rendered TPTP");

        let lookup = serde_json::json!({
            "id": 11, "method": "sumo/tptpLineForPosition",
            "params": {
                "textDocument": { "uri": "file:///preview.kif" },
                "position": { "line": 0, "character": 5 },
            }
        });
        let out = lsp.handle_message(&lookup.to_string()).expect("lookup");
        let resp: serde_json::Value = serde_json::from_str(&out[0]).expect("json");
        assert!(
            resp["result"]["line"].is_u64(),
            "cursor resolves to an export line: {resp}"
        );
    }

    #[test]
    fn lsp_lifecycle_answered_inline() {
        let prover = Session::new();
        let lsp = WasmLsp::new(&prover);

        let init = serde_json::json!({
            "id": 1, "method": "initialize",
            "params": { "capabilities": {} }
        });
        let out = lsp.handle_message(&init.to_string()).expect("initialize");
        assert_eq!(out.len(), 1);
        assert!(
            out[0].contains("\"sumo-lsp\""),
            "handshake carries server info"
        );

        let shutdown = serde_json::json!({ "id": 2, "method": "shutdown" });
        let out = lsp.handle_message(&shutdown.to_string()).expect("shutdown");
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("\"id\":2"));
    }
}
