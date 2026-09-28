// crates/wasm/src/session/ask.rs
//
// Proving ops: tell / ask / audit / clausify, plus parsing captured Vampire
// transcripts into the same shapes -- the wasm face of the SDK's
// `session/ask.rs`.

use wasm_bindgen::prelude::*;

use crate::config::Backend;
use crate::types::to_js;

use super::Session;

#[wasm_bindgen]
impl Session {
    /// Assert a single formula into the KB under the given session key.
    ///
    /// `session` defaults to `"default"` if omitted. `tptp` (default
    /// `false`) parses `text` as TPTP instead of SUO-KIF -- the entry point
    /// for the web UI's proof-language toggle.
    /// Returns `{ ok: bool, errors: string[] }`.
    #[wasm_bindgen]
    pub fn tell(
        &mut self,
        text: &str,
        session: Option<String>,
        tptp: Option<bool>,
    ) -> Result<JsValue, JsValue> {
        let mut session_guard = self.session.write().expect("kb lock not poisoned");
        let inner = session_guard.kb_mut();
        let s = session.as_deref().unwrap_or("default");
        let result = if tptp.unwrap_or(false) {
            inner.tell_tptp(text, s)
        } else {
            inner.tell(text, s)
        };
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

    /// Prove `query_kif` (a single KIF conjecture) against the KB plus
    /// optional `session` support, with the backend the active [`Config`]
    /// selects (set via [`configure`](Session::configure)): the in-browser
    /// native prover, or the Emscripten Vampire through the page's bridge
    /// (see [`crate::vampire`]).
    ///
    /// The wall-clock deadline (`Config.timeLimitSecs`) is enforced through
    /// `Date.now()` for the native prover and passed as `-t` to Vampire;
    /// native termination is also bounded by the step budget
    /// (`Config.maxSteps`), so a query cannot run unbounded.
    ///
    /// Returns a JS object describing the outcome:
    ///
    /// * `status` -- one of `"Proved"`, `"Disproved"`, `"Consistent"`,
    ///   `"Inconsistent"`, `"Timeout"`, `"InputError"`, `"Unknown"`;
    /// * `proved` -- `true` iff `status === "Proved"`;
    /// * `given_steps` -- given-clause steps the native loop executed (or `null`);
    /// * `raw_output` -- the engine's human-readable trace (Vampire: its
    ///   captured stdout + stderr);
    /// * `proof` -- on `Proved`, the SUO-KIF proof as `{ index, rule,
    ///   premises, kif, tptp, file, line }[]` (empty otherwise); `tptp` is
    ///   that same step reconstructed as TPTP (framed `cnf`/`fof`/`tff`/...
    ///   text, dialect chosen per the whole proof), or an inline `;`
    ///   comment explaining why the step couldn't be represented;
    /// * `proof_tptp_prologue` -- whole-proof TPTP material that doesn't
    ///   belong to any single step (e.g. TFF's type-declaration preamble),
    ///   to show once ahead of the per-step `tptp` text; empty for untyped
    ///   dialects or when `proof` is empty;
    /// * `graphviz` -- the same proof rendered as a Graphviz DOT digraph
    ///   (always a syntactically valid graph, even when `proof` is empty);
    /// * `input_tptp` -- Vampire backend with `Config.keepTptp` only: the
    ///   exact problem text handed to Vampire on the last run.
    ///
    /// `tptp` (default `false`) parses `query` as TPTP instead of SUO-KIF --
    /// the entry point for the web UI's proof-language toggle; the reported
    /// proof steps still carry both `kif` and `tptp` renderings either way.
    ///
    /// [`Config`]: crate::Config
    #[wasm_bindgen]
    pub fn ask(
        &self,
        query: &str,
        session: Option<String>,
        tptp: Option<bool>,
    ) -> Result<JsValue, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        let dialect = if tptp.unwrap_or(false) {
            sigmakee_rs_sdk::Parser::Tptp { options: None }
        } else {
            sigmakee_rs_sdk::Parser::Kif { options: None }
        };
        let axiom_count = session_guard.kb().sine_axiom_count();
        // Curated, JS-safe projection of `ProverResult` (see `AskResultView`).
        // The raw result is deliberately NOT serialized: its
        // `bindings`/`proof_kif` carry u64 symbol/sentence hashes that
        // overflow JS's safe-integer range and abort serde-wasm-bindgen.
        match self.config.selected_backend() {
            Backend::Native => {
                let opts = self.config.to_native_opts(axiom_count);
                to_js(&session_guard.ask_view_dialect_native(
                    query,
                    session.as_deref(),
                    opts,
                    dialect,
                ))
            }
            Backend::Vampire => {
                let opts = self.config.to_external_opts(axiom_count);
                let mut view =
                    session_guard.ask_view_dialect(query, session.as_deref(), &opts, dialect);
                view.input_tptp = self.vampire.last_tptp();
                to_js(&view)
            }
        }
    }

