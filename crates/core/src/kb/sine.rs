use std::collections::HashSet;
use std::path::PathBuf;

use crate::layer::{Layer, TopLayer};
use crate::types::SourceFile;
use crate::DiagResult;
use crate::{SentenceId, SineParams, SymbolId};

use super::KnowledgeBase;

// SInE knowledge base implementations (public API)
impl<L: Layer + TopLayer> KnowledgeBase<L> {
    // -- SInE axiom selection -------------------------------------------------

    /// Number of axioms currently tracked by the SInE index.
    pub fn sine_axiom_count(&self) -> usize {
        self.layer
            .semantic()
            .syntactic
            .sine_current(|idx| idx.axiom_count())
    }

    /// Extract the symbols of a KIF conjecture string without mutating
    /// the KB's logical state.
    ///
    /// Parses `query_kif` into the store under a temporary file tag,
    /// walks every resulting sentence to collect its symbol ids, then
    /// rolls the parse back — leaving no orphan sentences, taxonomy
    /// edges, or semantic-cache entries.
    ///
    /// Returns [`Diagnostic`](crate::Diagnostic) on parse failure.  On success the returned
    /// set may be empty if the conjecture references only variables
    /// and literals.
    ///
    /// The returned SymbolIds are a single-use seed: pass them
    /// straight into [`Self::sine_select_for_query`] or similar — they are
    /// not stable across multiple calls because the name→id interning
    /// resets under roll-back.
    pub fn query_symbols(&mut self, query_kif: &str) -> DiagResult<HashSet<SymbolId>> {
        let query_tag = crate::syntactic::session_tags::SESSION_SINE_QUERY;

        let outcome = self.ingest_source(
            SourceFile::inline_kif(query_tag, query_kif.to_string()),
            query_tag,
            true,
        );
        if !outcome.errors.is_empty() {
            // Roll back the partial parse; the truncate re-ingest's cascade
            // reverts every derived cache (taxonomy edges + lazy caches) on its own.
            let _ = self.ingest_source(
                SourceFile::truncate(PathBuf::from(query_tag)),
                query_tag,
                true,
            );
            return Err(outcome.errors.into_iter().next().unwrap().into());
        }

        // Every root the query maps to under its tag — *including* ones that
        // dedup to sentences already present.  Content-addressing means a query
        // identical to an existing axiom produces no `RootAdded`, so harvesting
        // only newly-added roots (`roots_from_outcome`) would extract no symbols
        // and SInE would select nothing.  `file_root_sids` reads the tag's full
        // membership via the source cache, new or deduped alike.
        let query_sids = self.layer.semantic().syntactic.file_root_sids(query_tag);

        let mut syms: HashSet<SymbolId> = HashSet::new();
        for &sid in &query_sids {
            syms.extend(self.layer.semantic().syntactic.sentence_symbols(sid));
        }

        // Roll back the temporary parse.  The SInE index is unaffected — we only
        // mutated file-tag-scoped state that re-ingesting empty undoes, and the
        // truncate's cascade reverts the taxonomy/semantic caches automatically.
        let _ = self.ingest_source(
            SourceFile::truncate(PathBuf::from(query_tag)),
            query_tag,
            true,
        );

        self.debug(format!(
            "query_symbols: extracted {} syms from {} query sentence(s)",
            syms.len(),
            query_sids.len()
        ));
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
    /// Tolerance is passed directly to [`SineIndex::select`] — no
    /// index rebuild is required regardless of the requested value.
    pub fn sine_select_for_query(
        &mut self,
        query_kif: &str,
        params: SineParams,
    ) -> DiagResult<HashSet<SentenceId>> {
        // Two-step: harvest symbols by parse-and-rollback, then dispatch
        // to the canonical sid-agnostic SInE entry.  The
        // `sine.query_symbols` span covers the parse cost (which dominates
        // when the conjecture is large — e.g. concatenated rendered
        // sentences).  The selection itself is timed inside
        // `sine_select_with_seed` as `sine.select_axioms`.
        let seed = {
            profile_span!(self, "sine.query_symbols");
            self.query_symbols(query_kif)?
        };
        Ok(self.sine_select_with_seed(seed, params))
    }

    /// SInE-select the relevant axiom subset for a set of sentences
    /// **already loaded in the KB**.  Skips the parse-rollback dance of
    /// [`Self::sine_select_for_query`] — the sample sids already have
    /// their symbols in the store, so we walk them directly.
    ///
    /// Intended for callers that have a known sentence subset in hand
    /// (`debug FILE.kif`, `serve`'s consistency check, etc.) and want
    /// the SInE relevance set without bouncing through KIF text +
    /// parser + taxonomy-undo.  On a SUMO-scale sample (~10K sentences)
    /// this saves the dominant SInE cost — ~30 s of pure overhead in
    /// the legacy path.
    pub fn sine_select_for_sids(
        &self,
        sids: &[SentenceId],
        params: SineParams,
    ) -> HashSet<SentenceId> {
        self.layer
            .semantic()
            .syntactic
            .sine_select_for_sids(sids, params, &self.prove_ctx())
    }

    /// Canonical SInE-selection entry point: given a pre-computed
    /// symbol seed, return the SentenceIds the index considers
    /// relevant at `params.tolerance` / `params.depth_limit`.
    ///
    /// The two text- and sid-based convenience wrappers above
    /// ([`Self::sine_select_for_query`], [`Self::sine_select_for_sids`])
    /// differ only in how they obtain the seed; both funnel here for
    /// the actual selection so spinner phases and log-line shapes
    /// stay consistent across call sites.
    pub fn sine_select_with_seed(
        &self,
        seed: HashSet<SymbolId>,
        params: SineParams,
    ) -> HashSet<SentenceId> {
        self.layer
            .semantic()
            .syntactic
            .sine_select_with_seed(seed, params, &self.prove_ctx())
    }

    /// Filter a SentenceId list by the canonical default-excluded
    /// head predicates (`documentation`, `termFormat`, `domain`, …).
    /// Layer-generic: both prover input pipelines (subprocess and
    /// native) drop bookkeeping sentences before clause conversion.
    pub fn filter_excluded_heads(&self, sids: &[SentenceId]) -> Vec<SentenceId> {
        self.layer.semantic().syntactic.filter_excluded_heads(sids)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;

    use crate::{KnowledgeBase, SineParams};

    fn load(kb: &mut KnowledgeBase, file: &str, kif: &str) {
        let r = kb.reload_kif(kif, &PathBuf::from(file), file);
        assert!(r.ok, "ingest: {:?}", r.diagnostics);
        kb.make_session_axiomatic(file).expect("promote");
    }

    /// Name of the symbol the root whose plain KIF is `kif` is pinned to.
    fn pinned_to(kb: &KnowledgeBase, kif: &str) -> Option<String> {
        let syn = kb.store_for_testing();
        let sid = syn
            .root_sids()
            .into_iter()
            .find(|&sid| crate::syntactic::display::sentence_to_plain_kif(sid, syn) == kif)
            .unwrap_or_else(|| panic!("no root {kif}"));
        let pin = syn.sine.with_ref(|idx| idx.pinned_of(sid))?;
        Some(syn.sym_name(pin.owner).unwrap().name().to_string())
    }

    #[test]
    fn facts_of_a_declared_transitive_relation_are_pinned_to_their_first_argument() {
        let mut kb = KnowledgeBase::new();
        load(
            &mut kb,
            "a.kif",
            "(instance part TransitiveRelation)\n(part Wheel Car)\n(color Sky Blue)",
        );
        assert_eq!(pinned_to(&kb, "(part Wheel Car)").as_deref(), Some("Wheel"));
        assert_eq!(pinned_to(&kb, "(color Sky Blue)"), None);
    }

    #[test]
    fn a_later_declaration_pins_existing_facts_and_its_removal_unpins_them() {
        let mut kb = KnowledgeBase::new();
        load(&mut kb, "facts.kif", "(part Wheel Car)");
        assert_eq!(pinned_to(&kb, "(part Wheel Car)"), None);

        load(&mut kb, "decl.kif", "(instance part TransitiveRelation)");
        assert_eq!(pinned_to(&kb, "(part Wheel Car)").as_deref(), Some("Wheel"));

        kb.reload_kif(
            "(instance other TransitiveRelation)",
            &PathBuf::from("decl.kif"),
            "decl.kif",
        );
        kb.commit("decl.kif");
        assert_eq!(pinned_to(&kb, "(part Wheel Car)"), None);
    }

    #[test]
    fn inherited_instances_pin_but_subrelations_of_a_transitive_relation_do_not() {
        let mut kb = KnowledgeBase::new();
        load(
            &mut kb,
            "a.kif",
            "(subclass PartialOrderingRelation TransitiveRelation)\n\
             (instance lessThan PartialOrderingRelation)\n\
             (lessThan One Two)\n\
             (instance ancestor TransitiveRelation)\n\
             (subrelation parent ancestor)\n\
             (parent Al Bo)\n\
             (ancestor Al Cy)",
        );
        assert_eq!(pinned_to(&kb, "(lessThan One Two)").as_deref(), Some("One"));
        assert_eq!(pinned_to(&kb, "(ancestor Al Cy)").as_deref(), Some("Al"));
        assert_eq!(pinned_to(&kb, "(parent Al Bo)"), None);
    }

    #[test]
    fn transitive_facts_are_selected_from_their_first_argument_even_when_it_is_common() {
        let mut kb = KnowledgeBase::new();
        load(
            &mut kb,
            "a.kif",
            "(instance part TransitiveRelation)\n\
             (part Wheel Car)\n\
             (instance W1 Wheel)\n\
             (instance W2 Wheel)\n\
             (instance W3 Wheel)",
        );
        let wheel: HashSet<_> = [kb.store_for_testing().sym_id("Wheel").unwrap()].into();
        let on = SineParams {
            pinned_ownership: true,
            ..SineParams::strict()
        };
        assert!(kb
            .sine_select_with_seed(wheel.clone(), SineParams::strict())
            .is_empty());
        let syn = kb.store_for_testing();
        let selected: HashSet<String> = kb
            .sine_select_with_seed(wheel, on)
            .into_iter()
            .map(|sid| crate::syntactic::display::sentence_to_plain_kif(sid, syn))
            .collect();
        // The fact, and (reached through `part`) the declaration that makes
        // the relation transitive.
        assert_eq!(
            selected,
            HashSet::from([
                "(part Wheel Car)".to_string(),
                "(instance part TransitiveRelation)".to_string(),
            ])
        );
    }

    /// Promotion goes through the `AxiomsPromoted` reactor, which must
    /// register the pins; selection then follows the pinned relation.
    #[test]
    fn pinned_ownership_applies_to_promoted_axioms() {
        let mut kb = KnowledgeBase::new();
        let r = kb.reload_kif(
            "(subclass Dog Animal)\n\
             (subclass Animal Entity)\n\
             (instance Rex Dog)\n\
             (instance Fido Dog)\n\
             (instance Spot Dog)",
            &std::path::PathBuf::from("pin.kif"),
            "load",
        );
        assert!(r.ok, "ingest: {:?}", r.diagnostics);
        kb.make_session_axiomatic("load").expect("promote");

        let seed = || {
            let dog = kb.store_for_testing().sym_id("Dog").unwrap();
            std::collections::HashSet::from([dog])
        };
        let off = SineParams::strict();
        let on = SineParams {
            pinned_ownership: true,
            ..SineParams::strict()
        };
        assert!(kb.sine_select_with_seed(seed(), off).is_empty());
        let selected = kb.sine_select_with_seed(seed(), on);
        assert_eq!(selected.len(), 2, "Dog's edge and the chain above it");
    }

    #[test]
    fn pinned_ownership_defaults_follow_the_environment_and_fixed_modes_stay_off() {
        let default = crate::syntactic::sine::default_pinned_ownership();
        assert_eq!(SineParams::default().pinned_ownership, default);
        assert_eq!(SineParams::auto(10).pinned_ownership, default);
        assert!(!SineParams::strict().pinned_ownership);
        assert!(!SineParams::benevolent(2.0).pinned_ownership);
        assert!(!SineParams::whole_kb().pinned_ownership);
        let from_json: SineParams = serde_json::from_str("{}").unwrap();
        assert_eq!(from_json.pinned_ownership, default);
    }
}
