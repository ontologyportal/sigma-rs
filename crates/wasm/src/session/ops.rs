// crates/wasm/src/session/ops.rs
//
// Non-proving ops: validation, TPTP translation, snapshot/restore, and the
// thread-pool knob -- the wasm face of the SDK's `session/ops.rs`.

use sigmakee_rs_sdk::{DiagnosticView, ScratchValidationView, TptpLang, TptpOptions};
use wasm_bindgen::prelude::*;

use crate::types::to_js;

use super::Session;

#[wasm_bindgen]
impl Session {
    /// Set the worker-pool size the KB's `plan_threads` gates should target,
    /// once the caller has spun up a rayon pool via `initThreadPool` (only
    /// exported in threaded builds). `available_parallelism()` reads 1 on
    /// wasm32 regardless of pool size, so without this call every
    /// `plan_threads` gate would serialize even against a live N-worker pool.
    /// A no-op degrade path everywhere else -- calling it against a bundle
    /// with no pool just biases planning without any workers to run on.
    #[wasm_bindgen(js_name = setMaxThreads)]
    pub fn set_max_threads(&self, n: u32) {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        session_guard.set_max_threads(n as usize);
    }

    /// Freeze the entire KB -- promoted axioms, symbols, indices, taxonomy --
    /// into a self-contained byte buffer (a `Uint8Array`).
    ///
    /// The bytes are heed-free and portable: stash them in IndexedDB / a file /
    /// a download, then rebuild the KB on a later visit with
    /// [`restore`](Session::restore) instead of re-ingesting and re-promoting.
    /// This is the browser freeze/thaw seam.
    #[wasm_bindgen]
    pub fn snapshot(&self) -> Result<Vec<u8>, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        session_guard
            .snapshot_bytes()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Thaw a KB previously produced by [`snapshot`](Session::snapshot), replacing
    /// this instance's contents. The active [`Config`](crate::Config) is preserved.
    #[wasm_bindgen]
    pub fn restore(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        let mut session_guard = self.session.write().expect("kb lock not poisoned");
        session_guard
            .restore_bytes(bytes)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Run semantic validation over the whole KB. Returns a JS array of
    /// structured diagnostics (empty means clean).
    #[wasm_bindgen]
    pub fn validate(&self) -> Result<JsValue, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        to_js(&DiagnosticView::from_slice(&session_guard.validate()))
    }

    /// Validate a single inline KIF formula without mutating the KB. Parse
    /// failures come back as diagnostics in the returned array.
    #[wasm_bindgen(js_name = validateFormula)]
    pub fn validate_formula(&mut self, kif: &str) -> Result<JsValue, JsValue> {
        let mut session_guard = self.session.write().expect("kb lock not poisoned");
        let diags = session_guard
            .validate_formula(kif)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        to_js(&DiagnosticView::from_slice(&diags))
    }

    /// Validate an Ask/Tell pair (assertions then query) in one scratch
    /// session against the live KB. Returns `{ assertions, query }` arrays.
    #[wasm_bindgen(js_name = validateScratch)]
    pub fn validate_scratch(&mut self, assertions: &str, query: &str) -> Result<JsValue, JsValue> {
        let mut session_guard = self.session.write().expect("kb lock not poisoned");
        let (a_diags, q_diags) = session_guard.validate_scratch(assertions, query);
        to_js(&ScratchValidationView {
            assertions: DiagnosticView::from_slice(&a_diags),
            query: DiagnosticView::from_slice(&q_diags),
        })
    }

    /// Render the WHOLE KB as TPTP -- `lang` `"fof"` (default) or `"tff"`,
    /// `hide_numbers` replaces numeric literals with `n__N` tokens.
    ///
    /// Intended for an occasional "generate the TPTP dump" action, not a
    /// per-keystroke call -- this re-translates the entire KB (thousands of
    /// axioms for a full SUMO load).  Editor-lane TPTP preview (per-line
    /// indexing, cursor follow) lives on the LSP facade instead:
    /// `sumo/toTptp` / `sumo/tptpLineForPosition` via [`WasmLsp`].
    ///
    /// [`WasmLsp`]: crate::WasmLsp
    #[wasm_bindgen(js_name = toTptpIndexed)]
    pub fn to_tptp_indexed(&mut self, lang: Option<String>, hide_numbers: Option<bool>) -> String {
        let mut session_guard = self.session.write().expect("kb lock not poisoned");
        let inner = session_guard.kb_mut();
        let opts = TptpOptions {
            lang: match lang.as_deref() {
                Some("thf") => TptpLang::Thf,
                Some("tff") => TptpLang::Tff,
                _ => TptpLang::Fof,
            },
            hide_numbers: hide_numbers.unwrap_or(true),
            ..TptpOptions::default()
        };
        inner.to_tptp_indexed(&opts, None, None)
    }

    /// Build a standalone TPTP problem for an external prover: whole-KB
    /// axioms, `assertions` folded in as `hypothesis`-role support (a
    /// scratch session, flushed before returning -- never left in the live
    /// KB), and `query_kif` appended as the `conjecture`. Returns the text
    /// rather than invoking any hook, so callers with an ASYNC prover (e.g.
    /// a WASM build of Vampire, run via `await`) can drive the prover
    /// themselves.
    ///
    /// Empty `assertions` is fine (no session created). Errors (as a
    /// diagnostic-message array) on a query that fails to parse or
    /// produces no sentence.
    ///
    /// `select_all` mirrors [`Config::selectAll`](crate::Config::select_all)
    /// on the native backend's `Config`, so ONE toggle in the UI means the
    /// same thing for both: `false` (default) SInE-selects a query-relevant
    /// axiom subset (seeded from the assertions + query, via the same
    /// selection primitive the native prover and the CLI's external-prover
    /// path both use); `true` emits the whole promoted KB, unfiltered.
    /// `selection_tolerance_pct` mirrors
    /// [`Config::selectionTolerancePct`](crate::Config::selection_tolerance_pct)
    /// -- ignored when `select_all` is true; `None` uses the engine default
    /// budget. Unlike the native backend's autoscaling loop, this is the
    /// FINAL budget: Vampire runs as a one-shot external engine with no
    /// feedback retry, so a query that needs more of the KB than the given
    /// percentage admits will fail here even though the native backend
    /// might still find it by widening its own selection.
    #[wasm_bindgen(js_name = toTptpForAsk)]
    pub fn to_tptp_for_ask(
        &mut self,
        assertions_kif: &str,
        query_kif: &str,
        select_all: Option<bool>,
        selection_tolerance_pct: Option<f64>,
    ) -> Result<String, JsValue> {
        let mut session_guard = self.session.write().expect("kb lock not poisoned");
        session_guard
            .tptp_for_ask(
                assertions_kif,
                query_kif,
                select_all.unwrap_or(false),
                selection_tolerance_pct,
            )
            .map_err(|errs| {
                let errors: Vec<String> = errs.iter().map(|e| e.to_string()).collect();
                serde_wasm_bindgen::to_value(&errors)
                    .unwrap_or_else(|_| JsValue::from_str("tptp_for_ask error"))
            })
    }
}
