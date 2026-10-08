//! `semantic::doc_coverage` -- which symbols carry `documentation`,
//! `termFormat`, and `format` entries, from one pass over those relations.
//!
//! The per-symbol `documentation` cache scans every documentation-style root
//! for each symbol it is asked about; checking every symbol in the KB needs
//! this bulk view instead. Every root counts, promoted or not.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::cache::events::{Event, EventKind};
use crate::cache::{LayerCache, WholeCacheBehavior};
use crate::semantics::consts::{DOC_RELATION, FORMAT_RELATION, TERM_RELATION};
use crate::semantics::SemanticLayer;
use crate::types::{Element, SymbolId};

/// Documentation coverage of the whole KB.
#[derive(Debug, Default)]
pub(crate) struct DocCoverage {
    /// `(documentation S Lang _)` counts, per symbol and language.
    pub documentation: HashMap<SymbolId, BTreeMap<String, usize>>,
    /// Symbols with a `(termFormat _ S _)` entry.
    pub term_format: HashSet<SymbolId>,
    /// Relations with a `(format _ R _)` entry.
    pub format: HashSet<SymbolId>,
}

/// Behavior for the `semantic::doc_coverage` cache.
#[derive(Debug, Default)]
pub(crate) struct DocCoverageCache;

impl WholeCacheBehavior for DocCoverageCache {
    type Parent = SemanticLayer;
    type Value = Arc<DocCoverage>;

    const NAME: &'static str = "semantic::doc_coverage";

    fn generate(&self, parent: &SemanticLayer) -> Arc<DocCoverage> {
        let syn = &parent.syntactic;
        let symbol_at = |sid, i: usize| match syn.sentence(sid)?.elements.get(i) {
            Some(Element::Symbol(sym)) => Some(sym.id()),
            _ => None,
        };
        let mut out = DocCoverage::default();
        for sid in syn.by_head_id(&DOC_RELATION.id()) {
            if let (Some(subject), Some(language)) = (symbol_at(sid, 1), symbol_at(sid, 2)) {
                let language = syn
                    .sym_name(language)
                    .map(|s| s.name().to_string())
                    .unwrap_or_default();
                *out.documentation
                    .entry(subject)
                    .or_default()
                    .entry(language)
                    .or_insert(0) += 1;
            }
        }
        out.term_format = syn
            .by_head_id(&TERM_RELATION.id())
            .into_iter()
            .filter_map(|sid| symbol_at(sid, 2))
            .collect();
        out.format = syn
            .by_head_id(&FORMAT_RELATION.id())
            .into_iter()
            .filter_map(|sid| symbol_at(sid, 2))
            .collect();
        Arc::new(out)
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::RootAdded, EventKind::RootRemoved]
    }

    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences", "syntactic::residue_index"]
    }

    fn react(
        &self,
        _parent: &SemanticLayer,
        events: &[&Event],
        store: &LayerCache<Arc<DocCoverage>>,
    ) -> Vec<Event> {
        if events
            .iter()
            .any(|e| matches!(e, Event::RootAdded { .. } | Event::RootRemoved { .. }))
        {
            store.invalidate();
        }
        Vec::new()
    }
}

impl SemanticLayer {
    /// Documentation coverage of the whole KB.
    pub(crate) fn doc_coverage(&self) -> Arc<DocCoverage> {
        self.doc_coverage.get(self)
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::kif_layer;

    #[test]
    fn coverage_reads_each_relation_s_subject_slot() {
        let layer = kif_layer(
            "(documentation Dog EnglishLanguage \"A dog.\")\n\
             (documentation Dog EnglishLanguage \"A canine.\")\n\
             (termFormat EnglishLanguage Cat \"cat\")\n\
             (format EnglishLanguage likes \"%1 likes %2\")",
        );
        let id = |n: &str| layer.syntactic.sym_id(n).unwrap();
        let cov = layer.doc_coverage();
        assert_eq!(cov.documentation[&id("Dog")]["EnglishLanguage"], 2);
        assert!(!cov.documentation.contains_key(&id("EnglishLanguage")));
        assert!(cov.term_format.contains(&id("Cat")));
        assert!(!cov.term_format.contains(&id("EnglishLanguage")));
        assert!(cov.format.contains(&id("likes")));
    }
}
