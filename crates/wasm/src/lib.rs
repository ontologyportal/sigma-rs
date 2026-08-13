/// WASM bindings for sigmakee-rs-core.
///
/// Exposes the KnowledgeBase API to JavaScript/Node.js via wasm-bindgen.
/// The `ask()` functionality is handled by a JS callback hook since WASM
/// cannot spawn native processes.
use wasm_bindgen::prelude::*;
use sigmakee_rs_core::{KnowledgeBase, TptpOptions, TptpLang};
use sigmakee_rs_core::{ProverLayer, NativeOpts};
use sigmakee_rs_core::{ManKind, ManPage, SearchHit, SearchOpts};
use sigmakee_rs_core::TopLayer;
use sigmakee_rs_core::AstKif;
use sigmakee_rs_core::TranslationLayer;

// Threaded builds only: re-exports `initThreadPool`, the JS entry point that
// spins up the wasm-bindgen-rayon worker pool. Plain (non-`atomics`) wasm32
// builds never link this — `sigmakee-rs-core/parallel` itself is
// compile_error!-banned there, so the feature can only be on in a
// -Zbuild-std threads-enabled build (see build-npm.sh's threaded variant).
#[cfg(feature = "parallel")]
pub use wasm_bindgen_rayon::init_thread_pool;

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

    /// Load KIF text into the KB under `file_tag` as **axioms**.
    ///
    /// The loaded source is promoted into the axiomatic theory
    /// (`make_session_axiomatic`) so it shows up in [`toTptp`](Self::to_tptp)
    /// and is sent as background axioms by [`ask`](Self::ask).  Without the
    /// promotion `to_tptp` renders only the (empty) axiomatic set and the
    /// loaded KIF is invisible.
    ///
    /// Returns a JSON array of error strings, or an empty array on success.
    #[wasm_bindgen(js_name = loadKif)]
    pub fn load_kif(&mut self, kif_text: &str, file_tag: &str) -> Result<JsValue, JsValue> {
        let result = self.inner.load(
            sigmakee_rs_core::SourceFile::kif(std::path::PathBuf::from(file_tag), kif_text.to_string()),
            file_tag,
        );
        let mut errors: Vec<String> = result.diagnostics.iter().map(|e: &sigmakee_rs_core::Diagnostic| e.to_string()).collect();
        if let Err(e) = self.inner.make_session_axiomatic(file_tag) {
            errors.push(format!("promote failed: {:?}", e));
        }
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
        let result = self.inner.tell(kif_text, s);
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"ok".into(), &JsValue::from_bool(result.ok))
            .map_err(|e| JsValue::from_str(&format!("{:?}", e)))?;
        let errors: Vec<String> = result.diagnostics.iter().map(|e| e.to_string()).collect();
        let errs_js = serde_wasm_bindgen::to_value(&errors)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_sys::Reflect::set(&obj, &"errors".into(), &errs_js)
            .map_err(|e| JsValue::from_str(&format!("{:?}", e)))?;
        Ok(obj.into())
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
        &mut self,
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

    /// Run semantic validation over the whole KB. Returns a JS `string[]` of
    /// diagnostics (empty ⇒ clean).
    #[wasm_bindgen]
    pub fn validate(&self) -> Result<JsValue, JsValue> {
        diagnostics_to_js(&self.inner.validate_all())
    }

    /// Validate a single inline KIF formula without mutating the KB. Parse
    /// failures come back as diagnostics in the returned `string[]`.
    #[wasm_bindgen(js_name = validateFormula)]
    pub fn validate_formula(&mut self, kif: &str) -> Result<JsValue, JsValue> {
        validate_formula_impl(&mut self.inner, kif)
    }

    /// Validate an Ask/Tell pair (assertions then query) in one scratch
    /// session against the live KB. Returns `{ assertions, query }` arrays.
    #[wasm_bindgen(js_name = validateScratch)]
    pub fn validate_scratch(&mut self, assertions: &str, query: &str) -> Result<JsValue, JsValue> {
        validate_scratch_impl(&mut self.inner, assertions, query)
    }

    /// Full-text / symbol search over the KB. `kind` filters by
    /// `"class"|"relation"|"function"|"predicate"|"instance"|"individual"`,
    /// `language` by tag (e.g. `"EnglishLanguage"`), `limit` caps results.
    /// Returns `{ symbol, kinds, source, language, text }[]`.
    #[wasm_bindgen]
    pub fn search(
        &self,
        query:    &str,
        kind:     Option<String>,
        language: Option<String>,
        limit:    Option<u32>,
    ) -> Result<JsValue, JsValue> {
        let opts = SearchOpts {
            kind:     kind.as_deref().and_then(man_kind_from_str),
            language: language.as_deref(),
            limit:    limit.map(|n| n as usize),
        };
        search_hits_to_js(&self.inner.search(query, &opts))
    }

    /// Structured "man page" for a symbol: kinds, documentation, taxonomy
    /// (parents/children), signature (arity/domains/range), and the full
    /// list of referencing formulas. Returns `null` if the symbol is unknown.
    #[wasm_bindgen]
    pub fn manpage(&self, symbol: &str) -> Result<JsValue, JsValue> {
        manpage_to_js(&self.inner, self.inner.manpage(symbol))
    }

    /// Direct taxonomy edges of `symbol` — `{ parents, children }` — without
    /// the man page's reference scan. Powers lazy taxonomy-tree expansion.
    #[wasm_bindgen]
    pub fn taxonomy(&self, symbol: &str) -> Result<JsValue, JsValue> {
        taxonomy_to_js(&self.inner, symbol)
    }

    /// The `(instance ? NaturalLanguage)` symbols, each with the English label
    /// from its `termFormat` (falling back to the bare symbol name). Sorted by
    /// label, with `EnglishLanguage` guaranteed present as a fallback. Powers the
    /// UI language selector.
    #[wasm_bindgen(js_name = naturalLanguages)]
    pub fn natural_languages(&self) -> Result<JsValue, JsValue> {
        natural_languages_to_js(&self.inner)
    }

    /// Natural-language paraphrase of a single KIF formula in `language`. Empty
    /// when the KIF doesn't parse to a statement.
    #[wasm_bindgen(js_name = renderNl)]
    pub fn render_nl(&self, kif: &str, language: &str) -> String {
        render_nl_string(&self.inner, kif, language)
    }

    /// Invoke the theorem prover via a JS callback.
    ///
    /// WASM cannot spawn native processes, so callers must supply an `ask_hook`
    /// function with signature:
    ///
    /// ```js
    /// // askHook runs vampire or another prover and returns its output string
    /// function askHook(tptpString) { return outputString; }
    /// ```
    ///
    /// The query KIF is parsed, converted to TPTP with the `conjecture` role,
    /// appended to the KB axioms, and the combined TPTP is passed to `ask_hook`.
    /// Returns the raw string output from the hook.
    #[wasm_bindgen]
    pub fn ask(&mut self, query_kif: &str, ask_hook: &js_sys::Function) -> Result<JsValue, JsValue> {
        let query_tag = "__query__";
        let tell_result = self.inner.tell(query_kif, query_tag);
        if !tell_result.ok {
            let errors: Vec<String> = tell_result.diagnostics.iter().map(|e| e.to_string()).collect();
            return Err(serde_wasm_bindgen::to_value(&errors)
                .unwrap_or_else(|_| JsValue::from_str("parse error")));
        }

        let query_sids = self.inner.session_sids(query_tag);
        if query_sids.is_empty() {
            self.inner.flush_session(query_tag);
            return Err(JsValue::from_str("No query sentence parsed"));
        }

        let kb_opts  = TptpOptions { hide_numbers: true, ..TptpOptions::default() };
        let mut tptp = self.inner.to_tptp(&kb_opts, None);

        let q_opts = TptpOptions { query: true, hide_numbers: true, ..TptpOptions::default() };
        for (i, &sid) in query_sids.iter().enumerate() {
            let conj = self.inner.format_sentence_tptp(sid, &q_opts);
            tptp.push_str(&format!("\nfof(query_{}, conjecture, ({})).\n", i, conj));
        }

        self.inner.flush_session(query_tag);

        let tptp_js = JsValue::from_str(&tptp);
        ask_hook.call1(&JsValue::NULL, &tptp_js)
            .map_err(|e| JsValue::from_str(&format!("ask_hook threw: {:?}", e)))
    }
}

