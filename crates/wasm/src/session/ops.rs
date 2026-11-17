// crates/wasm/src/session/ops.rs
//
// Non-proving ops: validation, TPTP translation, snapshot/restore, and the
// thread-pool knob -- the wasm face of the SDK's `session/ops.rs`.

use serde::Serialize;
use sigmakee_rs_core::celt;
use sigmakee_rs_sdk::{DiagnosticView, ScratchValidationView, TptpLang, TptpOptions};
use wasm_bindgen::prelude::*;

use crate::types::to_js;

use super::Session;

#[wasm_bindgen]
impl Session {
    /// Translate controlled English against the active KB using CELT.
    #[wasm_bindgen(js_name = translateCelt)]
    pub fn translate_celt(&self, text: &str) -> Result<JsValue, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        let Some(lexicon) = session_guard.lexicon() else {
            return to_js(&CeltTranslationView {
                available: false,
                error: Some("The WordNet-SUMO lexicon is not loaded.".to_string()),
                sentences: Vec::new(),
            });
        };

        let translation =
            celt::translate(session_guard.kb(), lexicon, text, &celt::Opts::default());
        to_js(&CeltTranslationView::from(translation))
    }

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
        self.session
            .write()
            .expect("kb lock not poisoned")
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
    pub fn to_tptp_indexed(&self, lang: Option<String>, hide_numbers: Option<bool>) -> String {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        let inner = session_guard.kb();
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
}

#[derive(Debug, Serialize)]
struct CeltTranslationView {
    available: bool,
    error: Option<String>,
    sentences: Vec<CeltSentenceView>,
}

/// One sentence's outcome. An accepted sentence fills `kind`, `kif`, `rule`,
/// `choices` and `declared`; a refused one fills `code`, `reason` and `span`.
#[derive(Debug, Serialize)]
struct CeltSentenceView {
    text: String,
    offset: usize,
    kind: Option<String>,
    kif: Option<String>,
    /// The sentence rule that produced the reading.
    rule: Option<String>,
    /// Lexical decisions, in the order they were taken.
    choices: Vec<CeltChoiceView>,
    /// Side assertions the reading relies on, as KIF.
    declared: Vec<String>,
    /// Refusal code (`unknown-name`).
    code: Option<String>,
    /// Refusal message, without the code.
    reason: Option<String>,
    /// Char span of the refused word or construct within `text`.
    span: Option<CeltSpanView>,
}

#[derive(Debug, Serialize)]
struct CeltChoiceView {
    word: String,
    term: String,
    sense: String,
    alternatives: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CeltSpanView {
    start: usize,
    end: usize,
}

impl From<celt::Translation> for CeltTranslationView {
    fn from(translation: celt::Translation) -> Self {
        Self {
            available: true,
            error: None,
            sentences: translation
                .sentences
                .into_iter()
                .map(CeltSentenceView::from)
                .collect(),
        }
    }
}

impl From<celt::SentenceResult> for CeltSentenceView {
    fn from(sentence: celt::SentenceResult) -> Self {
        let mut view = Self {
            text: sentence.text,
            offset: sentence.offset,
            kind: None,
            kif: None,
            rule: None,
            choices: Vec::new(),
            declared: Vec::new(),
            code: None,
            reason: None,
            span: None,
        };
        match sentence.outcome {
            Ok(logic) => {
                view.kind = Some(
                    match logic.kind {
                        celt::Kind::Assert => "assertion",
                        celt::Kind::Query => "question",
                    }
                    .to_string(),
                );
                view.kif = Some(logic.formula().to_string());
                view.rule = Some(logic.rule.to_string());
                view.declared = logic
                    .declared
                    .conjuncts()
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                view.choices = logic
                    .choices
                    .into_iter()
                    .map(|c| CeltChoiceView {
                        word: c.word,
                        term: c.term,
                        sense: c.sense,
                        alternatives: c.alternatives,
                    })
                    .collect();
            }
            Err(refused) => {
                view.code = Some(refused.reason.code());
                view.reason = Some(refused.reason.to_string());
                view.span = Some(CeltSpanView {
                    start: refused.start,
                    end: refused.end,
                });
            }
        }
        view
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigmakee_rs_core::celt::ir::{Drs, Term};
    use sigmakee_rs_core::celt::{Choice, Kind, Logic, Refusal, Refused, SentenceResult};

    fn accepted() -> SentenceResult {
        let mut drs = Drs::default();
        drs.atom(
            "instance",
            vec![Term::Const("Rex".into()), Term::Const("Dog".into())],
        );
        let mut declared = Drs::default();
        declared.atom(
            "instance",
            vec![Term::Const("Rex".into()), Term::Const("Animal".into())],
        );
        SentenceResult {
            text: "Rex is a dog.".into(),
            offset: 4,
            outcome: Ok(Logic {
                kind: Kind::Assert,
                drs,
                choices: vec![Choice {
                    word: "dog".into(),
                    term: "Dog".into(),
                    sense: "dog%1:05:00::".into(),
                    alternatives: vec!["Canine".into()],
                }],
                rule: "copula",
                declared,
            }),
        }
    }

    #[test]
    fn accepted_sentence_carries_rule_choices_and_declared() {
        let view = CeltSentenceView::from(accepted());
        assert_eq!(view.offset, 4);
        assert_eq!(view.kind.as_deref(), Some("assertion"));
        assert_eq!(view.kif.as_deref(), Some("(instance Rex Dog)"));
        assert_eq!(view.rule.as_deref(), Some("copula"));
        assert_eq!(view.declared, vec!["(instance Rex Animal)".to_string()]);
        assert_eq!(view.choices.len(), 1);
        assert_eq!(view.choices[0].term, "Dog");
        assert_eq!(view.choices[0].alternatives, vec!["Canine".to_string()]);
        assert!(view.code.is_none() && view.reason.is_none() && view.span.is_none());
    }

    #[test]
    fn refused_sentence_splits_code_from_message_and_keeps_span() {
        let view = CeltSentenceView::from(SentenceResult {
            text: "Florp the zib.".into(),
            offset: 0,
            outcome: Err(Refused::new(
                Refusal::UnknownName {
                    name: "Florp".into(),
                },
                0,
                5,
            )),
        });
        assert_eq!(view.code.as_deref(), Some("unknown-name"));
        let reason = view.reason.expect("refusal message");
        assert!(reason.contains("Florp"));
        assert!(!reason.contains("[unknown-name]"));
        let span = view.span.expect("refusal span");
        assert_eq!((span.start, span.end), (0, 5));
        assert!(view.kind.is_none() && view.kif.is_none() && view.rule.is_none());
        assert!(view.choices.is_empty() && view.declared.is_empty());
    }

    #[test]
    fn empty_translation_is_available_with_no_sentences() {
        let view = CeltTranslationView::from(celt::Translation::default());
        assert!(view.available);
        assert!(view.error.is_none());
        assert!(view.sentences.is_empty());
    }
}
