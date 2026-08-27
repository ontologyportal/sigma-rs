// crates/wasm/src/session/ingest.rs
//
// Loading KIF into the KB and managing session assertions -- the wasm face
// of the SDK's `session/ingest.rs` (fetch-based `Source` handling lives in
// the JS facade, which hands the fetched text down to these).

use wasm_bindgen::prelude::*;

use super::Session;

#[wasm_bindgen]
impl Session {
    /// Load KIF text into the KB under `file_tag` as **axioms**.
    ///
    /// The native prover searches over a promoted axiom base, so this loads the
    /// text and then promotes it into the axiomatic theory
    /// (`make_session_axiomatic`) -- the loaded KIF becomes background theory
    /// every subsequent [`ask`](Session::ask) sees.
    ///
    /// Returns a JSON array of error strings, or an empty array on success.
    #[wasm_bindgen(js_name = loadKif)]
    pub fn load_kif(&mut self, kif_text: &str, file_tag: &str) -> Result<JsValue, JsValue> {
        let mut session_guard = self.session.write().expect("kb lock not poisoned");
        let inner = session_guard.kb_mut();
        let result = inner.load(
            sigmakee_rs_core::SourceFile::kif(
                std::path::PathBuf::from(file_tag),
                kif_text.to_string(),
            ),
            file_tag,
        );
        let mut errors: Vec<String> = result
            .diagnostics
            .iter()
            .map(|e: &sigmakee_rs_core::Diagnostic| e.to_string())
            .collect();
        // Promote the freshly-loaded source into the searchable axiom base.
        // Skipping this leaves the axioms as inert session support the
        // given-clause loop never force-includes, so queries come back
        // Disproved/Unknown against an effectively empty theory.
        if let Err(e) = inner.make_session_axiomatic(file_tag) {
            errors.push(format!("promote failed: {:?}", e));
        }
        serde_wasm_bindgen::to_value(&errors).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Load KIF under `file_tag` WITHOUT promoting it to axioms.
    ///
    /// Enables search / man pages / editing immediately (they read the ingested
    /// store); proving and the full man-page taxonomy require a later
    /// [`promote`](Session::promote). Returns parse-error strings ([] on success).
    #[wasm_bindgen(js_name = ingest)]
    pub fn ingest(&mut self, kif_text: &str, file_tag: &str) -> Result<JsValue, JsValue> {
        let mut session_guard = self.session.write().expect("kb lock not poisoned");
        let inner = session_guard.kb_mut();
        let result = inner.load(
            sigmakee_rs_core::SourceFile::kif(
                std::path::PathBuf::from(file_tag),
                kif_text.to_string(),
            ),
            file_tag,
        );
        let errors: Vec<String> = result
            .diagnostics
            .iter()
            .map(|e: &sigmakee_rs_core::Diagnostic| e.to_string())
            .collect();
        serde_wasm_bindgen::to_value(&errors).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Promote a previously-[`ingest`](Session::ingest)ed source into the axiom
    /// base (`make_session_axiomatic`) -- the deferred, heavier step that enables
    /// proving. Returns error strings ([] on success).
    #[wasm_bindgen(js_name = promote)]
    pub fn promote(&mut self, file_tag: &str) -> Result<JsValue, JsValue> {
        let mut session_guard = self.session.write().expect("kb lock not poisoned");
        let inner = session_guard.kb_mut();
        let mut errors: Vec<String> = Vec::new();
        if let Err(e) = inner.make_session_axiomatic(file_tag) {
            errors.push(format!("promote failed: {:?}", e));
        }
        serde_wasm_bindgen::to_value(&errors).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Remove assertions for a specific session only.
    #[wasm_bindgen(js_name = flushSession)]
    pub fn flush_session(&mut self, session: &str) {
        let mut session_guard = self.session.write().expect("kb lock not poisoned");
        let inner = session_guard.kb_mut();
        inner.flush_session(session);
    }
}
