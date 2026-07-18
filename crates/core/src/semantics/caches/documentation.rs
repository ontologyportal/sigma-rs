//! `semantic::documentation` cache: memoises the documentation-style entries for
//! a symbol across every relation in the `DOCUMENTATION_RELATIONS` set —
//! `documentation`, `format`, and `termFormat`.  Each entry records its source
//! relation (`DocEntry::rel`) plus the language and text.  All languages are
//! stored; the wrapper filters by language.

use crate::{Element, Literal, SentenceId, SymbolId};
use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::SemanticLayer;
use crate::semantics::consts::DOCUMENTATION_RELATIONS;
use crate::semantics::types::{DocEntry, Scope, Scoped};
use crate::syntactic::caches::session::session_id;

/// Behavior for the `semantic::documentation` cache.
#[derive(Debug, Default)]
pub(crate) struct Documentation;

impl CacheBehavior for Documentation {
    type Parent = SemanticLayer;
    type Key    = Scoped<SymbolId>;
    type Value  = Vec<DocEntry>;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "semantic::documentation";

    fn generate(&self, parent: &SemanticLayer, &Scoped { scope, key: sym }: &Scoped<SymbolId>) -> Vec<DocEntry> {
        let mut out = Vec::new();
        for (_, rel) in DOCUMENTATION_RELATIONS {
            out.extend(collect_doc_entries(parent, rel.id(), sym, scope));
        }
        out
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        use crate::cache::events::EventKind;
        &[EventKind::TaxonomyChanged, EventKind::OtherRootsChanged,
          EventKind::SessionReferenced, EventKind::SessionRetracted]
    }

    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences", "syntactic::residue_index", "syntactic::sessions"]
    }

    fn react(
        &self,
        parent:  &SemanticLayer,
        events:  &[&crate::cache::events::Event],
        store:   &EntryCache<Scoped<SymbolId>, Vec<DocEntry>>,
        _side:   &Self::Side,
    ) -> Vec<crate::cache::events::Event> {
        use crate::cache::events::Event;
        for event in events {
            match event {
                Event::TaxonomyChanged { .. } | Event::OtherRootsChanged { .. } => {
                    store.clear();
                    return Vec::new();
                }
                Event::SessionReferenced { session, sids } => {
                    let s = Scope::Session(session_id(session));
                    for sid in sids {
                        for sym in doc_symbols(parent, *sid) {
                            store.evict_keys(&[Scoped { scope: s, key: sym }]);
                        }
                    }
                }
                Event::SessionRetracted { session } => {
                    let s = Scope::Session(session_id(session));
                    store.retain(|scoped, _| scoped.scope != s);
                }
                _ => {}
            }
        }
        Vec::new()
    }
}

impl SemanticLayer {
    /// Documentation entries for `sym` in the `Base` taxonomy (promoted/global
    /// metadata only), optionally filtered by language.
    pub(crate) fn documentation(&self, sym: SymbolId, language: Option<&str>) -> Vec<DocEntry> {
        self.documentation_scoped(sym, language, Scope::Base)
    }

    /// Documentation entries for `sym` in an explicit [`Scope`]: a session
    /// additionally sees its own transient (un-promoted) `documentation`/
    /// `format`/`termFormat` entries. Optionally filtered by language.
    pub(crate) fn documentation_scoped(
        &self,
        sym:      SymbolId,
        language: Option<&str>,
        scope:    Scope,
    ) -> Vec<DocEntry> {
        filter_lang(&self.documentation.get(self, Scoped { scope, key: sym }), language)
    }

    /// Documentation entries for `sym` from **every** source, promoted or not.
    /// Man-page rendering uses this so metadata (`documentation` / `format` /
    /// `termFormat`) is visible immediately after ingest, before promotion.
    /// Not cached — man-page rendering is cold and this bypasses the scoped cache.
    pub(crate) fn documentation_any(&self, sym: SymbolId, language: Option<&str>) -> Vec<DocEntry> {
        let mut out = Vec::new();
        for (_, rel) in DOCUMENTATION_RELATIONS {
            out.extend(collect_doc_entries_any(self, rel.id(), sym));
        }
        filter_lang(&out, language)
    }
}

/// The symbol arguments of `sid` iff it is a documentation-style root, else an
/// empty vector.
fn doc_symbols(parent: &SemanticLayer, sid: SentenceId) -> Vec<SymbolId> {
    let Some(sent) = parent.syntactic.sentence(sid) else { return Vec::new() };
    let Some(head) = sent.head_symbol() else { return Vec::new() };
    if !DOCUMENTATION_RELATIONS.iter().any(|(_, rel)| rel.id() == head) {
        return Vec::new();
    }
    sent.elements.iter().skip(1).filter_map(|el| match el {
        Element::Symbol(sym) => Some(sym.id()),
        _ => None,
    }).collect()
}

/// Scan head-indexed root sentences for a documentation-style relation,
/// collecting `(language, text)` entries describing `target`.
///
/// Layout-agnostic: it locates the string literal (the text) and the two symbol
/// arguments, treats whichever symbol equals `target` as the subject, and takes
/// the other symbol as the language.  A sentence is skipped unless it mentions
/// `target` alongside a distinct language symbol and a text literal.
pub(crate) fn collect_doc_entries(
    parent: &SemanticLayer,
    head:   SymbolId,
    target: SymbolId,
    scope:  Scope,
) -> Vec<DocEntry> {
    let store = &parent.syntactic;
    let sids = parent.scope_filter_sids(store.by_head_id(&head).iter().copied(), scope);
    collect_from_sids(store, head, target, sids)
}

