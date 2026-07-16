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
}

impl WasmConfig {
    /// Build a runtime [`NativeOpts`] seeded with these defaults; per-query
    /// `session` is layered on by the caller.  Mirrors
    /// `NativeProverConfig::to_native_opts`.
    fn to_native_opts(&self) -> NativeOpts {
        NativeOpts {
            time_limit_secs: self.time_limit_secs,
            max_steps:       self.max_steps,
            max_lits:        self.max_lits,
            forward_close:   self.forward_close,
            want_proof:      self.want_proof,
            profile:         self.profile,
            ..NativeOpts::default()
        }
    }
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
    inner:  KnowledgeBase<ProverLayer>,
    config: WasmConfig,
}

#[wasm_bindgen]
impl WasmNativeProver {
    /// Create an empty native-prover knowledge base with default [`Config`].
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self { inner: KnowledgeBase::new_native(), config: WasmConfig::new() }
    }

    /// Replace the active [`Config`] used by subsequent [`ask`](Self::ask) calls.
    #[wasm_bindgen]
    pub fn configure(&mut self, config: &WasmConfig) {
        self.config = config.clone();
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
    ///   `{ steps: { index, rule, premises, kif, file, line }[] }`; `file`/`line`
    ///   are `null` for derived/anonymous steps that don't trace to an input axiom.
    #[wasm_bindgen(js_name = auditConsistency)]
    pub fn audit_consistency(&self, limit: Option<u32>) -> Result<JsValue, JsValue> {
        let opts = self.config.to_native_opts();
        let result = self.inner.audit_consistency(&[], opts, limit.unwrap_or(5) as usize);
        let src_idx = self.inner.build_axiom_source_index();

        let contradictions: Vec<ContradictionJs> = result.contradiction_proofs.iter().map(|steps| {
            ContradictionJs {
                steps: steps.iter().map(|s| {
                    let loc = s.source_sid.and_then(|sid| src_idx.lookup_by_sid(sid));
                    AuditStepJs {
                        index:    s.index,
                        rule:     s.rule.clone(),
                        premises: s.premises.clone(),
                        kif:      s.formula.format_plain(0),
                        file:     loc.map(|a| a.file.clone()),
                        line:     loc.map(|a| a.line),
                    }
                }).collect(),
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
    ///   `{ index, rule, premises, kif }[]` (empty otherwise).
    #[wasm_bindgen]
    pub fn ask(
        &self,
        query_kif: &str,
        session:   Option<String>,
    ) -> Result<JsValue, JsValue> {
        let opts   = self.config.to_native_opts();
        let sine   = opts.selection.clone();
        let result = self.inner.ask_query(query_kif, session.as_deref(), sine, opts);

        // Curated, JS-safe projection of `ProverResult`.  We deliberately do
        // NOT serialize the raw result: its `bindings`/`proof_kif` carry u64
        // symbol/sentence hashes that overflow JS's safe-integer range and
        // abort serde-wasm-bindgen.  Proof formulas render to KIF text via
        // `AstNode`'s `Display`; every field here is `usize`/`String`/`bool`.
        let proof: Vec<ProofStepJs> = result.proof_kif.iter().map(|s| ProofStepJs {
            index:    s.index,
            rule:     s.rule.clone(),
            premises: s.premises.clone(),
            kif:      format!("{}", s.formula),
        }).collect();

        let out = AskResultJs {
            status:      format!("{:?}", result.status),
            proved:      result.status == sigmakee_rs_core::ProverStatus::Proved,
            given_steps: result.given_steps,
            raw_output:  result.raw_output,
            proof,
        };
        serde_wasm_bindgen::to_value(&out)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

/// One SUO-KIF proof step, JS-safe (no internal hash IDs).
#[derive(serde::Serialize)]
struct ProofStepJs {
    index:    usize,
    rule:     String,
    premises: Vec<usize>,
    kif:      String,
}

/// Curated native-prover result projected to JS-safe types.
#[derive(serde::Serialize)]
struct AskResultJs {
    status:      String,
    proved:      bool,
    given_steps: Option<usize>,
    raw_output:  String,
    proof:       Vec<ProofStepJs>,
}

/// One step of a cited contradiction derivation (see [`WasmNativeProver::audit_consistency`]).
#[derive(serde::Serialize)]
struct AuditStepJs {
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
    steps: Vec<AuditStepJs>,
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
        diags.extend(kb.validate_sentence(sid));
    }
    kb.flush_session(TAG);
    diagnostics_to_js(&diags)
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
#[derive(serde::Serialize)]
struct ManPageRefJs {
    position: Option<usize>,
    kif:      String,
    file:     Option<String>,
    line:     Option<u32>,
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

fn manpage_to_js<L: TopLayer>(kb: &KnowledgeBase<L>, page: Option<ManPage>) -> Result<JsValue, JsValue> {
    let Some(p) = page else { return Ok(JsValue::NULL) };
    let docs = |v: &[sigmakee_rs_core::DocEntry]| -> Vec<DocJs> {
        v.iter().map(|d| DocJs { language: d.language.clone(), text: d.text.clone() }).collect()
    };
    let edges = |v: &[sigmakee_rs_core::ParentEdge]| -> Vec<EdgeJs> {
        v.iter().map(|e| EdgeJs { relation: e.relation.clone(), parent: e.parent.clone() }).collect()
    };
    let sort = |s: &sigmakee_rs_core::SortSig| SortJs { class: s.class.clone(), subclass: s.subclass };
    let reference = |sid: sigmakee_rs_core::SentenceId, position: Option<usize>| -> ManPageRefJs {
        let span = sigmakee_rs_core::DiagnosticSource::sentence_location(kb, sid);
        ManPageRefJs {
            position,
            kif:  kb.pretty_print_sentence_plain(sid, 0),
            file: span.as_ref().map(|s| s.file.clone()),
            line: span.as_ref().map(|s| s.line),
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
