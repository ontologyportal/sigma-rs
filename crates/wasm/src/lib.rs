/// WASM bindings for sigmakee-rs-core.
///
/// Exposes the KnowledgeBase API to JavaScript/Node.js via wasm-bindgen.
/// The `ask()` functionality is handled by a JS callback hook since WASM
/// cannot spawn native processes.
use wasm_bindgen::prelude::*;
use sigmakee_rs_core::{KnowledgeBase, TptpOptions, TptpLang};

// -- WasmKnowledgeBase ---------------------------------------------------------

/// A KIF knowledge base exposed to JavaScript.
#[wasm_bindgen]
pub struct WasmKnowledgeBase {
    inner: KnowledgeBase,
}

#[wasm_bindgen]
impl WasmKnowledgeBase {
    /// Create an empty knowledge base.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self { inner: KnowledgeBase::new() }
    }

    /// Load KIF text into the KB under `file_tag`.
    ///
    /// Returns a JSON array of error strings, or an empty array on success.
    #[wasm_bindgen(js_name = loadKif)]
    pub fn load_kif(&mut self, kif_text: &str, file_tag: &str) -> Result<JsValue, JsValue> {
        let result = self.inner.load_kif(kif_text, file_tag, None);
        let errors: Vec<String> = result.errors.iter().map(|e| e.to_string()).collect();
        serde_wasm_bindgen::to_value(&errors)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Assert a single KIF formula into the KB under the given session key.
    ///
    /// `session` defaults to `"default"` if omitted.
    /// Returns `{ ok: bool, errors: string[] }`.
    #[wasm_bindgen]
    pub fn tell(&mut self, kif_text: &str, session: Option<String>) -> Result<JsValue, JsValue> {
        let s = session.as_deref().unwrap_or("default");
        let result = self.inner.tell(s, kif_text);
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"ok".into(), &JsValue::from_bool(result.ok))
            .map_err(|e| JsValue::from_str(&format!("{:?}", e)))?;
        let errors: Vec<String> = result.errors.iter().map(|e| e.to_string()).collect();
        let errs_js = serde_wasm_bindgen::to_value(&errors)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_sys::Reflect::set(&obj, &"errors".into(), &errs_js)
            .map_err(|e| JsValue::from_str(&format!("{:?}", e)))?;
        Ok(obj.into())
    }

    /// Remove all `tell()` assertions from every session.
    #[wasm_bindgen]
    pub fn flush(&mut self) {
        self.inner.flush_assertions();
    }

    /// Remove assertions for a specific session only.
    #[wasm_bindgen(js_name = flushSession)]
    pub fn flush_session(&mut self, session: &str) {
        self.inner.flush_session(session);
    }

    /// Render the KB (and any session assertions) as a TPTP string.
    ///
    /// `lang` should be `"fof"` (default) or `"tff"`.
    /// `hide_numbers` replaces numeric literals with `n__N` tokens.
    /// `session` filters which session's assertions are included as hypotheses
    /// (omit or pass `undefined` to include all sessions).
    #[wasm_bindgen(js_name = toTptp)]
    pub fn to_tptp(
        &self,
        lang:         Option<String>,
        hide_numbers: Option<bool>,
        session:      Option<String>,
    ) -> String {
        let tptp_lang = match lang.as_deref() {
            Some("tff") => TptpLang::Tff,
            _           => TptpLang::Fof,
        };
        let opts = TptpOptions {
            lang:         tptp_lang,
            hide_numbers: hide_numbers.unwrap_or(true),
            ..TptpOptions::default()
        };
        self.inner.to_tptp(&opts, session.as_deref())
    }

    /// Pattern-based lookup.  Returns a JSON array of matched sentence strings.
    ///
    /// Pattern syntax: whitespace-separated tokens; `_` is a wildcard.
    /// Example: `"instance _ Entity"`
    #[wasm_bindgen]
    pub fn lookup(&self, pattern: &str) -> Result<JsValue, JsValue> {
        let sids = self.inner.lookup(pattern);
        let results: Vec<String> = sids
            .iter()
            .map(|&sid| self.inner.sentence_to_string(sid))
            .collect();
        serde_wasm_bindgen::to_value(&results)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Invoke the theorem prover via a JS callback.
    ///
    /// WASM cannot spawn native processes, so callers must supply an `ask_hook`
    /// function with signature:
    ///
    /// ```js
    /// function askHook(tptpString) { /* run vampire or other prover */ return outputString; }
    /// ```
    ///
    /// The query KIF is parsed, converted to TPTP with the `conjecture` role,
    /// appended to the KB axioms, and the combined TPTP is passed to `ask_hook`.
    /// Returns the raw string output from the hook.
    #[wasm_bindgen]
    pub fn ask(&mut self, query_kif: &str, ask_hook: &js_sys::Function) -> Result<JsValue, JsValue> {
        // Parse the query into a temporary session.
        let query_tag = "__query__";
        let tell_result = self.inner.tell(query_tag, query_kif);
        if !tell_result.ok {
            let errors: Vec<String> = tell_result.errors.iter().map(|e| e.to_string()).collect();
            return Err(serde_wasm_bindgen::to_value(&errors)
                .unwrap_or_else(|_| JsValue::from_str("parse error")));
        }

        let query_sids = self.inner.session_sids(query_tag);
        if query_sids.is_empty() {
            self.inner.flush_session(query_tag);
            return Err(JsValue::from_str("No query sentence parsed"));
        }

        // Build KB axioms as TPTP.
        let kb_opts  = TptpOptions { hide_numbers: true, ..TptpOptions::default() };
        let mut tptp = self.inner.to_tptp(&kb_opts, None);

        // Append the conjecture(s).
        let q_opts = TptpOptions { query: true, hide_numbers: true, ..TptpOptions::default() };
        for (i, &sid) in query_sids.iter().enumerate() {
            let conj = self.inner.format_sentence_tptp(sid, &q_opts);
            tptp.push_str(&format!("\nfof(query_{}, conjecture, ({})).\n", i, conj));
        }

        self.inner.flush_session(query_tag);

        // Delegate to the JS hook.
        let tptp_js = JsValue::from_str(&tptp);
        ask_hook.call1(&JsValue::NULL, &tptp_js)
            .map_err(|e| JsValue::from_str(&format!("ask_hook threw: {:?}", e)))
    }
}
