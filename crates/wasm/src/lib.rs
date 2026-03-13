/// WASM bindings for sumo-parser-core.
///
/// Exposes the KnowledgeBase API to JavaScript/Node.js via wasm-bindgen.
/// The `ask()` functionality is handled by a JS callback hook since WASM
/// cannot spawn native processes.
use wasm_bindgen::prelude::*;
use sumo_parser_core::{KifStore, KnowledgeBase, TptpOptions, TptpLang, load_kif, kb_to_tptp};

// ── WasmKnowledgeBase ─────────────────────────────────────────────────────────

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
        Self { inner: KnowledgeBase::new(KifStore::default()) }
    }

    /// Load KIF text (from a string) into the KB.
    ///
    /// Returns a JSON array of parse error strings, or an empty array on success.
    #[wasm_bindgen(js_name = loadKif)]
    pub fn load_kif(&mut self, kif_text: &str, file_tag: &str) -> Result<JsValue, JsValue> {
        let errors = self.inner.load_kif(kif_text, file_tag);
        let msgs: Vec<String> = errors.iter().map(|(.., e)| e.to_string()).collect();
        serde_wasm_bindgen::to_value(&msgs).map_err(|e| JsValue::from_str(&e.to_string()))
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
        let errs: Vec<String> = result.errors;
        let errs_js = serde_wasm_bindgen::to_value(&errs)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_sys::Reflect::set(&obj, &"errors".into(), &errs_js)
            .map_err(|e| JsValue::from_str(&format!("{:?}", e)))?;
        Ok(obj.into())
    }

    /// Remove all `tell()` assertions from every session.
    #[wasm_bindgen]
    pub fn flush(&mut self) {
        self.inner.flush();
    }

    /// Remove assertions for a specific session only.
    #[wasm_bindgen(js_name = flushSession)]
    pub fn flush_session(&mut self, session: &str) {
        self.inner.flush_session(session);
    }

    /// Render the KB (and any assertions) as a TPTP string.
    ///
    /// `lang` should be `"fof"` (default) or `"tff"`.
    /// `hide_numbers` replaces numeric literals with `n__N` tokens.
    /// `session` filters which session's assertions are included as hypotheses
    /// (omit or pass `undefined` for all sessions).
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
            lang: tptp_lang,
            hide_numbers: hide_numbers.unwrap_or(true),
            ..TptpOptions::default()
        };
        kb_to_tptp(&self.inner, "kb", &opts, session.as_deref())
    }

    /// Pattern-based lookup.  Returns a JSON array of matched sentence strings.
    ///
    /// Pattern syntax: whitespace-separated tokens; `_` is a wildcard.
    /// Example: `"instance _ Entity"`
    #[wasm_bindgen]
    pub fn lookup(&self, pattern: &str) -> Result<JsValue, JsValue> {
        let sids = self.inner.store.lookup(pattern);
        let results: Vec<String> = sids
            .iter()
            .map(|&sid| sentence_to_string(sid, &self.inner.store))
            .collect();
        serde_wasm_bindgen::to_value(&results).map_err(|e| JsValue::from_str(&e.to_string()))
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
    /// The query KIF is converted to TPTP with the `conjecture` role appended,
    /// then passed to `ask_hook`.  Returns the raw string output from the hook.
    #[wasm_bindgen]
    pub fn ask(&self, query_kif: &str, ask_hook: &js_sys::Function) -> Result<JsValue, JsValue> {
        // Parse query into a throw-away store and convert to TPTP conjecture
        let mut tmp_store = KifStore::default();
        let errs = load_kif(&mut tmp_store, query_kif, "__query__");
        if !errs.is_empty() {
            let msgs: Vec<String> = errs.iter().map(|(.., e)| e.to_string()).collect();
            return Err(serde_wasm_bindgen::to_value(&msgs)
                .unwrap_or_else(|_| JsValue::from_str("parse error")));
        }

        let sid = match tmp_store.roots.first().copied() {
            Some(id) => id,
            None => return Err(JsValue::from_str("No query sentence parsed")),
        };

        let query_opts = TptpOptions {
            query: true,
            hide_numbers: true,
            ..TptpOptions::default()
        };
        let tmp_kb = KnowledgeBase::new(tmp_store);
        let conjecture_formula = sumo_parser_core::sentence_to_tptp(sid, &tmp_kb, &query_opts);
        let conjecture = format!("fof(query_0,conjecture,({})).\n", conjecture_formula);

        // Build KB TPTP + conjecture
        let kb_opts = TptpOptions { hide_numbers: true, ..TptpOptions::default() };
        let kb_tptp = kb_to_tptp(&self.inner, "kb", &kb_opts, None);
        let full_tptp = format!("{}\n{}", kb_tptp, conjecture);

        // Call the JS hook
        let tptp_js = JsValue::from_str(&full_tptp);
        let result = ask_hook.call1(&JsValue::NULL, &tptp_js)
            .map_err(|e| JsValue::from_str(&format!("ask_hook threw: {:?}", e)))?;

        Ok(result)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn sentence_to_string(sid: sumo_parser_core::store::SentenceId, store: &KifStore) -> String {
    use sumo_parser_core::store::Element;
    let sentence = &store.sentences[sid];
    let parts: Vec<String> = sentence.elements.iter().map(|e| match e {
        Element::Symbol(id)            => store.sym_name(*id).to_owned(),
        Element::Variable { name, .. } => name.clone(),
        Element::Literal(sumo_parser_core::store::Literal::Str(s))    => s.clone(),
        Element::Literal(sumo_parser_core::store::Literal::Number(n)) => n.clone(),
        Element::Op(op)                => op.name().to_owned(),
        Element::Sub(sub_id)           => format!("({})", sentence_to_string(*sub_id, store)),
    }).collect();
    format!("({})", parts.join(" "))
}