/// Documentation-style entries for `target` from **every** source headed by
/// `head` — promoted axioms and un-promoted (ingested-but-not-yet-axiomatic)
/// sessions alike, no scope filtering. Man pages use this so `documentation` /
/// `format` / `termFormat` render immediately on ingest, before promotion.
pub(crate) fn collect_doc_entries_any(
    parent: &SemanticLayer,
    head:   SymbolId,
    target: SymbolId,
) -> Vec<DocEntry> {
    let store = &parent.syntactic;
    collect_from_sids(store, head, target, store.by_head_id(&head).iter().copied())
}

fn collect_from_sids(
    store:  &crate::syntactic::SyntacticLayer,
    head:   SymbolId,
    target: SymbolId,
    sids:   impl IntoIterator<Item = SentenceId>,
) -> Vec<DocEntry> {
    let mut out = Vec::new();
    for sid in sids {
        let Some(sent) = store.sentence(sid) else { continue };
        let mut syms = Vec::new();
        let mut text = None;
        for el in sent.elements.iter().skip(1) {
            match el {
                Element::Symbol(sym) => syms.push(sym.id()),
                Element::Literal(Literal::Str(s)) if text.is_none() => {
                    text = Some(strip_quotes(s));
                }
                _ => {}
            }
        }
        let Some(text) = text else { continue };
        if !syms.contains(&target) { continue; }
        let Some(&lang_id) = syms.iter().find(|&&id| id != target) else { continue };
        let Some(lang_sym) = store.sym_name(lang_id) else { continue };
        out.push(DocEntry { rel: head, language: lang_sym.name().to_string(), text });
    }
    out
}

/// Filter doc entries by language (`None` = all).
pub(crate) fn filter_lang(entries: &[DocEntry], want: Option<&str>) -> Vec<DocEntry> {
    match want {
        None    => entries.to_vec(),
        Some(l) => entries.iter().filter(|e| e.language == l).cloned().collect(),
    }
}


/// Strip the surrounding double-quotes from a KIF string literal.
fn strip_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::kif_layer;
    use crate::semantics::consts::{DOC_RELATION, FORMAT_RELATION, TERM_RELATION};
    use crate::semantics::types::DocEntry;

    #[test]
    fn documentation_single_entry() {
        let layer = kif_layer(r#"
            (documentation Animal EnglishLanguage "A living organism.")
        "#);
        let animal = layer.syntactic.sym_id("Animal").unwrap();
        assert_eq!(layer.documentation(animal, None), vec![DocEntry {
            rel: DOC_RELATION.id(),
            language: "EnglishLanguage".into(),
            text:     "A living organism.".into(),
        }]);
    }

    #[test]
    fn documentation_language_filter() {
        let layer = kif_layer(r#"
            (documentation Animal EnglishLanguage "A living organism.")
            (documentation Animal GermanLanguage "Ein Lebewesen.")
        "#);
        let animal = layer.syntactic.sym_id("Animal").unwrap();
        assert_eq!(layer.documentation(animal, None).len(), 2);
        let german = layer.documentation(animal, Some("GermanLanguage"));
        assert_eq!(german.len(), 1);
        assert_eq!(german[0].text, "Ein Lebewesen.");
        assert!(layer.documentation(animal, Some("FrenchLanguage")).is_empty());
    }

    #[test]
    fn format_entry_flipped_layout() {
        let layer = kif_layer(r#"
            (format EnglishLanguage instance "%1 is an instance of %2")
        "#);
        let instance = layer.syntactic.sym_id("instance").unwrap();
        assert_eq!(layer.documentation(instance, None), vec![DocEntry {
            rel:      FORMAT_RELATION.id(),
            language: "EnglishLanguage".into(),
            text:     "%1 is an instance of %2".into(),
        }]);
    }

    #[test]
    fn term_format_entry_flipped_layout() {
        let layer = kif_layer(r#"
            (termFormat EnglishLanguage Entity "entity")
        "#);
        let entity = layer.syntactic.sym_id("Entity").unwrap();
        assert_eq!(layer.documentation(entity, None), vec![DocEntry {
            rel:      TERM_RELATION.id(),
            language: "EnglishLanguage".into(),
            text:     "entity".into(),
        }]);
    }

    #[test]
    fn collects_across_all_doc_relations() {
        let layer = kif_layer(r#"
            (documentation Entity EnglishLanguage "The root class.")
            (termFormat EnglishLanguage Entity "entity")
            (format EnglishLanguage Entity "%1 entity")
        "#);
        let entity = layer.syntactic.sym_id("Entity").unwrap();
        let entries = layer.documentation(entity, None);
        assert_eq!(entries.len(), 3, "one entry per doc relation");

        let by_rel = |rel: u64| entries.iter().find(|e| e.rel == rel);
        assert_eq!(by_rel(DOC_RELATION.id()).map(|e| e.text.as_str()),  Some("The root class."));
        assert_eq!(by_rel(TERM_RELATION.id()).map(|e| e.text.as_str()), Some("entity"));
        assert_eq!(by_rel(FORMAT_RELATION.id()).map(|e| e.text.as_str()), Some("%1 entity"));
    }
}
