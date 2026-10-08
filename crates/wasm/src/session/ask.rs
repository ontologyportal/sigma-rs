// crates/wasm/src/session/ask.rs
//
// Proving ops: tell / ask / audit / clausify, plus parsing captured Vampire
// transcripts into the same shapes -- the wasm face of the SDK's
// `session/ask.rs`.

use wasm_bindgen::prelude::*;

use sigmakee_rs_core::SentenceId;
use sigmakee_rs_sdk::AuditFocusView;

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
    /// (see [`crate::external_prover`]).
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
            Backend::Vampire | Backend::E => {
                let opts = self.config.to_external_opts(axiom_count);
                let mut view =
                    session_guard.ask_view_dialect(query, session.as_deref(), &opts, dialect);
                view.input_tptp = self.runner.last_tptp();
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
    /// depth?, focus?, at? }`: sweep seed (default 0), start position (0),
    /// sentences to check (1), sentences per subproblem (1), distinct
    /// contradictions to stop at (5), an optional loaded file tag to restrict
    /// the sweep to, and the subproblem size -- the most axioms a
    /// neighborhood may select and the SInE expansion depth -- overriding the
    /// config's selection budget when given. `focus` (a tracked source
    /// formula, native only) or `at: { file, offset }` (the sentence enclosing
    /// UTF-8 byte `offset` of loaded file `file`) audits that one sentence
    /// instead of a sweep (`seed` and `scope` are then ignored), and fails
    /// when it can't be resolved.
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
        let mut sample = sigmakee_rs_core::AuditSample {
            seed: req.seed,
            step: req.step,
            count: req.count,
            batch: req.batch.max(1),
            limit: req.limit.max(1),
        };
        let session_guard = self.session.read().expect("kb lock not poisoned");
        let focus = req
            .focus
            .as_ref()
            .map(|focus| {
                resolve_audit_focus(session_guard.kb(), focus).map_err(|e| JsValue::from_str(&e))
            })
            .transpose()?;
        let at = req
            .at
            .as_ref()
            .map(|at| {
                session_guard
                    .kb()
                    .sentence_at(&at.file, at.offset)
                    .ok_or_else(|| {
                        JsValue::from_str(&format!(
                            "no sentence at byte {} of {}",
                            at.offset, at.file
                        ))
                    })
            })
            .transpose()?;
        let targeted: Vec<SentenceId> = focus.or(at).into_iter().collect();
        if !targeted.is_empty() {
            sample.step = 0;
            sample.count = 1;
            sample.batch = 1;
        }
        let target = if targeted.is_empty() {
            sigmakee_rs_sdk::AuditTarget::Sweep(req.scope.as_deref())
        } else {
            sigmakee_rs_sdk::AuditTarget::Sentences(&targeted)
        };
        let axiom_count = session_guard.kb().sine_axiom_count();
        match self.config.selected_backend() {
            Backend::Native => {
                let mut opts = self.config.to_native_opts(axiom_count);
                req.size(&mut opts.selection);
                to_js(&session_guard.audit_view_native(opts, sample, target))
            }
            Backend::Vampire | Backend::E => {
                if focus.is_some() {
                    return Err(JsValue::from_str(
                        "Targeted audit rechecks require the native backend.",
                    ));
                }
                let mut opts = self.config.to_external_opts(axiom_count);
                req.size(&mut opts.selection);
                self.runner.set_audit_limit(sample.limit);
                self.runner
                    .set_selection_budget(opts.selection.auto_budget.unwrap_or(axiom_count));
                to_js(&session_guard.audit_view(opts, sample, target))
            }
        }
    }

    /// Parse formula identities in source order, without mutating the KB.
    #[wasm_bindgen(js_name = auditDocument)]
    pub fn audit_document(&self, file: &str, text: &str) -> Result<JsValue, JsValue> {
        let doc = sigmakee_rs_core::parse_document(
            file,
            text,
            sigmakee_rs_core::Parser::Kif { options: None },
        );
        if doc.has_errors() {
            return Err(JsValue::from_str(
                "Fix parse errors before rechecking contradictions.",
            ));
        }
        to_js(
            &doc.root_hashes
                .iter()
                .map(|fp| format!("{fp:016x}"))
                .collect::<Vec<_>>(),
        )
    }

    /// The live engine's source identities, including unsaved editor changes.
    #[wasm_bindgen(js_name = auditSources)]
    pub fn audit_sources(&self) -> Result<JsValue, JsValue> {
        let guard = self.session.read().expect("kb lock not poisoned");
        let kb = guard.kb();
        let files: Vec<_> = kb
            .iter_files()
            .into_iter()
            .map(|file| {
                let hashes: Vec<_> = kb
                    .file_hashes(&file)
                    .into_iter()
                    .map(|fp| format!("{fp:016x}"))
                    .collect();
                AuditSourceFile { file, keys: hashes }
            })
            .collect();
        to_js(&files)
    }

    /// Resolve reported sweep positions to source-backed targets before editing.
    #[wasm_bindgen(js_name = auditTargets)]
    pub fn audit_targets(&self, positions: JsValue) -> Result<JsValue, JsValue> {
        let positions: Vec<AuditPosition> = serde_wasm_bindgen::from_value(positions)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let guard = self.session.read().expect("kb lock not poisoned");
        let kb = guard.kb();
        let mut orders = std::collections::HashMap::new();
        let mut out = Vec::new();
        for position in positions {
            let order = orders
                .entry(position.seed)
                .or_insert_with(|| kb.audit_sweep_order(None, position.seed));
            let sid = *order
                .get(position.step)
                .ok_or_else(|| JsValue::from_str("Reported audit target is missing."))?;
            let mut sources = kb.sentence_source_hashes(sid);
            sources.sort_unstable();
            let source = *sources
                .first()
                .ok_or_else(|| JsValue::from_str("Audit target has no source formula."))?;
            out.push(AuditTarget {
                source: format!("{source:016x}"),
                kif: kb.render_sentence(sid, sigmakee_rs_core::SentenceForm::Normalized, false, 0),
                roots: kb.source_roots(source).len(),
            });
        }
        to_js(&out)
    }

    /// The root sentence enclosing UTF-8 byte `offset` of loaded file `file`,
    /// as `{ kif, file, line }`, or `null` when none does (the file isn't
    /// loaded, or the offset is outside every sentence).
    #[wasm_bindgen(js_name = sentenceAt)]
    pub fn sentence_at(&self, file: &str, offset: usize) -> Result<JsValue, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        let kb = session_guard.kb();
        match kb.sentence_at(file, offset) {
            Some(sid) => to_js(&AuditFocusView::of(kb, sid)),
            None => Ok(JsValue::NULL),
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
            Some(kif) => inner
                .clausify_formula(&kif)
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            None => inner.clausify_all(),
        };
        serde_wasm_bindgen::to_value(&clauses).map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

