// crates/wasm/src/session/ask.rs
//
// Proving ops: tell / ask / audit / clausify, plus parsing captured Vampire
// transcripts into the same shapes -- the wasm face of the SDK's
// `session/ask.rs`.

use wasm_bindgen::prelude::*;

use crate::types::to_js;

use super::Session;

#[wasm_bindgen]
impl Session {
    /// Assert a single KIF formula into the KB under the given session key.
    ///
    /// `session` defaults to `"default"` if omitted.
    /// Returns `{ ok: bool, errors: string[] }`.
    #[wasm_bindgen]
    pub fn tell(&mut self, kif_text: &str, session: Option<String>) -> Result<JsValue, JsValue> {
        let mut session_guard = self.session.write().expect("kb lock not poisoned");
        let inner = session_guard.kb_mut();
        let s = session.as_deref().unwrap_or("default");
        let result = inner.tell(kif_text, s);
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"ok".into(), &JsValue::from_bool(result.ok))
            .map_err(|e| JsValue::from_str(&format!("{:?}", e)))?;
        let errors: Vec<String> = result.diagnostics.iter().map(|e| e.to_string()).collect();
        let errs_js =
            serde_wasm_bindgen::to_value(&errors).map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_sys::Reflect::set(&obj, &"errors".into(), &errs_js)
            .map_err(|e| JsValue::from_str(&format!("{:?}", e)))?;
        Ok(obj.into())
    }

    /// Prove `query_kif` (a single KIF conjecture) in-browser against the KB
    /// plus optional `session` support, using the active [`Config`] (set via
    /// [`configure`](Session::configure)).
    ///
    /// The wall-clock deadline (`Config.timeLimitSecs`) is enforced through
    /// `Date.now()`; termination is also bounded by the step budget
    /// (`Config.maxSteps`), so a query cannot run unbounded.
    ///
    /// Returns a JS object describing the outcome:
    ///
    /// * `status` -- one of `"Proved"`, `"Disproved"`, `"Consistent"`,
    ///   `"Inconsistent"`, `"Timeout"`, `"InputError"`, `"Unknown"`;
    /// * `proved` -- `true` iff `status === "Proved"`;
    /// * `given_steps` -- given-clause steps the native loop executed (or `null`);
    /// * `raw_output` -- the engine's human-readable trace;
    /// * `proof` -- on `Proved`, the SUO-KIF proof as
    ///   `{ index, rule, premises, kif }[]` (empty otherwise);
    /// * `graphviz` -- the same proof rendered as a Graphviz DOT digraph
    ///   (always a syntactically valid graph, even when `proof` is empty).
    ///
    /// [`Config`]: crate::Config
    #[wasm_bindgen]
    pub fn ask(&self, query_kif: &str, session: Option<String>) -> Result<JsValue, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        // Curated, JS-safe projection of `ProverResult` (see `AskResultView`).
        // The raw result is deliberately NOT serialized: its
        // `bindings`/`proof_kif` carry u64 symbol/sentence hashes that
        // overflow JS's safe-integer range and abort serde-wasm-bindgen.
        let opts = self
            .config
            .to_native_opts(session_guard.kb().sine_axiom_count());
        to_js(&session_guard.ask_view(query_kif, session.as_deref(), opts))
    }

    /// Audit the whole KB for logical consistency via the native saturation
    /// prover -- enumerates up to `limit` (default 5) distinct contradictions,
    /// each cited back to `file:line` wherever a step traces to an input
    /// axiom. In-browser analogue of the `sumo audit` CLI command; uses the
    /// active [`Config`] (set via [`configure`](Session::configure)) for its
    /// time/step budget.
    ///
    /// Returns a JS object:
    ///
    /// * `status` -- one of `"Consistent"`, `"Inconsistent"`, `"Timeout"`,
    ///   `"InputError"`, `"Unknown"`;
    /// * `inconsistent` -- `true` iff `status === "Inconsistent"`;
    /// * `given_steps` -- given-clause steps the native loop executed (or `null`);
    /// * `raw_output` -- the engine's human-readable trace;
    /// * `contradictions` -- one entry per distinct contradiction found, each
    ///   `{ steps: { index, rule, premises, kif, file, line }[], graphviz }`;
    ///   `file`/`line` are `null` for derived/anonymous steps that don't trace
    ///   to an input axiom; `graphviz` is that contradiction's derivation
    ///   rendered as a DOT digraph.
    ///
    /// [`Config`]: crate::Config
    #[wasm_bindgen(js_name = auditConsistency)]
    pub fn audit_consistency(&self, limit: Option<u32>) -> Result<JsValue, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        let opts = self
            .config
            .to_native_opts(session_guard.kb().sine_axiom_count());
        to_js(&session_guard.audit_view(opts, limit.unwrap_or(5) as usize))
    }

    /// Clausify the KB and return its CNF form as SUO-KIF, via the native
    /// prover's own clausifier -- one clause string per array entry. `formula`
    /// clausifies just that ad hoc KIF text instead of the whole KB when
    /// given (the loaded KB is untouched either way).
    #[wasm_bindgen(js_name = clausify)]
    pub fn clausify(&self, formula: Option<String>) -> Result<JsValue, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        let inner = session_guard.kb();
        let clauses = match formula {
            Some(kif) => inner.clausify_formula(&kif),
            None => inner.clausify_all(),
        };
        serde_wasm_bindgen::to_value(&clauses).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Parse a captured Vampire (WASM) run's combined stdout+stderr into the
    /// SAME shape [`ask`](Session::ask) returns for the native prover -- status
    /// (with Vampire's own Theorem-vs-ContradictoryAxioms mislabelling
    /// corrected, same as the native `ask`/subprocess `VampireRunner` paths),
    /// proof steps, Graphviz digraph, and English prose -- so both backends
    /// render through one UI code path.
    ///
    /// `raw_output` -- Vampire's stdout+stderr, verbatim (see the demo's
    /// `sigma.worker.js`, which runs the Vampire WASM binary and hands its
    /// captured output straight to this method).
    /// `query_kif` -- the conjecture KIF text, reparsed only for the prose's
    /// goal restatement; a parse failure just drops that opener line.
    #[wasm_bindgen(js_name = parseVampireAskResult)]
    pub fn parse_vampire_ask_result(
        &self,
        raw_output: &str,
        query_kif: &str,
    ) -> Result<JsValue, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        to_js(&session_guard.vampire_ask_view(raw_output, query_kif))
    }

    /// Parse a captured Vampire (WASM) consistency-check run into the SAME
    /// shape [`audit_consistency`](Session::audit_consistency) returns for the
    /// native prover. Vampire's one-shot run yields at most a single
    /// contradiction (no enumerator, unlike the native audit's driver), so
    /// `contradictions` has 0 or 1 entries.
    ///
    /// `raw_output` -- Vampire's stdout+stderr, verbatim, from a run over the
    /// whole-KB TPTP dump (no conjecture -- see the demo's `auditVampire`).
    #[wasm_bindgen(js_name = parseVampireAuditResult)]
    pub fn parse_vampire_audit_result(&self, raw_output: &str) -> Result<JsValue, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        to_js(&session_guard.vampire_audit_view(raw_output))
    }
}