// -- Config --------------------------------------------------------------------

/// Native-prover configuration exposed to JavaScript.
///
/// The browser analogue of the SDK's [`KBManager`] `NativeProverConfig`: a
/// serde-able subset of [`NativeOpts`](sigmakee_rs_core::NativeOpts) whose
/// camelCase properties map 1:1 to the `<prover type="native">` preference keys
/// (`timeLimitSecs`, `maxSteps`, `forwardClose`, `wantProof`, …).  Per-query
/// runtime fields (`session`, `cancel`) are excluded.  Nested `selection`
/// (SInE) and `strategy` tuning stay at their engine defaults.
///
/// [`KBManager`]: https://docs.rs/sigmakee-rs-sdk
///
/// ```js
/// const cfg = new Config();
/// cfg.timeLimitSecs = 10;
/// cfg.wantProof = true;
/// prover.configure(cfg);
/// ```
#[wasm_bindgen(js_name = Config)]
#[derive(Clone)]
pub struct WasmConfig {
    time_limit_secs: u64,
    max_steps:       usize,
    max_lits:        usize,
    forward_close:   bool,
    want_proof:      bool,
    profile:         bool,
    select_all:      bool,
    selection_tolerance_pct: Option<f64>,
}

impl WasmConfig {
    /// Build a runtime [`NativeOpts`] seeded with these defaults; per-query
    /// `session` is layered on by the caller.  Mirrors
    /// `NativeProverConfig::to_native_opts`.
    ///
    /// `axiom_count` is the live KB's current [`sine_axiom_count`]
    /// (`KnowledgeBase::sine_axiom_count`) — needed to turn
    /// [`Self::selection_tolerance_pct`] (a percentage) into the absolute
    /// SInE auto-budget the engine actually takes.
    fn to_native_opts(&self, axiom_count: usize) -> NativeOpts {
        NativeOpts {
            time_limit_secs: self.time_limit_secs,
            max_steps:       self.max_steps,
            max_lits:        self.max_lits,
            forward_close:   self.forward_close,
            want_proof:      self.want_proof,
            profile:         self.profile,
            selection: if self.select_all {
                sigmakee_rs_core::SineParams::whole_kb()
            } else if let Some(pct) = self.selection_tolerance_pct {
                sigmakee_rs_core::SineParams::auto(selection_budget(axiom_count, pct))
            } else {
                sigmakee_rs_core::SineParams::default()
            },
            ..NativeOpts::default()
        }
    }
}

/// Percentage (0-100) of `axiom_count` to admit into the SInE selection —
/// the absolute count [`SineParams::auto`] expects. Always at least 1, so a
/// non-empty KB never gets a zero budget from a very low slider value.
fn selection_budget(axiom_count: usize, pct: f64) -> usize {
    (((axiom_count as f64) * (pct.clamp(0.0, 100.0) / 100.0)).round() as usize).max(1)
}

#[wasm_bindgen]
impl WasmConfig {
    /// Construct a config with the native prover's defaults, except `wantProof`
    /// which is on (proofs are cheap to surface and useful in a UI).
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        // Mirrors NativeOpts::default() (see NativeProverConfig::default).
        Self {
            time_limit_secs: 30,
            max_steps:       4000,
            max_lits:        8,
            forward_close:   true,
            want_proof:      true,
            profile:         false,
            select_all:      false,
            selection_tolerance_pct: None,
        }
    }

    /// Wall-clock budget in seconds (0 = unlimited; the step cap still bounds it).
    #[wasm_bindgen(getter = timeLimitSecs)]
    pub fn time_limit_secs(&self) -> u32 { self.time_limit_secs as u32 }
    #[wasm_bindgen(setter = timeLimitSecs)]
    pub fn set_time_limit_secs(&mut self, v: u32) { self.time_limit_secs = v as u64; }

    /// Maximum given-clause steps before the loop gives up.
    #[wasm_bindgen(getter = maxSteps)]
    pub fn max_steps(&self) -> u32 { self.max_steps as u32 }
    #[wasm_bindgen(setter = maxSteps)]
    pub fn set_max_steps(&mut self, v: u32) { self.max_steps = v as usize; }

    /// Maximum literals per retained clause.
    #[wasm_bindgen(getter = maxLits)]
    pub fn max_lits(&self) -> u32 { self.max_lits as u32 }
    #[wasm_bindgen(setter = maxLits)]
    pub fn set_max_lits(&mut self, v: u32) { self.max_lits = v as usize; }

    /// Run forward-closure over the theory before the given-clause loop.
    #[wasm_bindgen(getter = forwardClose)]
    pub fn forward_close(&self) -> bool { self.forward_close }
    #[wasm_bindgen(setter = forwardClose)]
    pub fn set_forward_close(&mut self, v: bool) { self.forward_close = v; }

    /// Populate the `proof` array on a `Proved` result.
    #[wasm_bindgen(getter = wantProof)]
    pub fn want_proof(&self) -> bool { self.want_proof }
    #[wasm_bindgen(setter = wantProof)]
    pub fn set_want_proof(&mut self, v: bool) { self.want_proof = v; }

    /// Emit phase-timing spans into `raw_output`.
    #[wasm_bindgen(getter)]
    pub fn profile(&self) -> bool { self.profile }
    #[wasm_bindgen(setter)]
    pub fn set_profile(&mut self, v: bool) { self.profile = v; }

    /// Disable SInE axiom selection — search the WHOLE promoted KB instead of
    /// a query-relevant subset. Off (`false`) by default, matching the
    /// engine's own default (`SineParams::default()`, auto-budget SInE on);
    /// `true` uses `SineParams::whole_kb()`. Slower and more memory-hungry,
    /// but sidesteps selection ever excluding an axiom the query actually
    /// needs — useful for debugging a query that fails under selection.
    #[wasm_bindgen(getter = selectAll)]
    pub fn select_all(&self) -> bool { self.select_all }
    #[wasm_bindgen(setter = selectAll)]
    pub fn set_select_all(&mut self, v: bool) { self.select_all = v; }

    /// SInE selection budget, as a percentage (0-100) of the KB's total
    /// axiom count — how much of the ontology a query-relevant selection is
    /// allowed to admit. `null`/`undefined` (the default) uses the engine's
    /// own default budget (a fixed axiom count, not a percentage — see
    /// `SineParams::default`) instead of a KB-relative one. Ignored when
    /// [`Self::selectAll`](Self::select_all) is set. Applies to BOTH the
    /// native backend (as the auto-tolerance loop's starting budget, which
    /// may still widen from there) and Vampire (as the final, one-shot
    /// budget — see [`WasmNativeProver::to_tptp_for_ask`]).
    #[wasm_bindgen(getter = selectionTolerancePct)]
    pub fn selection_tolerance_pct(&self) -> Option<f64> { self.selection_tolerance_pct }
    #[wasm_bindgen(setter = selectionTolerancePct)]
    pub fn set_selection_tolerance_pct(&mut self, v: Option<f64>) { self.selection_tolerance_pct = v; }
}

// -- WasmNativeProver ----------------------------------------------------------

/// A KIF knowledge base backed by the **native saturation prover**.
///
/// Unlike [`WasmKnowledgeBase`] — which can only emit TPTP for an external
/// prover reached through a JS `ask_hook` — this type discharges queries
/// entirely in-browser: the pure-Rust given-clause loop runs inside the WASM
/// module, with no subprocess and no callback.  It is the same engine that
/// solves the SUMO TQ suite natively.
#[wasm_bindgen]
pub struct WasmNativeProver {
    inner:  KnowledgeBase<ProverLayer<TranslationLayer>>,
    config: WasmConfig,
    /// The sid→line map from the last [`to_tptp_indexed`](Self::to_tptp_indexed)
    /// call, consulted by [`tptp_line_for_position`](Self::tptp_line_for_position).
    /// Never serialized to JS — `SentenceId` doesn't cross the wasm boundary
    /// (see the module doc on `DiagnosticJs`/`search_hits_to_js` for why).
    tptp_lines: std::collections::HashMap<sigmakee_rs_core::SentenceId, u32>,
}