#[derive(serde::Deserialize)]
struct AuditPosition {
    seed: u32,
    step: usize,
}

#[derive(serde::Serialize)]
struct AuditTarget {
    source: String,
    kif: String,
    roots: usize,
}

#[derive(serde::Serialize)]
struct AuditSourceFile {
    file: String,
    keys: Vec<String>,
}

#[derive(serde::Deserialize)]
struct AuditFocus {
    file: String,
    source: String,
    /// Original normalized formula, used to distinguish unchanged expansions.
    kif: String,
    unchanged: bool,
}

fn resolve_audit_focus(
    kb: &sigmakee_rs_core::KnowledgeBase<super::NativeStack>,
    focus: &AuditFocus,
) -> Result<sigmakee_rs_core::SentenceId, String> {
    let source = u64::from_str_radix(&focus.source, 16)
        .map_err(|_| "Invalid audit source identity.".to_string())?;
    if !kb.file_hashes(&focus.file).contains(&source) {
        return Err("The tracked audit formula is no longer loaded.".into());
    }
    let roots = kb.source_roots(source);
    let candidates: Vec<_> = if focus.unchanged {
        roots
            .into_iter()
            .filter(|&sid| {
                kb.render_sentence(sid, sigmakee_rs_core::SentenceForm::Normalized, false, 0)
                    == focus.kif
            })
            .collect()
    } else {
        roots
    };
    if candidates.len() != 1 {
        return Err(
            "The edited audit target is ambiguous or has changed its expansion. Load a new report."
                .into(),
        );
    }
    let sid = candidates[0];
    if !kb.audit_sweep_order(None, 0).contains(&sid) {
        return Err("The tracked formula is no longer eligible for an audit.".into());
    }
    Ok(sid)
}

