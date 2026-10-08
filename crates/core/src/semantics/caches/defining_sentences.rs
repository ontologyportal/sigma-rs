//! `semantic::defining_sentences` -- the sentence that defines each symbol.
//!
//! A symbol's defining sentence is its first `(subclass S _)`,
//! `(instance S _)`, `(subrelation S _)`, `(subAttribute S _)`, or
//! `(documentation S _ _)` root with a source location, in that priority
//! order; failing those, the first such root headed by `S`. The lowest sid wins
//! within a tier, so the choice is deterministic. Every root counts, promoted
//! or not, so an editor buffer's own declarations anchor its symbols.

use std::collections::HashMap;
use std::sync::Arc;

use crate::cache::events::{Event, EventKind};
use crate::cache::{LayerCache, WholeCacheBehavior};
use crate::semantics::consts::{
    DOC_RELATION, INSTANCE_RELATION, SUBATTRIBUTE_RELATION, SUBCLASS_RELATION, SUBRELATION_RELATION,
};
use crate::semantics::SemanticLayer;
use crate::types::{Element, SentenceId, SymbolId};

/// Behavior for the `semantic::defining_sentences` cache.
#[derive(Debug, Default)]
pub(crate) struct DefiningSentences;

impl WholeCacheBehavior for DefiningSentences {
    type Parent = SemanticLayer;
    type Value = Arc<HashMap<SymbolId, SentenceId>>;

    const NAME: &'static str = "semantic::defining_sentences";

    fn generate(&self, parent: &SemanticLayer) -> Arc<HashMap<SymbolId, SentenceId>> {
        let syn = &parent.syntactic;
        let located = syn.source_span_index();
        let mut out: HashMap<SymbolId, SentenceId> = HashMap::new();
        for head in [
            &SUBCLASS_RELATION,
            &INSTANCE_RELATION,
            &SUBRELATION_RELATION,
            &SUBATTRIBUTE_RELATION,
            &DOC_RELATION,
        ] {
            let mut sids = syn.by_head_id(&head.id());
            sids.sort_unstable();
            for sid in sids.into_iter().filter(|sid| located.contains_key(sid)) {
                let Some(s) = syn.sentence(sid) else {
                    continue;
                };
                if let Some(Element::Symbol(sym)) = s.elements.get(1) {
                    out.entry(sym.id()).or_insert(sid);
                }
            }
        }
        let mut roots = syn.root_sids();
        roots.sort_unstable();
        for sid in roots.into_iter().filter(|sid| located.contains_key(sid)) {
            if let Some(head) = syn.sentence(sid).and_then(|s| s.head_symbol()) {
                out.entry(head).or_insert(sid);
            }
        }
        Arc::new(out)
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[
            EventKind::RootAdded,
            EventKind::RootRemoved,
            EventKind::SourceReferencesChanged,
        ]
    }

    fn reads(&self) -> &'static [&'static str] {
        &[
            "syntactic::sentences",
            "syntactic::residue_index",
            "syntactic::source",
        ]
    }

    fn react(
        &self,
        _parent: &SemanticLayer,
        events: &[&Event],
        store: &LayerCache<Arc<HashMap<SymbolId, SentenceId>>>,
    ) -> Vec<Event> {
        if events.iter().any(|e| {
            matches!(
                e,
                Event::RootAdded { .. }
                    | Event::RootRemoved { .. }
                    | Event::SourceReferencesChanged
            )
        }) {
            store.invalidate();
        }
        Vec::new()
    }
}

impl SemanticLayer {
    /// The sentence that defines `sym` (see the module docs), if any.
    pub(crate) fn defining_sentence(&self, sym: SymbolId) -> Option<SentenceId> {
        self.defining_sentences.get(self).get(&sym).copied()
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::kif_layer;

    fn defining(kif: &str, sym: &str) -> Option<String> {
        let layer = kif_layer(kif);
        let id = layer.syntactic.sym_id(sym)?;
        let sid = layer.defining_sentence(id)?;
        Some(crate::syntactic::display::sentence_to_plain_kif(
            sid,
            &layer.syntactic,
        ))
    }

    #[test]
    fn declarations_win_in_priority_order() {
        let kif = "(documentation Dog EnglishLanguage \"A dog.\")\n\
                   (instance Dog Class)\n\
                   (subclass Dog Animal)";
        assert_eq!(
            defining(kif, "Dog").as_deref(),
            Some("(subclass Dog Animal)")
        );
    }

    #[test]
    fn a_headed_root_is_the_fallback() {
        let kif = "(likes John Mary)";
        assert_eq!(defining(kif, "likes").as_deref(), Some("(likes John Mary)"));
        assert_eq!(
            defining(kif, "John"),
            None,
            "an argument alone defines nothing"
        );
    }
}