    /// Sampled consistency audit with the backend the active [`Config`]
    /// selects: walk a seeded pseudorandom sweep of the KB's eligible axioms
    /// (bookkeeping heads excluded) and, for each group of `batch` of them,
    /// check their SInE neighborhood -- sized by the config's selection
    /// budget -- for a contradiction.  Uses the active config's time limit per
    /// subproblem, and (native) its step cap.
    ///
    /// `request` is `{ seed?, step?, count?, batch?, limit?, scope?, budget?,
    /// depth? }`: sweep seed (default 0), start position (0), sentences to
    /// check (1), sentences per subproblem (1), distinct contradictions to
    /// stop at (5), an optional loaded file tag to restrict the sweep to, and
    /// the subproblem size -- the most axioms a neighborhood may select and
    /// the SInE expansion depth -- overriding the config's selection budget
    /// when given.
    ///
    /// Returns a JS object:
    ///
    /// * `status` -- `"Inconsistent"` or `"Unknown"` (never `"Consistent"`:
    ///   unchecked neighborhoods remain);
    /// * `inconsistent` -- `true` iff `status === "Inconsistent"`;
    /// * `raw_output` -- the engine's human-readable trace, one line per
    ///   subproblem;
    /// * `seed`, `step`, `next_step`, `total` -- the sweep position checked
    ///   and where to resume (`total` is the sweep's size);
    /// * `batches` -- one per subproblem: `{ focus: { kif, file, line }[],
    ///   status, stop_reason, elapsed_ms }`, where `status` `"Consistent"`
    ///   means that neighborhood saturated clean and `stop_reason` is
    ///   `"TimeLimit"`, `"StepLimit"`, `"IncompleteLoad"`, `"GaveUp"` or `null`;
    /// * `contradictions` -- one entry per distinct contradiction found, each
    ///   `{ steps: { index, rule, premises, kif, tptp, file, line }[],
    ///   graphviz, prose, prose_missing, proof_tptp_prologue }` (see
    ///   [`ask`](Session::ask) for `tptp`/`proof_tptp_prologue`).
    ///
    /// [`Config`]: crate::Config
    #[wasm_bindgen(js_name = auditConsistency)]
    pub fn audit_consistency(&self, request: JsValue) -> Result<JsValue, JsValue> {
        let req: AuditRequest = if request.is_undefined() || request.is_null() {
            AuditRequest::default()
        } else {
            serde_wasm_bindgen::from_value(request)
                .map_err(|e| JsValue::from_str(&format!("audit request: {e}")))?
        };
        let sample = sigmakee_rs_core::AuditSample {
            seed: req.seed,
            step: req.step,
            count: req.count,
            batch: req.batch.max(1),
            limit: req.limit.max(1),
        };
        let scope = req.scope.as_deref();
        let session_guard = self.session.read().expect("kb lock not poisoned");
        let axiom_count = session_guard.kb().sine_axiom_count();
        match self.config.selected_backend() {
            Backend::Native => {
                let mut opts = self.config.to_native_opts(axiom_count);
                req.size(&mut opts.selection);
                to_js(&session_guard.audit_view_native(opts, sample, scope))
            }
            Backend::Vampire => {
                let mut opts = self.config.to_external_opts(axiom_count);
                req.size(&mut opts.selection);
                to_js(&session_guard.audit_view(opts, sample, scope))
            }
        }
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
}

/// The JS request object of [`Session::audit_consistency`].
#[derive(serde::Deserialize)]
#[serde(default)]
struct AuditRequest {
    seed: u32,
    step: usize,
    count: usize,
    batch: usize,
    limit: usize,
    scope: Option<String>,
    budget: Option<usize>,
    depth: Option<usize>,
}

impl AuditRequest {
    /// Apply the requested subproblem size to a prover's selection params.
    fn size(&self, selection: &mut sigmakee_rs_core::SineParams) {
        if let Some(budget) = self.budget {
            selection.auto_budget = Some(budget.max(1));
            selection.select_all = false;
        }
        if self.depth.is_some() {
            selection.depth_limit = self.depth;
        }
    }
}

impl Default for AuditRequest {
    fn default() -> Self {
        Self {
            seed: 0,
            step: 0,
            count: 1,
            batch: 1,
            limit: 5,
            scope: None,
            budget: None,
            depth: None,
        }
    }
}
