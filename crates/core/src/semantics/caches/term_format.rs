//! `semantic::term_format` cache: memoises `(termFormat lang sym text)` entries.

use crate::SymbolId;
use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::SemanticLayer;
use crate::semantics::caches::documentation::{collect_doc_relation, filter_lang};
use crate::semantics::consts::TERM_RELATION;
use crate::semantics::types::DocEntry;

/// Behavior for the `semantic::term_format` cache.
#[derive(Debug, Default)]
pub(crate) struct TermFormat;

impl CacheBehavior for TermFormat {
    type Parent = SemanticLayer;
    type Key    = SymbolId;
    type Value  = Vec<DocEntry>;

    const NAME: &'static str = "semantic::term_format";

    fn generate(&self, parent: &SemanticLayer, &sym: &SymbolId) -> Vec<DocEntry> {
        // `(termFormat lang sym text)` — target at idx 2, lang 1, text 3.
        collect_doc_relation(&parent.syntactic, TERM_RELATION, sym, 2, 1, 3)
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        &[crate::cache::events::EventKind::TaxonomyChanged, crate::cache::events::EventKind::OtherRootsChanged]
    }

    fn react(
        &self,
        _parent: &SemanticLayer,
        events:  &[&crate::cache::events::Event],
        store:   &EntryCache<SymbolId, Vec<DocEntry>>,
    ) -> Vec<crate::cache::events::Event> {
        use crate::cache::events::Event;
        if events.iter().any(|e| matches!(e, Event::TaxonomyChanged { .. } | Event::OtherRootsChanged { .. })) {
            store.clear();
        }
        Vec::new()
    }
}

impl SemanticLayer {
    /// `(termFormat lang sym text)` entries for `sym`, optionally filtered by language.
    pub(crate) fn term_format(&self, sym: SymbolId, language: Option<&str>) -> Vec<DocEntry> {
        filter_lang(&self.term_format.get(self, sym), language)
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::kif_layer;
    use crate::semantics::types::DocEntry;

    #[test]
    fn term_format_returns_entry() {
        // termFormat has arg order: (termFormat lang sym text)
        let layer = kif_layer(r#"(termFormat EnglishLanguage Animal "animal")"#);
        let animal = layer.syntactic.sym_id("Animal").unwrap();
        assert_eq!(
            layer.term_format(animal, Some("EnglishLanguage")),
            vec![DocEntry { language: "EnglishLanguage".into(), text: "animal".into() }],
        );
    }

    #[test]
    fn term_format_language_filter() {
        let layer = kif_layer(r#"
            (termFormat EnglishLanguage Animal "animal")
            (termFormat GermanLanguage Animal "Tier")
        "#);
        let animal = layer.syntactic.sym_id("Animal").unwrap();
        assert_eq!(layer.term_format(animal, None).len(), 2);
        assert_eq!(layer.term_format(animal, Some("GermanLanguage")).len(), 1);
        assert!(layer.term_format(animal, Some("FrenchLanguage")).is_empty());
    }
}
