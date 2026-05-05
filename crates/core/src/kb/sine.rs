use std::collections::HashSet;

use crate::{
    SentenceId, SineIndex, SineParams, SymbolId, syntactic::load_kif,
    sine::collect_conjecture_symbols
};

use super::{KnowledgeBase, KbError};

// SInE knowledge base implementations (public API)
impl KnowledgeBase {
    // -- SInE axiom selection -------------------------------------------------
    //
    // The SInE index is maintained eagerly by `make_session_axiomatic`,
    // `promote_assertions_unchecked`, and `open`: every axiom promotion
    // incrementally updates the D-relation.  Query-path methods below
    // are pure reads (plus a parse-and-roll-back to extract conjecture
    // symbols) and pay zero rebuild cost at query time.
    //
    // Consumers don't need the `ask` feature — SInE is a plain
    // axiom-relevance index that also powers `reconcile_file`'s smart
    // revalidation and any LSP-side "related axioms" feature.

    /// Number of axioms currently tracked by the SInE index.
    pub fn sine_axiom_count(&self) -> usize {
        self.sine_index.read().expect("sine_index poisoned").axiom_count()
    }

    /// The tolerance at which the SInE D-relation is currently computed.
    pub fn sine_tolerance(&self) -> f32 {
        self.sine_index.read().expect("sine_index poisoned").tolerance()
    }

    /// Rebuild the SInE index from scratch over the current axiom set.
    /// Normally not needed — the index is maintained eagerly — but
    /// useful as an escape hatch after non-standard axiom mutations.
    pub fn rebuild_sine_index(&mut self) {
        let axiom_ids = self.axiom_ids_set();
        let tolerance = self.sine_index.read().expect("sine_index poisoned").tolerance();
        let mut idx = SineIndex::new(tolerance);
        idx.add_axioms(&self.layer.semantic.syntactic, axiom_ids.into_iter());
        *self.sine_index.write().expect("sine_index poisoned") = idx;
    }

    /// Extract the symbols of a KIF conjecture string without mutating
    /// the KB's logical state.
    ///
    /// Parses `query_kif` into the store under a temporary file tag,
    /// walks every resulting sentence to collect its symbol ids, then
    /// rolls the parse back — leaving no orphan sentences, taxonomy
    /// edges, or semantic-cache entries.
    ///
    /// Returns [`KbError`] on parse failure.  On success the returned
    /// set may be empty if the conjecture references only variables
    /// and literals.
    ///
    /// The returned SymbolIds are a single-use seed: pass them
    /// straight into [`Self::sine_select_for_query`] or similar — they are
    /// not stable across multiple calls because the name→id interning
    /// resets under roll-back.
    pub fn query_symbols(&mut self, query_kif: &str) -> Result<HashSet<SymbolId>, KbError> {
        let query_tag = crate::session_tags::SESSION_SINE_QUERY;
        let prev_count = self.layer.semantic.syntactic.file_roots
            .get(query_tag).map(|v| v.len()).unwrap_or(0);

        let parse_errors = load_kif(&mut self.layer.semantic.syntactic, query_kif, query_tag);
        if !parse_errors.is_empty() {
            self.layer.semantic.syntactic.remove_file(query_tag);
            self.layer.semantic.rebuild_taxonomy();
            self.layer.semantic.invalidate_cache();
            let (_, e) = parse_errors.into_iter().next().unwrap();
            return Err(e);
        }

        let query_sids: Vec<SentenceId> = self.layer.semantic.syntactic.file_roots
            .get(query_tag)
            .map(|v| v[prev_count..].to_vec())
            .unwrap_or_default();

        let mut syms: HashSet<SymbolId> = HashSet::new();
        for &sid in &query_sids {
            collect_conjecture_symbols(&self.layer.semantic.syntactic, sid, &mut syms);
        }

        // Roll back the temporary parse.  The SInE index is unaffected —
        // we only mutated file-tag-scoped state that `remove_file` fully
        // undoes.
        self.layer.semantic.syntactic.remove_file(query_tag);
        self.layer.semantic.rebuild_taxonomy();
        self.layer.semantic.invalidate_cache();

        self.debug(format!("query_symbols: extracted {} syms from {} query sentence(s)", syms.len(), query_sids.len()));
        Ok(syms)
    }

    /// Return the SentenceIds of promoted axioms that SInE identifies
    /// as relevant to `query_kif` at the given parameters.
    ///
    /// Session assertions are **not** included — SInE operates over
    /// the stable promoted axiom base only.  Callers wiring this into
    /// a prover call are responsible for unioning in any session
    /// assertions they want kept as hypotheses.
    ///
    /// The conjecture's parse is rolled back before this method
    /// returns, so repeated calls with different queries do not
    /// accumulate state.
    ///
    /// Tolerance handling: the eager index caches the D-relation at
    /// a single tolerance.  If `params.tolerance` differs, this
    /// method rebuilds the D-relation in place (preserving the
    /// tolerance-independent per-axiom symbol sets and generality
    /// counts).  In the common case — all queries at the same
    /// tolerance — this rebuild never fires.
    pub fn sine_select_for_query(
        &mut self,
        query_kif: &str,
        params: SineParams,
    ) -> Result<HashSet<SentenceId>, KbError> {
        let seed = self.query_symbols(query_kif)?;

        // Ensure the cached D-relation matches the requested tolerance.
        {
            let current = self.sine_index
                .read().expect("sine_index poisoned").tolerance();
            if (current - params.tolerance.max(1.0)).abs() > f32::EPSILON {
                self.sine_index
                    .write().expect("sine_index poisoned")
                    .set_tolerance(params.tolerance);
            }
        }

        let idx = self.sine_index.read().expect("sine_index poisoned");
        let selected = idx.select(&seed, params.depth_limit);
        self.info(format!("sine_select_for_query: {} seed syms -> {} relevant axioms (of {} total) \
             at tolerance {}", seed.len(), selected.len(), idx.axiom_count(), idx.tolerance()));
        Ok(selected)
    }
}