#[wasm_bindgen]
impl WasmNativeProver {
    /// Create an empty native-prover knowledge base with default [`Config`].
    ///
    /// Topped by [`ProverLayer<TranslationLayer>`] rather than a bare
    /// [`ProverLayer`] — native proving AND TPTP export off one shared KB
    /// (see [`toTptpIndexed`](Self::to_tptp_indexed)), no dual KB.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner:      KnowledgeBase::new_native_translating(),
            config:     WasmConfig::new(),
            tptp_lines: std::collections::HashMap::new(),
        }
    }

    /// Replace the active [`Config`] used by subsequent [`ask`](Self::ask) calls.
    #[wasm_bindgen]
    pub fn configure(&mut self, config: &WasmConfig) {
        self.config = config.clone();
    }

    /// Set the worker-pool size the KB's `plan_threads` gates should target,
    /// once the caller has spun up a rayon pool via `initThreadPool` (only
    /// exported in threaded builds). `available_parallelism()` reads 1 on
    /// wasm32 regardless of pool size, so without this call every
    /// `plan_threads` gate would serialize even against a live N-worker pool.
    /// A no-op degrade path everywhere else — calling it against a bundle
    /// with no pool just biases planning without any workers to run on.
    #[wasm_bindgen(js_name = setMaxThreads)]
    pub fn set_max_threads(&self, n: u32) {
        self.inner.cache_config().set_max_threads(n as usize);
    }

    /// Load KIF text into the KB under `file_tag` as **axioms**.
    ///
    /// The native prover searches over a promoted axiom base, so this loads the
    /// text and then promotes it into the axiomatic theory
    /// (`make_session_axiomatic`) — the loaded KIF becomes background theory
    /// every subsequent [`ask`](Self::ask) sees.
    ///
    /// Returns a JSON array of error strings, or an empty array on success.
    #[wasm_bindgen(js_name = loadKif)]
    pub fn load_kif(&mut self, kif_text: &str, file_tag: &str) -> Result<JsValue, JsValue> {
        let result = self.inner.load(
            sigmakee_rs_core::SourceFile::kif(std::path::PathBuf::from(file_tag), kif_text.to_string()),
            file_tag,
        );
        let mut errors: Vec<String> = result.diagnostics.iter().map(|e: &sigmakee_rs_core::Diagnostic| e.to_string()).collect();
        // Promote the freshly-loaded source into the searchable axiom base.
        // Skipping this leaves the axioms as inert session support the
        // given-clause loop never force-includes, so queries come back
        // Disproved/Unknown against an effectively empty theory.
        if let Err(e) = self.inner.make_session_axiomatic(file_tag) {
            errors.push(format!("promote failed: {:?}", e));
        }
        serde_wasm_bindgen::to_value(&errors)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Load KIF under `file_tag` WITHOUT promoting it to axioms.
    ///
    /// Enables search / man pages / editing immediately (they read the ingested
    /// store); proving and the full man-page taxonomy require a later
    /// [`promote`](Self::promote). Returns parse-error strings ([] on success).
    #[wasm_bindgen(js_name = ingest)]
    pub fn ingest(&mut self, kif_text: &str, file_tag: &str) -> Result<JsValue, JsValue> {
        let result = self.inner.load(
            sigmakee_rs_core::SourceFile::kif(std::path::PathBuf::from(file_tag), kif_text.to_string()),
            file_tag,
        );
        let errors: Vec<String> = result.diagnostics.iter().map(|e: &sigmakee_rs_core::Diagnostic| e.to_string()).collect();
        serde_wasm_bindgen::to_value(&errors).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Promote a previously-[`ingest`](Self::ingest)ed source into the axiom
    /// base (`make_session_axiomatic`) — the deferred, heavier step that enables
    /// proving. Returns error strings ([] on success).
    #[wasm_bindgen(js_name = promote)]
    pub fn promote(&mut self, file_tag: &str) -> Result<JsValue, JsValue> {
        let mut errors: Vec<String> = Vec::new();
        if let Err(e) = self.inner.make_session_axiomatic(file_tag) {
            errors.push(format!("promote failed: {:?}", e));
        }
        serde_wasm_bindgen::to_value(&errors).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Clausify the KB and return its CNF form as SUO-KIF, via the native
    /// prover's own clausifier — one clause string per array entry. `formula`
    /// clausifies just that ad hoc KIF text instead of the whole KB when
    /// given (the loaded KB is untouched either way).
    #[wasm_bindgen(js_name = clausify)]
    pub fn clausify(&self, formula: Option<String>) -> Result<JsValue, JsValue> {
        let clauses = match formula {
            Some(kif) => self.inner.clausify_formula(&kif),
            None      => self.inner.clausify_all(),
        };
        serde_wasm_bindgen::to_value(&clauses).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Revalidate an edited buffer for `file` **with full KB context**, updating
    /// the live KB to match the buffer.
    ///
    /// Staging the buffer under the file's own name is a diff, so only the
    /// lines that changed are processed — judging a 600KB constituent costs a
    /// handful of sentences, not a re-ingest — and the changed sentences are
    /// validated against the whole KB, so symbol resolution works. The change
    /// is committed and left in place; the KB tracks the editor.
    ///
    /// Prefer [`validate_formula`](Self::validate_formula) for scratch input
    /// that belongs to no file.
    #[wasm_bindgen(js_name = validateBuffer)]
    pub fn validate_buffer(&mut self, file: &str, text: &str) -> Result<JsValue, JsValue> {
        use sigmakee_rs_core::SourceFile;
        let path = std::path::PathBuf::from(file);

        // A syntactically broken buffer must never touch the KB: it parses to
        // zero forms, which the source cache treats as "the file is now empty"
        // and retracts every sentence the file previously contributed —
        // seconds of churn, and search/taxonomy/proofs silently lose the file
        // until the syntax is fixed. Vet the syntax first (parse-only,
        // milliseconds) and report just those findings.
        let parse_diags = sigmakee_rs_core::kif_parse_diagnostics(text, file);
        if !parse_diags.is_empty() {
            return diagnostics_to_js(&parse_diags);
        }

        // Diff the buffer into the file's own session and commit it live: the KB
        // simply tracks what the editor holds. No restore step, so a pure
        // addition emits no FormulaRemoved and cannot trigger the symbol prune.
        let staged = self.inner.stage(SourceFile::kif(path, text.to_string()), file);
        self.inner.commit(file);
        if !staged.ok {
            return diagnostics_to_js(&staged.diagnostics);
        }

        // Whole-file findings, not just the changed sentences': markers are
        // replaced wholesale on the JS side, so a diff-only result would erase
        // every pre-existing finding. Session scope, because buffer-committed
        // sentences are not yet promoted — their declarations are visible only
        // in the file's own session overlay, and Base-scope validation would
        // falsely flag symbols they connect.
        diagnostics_to_js(&self.inner.validate_file_in_session(file, file))
    }

    /// Render the WHOLE KB as TPTP — `lang` `"fof"` (default) or `"tff"`,
    /// `hide_numbers` replaces numeric literals with `n__N` tokens — and
    /// remember which line each axiom landed on, for
    /// [`tptpLineForPosition`](Self::tptp_line_for_position) to consult.
    ///
    /// Intended for an occasional "generate/refresh the TPTP preview" action,
    /// not a per-keystroke call — this re-translates the entire KB (thousands
    /// of axioms for a full SUMO load), whereas `tptpLineForPosition` itself
    /// is cheap and fine to call on every cursor move.
    #[wasm_bindgen(js_name = toTptpIndexed)]
    pub fn to_tptp_indexed(&mut self, lang: Option<String>, hide_numbers: Option<bool>) -> String {
        let opts = TptpOptions {
            lang: match lang.as_deref() {
                Some("tff") => TptpLang::Tff,
                _           => TptpLang::Fof,
            },
            hide_numbers: hide_numbers.unwrap_or(true),
            ..TptpOptions::default()
        };
        self.tptp_lines.clear();
        self.inner.to_tptp_indexed(&opts, None, Some(&mut self.tptp_lines))
    }

    /// The 0-based line in the last [`toTptpIndexed`](Self::to_tptp_indexed)
    /// output that renders the sentence enclosing byte `offset` in `file` —
    /// e.g. wherever the editor's cursor currently sits. `null` when `file`
    /// isn't loaded, `offset` falls outside every sentence, or that sentence
    /// wasn't part of the last generated TPTP (stale index — the KB changed
    /// since; call `toTptpIndexed` again first).
    #[wasm_bindgen(js_name = tptpLineForPosition)]
    pub fn tptp_line_for_position(&self, file: &str, offset: usize) -> Option<u32> {
        let sid = self.inner.sentence_at(file, offset)?;
        self.tptp_lines.get(&sid).copied()
    }

    /// Build a standalone TPTP problem for an external prover: whole-KB
    /// axioms, `assertions` folded in as `hypothesis`-role support (a
    /// scratch session, flushed before returning — never left in the live
    /// KB), and `query_kif` appended as the `conjecture`. Mirrors
    /// [`WasmKnowledgeBase::ask`]'s TPTP assembly, but returns the text
    /// instead of invoking a (synchronous) hook — callers that need an
    /// ASYNC prover (e.g. a WASM build of Vampire, run via `await`) can't
    /// use that hook shape, so they get the text and drive the prover
    /// themselves.
    ///
    /// Empty `assertions` is fine (no session created). Errors (as a
    /// diagnostic-message array) on a query that fails to parse or
    /// produces no sentence.
    ///
    /// `select_all` mirrors [`WasmConfig::selectAll`](WasmConfig::select_all)
    /// on the native backend's `Config`, so ONE toggle in the UI means the
    /// same thing for both: `false` (default) SInE-selects a query-relevant
    /// axiom subset (seeded from the assertions + query, via
    /// [`KnowledgeBase::to_tptp_selected`] — the same selection primitive
    /// the native prover and the CLI's external-prover path both use);
    /// `true` emits the whole promoted KB, unfiltered. `selection_tolerance_pct`
    /// mirrors [`WasmConfig::selectionTolerancePct`](WasmConfig::selection_tolerance_pct)
    /// — ignored when `select_all` is true; `None` uses the engine default
    /// budget. Unlike the native backend's autoscaling loop, this is the
    /// FINAL budget: Vampire runs as a one-shot external engine with no
    /// feedback retry, so a query that needs more of the KB than the given
    /// percentage admits will fail here even though the native backend
    /// might still find it by widening its own selection.
    #[wasm_bindgen(js_name = toTptpForAsk)]
    pub fn to_tptp_for_ask(
        &mut self,
        assertions_kif: &str,
        query_kif:      &str,
        select_all:     Option<bool>,
        selection_tolerance_pct: Option<f64>,
    ) -> Result<String, JsValue> {
        // Assertions and the query go into SEPARATE session tags (rather
        // than one shared tag) so the query's own sids don't need to be
        // set-differenced out of the assertions' — `session_sids(QUERY_TAG)`
        // is exactly the query's sentences, nothing more.
        const ASSERT_TAG: &str = "__vampire_ask_assertions__";
        const QUERY_TAG:  &str = "__vampire_ask_query__";
        if !assertions_kif.trim().is_empty() {
            let r = self.inner.tell(assertions_kif, ASSERT_TAG);
            if !r.ok {
                self.inner.flush_session(ASSERT_TAG);
                let errors: Vec<String> = r.diagnostics.iter().map(|e| e.to_string()).collect();
                return Err(serde_wasm_bindgen::to_value(&errors)
                    .unwrap_or_else(|_| JsValue::from_str("assertions parse error")));
            }
        }
        let query_tell = self.inner.tell(query_kif, QUERY_TAG);
        if !query_tell.ok {
            self.inner.flush_session(ASSERT_TAG);
            self.inner.flush_session(QUERY_TAG);
            let errors: Vec<String> = query_tell.diagnostics.iter().map(|e| e.to_string()).collect();
            return Err(serde_wasm_bindgen::to_value(&errors)
                .unwrap_or_else(|_| JsValue::from_str("query parse error")));
        }
        let query_sids = self.inner.session_sids(QUERY_TAG);
        if query_sids.is_empty() {
            self.inner.flush_session(ASSERT_TAG);
            self.inner.flush_session(QUERY_TAG);
            return Err(JsValue::from_str("No query sentence parsed"));
        }

        let kb_opts = TptpOptions { hide_numbers: true, ..TptpOptions::default() };
        let mut tptp = if select_all.unwrap_or(false) {
            self.inner.to_tptp(&kb_opts, Some(ASSERT_TAG))
        } else {
            // Seed relevance from BOTH the assertions and the query — same
            // shape the native prover's own seed-building uses (support
            // hypotheses + conjecture) — not just the query alone, so an
            // assertion's own vocabulary can pull in axioms it needs too.
            let mut seed_sids = self.inner.session_sids(ASSERT_TAG);
            seed_sids.extend(query_sids.iter().copied());
            self.inner.to_tptp_selected(&kb_opts, &seed_sids, Some(ASSERT_TAG), None, selection_tolerance_pct)
        };

        let q_opts = TptpOptions { query: true, hide_numbers: true, ..TptpOptions::default() };
        for (i, &sid) in query_sids.iter().enumerate() {
            let conj = self.inner.format_sentence_tptp(sid, &q_opts);
            tptp.push_str(&format!("\nfof(query_{}, conjecture, ({})).\n", i, conj));
        }

        self.inner.flush_session(ASSERT_TAG);
        self.inner.flush_session(QUERY_TAG);
        Ok(tptp)
    }

    /// Summary counts describing the loaded KB, for an overview page.
    ///
    /// Returns `{ files, symbols, axioms, rules }`:
    /// * `symbols` — interned names a reader would recognise: KIF variables
    ///   (`?x` / `@row`) and the prover's skolem constants are excluded;
    /// * `axioms` — top-level formulas contributed by the loaded files;
    /// * `rules` — those whose top-level connective is `=>` or `<=>`.
    #[wasm_bindgen]
    pub fn stats(&self) -> Result<JsValue, JsValue> {
        use sigmakee_rs_core::{Element, OpKind};

        // Count ontology terms: exclude KIF variables (`?x`/`@row`), the
        // scope-qualified variable symbols the store interns (`X__<scope>`),
        // and CNF skolem constants.
        let symbols = self.inner.iter_symbols()
            .filter(|(_, name)| !name.starts_with('?') && !name.starts_with('@'))
            .filter(|(_, name)| !self.inner.symbol_is_variable(name))
            .filter(|(_, name)| !self.inner.symbol_is_skolem(name))
            .count();

        // Internal scratch sessions (`__inline(N)__`, `__wasm:…`) are not
        // constituents and would inflate every count.
        let files: Vec<String> = self.inner.iter_files()
            .into_iter().filter(|f| !f.starts_with("__")).collect();

        let mut axioms = 0usize;
        let mut rules  = 0usize;
        for f in &files {
            for sid in self.inner.file_roots(f) {
                axioms += 1;
                if let Some(sent) = self.inner.sentence(sid) {
                    if matches!(sent.elements.first(),
                                Some(Element::Op(OpKind::Implies | OpKind::Iff))) {
                        rules += 1;
                    }
                }
            }
        }

        let v = self.inner.vocab_stats();
        let out = KbStatsJs {
            files: files.len(), symbols, axioms, rules,
            classes: v.classes, instances: v.instances, relations: v.relations,
            predicates: v.predicates, functions: v.functions,
            documented: v.documented, labeled: v.labeled,
            doc_languages: v.doc_languages.into_iter()
                .map(|(language, documented)| DocLangJs { language, documented })
                .collect(),
            term_languages: v.term_languages.into_iter()
                .map(|(language, documented)| DocLangJs { language, documented })
                .collect(),
        };
        serde_wasm_bindgen::to_value(&out).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Freeze the entire KB — promoted axioms, symbols, indices, taxonomy — into
    /// a self-contained byte buffer (a `Uint8Array`).
    ///
    /// The bytes are heed-free and portable: stash them in IndexedDB / a file /
    /// a download, then rebuild the KB on a later visit with
    /// [`restore`](Self::restore) instead of re-ingesting and re-promoting. This
    /// is the browser freeze/thaw seam.
    #[wasm_bindgen]
    pub fn snapshot(&self) -> Result<Vec<u8>, JsValue> {
        self.inner.snapshot_bytes().map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Thaw a KB previously produced by [`snapshot`](Self::snapshot), replacing
    /// this instance's contents. The active [`Config`] is preserved.
    #[wasm_bindgen]
    pub fn restore(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        let kb = KnowledgeBase::<ProverLayer<TranslationLayer>>::restore_from_bytes(bytes)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.inner = kb;
        Ok(())
    }

    /// Assert a single KIF formula into the KB under the given session key.
    ///
    /// `session` defaults to `"default"` if omitted.
    /// Returns `{ ok: bool, errors: string[] }`.
    #[wasm_bindgen]
    pub fn tell(&mut self, kif_text: &str, session: Option<String>) -> Result<JsValue, JsValue> {
        let s = session.as_deref().unwrap_or("default");
        let result = self.inner.tell(kif_text, s);
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"ok".into(), &JsValue::from_bool(result.ok))
            .map_err(|e| JsValue::from_str(&format!("{:?}", e)))?;
        let errors: Vec<String> = result.diagnostics.iter().map(|e| e.to_string()).collect();
        let errs_js = serde_wasm_bindgen::to_value(&errors)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        js_sys::Reflect::set(&obj, &"errors".into(), &errs_js)
            .map_err(|e| JsValue::from_str(&format!("{:?}", e)))?;
        Ok(obj.into())
    }

    /// Remove assertions for a specific session only.
    #[wasm_bindgen(js_name = flushSession)]
    pub fn flush_session(&mut self, session: &str) {
        self.inner.flush_session(session);
    }

    /// Pattern-based lookup.  Returns a JSON array of matched sentence strings.
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

    /// Run semantic validation over the whole KB. Returns a JS `string[]` of
    /// diagnostics (empty ⇒ clean).
    #[wasm_bindgen]
    pub fn validate(&self) -> Result<JsValue, JsValue> {
        diagnostics_to_js(&self.inner.validate_all())
    }

    /// Validate a single inline KIF formula without mutating the KB. Parse
    /// failures come back as diagnostics in the returned `string[]`.
    #[wasm_bindgen(js_name = validateFormula)]
    pub fn validate_formula(&mut self, kif: &str) -> Result<JsValue, JsValue> {
        validate_formula_impl(&mut self.inner, kif)
    }

    /// Validate an Ask/Tell pair (assertions then query) in one scratch
    /// session against the live KB. Returns `{ assertions, query }` arrays.
    #[wasm_bindgen(js_name = validateScratch)]
    pub fn validate_scratch(&mut self, assertions: &str, query: &str) -> Result<JsValue, JsValue> {
        validate_scratch_impl(&mut self.inner, assertions, query)
    }

    /// Full-text / symbol search over the KB. `kind` filters by
    /// `"class"|"relation"|"function"|"predicate"|"instance"|"individual"`,
    /// `language` by tag (e.g. `"EnglishLanguage"`), `limit` caps results.
    /// Returns `{ symbol, kinds, source, language, text }[]`.
    #[wasm_bindgen]
    pub fn search(
        &self,
        query:    &str,
        kind:     Option<String>,
        language: Option<String>,
        limit:    Option<u32>,
    ) -> Result<JsValue, JsValue> {
        let opts = SearchOpts {
            kind:     kind.as_deref().and_then(man_kind_from_str),
            language: language.as_deref(),
            limit:    limit.map(|n| n as usize),
        };
        search_hits_to_js(&self.inner.search(query, &opts))
    }

    /// Structured "man page" for a symbol: kinds, documentation, taxonomy
    /// (parents/children), signature (arity/domains/range), and the full
    /// list of referencing formulas. Returns `null` if the symbol is unknown.
    #[wasm_bindgen]
    pub fn manpage(&self, symbol: &str) -> Result<JsValue, JsValue> {
        manpage_to_js(&self.inner, self.inner.manpage(symbol))
    }

    /// Direct taxonomy edges of `symbol` — `{ parents, children }` — without
    /// the man page's reference scan. Powers lazy taxonomy-tree expansion.
    #[wasm_bindgen]
    pub fn taxonomy(&self, symbol: &str) -> Result<JsValue, JsValue> {
        taxonomy_to_js(&self.inner, symbol)
    }

    /// The `(instance ? NaturalLanguage)` symbols, each with the English label
    /// from its `termFormat` (falling back to the bare symbol name). Sorted by
    /// label, with `EnglishLanguage` guaranteed present. Powers the UI language
    /// selector. Mirrors [`WasmKnowledgeBase::natural_languages`].
    #[wasm_bindgen(js_name = naturalLanguages)]
    pub fn natural_languages(&self) -> Result<JsValue, JsValue> {
        natural_languages_to_js(&self.inner)
    }

    /// Natural-language paraphrase of a single KIF formula in `language`. Empty
    /// when the KIF doesn't parse to a statement.
    #[wasm_bindgen(js_name = renderNl)]
    pub fn render_nl(&self, kif: &str, language: &str) -> String {
        render_nl_string(&self.inner, kif, language)
    }

    /// Audit the whole KB for logical consistency via the native saturation
    /// prover — enumerates up to `limit` (default 5) distinct contradictions,
    /// each cited back to `file:line` wherever a step traces to an input
    /// axiom. In-browser analogue of the `sumo audit` CLI command; uses the
    /// active [`Config`] (set via [`configure`](Self::configure)) for its
    /// time/step budget.
    ///
    /// Returns a JS object:
    ///
    /// * `status` — one of `"Consistent"`, `"Inconsistent"`, `"Timeout"`,
    ///   `"InputError"`, `"Unknown"`;
    /// * `inconsistent` — `true` iff `status === "Inconsistent"`;
    /// * `given_steps` — given-clause steps the native loop executed (or `null`);
    /// * `raw_output` — the engine's human-readable trace;
    /// * `contradictions` — one entry per distinct contradiction found, each
    ///   `{ steps: { index, rule, premises, kif, file, line }[], graphviz }`;
    ///   `file`/`line` are `null` for derived/anonymous steps that don't trace
    ///   to an input axiom; `graphviz` is that contradiction's derivation
    ///   rendered as a DOT digraph.
    #[wasm_bindgen(js_name = auditConsistency)]
    pub fn audit_consistency(&self, limit: Option<u32>) -> Result<JsValue, JsValue> {
        let opts = self.config.to_native_opts(self.inner.sine_axiom_count());
        let result = self.inner.audit_consistency(&[], opts, limit.unwrap_or(5) as usize);
        let src_idx = self.inner.build_axiom_source_index();

        let contradictions: Vec<ContradictionJs> = result.contradiction_proofs.iter().enumerate().map(|(i, steps)| {
            // A contradiction has no conjecture to restate — it refutes the KB
            // itself — so the prose opens straight into the derivation. Reuse
            // the one index built above: rendering N contradictions would
            // otherwise repeat a whole-KB fingerprint pass N times.
            let prose_report = self.inner.render_proof_prose_with(
                None, steps, "EnglishLanguage", &src_idx);
            ContradictionJs {
                graphviz: sigmakee_rs_core::render_graphviz(steps, &format!("contradiction-{}", i + 1), "Inconsistent"),
                prose:         prose_report.rendered,
                prose_missing: prose_report.missing,
                steps: proof_steps_js(steps, &src_idx),
            }
        }).collect();

        let out = AuditResultJs {
            status:         format!("{:?}", result.status),
            inconsistent:   result.status == sigmakee_rs_core::ProverStatus::Inconsistent,
            given_steps:    result.given_steps,
            raw_output:     result.raw_output,
            contradictions,
        };
        serde_wasm_bindgen::to_value(&out).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Prove `query_kif` (a single KIF conjecture) in-browser against the KB
    /// plus optional `session` support, using the active [`Config`] (set via
    /// [`configure`](Self::configure)).
    ///
    /// The wall-clock deadline (`Config.timeLimitSecs`) is enforced through
    /// `Date.now()`; termination is also bounded by the step budget
    /// (`Config.maxSteps`), so a query cannot run unbounded.
    ///
    /// Returns a JS object describing the outcome:
    ///
    /// * `status` — one of `"Proved"`, `"Disproved"`, `"Consistent"`,
    ///   `"Inconsistent"`, `"Timeout"`, `"InputError"`, `"Unknown"`;
    /// * `proved` — `true` iff `status === "Proved"`;
    /// * `given_steps` — given-clause steps the native loop executed (or `null`);
    /// * `raw_output` — the engine's human-readable trace;
    /// * `proof` — on `Proved`, the SUO-KIF proof as
    ///   `{ index, rule, premises, kif }[]` (empty otherwise);
    /// * `graphviz` — the same proof rendered as a Graphviz DOT digraph
    ///   (always a syntactically valid graph, even when `proof` is empty).
    #[wasm_bindgen]
    pub fn ask(
        &self,
        query_kif: &str,
        session:   Option<String>,
    ) -> Result<JsValue, JsValue> {
        let opts   = self.config.to_native_opts(self.inner.sine_axiom_count());
        let sine   = opts.selection.clone();
        let result = self.inner.ask_query(query_kif, session.as_deref(), sine, opts);

        // Curated, JS-safe projection of `ProverResult`.  We deliberately do
        // NOT serialize the raw result: its `bindings`/`proof_kif` carry u64
        // symbol/sentence hashes that overflow JS's safe-integer range and
        // abort serde-wasm-bindgen.  Proof formulas render to KIF text via
        // `AstNode`'s `Display`; every field here is `usize`/`String`/`bool`.
        let status_str = format!("{:?}", result.status);
        let graphviz = sigmakee_rs_core::render_graphviz(&result.proof_kif, "ask", &status_str);

        // Building the source index walks and fingerprints every root sentence,
        // so do it once and only when there is actually a proof to cite — an
        // unproved ask (Timeout/Unknown) would otherwise pay a whole-KB pass to
        // project an empty vec.
        let (proof, prose, prose_missing) = if result.proof_kif.is_empty() {
            (Vec::new(), String::new(), Vec::new())
        } else {
            let src_idx = self.inner.build_axiom_source_index();
            let proof = proof_steps_js(&result.proof_kif, &src_idx);
            // The goal restatement needs the conjecture as an AST, so re-parse
            // the query (cheap — one formula); a parse failure just drops the
            // opener, it never fails the ask.
            let goal_doc = sigmakee_rs_core::parse_document(
                "__prose_goal__", query_kif.to_string(), sigmakee_rs_core::Parser::Kif);
            let goal_ast = goal_doc.ast.iter().find_map(|d| d.as_stmt());
            let report = self.inner.render_proof_prose_with(
                goal_ast, &result.proof_kif, "EnglishLanguage", &src_idx);
            (proof, report.rendered, report.missing)
        };

        let out = AskResultJs {
            status:      status_str,
            proved:      result.status == sigmakee_rs_core::ProverStatus::Proved,
            given_steps: result.given_steps,
            raw_output:  result.raw_output,
            proof,
            graphviz,
            prose,
            prose_missing,
        };
        serde_wasm_bindgen::to_value(&out)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

/// Project a proof/contradiction transcript to JS-safe steps, citing each
/// step's source axiom where it has one.
///
/// A refutation proof and an audit contradiction are the same shape, so both
/// endpoints share this — and the UI renders them through one code path.
fn proof_steps_js(
    steps:   &[sigmakee_rs_core::KifProofStep],
    src_idx: &sigmakee_rs_core::AxiomSourceIndex,
) -> Vec<ProofStepJs> {
    steps.iter().map(|s| {
        let loc = s.source_sid.and_then(|sid| src_idx.lookup_by_sid(sid));
        ProofStepJs {
            index:    s.index,
            rule:     s.rule.clone(),
            premises: s.premises.clone(),
            kif:      s.formula.format_plain(0),
            file:     loc.map(|a| a.file.clone()),
            line:     loc.map(|a| a.line),
        }
    }).collect()
}

/// Summary counts describing the loaded KB (see [`WasmNativeProver::stats`]).
/// The vocabulary/coverage fields come from `KnowledgeBase::vocab_stats`;
/// `documented`/`labeled` divide by `symbols` for a coverage percentage.
#[derive(serde::Serialize)]
struct KbStatsJs {
    files:      usize,
    symbols:    usize,
    axioms:     usize,
    rules:      usize,
    classes:    usize,
    instances:  usize,
    relations:  usize,
    predicates: usize,
    functions:  usize,
    documented: usize,
    labeled:    usize,
    doc_languages:  Vec<DocLangJs>,
    term_languages: Vec<DocLangJs>,
}

/// One language's documentation coverage (see `KbStatsJs.doc_languages`).
#[derive(serde::Serialize)]
struct DocLangJs {
    language:   String,
    documented: usize,
}

/// Curated native-prover result projected to JS-safe types.
#[derive(serde::Serialize)]
struct AskResultJs {
    status:      String,
    proved:      bool,
    given_steps: Option<usize>,
    raw_output:  String,
    proof:       Vec<ProofStepJs>,
    /// The proof rendered as a Graphviz DOT digraph (one node per step, one
    /// edge per premise) — always a syntactically valid graph, even when
    /// `proof` is empty. Safe to hand straight to a DOT renderer.
    graphviz:    String,
    /// The proof narrated as connected English prose (goal restatement, the
    /// axioms/hypotheses used, then the derivation chain). Empty when there is
    /// no proof to narrate.
    prose:       String,
    /// Symbols the prose showed by bare name because they have no
    /// `format`/`termFormat` in the rendering language. Sorted, de-duplicated.
    prose_missing: Vec<String>,
}

/// One step of a cited derivation — a refutation proof or an audit
/// contradiction; both endpoints project to this single shape.
#[derive(serde::Serialize)]
struct ProofStepJs {
    index:    usize,
    rule:     String,
    premises: Vec<usize>,
    kif:      String,
    file:     Option<String>,
    line:     Option<u32>,
}

/// One distinct contradiction the audit found — a full derivation to `FALSE`.
#[derive(serde::Serialize)]
struct ContradictionJs {
    steps:    Vec<ProofStepJs>,
    /// This contradiction's derivation rendered as a Graphviz DOT digraph.
    graphviz: String,
    /// This contradiction's derivation narrated as connected English prose.
    prose:    String,
    /// Symbols the prose showed by bare name (no `format`/`termFormat`).
    prose_missing: Vec<String>,
}

/// Curated native-prover consistency-audit result projected to JS-safe types.
#[derive(serde::Serialize)]
struct AuditResultJs {
    status:         String,
    inconsistent:   bool,
    given_steps:    Option<usize>,
    raw_output:     String,
    contradictions: Vec<ContradictionJs>,
}

// -- Shared projections for validate / search / manpage ------------------------
//
// The core `SearchHit`/`ManPage` carry `SentenceId`/`SymbolId` (u64) fields that
// overflow JS's safe-integer range, so — as with `AskResultJs` — we project to
// curated structs of JS-safe types (String/usize/bool/i32) rather than
// serializing the raw values.  `validate` / `search` / `manpage` themselves are
// backend-agnostic (`impl<L: TopLayer + Layer> KnowledgeBase<L>`), so both
// `WasmNativeProver` and `WasmKnowledgeBase` call these helpers on `self.inner`.

/// A JS-safe diagnostic: severity/kind/code/message plus the source location
/// (`file`, 1-based `line`/`col` and end position) from the diagnostic's span.
/// The internal sentence-id list is dropped.
#[derive(serde::Serialize)]
struct DiagnosticJs {
    severity: String,
    kind:     String,
    code:     String,
    message:  String,
    file:     String,
    line:     u32,
    col:      u32,
    end_line: u32,
    end_col:  u32,
}

/// Serialize a diagnostics list to structured JS objects (see [`DiagnosticJs`]).
fn diagnostics_to_js(diags: &[sigmakee_rs_core::Diagnostic]) -> Result<JsValue, JsValue> {
    let out: Vec<DiagnosticJs> = diags.iter().map(|d| DiagnosticJs {
        severity: d.severity.as_str().to_string(),
        kind:     d.kind.to_string(),
        code:     d.code.to_string(),
        message:  d.message.clone(),
        file:     d.range.file.clone(),
        line:     d.range.line,
        col:      d.range.col,
        end_line: d.range.end_line,
        end_col:  d.range.end_col,
    }).collect();
    serde_wasm_bindgen::to_value(&out).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Validate one inline formula against a scratch session, then flush it so the
/// KB is left untouched (mirrors `Session::validate_formula`).  Generic over the
/// backend: `TopLayer: Layer`, so the `validate_sentence` bound is satisfied.
fn validate_formula_impl<L: TopLayer>(
    kb:  &mut KnowledgeBase<L>,
    kif: &str,
) -> Result<JsValue, JsValue> {
    const TAG: &str = "__wasm:validate_formula__";
    let r = kb.tell(kif, TAG);
    if !r.ok {
        kb.flush_session(TAG);
        return diagnostics_to_js(&r.diagnostics); // parse failures are findings
    }
    let sids = kb.session_sids(TAG);
    let mut diags = Vec::new();
    for sid in sids {
        // Session scope: symbols the scratch input itself declares are only
        // visible in the session overlay.
        diags.extend(kb.validate_sentence_in_session(sid, TAG));
    }
    kb.flush_session(TAG);
    diagnostics_to_js(&diags)
}

/// Validate an Ask/Tell pair in ONE scratch session against the live KB: the
/// assertions are told first, then the query is validated with those
/// declarations in scope.  Returns `{ assertions: Diagnostic[], query:
/// Diagnostic[] }`; the session is flushed either way.
/// Parse a `.kif.tq` test file. Pure: no KB, no state. Throws with the
/// parse diagnostic's message on malformed input.
#[wasm_bindgen(js_name = parseTest)]
pub fn parse_test(name: &str, text: &str) -> Result<JsValue, JsValue> {
    let tc = sigmakee_rs_core::parse_test_content(text, name)
        .map_err(|d| JsValue::from_str(&d.to_string()))?;
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct TestJs {
        name:            String,
        note:            String,
        timeout:         u32,
        query_kif:       Option<String>,
        axiom_kif:       String,
        expected_proof:  Option<bool>,
        expected_answer: Option<Vec<String>>,
        extra_files:     Vec<String>,
    }
    let out = TestJs {
        name:            tc.file_name.clone(),
        note:            tc.note.clone(),
        timeout:         tc.timeout,
        query_kif:       tc.query_kif(),
        axiom_kif:       tc.axiom_kif(),
        expected_proof:  tc.expected_proof,
        expected_answer: tc.expected_answer.clone(),
        extra_files:     tc.extra_files.clone(),
    };
    serde_wasm_bindgen::to_value(&out).map_err(|e| JsValue::from_str(&e.to_string()))
}

fn validate_scratch_impl<L: TopLayer>(
    kb:         &mut KnowledgeBase<L>,
    assertions: &str,
    query:      &str,
) -> Result<JsValue, JsValue> {
    const TAG: &str = "__wasm:validate_scratch__";

    let collect = |kif: &str, kb: &mut KnowledgeBase<L>| -> Vec<sigmakee_rs_core::Diagnostic> {
        if kif.trim().is_empty() { return Vec::new(); }
        let before: std::collections::HashSet<_> = kb.session_sids(TAG).into_iter().collect();
        let r = kb.tell(kif, TAG);
        if !r.ok {
            return r.diagnostics;
        }
        kb.session_sids(TAG).into_iter()
            .filter(|sid| !before.contains(sid))
            .flat_map(|sid| kb.validate_sentence_in_session(sid, TAG))
            .collect()
    };

    kb.flush_session(TAG);
    let a_diags = collect(assertions, kb);
    let q_diags = collect(query, kb);
    kb.flush_session(TAG);

    #[derive(serde::Serialize)]
    struct Out { assertions: Vec<DiagnosticJs>, query: Vec<DiagnosticJs> }
    let to_js = |diags: Vec<sigmakee_rs_core::Diagnostic>| diags.iter().map(|d| DiagnosticJs {
        severity: d.severity.as_str().to_string(),
        kind:     d.kind.to_string(),
        code:     d.code.to_string(),
        message:  d.message.clone(),
        file:     d.range.file.clone(),
        line:     d.range.line,
        col:      d.range.col,
        end_line: d.range.end_line,
        end_col:  d.range.end_col,
    }).collect::<Vec<_>>();
    let out = Out { assertions: to_js(a_diags), query: to_js(q_diags) };
    serde_wasm_bindgen::to_value(&out).map_err(|e| JsValue::from_str(&e.to_string()))
}

#[derive(serde::Serialize)]
struct SearchHitJs {
    symbol:   String,
    kinds:    Vec<String>,
    source:   String,
    language: String,
    text:     String,
    rank:     f32,
}

/// Project search hits to JS-safe objects (dropping each hit's internal `sid`).
fn search_hits_to_js(hits: &[SearchHit]) -> Result<JsValue, JsValue> {
    let out: Vec<SearchHitJs> = hits.iter().map(|h| SearchHitJs {
        symbol:   h.symbol.clone(),
        kinds:    h.kinds.iter().map(|k| k.as_str().to_string()).collect(),
        source:   h.source.as_str().to_string(),
        language: h.language.clone(),
        text:     h.text.clone(),
        rank:     h.rank,
    }).collect();
    serde_wasm_bindgen::to_value(&out).map_err(|e| JsValue::from_str(&e.to_string()))
}

fn man_kind_from_str(s: &str) -> Option<ManKind> {
    match s.to_ascii_lowercase().as_str() {
        "class"      => Some(ManKind::Class),
        "relation"   => Some(ManKind::Relation),
        "function"   => Some(ManKind::Function),
        "predicate"  => Some(ManKind::Predicate),
        "instance"   => Some(ManKind::Instance),
        "individual" => Some(ManKind::Individual),
        _            => None,
    }
}

#[derive(serde::Serialize)]
struct DocJs { language: String, text: String }
#[derive(serde::Serialize)]
struct EdgeJs { relation: String, parent: String }
#[derive(serde::Serialize)]
struct SortJs { class: String, subclass: bool }
#[derive(serde::Serialize)]
struct DomainJs { position: usize, sort: SortJs }

/// One formula that references the man-paged symbol: its rendered KIF text
/// plus source location (when the sentence has one — synthetic/CNF sentences
/// don't). `position` is the symbol's 0-based root-level position in the
/// sentence, or `null` when it only occurs nested inside a sub-sentence.
///
/// `kind` / `arg_pos` classify the formula's top-level shape for the reference
/// filter (see [`classify_reference`]): `kind` is `"fact"` (a relation atom,
/// possibly under `not`), `"=>"`, `"<=>"`, `"and"`, `"or"`, or `"other"`; for a
/// fact, `arg_pos` is the symbol's argument index in the atom (0 = the relation
/// itself), after peeling one top-level `not`, or `null` when it isn't a direct
/// argument.
#[derive(serde::Serialize)]
struct ManPageRefJs {
    position: Option<usize>,
    kif:      String,
    file:     Option<String>,
    line:     Option<u32>,
    kind:     String,
    arg_pos:  Option<usize>,
}

/// A JS-safe projection of `ManPage` — the human-facing fields, with the raw
/// `SentenceId`/`SymbolId` reference lists resolved to rendered KIF + source
/// location (see [`ManPageRefJs`]) rather than dropped.
#[derive(serde::Serialize)]
struct ManPageJs {
    name:             String,
    kinds:            Vec<String>,
    documentation:    Vec<DocJs>,
    term_format:      Vec<DocJs>,
    format:           Vec<DocJs>,
    parents:          Vec<EdgeJs>,
    children:         Vec<EdgeJs>,
    arity:            Option<i32>,
    domains:          Vec<DomainJs>,
    range:            Option<SortJs>,
    appears_in_count: usize,
    consequent_count: usize,
    references:       Vec<ManPageRefJs>,
}

/// Direct taxonomy edges of `symbol` as `{ parents, children }` of
/// `{ relation, parent }` rows (downward rows carry the *child* in `parent`,
/// matching `ManPageJs.children`). The lightweight peer of `manpage` for the
/// lazily-expanded taxonomy tree. Shared by both backends' `taxonomy` binding.
fn taxonomy_to_js<L: TopLayer>(kb: &KnowledgeBase<L>, symbol: &str) -> Result<JsValue, JsValue> {
    #[derive(serde::Serialize)]
    struct TaxJs { parents: Vec<EdgeJs>, children: Vec<EdgeJs> }
    let (parents, children) = kb.taxonomy_edges(symbol);
    let edge = |e: sigmakee_rs_core::ParentEdge| EdgeJs { relation: e.relation, parent: e.parent };
    let tax = TaxJs {
        parents:  parents.into_iter().map(edge).collect(),
        children: children.into_iter().map(edge).collect(),
    };
    serde_wasm_bindgen::to_value(&tax).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// `(instance ? NaturalLanguage)` symbols — including instances of
/// `NaturalLanguage` subclasses like `ChineseLanguage` — with English
/// `termFormat` labels, for the UI language selector. Shared by both backends'
/// `naturalLanguages` binding.
fn natural_languages_to_js<L: TopLayer>(kb: &KnowledgeBase<L>) -> Result<JsValue, JsValue> {
    #[derive(serde::Serialize)]
    struct LangJs { symbol: String, label: String }
    let mut langs: Vec<LangJs> = Vec::new();
    for symbol in kb.instances_of("NaturalLanguage") {
        let label = kb.term_format(&symbol, Some("EnglishLanguage"))
            .first().map(|d| d.text.clone())
            .unwrap_or_else(|| symbol.clone());
        langs.push(LangJs { symbol, label });
    }
    if !langs.iter().any(|l| l.symbol == "EnglishLanguage") {
        langs.push(LangJs { symbol: "EnglishLanguage".into(), label: "English".into() });
    }
    langs.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    serde_wasm_bindgen::to_value(&langs).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Natural-language paraphrase of a single KIF formula in `language`, using the
/// KB's format / termFormat templates. Empty string when the KIF does not parse
/// to a statement. Shared by both backends' `renderNl` binding.
fn render_nl_string<L: TopLayer>(kb: &KnowledgeBase<L>, kif: &str, language: &str) -> String {
    let doc = sigmakee_rs_core::parse_document(
        "__wasm:render_nl__", kif.to_string(), sigmakee_rs_core::Parser::Kif);
    match doc.ast.iter().find_map(|d| d.as_stmt()) {
        Some(ast) => kb.render_formula(ast, language).rendered,
        None => String::new(),
    }
}

/// Classify a reference formula's top-level shape for the man-page filter.
/// Returns `(kind, arg_pos)`:
///   - `kind`: `"fact"` (relation atom, possibly wrapped in a single `not`),
///     `"=>"`, `"<=>"`, `"and"`, `"or"`, or `"other"` (quantifier / equal /
///     unresolvable). A leading `not` is transparent — it's peeled before
///     classifying, so `(not (p a b))` is a fact and `(not (=> a b))` an `"=>"`.
///   - `arg_pos`: for a fact, `target`'s argument index in the atom (0 = the
///     relation symbol, 1 = first argument, …); `None` when `target` isn't a
///     direct argument (e.g. it sits inside a nested function term) or the
///     formula isn't a fact.
fn classify_reference<L: TopLayer>(
    kb:     &KnowledgeBase<L>,
    sid:    sigmakee_rs_core::SentenceId,
    target: sigmakee_rs_core::SymbolId,
) -> (String, Option<usize>) {
    use sigmakee_rs_core::{Element, OpKind};
    let Some(root) = kb.sentence(sid) else { return ("other".into(), None) };
    // A leading `not` wraps its operand as a nested sub-sentence; peel one.
    let atom = if matches!(root.op(), Some(OpKind::Not)) {
        match root.elements.get(1) {
            Some(Element::Sub(inner)) => kb.sentence(*inner),
            _                         => Some(root.clone()),
        }
    } else {
        Some(root.clone())
    };
    let Some(atom) = atom else { return ("other".into(), None) };
    match atom.elements.first() {
        Some(Element::Symbol(_)) => {
            let pos = atom.elements.iter()
                .position(|el| matches!(el, Element::Symbol(s) if s.id() == target));
            ("fact".into(), pos)
        }
        Some(Element::Op(op)) => {
            let k = match op {
                OpKind::Implies => "=>",
                OpKind::Iff     => "<=>",
                OpKind::And     => "and",
                OpKind::Or      => "or",
                _               => "other",
            };
            (k.into(), None)
        }
        _ => ("other".into(), None),
    }
}

fn manpage_to_js<L: TopLayer>(kb: &KnowledgeBase<L>, page: Option<ManPage>) -> Result<JsValue, JsValue> {
    let Some(p) = page else { return Ok(JsValue::NULL) };
    let docs = |v: &[sigmakee_rs_core::DocEntry]| -> Vec<DocJs> {
        v.iter().map(|d| DocJs { language: d.language.clone(), text: d.text.clone() }).collect()
    };
    let edges = |v: &[sigmakee_rs_core::ParentEdge]| -> Vec<EdgeJs> {
        v.iter().map(|e| EdgeJs { relation: e.relation.clone(), parent: e.parent.clone() }).collect()
    };
    let sort = |s: &sigmakee_rs_core::SortSig| SortJs { class: s.class.clone(), subclass: s.subclass };
    let target = kb.symbol_id(&p.name);
    let reference = |sid: sigmakee_rs_core::SentenceId, position: Option<usize>| -> ManPageRefJs {
        let span = sigmakee_rs_core::DiagnosticSource::sentence_location(kb, sid);
        let (kind, arg_pos) = match target {
            Some(t) => classify_reference(kb, sid, t),
            None    => ("other".to_string(), None),
        };
        ManPageRefJs {
            position,
            kif:  kb.pretty_print_sentence_plain(sid, 0),
            file: span.as_ref().map(|s| s.file.clone()),
            line: span.as_ref().map(|s| s.line),
            kind,
            arg_pos,
        }
    };
    let mut references: Vec<ManPageRefJs> = p.ref_args.iter()
        .map(|sigmakee_rs_core::SentenceRef(pos, sid)| reference(*sid, Some(*pos)))
        .collect();
    references.extend(p.ref_nested.iter().map(|&sid| reference(sid, None)));
    let out = ManPageJs {
        name:             p.name.clone(),
        kinds:            p.kinds.iter().map(|k| k.as_str().to_string()).collect(),
        documentation:    docs(&p.documentation),
        term_format:      docs(&p.term_format),
        format:           docs(&p.format),
        parents:          edges(&p.parents),
        children:         edges(&p.children),
        arity:            p.arity,
        domains:          p.domains.iter().map(|(pos, s)| DomainJs { position: *pos, sort: sort(s) }).collect(),
        range:            p.range.as_ref().map(sort),
        appears_in_count: p.appears_in_count,
        consequent_count: p.consequent_count,
        references,
    };
    serde_wasm_bindgen::to_value(&out).map_err(|e| JsValue::from_str(&e.to_string()))
}