/// The JS request object of [`Session::audit_consistency`].
#[derive(serde::Deserialize)]
#[serde(default)]
struct AuditRequest {
    focus: Option<AuditFocus>,
    seed: u32,
    step: usize,
    count: usize,
    batch: usize,
    limit: usize,
    scope: Option<String>,
    budget: Option<usize>,
    depth: Option<usize>,
    at: Option<FocusAt>,
}

/// A position in a loaded file: `{ file, offset }`, offset in UTF-8 bytes.
#[derive(serde::Deserialize)]
struct FocusAt {
    file: String,
    offset: usize,
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
            focus: None,
            seed: 0,
            step: 0,
            count: 1,
            batch: 1,
            limit: 5,
            scope: None,
            budget: None,
            depth: None,
            at: None,
        }
    }
}

#[cfg(test)]
mod recheck_tests {
    use super::*;
    use sigmakee_rs_core::{KnowledgeBase, Prover};

    fn kb(text: &str) -> KnowledgeBase<super::super::NativeStack> {
        let runner = std::sync::Arc::new(crate::external_prover::WasmExternalRunner::default());
        let mut kb = KnowledgeBase::new_external_native(Prover::Custom(runner));
        assert!(
            kb.load(
                sigmakee_rs_core::SourceFile::kif(
                    std::path::PathBuf::from("test.kif"),
                    text.to_string()
                ),
                "test.kif"
            )
            .ok
        );
        kb.make_session_axiomatic("test.kif").expect("promote");
        kb
    }

    fn focus(
        kb: &KnowledgeBase<super::super::NativeStack>,
        text: &str,
        unchanged: bool,
    ) -> AuditFocus {
        let doc = sigmakee_rs_core::parse_document(
            "test.kif",
            text,
            sigmakee_rs_core::Parser::Kif { options: None },
        );
        let fp = doc.root_hashes[0];
        let sid = kb.source_roots(fp)[0];
        AuditFocus {
            file: "test.kif".into(),
            source: format!("{fp:016x}"),
            kif: kb.render_sentence(sid, sigmakee_rs_core::SentenceForm::Normalized, false, 0),
            unchanged,
        }
    }

    #[test]
    fn recheck_resolves_current_source_not_original_sweep_position() {
        let before = kb("(instance Alice Human)");
        let original = focus(&before, "(instance Alice Human)", true);
        assert!(resolve_audit_focus(&before, &original).is_ok());
        let after = kb("(instance Alice Animal)");
        assert!(resolve_audit_focus(&after, &original).is_err());
        let mut edited = focus(&after, "(instance Alice Animal)", false);
        edited.kif = original.kif;
        let sid = resolve_audit_focus(&after, &edited).expect("edited target");
        assert!(after
            .render_sentence(sid, sigmakee_rs_core::SentenceForm::Normalized, false, 0)
            .contains("Animal"));
        edited.file = "missing.kif".into();
        assert!(resolve_audit_focus(&after, &edited).is_err());
    }

    #[test]
    fn recheck_rejects_expansion_changes_and_ineligible_targets() {
        let expanded = kb("(<=> (instance Alice Human) (instance Alice Animal))");
        let target = focus(
            &expanded,
            "(<=> (instance Alice Human) (instance Alice Animal))",
            false,
        );
        assert!(resolve_audit_focus(&expanded, &target).is_err());
        let docs = kb("(documentation Alice EnglishLanguage \"A person\")");
        let target = focus(
            &docs,
            "(documentation Alice EnglishLanguage \"A person\")",
            false,
        );
        assert!(resolve_audit_focus(&docs, &target).is_err());
    }
}
