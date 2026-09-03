// crates/wasm/src/session/views.rs
//
// Query projections: search / man pages / taxonomy / stats / NL rendering /
// pattern lookup -- the wasm face of the SDK's `session/views.rs` ops.

use sigmakee_rs_sdk::{man_kind_from_str, SearchOpts, TaxConstraint};
use wasm_bindgen::prelude::*;

use crate::types::to_js;

use super::Session;

#[wasm_bindgen]
impl Session {
    /// Pattern-based lookup.  Returns a JSON array of matched sentence strings.
    #[wasm_bindgen]
    pub fn lookup(&self, pattern: &str) -> Result<JsValue, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        let inner = session_guard.kb();
        let sids = inner.lookup(pattern);
        let results: Vec<String> = sids
            .iter()
            .map(|&sid| inner.sentence_to_string(sid))
            .collect();
        serde_wasm_bindgen::to_value(&results).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Full-text / symbol search over the KB. `kind` filters by
    /// `"class"|"relation"|"function"|"predicate"|"instance"|"individual"`,
    /// `language` by tag (e.g. `"EnglishLanguage"`), `limit` caps results.
    /// With a lexicon loaded ([`loadWordNet`](Session::load_wordnet)),
    /// results include WordNet synonym hits (`source: "wn"`); `wordnetOnly:
    /// true` returns only those. `taxonomy` ANDs a list of taxonomic
    /// constraints (all satisfied to keep a hit), each one of
    /// `{subclassOf}`/`{instanceOf}`/`{rangeOf}`/`{rangeSubclassOf}` keyed to
    /// a class name, e.g. `[{subclassOf: "Animal"}]` -- `undefined`/`null`/
    /// `[]` means unconstrained. `subclassOf`/`instanceOf` are also
    /// expressible inline in `query` itself (`-subclass->Class` /
    /// `-instance->Class`); a non-empty `taxonomy` here wins over that
    /// inline form rather than combining with it. Returns `{ symbol, kinds,
    /// source, language, text, sense, rank }[]`.
    #[wasm_bindgen]
    pub fn search(
        &self,
        query: &str,
        kind: Option<String>,
        language: Option<String>,
        limit: Option<u32>,
        wordnet_only: Option<bool>,
        taxonomy: JsValue,
    ) -> Result<JsValue, JsValue> {
        let taxonomy: Vec<TaxConstraint> = if taxonomy.is_undefined() || taxonomy.is_null() {
            Vec::new()
        } else {
            serde_wasm_bindgen::from_value(taxonomy)
                .map_err(|e| JsValue::from_str(&format!("invalid taxonomy: {e}")))?
        };
        let session_guard = self.session.read().expect("kb lock not poisoned");
        let opts = SearchOpts {
            kind: kind.as_deref().and_then(man_kind_from_str),
            language: language.as_deref(),
            limit: limit.map(|n| n as usize),
            taxonomy,
            lexicon: None, // overridden by `search_view` from the session's own installed lexicon
            wordnet_only: wordnet_only.unwrap_or(false),
        };
        to_js(&session_guard.search_view(query, &opts))
    }

    /// Structured "man page" for a symbol: kinds, documentation, taxonomy
    /// (parents/children), signature (arity/domains/range), and the full
    /// list of referencing formulas. Returns `null` if the symbol is unknown.
    #[wasm_bindgen]
    pub fn manpage(&self, symbol: &str) -> Result<JsValue, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        match session_guard.manpage_detail(symbol) {
            Some(detail) => to_js(&detail),
            None => Ok(JsValue::NULL),
        }
    }

    /// Direct taxonomy edges of `symbol` -- `{ parents, children }` -- without
    /// the man page's reference scan. Powers lazy taxonomy-tree expansion.
    #[wasm_bindgen]
    pub fn taxonomy(&self, symbol: &str) -> Result<JsValue, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        to_js(&session_guard.taxonomy_view(symbol))
    }

    /// The `(instance ? NaturalLanguage)` symbols, each with the English label
    /// from its `termFormat` (falling back to the bare symbol name). Sorted by
    /// label, with `EnglishLanguage` guaranteed present. Powers the UI language
    /// selector.
    #[wasm_bindgen(js_name = naturalLanguages)]
    pub fn natural_languages(&self) -> Result<JsValue, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        to_js(&session_guard.natural_languages_view())
    }

    /// Natural-language paraphrase of a single KIF formula in `language`. Empty
    /// when the KIF doesn't parse to a statement.
    #[wasm_bindgen(js_name = renderNl)]
    pub fn render_nl(&self, kif: &str, language: &str) -> String {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        session_guard.render_nl(kif, language)
    }

    /// Summary counts describing the loaded KB, for an overview page.
    ///
    /// Returns `{ files, symbols, axioms, rules }`:
    /// * `symbols` -- interned names a reader would recognise: KIF variables
    ///   (`?x` / `@row`) and the prover's skolem constants are excluded;
    /// * `axioms` -- top-level formulas contributed by the loaded files;
    /// * `rules` -- those whose top-level connective is `=>` or `<=>`.
    #[wasm_bindgen]
    pub fn stats(&self) -> Result<JsValue, JsValue> {
        let session_guard = self.session.read().expect("kb lock not poisoned");
        to_js(&session_guard.stats_view())
    }
}
