//! Substring search over the curated natural-language fields of the KB:
//!
//!   (documentation  Symbol   Language "long text...")
//!   (termFormat     Language Symbol   "short name")
//!   (format         Language Relation "format-string")
//!
//! A linear scan over these three predicates' head-indexed sentences plus a
//! `string.contains()` against the literal payload powers the `sumo search`
//! discovery command: `man` deep-dives a known symbol, `search` surfaces
//! candidate symbols from an English keyword.
//!
//! A second pass matches the query directly against **symbol names**
//! (independent of the text scan above) — see [`KnowledgeBase::search`]'s
//! doc comment for why this exists.

use std::collections::{HashMap, HashSet};

use super::KnowledgeBase;
use crate::SentenceId;
use crate::kb::man::ManKind;
use crate::types::{Element, Literal, SymbolId};
use crate::layer::{TopLayer, Layer};

// -- Public types ------------------------------------------------------------

/// Which of the three documentation predicates produced a match.
///
/// Used by the CLI to render a label ("doc" / "term" / "format") and to sort
/// hits by source relevance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchSource {
    /// Hit was in the third arg of `(termFormat …)` — the symbol's
    /// short natural-language name.
    TermFormat,
    /// Hit was in the third arg of `(documentation …)` — the long
    /// English description.
    Documentation,
    /// Hit was in the third arg of `(format …)` — a relation's
    /// natural-language template.
    Format,
}

impl SearchSource {
    /// Short label for this source (`"term"`, `"doc"`, or `"format"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TermFormat    => "term",
            Self::Documentation => "doc",
            Self::Format        => "format",
        }
    }
}

/// One match: either a documentation/termFormat/format axiom whose text
/// contains the query, or (see [`KnowledgeBase::search`]) a symbol whose own
/// *name* matches the query but which has no such axiom to cite — the latter
/// carries an empty `language`/`text` and `sid == SentenceId::MAX` as a
/// "no backing axiom" sentinel.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// The SUMO symbol whose documentation/termFormat/format axiom matched,
    /// or whose own name matched the query directly.
    pub symbol:   String,
    /// Classification labels for the symbol (mirrors `ManPage::kinds`).
    pub kinds:    Vec<ManKind>,
    /// Which predicate produced the hit (best-effort — `Documentation` when
    /// the hit came from the unsourced name-match pass).
    pub source:   SearchSource,
    /// The language tag of the matching axiom (e.g. `"EnglishLanguage"`), or
    /// `""` for an unsourced name-match hit.
    pub language: String,
    /// The full matching string, surrounding quotes stripped, or `""` for an
    /// unsourced name-match hit.
    pub text:     String,
    /// SentenceId of the matching axiom, or `SentenceId::MAX` for an
    /// unsourced name-match hit (no backing axiom to cite).
    pub sid:      SentenceId,
    /// Relevance score, higher = better.  Combines symbol-name match quality
    /// (exact > prefix > substring > name doesn't contain the query), the
    /// source tier (termFormat > documentation > format), and how early the
    /// query appears in the matched text.  Hits are returned sorted by this
    /// descending (ties broken by symbol name, then `sid`).
    pub rank:     f32,
}

/// Optional filters for [`KnowledgeBase::search`].  All fields are
/// best-effort: unknown kinds simply match nothing, unknown
/// languages match nothing.
#[derive(Debug, Clone, Default)]
pub struct SearchOpts<'a> {
    /// Filter to only hits of this kind (e.g. only `Class`).
    /// `None` accepts any kind.
    pub kind: Option<ManKind>,
    /// Filter to only axioms tagged with this language.
    /// E.g. `Some("EnglishLanguage")`.  `None` accepts any language.
    pub language: Option<&'a str>,
    /// Cap on the number of results returned.  `None` = no cap.
    pub limit: Option<usize>,
}

// -- KB method ---------------------------------------------------------------

impl<L: TopLayer + Layer> KnowledgeBase<L> {
    /// Substring search across SUMO's natural-language fields, **plus** a
    /// direct match against symbol names.
    ///
    /// Returns every documentation / termFormat / format axiom whose
    /// payload string contains `query` (case-insensitive), paired
    /// with the symbol it describes and the symbol's kind.
    ///
    /// That text scan alone misses well-known symbols whose own prose never
    /// repeats their name — e.g. SUMO's `Human` class is glossed as "Modern
    /// man, the only remaining species of the Homo genus." and has no
    /// `termFormat` entry, so a query for `"Human"` would never find `Human`
    /// itself, only symbols like `HumanDoll` whose *documentation* happens to
    /// contain the substring "Human". To close that gap, a second pass (see
    /// [`name_match_hits`]) matches `query` directly against every symbol's
    /// own name, independent of what its documentation says.
    ///
    /// Hits are sorted by [`SearchHit::rank`] (relevance, descending): a
    /// symbol whose *name* matches the query (exact > prefix > substring)
    /// outranks one that only matched inside a documentation blurb, with the
    /// source tier (termFormat → documentation → format) and match position as
    /// tie-breakers, then symbol name and `sid` for determinism.  Apply
    /// [`SearchOpts::kind`] / [`SearchOpts::language`] for narrowing; pass
    /// `SearchOpts::default()` for no filtering.
    pub fn search(&self, query: &str, opts: &SearchOpts) -> Vec<SearchHit> {
        if query.is_empty() {
            return Vec::new();
        }
        let q = query.to_lowercase();
        let syn = &self.layer.semantic().syntactic;

        // Best text hit per symbol — one row per symbol, keeping the
        // highest-ranked match when the query hits several of its text fields
        // (the displayed snippet is unified from `backing` below, so extra
        // rows would render as duplicates).
        let mut text_hits: HashMap<SymbolId, SearchHit> = HashMap::new();
        // Per-symbol (sid, source, language, text) from the scan below, kept
        // regardless of whether `q` matched — the name-match pass uses this to
        // give a symbol with no text hit of its own a real citation + preview
        // instead of a bare, unsourced row.  The preview prefers the
        // *documentation* string (the real description) over the terse
        // `termFormat` label, then `format`; ties within a tier keep first-seen.
        // When a language filter is set, off-language entries never enter the
        // map: the preview must respect the same filter the matches do.
        let mut backing: HashMap<SymbolId, (SentenceId, SearchSource, String, String)> = HashMap::new();

        // (head_name, symbol_arg_index, lang_arg_index, text_arg_index, source).
        // Arg indices are into `Sentence.elements`, where `elements[0]` is the
        // head and arguments start at `elements[1]`.
        const SCHEMAS: &[(&str, usize, usize, usize, SearchSource)] = &[
            ("termFormat",    2, 1, 3, SearchSource::TermFormat),
            ("documentation", 1, 2, 3, SearchSource::Documentation),
            ("format",        2, 1, 3, SearchSource::Format),
        ];

        for &(head, sym_pos, lang_pos, text_pos, source) in SCHEMAS {
            for sid in syn.by_head(head).iter().copied() {
                let Some(sent) = syn.sentence(sid) else { continue };

                let text = match sent.elements.get(text_pos) {
                    Some(Element::Literal(Literal::Str(s))) => s,
                    _ => continue,
                };

                let sym_id: SymbolId = match sent.elements.get(sym_pos) {
                    Some(Element::Symbol(sym)) => sym.id(),
                    _ => continue,
                };

                let lang = match sent.elements.get(lang_pos) {
                    Some(Element::Symbol(sym)) => sym.to_string(),
                    _ => continue,
                };

                // Preview preference: documentation (the real description)
                // beats the terse termFormat label beats format; first-seen
                // breaks ties.  Entries in a filtered-out language are never
                // eligible as previews.
                if opts.language.is_none_or(|want| lang == want) {
                    let better = backing.get(&sym_id)
                        .is_none_or(|cur| source_preview_rank(source) < source_preview_rank(cur.1));
                    if better {
                        backing.insert(sym_id, (sid, source, lang.clone(), strip_quotes(text)));
                    }
                }

                let text_lc = text.to_lowercase();
                let Some(match_idx) = text_lc.find(&q) else { continue };

                if let Some(want) = opts.language {
                    if lang != want { continue; }
                }

                let kinds = self.kinds_of(sym_id);
                if let Some(want) = opts.kind {
                    if !kind_matches(&kinds, want) { continue; }
                }

                let symbol = match syn.sym_name(sym_id) {
                    Some(s) => s.name().to_string(),
                    None => continue,
                };
                let rank = search_rank(&q, &symbol, source, match_idx);
                let keep = text_hits.get(&sym_id).is_none_or(|cur| rank > cur.rank);
                if keep {
                    text_hits.insert(sym_id, SearchHit {
                        symbol,
                        kinds,
                        source,
                        language: lang,
                        text:     strip_quotes(text),
                        sid,
                        rank,
                    });
                }
            }
        }

        // The snippet shown is the symbol's *description*, not necessarily the
        // field the query matched: a query that hit a one-word `termFormat`
        // still displays the documentation string. Rank (relevance) is left as
        // computed from the actual match; only the displayed citation changes.
        for (id, h) in text_hits.iter_mut() {
            if let Some((sid, source, lang, text)) = backing.get(id) {
                if !text.is_empty() {
                    h.sid = *sid;
                    h.source = *source;
                    h.language = lang.clone();
                    h.text = text.clone();
                }
            }
        }

        let mut hits: Vec<SearchHit> = text_hits.into_values().collect();
        let already_hit: HashSet<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        let name_hits = self.name_match_hits(&q, opts, &backing, &already_hit);
        hits.extend(name_hits);

        // Sort by relevance (descending), then deterministic tie-breaks. The
        // stable sort preserves KB order for hits with an identical key.
        hits.sort_by(|a, b| {
            b.rank.partial_cmp(&a.rank).unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.symbol.cmp(&b.symbol))
                .then_with(|| a.sid.cmp(&b.sid))
        });

        if let Some(n) = opts.limit {
            hits.truncate(n);
        }
        hits
    }

    /// The name-match pass described in [`Self::search`]'s doc comment: every
    /// interned, non-Skolem, non-variable symbol whose own name contains `q`
    /// (case-folded), skipping any symbol already covered by a text-field hit
    /// (`already_hit`) so a symbol never appears twice for the same query.
    /// `backing` supplies a real `(sid, source, language, text)` citation when
    /// the symbol has one; symbols with none get an unsourced hit
    /// (`sid = SentenceId::MAX`, empty language/text) rather than being
    /// dropped, since the name match itself is still a legitimate result.
    ///
    /// `backing` is already restricted to [`SearchOpts::language`] by the
    /// caller, so a name match never carries an off-filter citation: a symbol
    /// documented only in another language surfaces as an unsourced hit
    /// (the name match itself is still a legitimate result).
    fn name_match_hits(
        &self,
        q:           &str,
        opts:        &SearchOpts,
        backing:     &HashMap<SymbolId, (SentenceId, SearchSource, String, String)>,
        already_hit: &HashSet<&str>,
    ) -> Vec<SearchHit> {
        let syn = &self.layer.semantic().syntactic;
        let mut out = Vec::new();
        syn.symbols.entries().for_each(|(&sym_id, sym)| {
            if syn.is_skolem(sym_id) { return; }
            let name = sym.name();
            // `?X`/`@X` variables are interned into this same table under a
            // scope-qualified key (`"<name>__<scope-id>"`, e.g. `X__3` — see
            // `Element::from_node`'s `Variable` arm) so that two distinct
            // quantifier scopes don't alias to one symbol. That's an
            // interning detail, not KB vocabulary, and must never surface as
            // a search result — e.g. a KB axiom binding `?Human` would
            // otherwise show up as a hit named `Human__15551`.
            if is_scoped_variable_name(&name) { return; }
            if !name.to_lowercase().contains(q) { return; }
            if already_hit.contains(name.as_ref()) { return; }

            let kinds = self.kinds_of(sym_id);
            if let Some(want) = opts.kind {
                if !kind_matches(&kinds, want) { return; }
            }

            let (sid, source, language, text) = match backing.get(&sym_id) {
                Some((sid, source, lang, text)) => (*sid, *source, lang.clone(), text.clone()),
                None => (SentenceId::MAX, SearchSource::Documentation, String::new(), String::new()),
            };

            let rank = search_rank(q, &name, source, 0);
            out.push(SearchHit { symbol: name.to_string(), kinds, source, language, text, sid, rank });
        });
        out
    }
}

// -- Helpers -----------------------------------------------------------------

/// Relevance score for a search hit (higher = better).
///
/// `query_lc` and the compared symbol are lowercased; `match_idx` is the byte
/// offset of the query within the (already lowercased) matched text.  The
/// symbol-name term dominates so an exact/prefix name match outranks a hit that
/// only matched deep inside a documentation blurb; the source tier and match
/// position are secondary nudges.
fn search_rank(query_lc: &str, symbol: &str, source: SearchSource, match_idx: usize) -> f32 {
    let sym_lc = symbol.to_lowercase();
    let name = if sym_lc == query_lc {
        100.0
    } else if sym_lc.starts_with(query_lc) {
        60.0
    } else if sym_lc.contains(query_lc) {
        40.0
    } else {
        0.0
    };
    let src = match source {
        SearchSource::TermFormat    => 12.0,
        SearchSource::Documentation => 6.0,
        SearchSource::Format        => 0.0,
    };
    // Earlier matches score a little higher; a match at the very start gets a
    // small flat bonus.
    let pos = if match_idx == 0 { 4.0 } else { 2.0 / (1.0 + match_idx as f32) };
    name + src + pos
}

/// Preview preference for the name-match snippet: lower is preferred. The
/// documentation string is the real description, so it wins over the terse
/// `termFormat` label and the `format` template. (Distinct from `search_rank`'s
/// source tier, which scores *content* relevance.)
fn source_preview_rank(source: SearchSource) -> u8 {
    match source {
        SearchSource::Documentation => 0,
        SearchSource::TermFormat    => 1,
        SearchSource::Format        => 2,
    }
}

/// Kind-filter matcher.  `--kind relation` matches the broad sense (any of
/// Relation, Predicate, Function); all other kinds require an exact match.
fn kind_matches(have: &[ManKind], want: ManKind) -> bool {
    if want == ManKind::Relation {
        have.iter().any(|k| matches!(
            k,
            ManKind::Relation | ManKind::Predicate | ManKind::Function
        ))
    } else {
        have.contains(&want)
    }
}

/// `true` if `name` is a quantifier/free-variable's scope-qualified interning
/// key rather than real KB vocabulary — i.e. matches `"<base>__<scope-id>"`
/// where `<scope-id>` is the all-digit suffix `Element::from_node`'s
/// `Variable` arm mints per binding scope (see `ScopeCtx::scope_for`).
pub(crate) fn is_scoped_variable_name(name: &str) -> bool {
    match name.rfind("__") {
        Some(idx) if idx > 0 => {
            let suffix = &name[idx + 2..];
            !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
        }
        _ => false,
    }
}

/// Strips a single pair of surrounding double quotes from `s`, if present.
fn strip_quotes(s: &str) -> String {
    let mut s = s.to_string();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s.remove(0);
        s.pop();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kb_from(kif: &str) -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("test.kif"), "test.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        let r = kb.make_session_axiomatic("test.kif");
        assert!(matches!(r, Ok(_)), "promotion failed: {:?}", r.err());
        kb
    }

    #[test]
    fn scoped_variable_name_detection() {
        assert!(is_scoped_variable_name("X__3"));
        assert!(is_scoped_variable_name("Human__15551"));
        assert!(!is_scoped_variable_name("HumanDoll"));
        assert!(!is_scoped_variable_name("subordinateInOrganization"));
        assert!(!is_scoped_variable_name("w__chase_12")); // skolem naming, non-digit suffix
        assert!(!is_scoped_variable_name("__3"));         // no base name before the scope
        assert!(!is_scoped_variable_name("plain"));
    }

    /// The exact bug this exists to prevent: a KB axiom binding `?Human`
    /// interns `Human__<scope>` into the same symbol table as ground
    /// symbols. Searching "Human" must surface the `Human` class (whose own
    /// documentation never repeats its name) without also surfacing that
    /// scope-qualified variable id as if it were a real symbol.
    #[test]
    fn flushed_session_sentences_are_not_searchable() {
        // The editor validates a buffer by telling it into the KB and flushing
        // it again. Nothing that round trip leaves behind should stay visible:
        // a half-typed term surfaced in search as a ghost with partial docs.
        let mut kb = KnowledgeBase::new();
        let r = kb.reload_kif("(documentation Dog EnglishLanguage \"a real dog\")",
                              &std::path::PathBuf::from("t.kif"), "t.kif");
        assert!(r.ok, "seed load failed: {:?}", r.diagnostics);

        kb.tell("(documentation ZzGhost EnglishLanguage \"half typed thing\")", "__scratch__");
        kb.flush_session("__scratch__");

        let opts = SearchOpts { kind: None, language: None, limit: None };
        let hits = kb.search("half typed", &opts);
        assert!(hits.is_empty(),
            "flushed scratch content must not be searchable, got {:?}",
            hits.iter().map(|h| h.symbol.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn search_excludes_scope_qualified_variable_names() {
        let kb = kb_from(
            r#"
            (documentation Human EnglishLanguage "Modern man, the only remaining species of the Homo genus.")
            (subclass Human Hominid)
            (=> (instance ?Human Human) (attribute ?Human Mortal))
            "#,
        );
        let hits = kb.search("Human", &SearchOpts::default());
        assert!(hits.iter().any(|h| h.symbol == "Human"), "expected an exact `Human` hit, got {:?}",
            hits.iter().map(|h| &h.symbol).collect::<Vec<_>>());
        assert!(
            hits.iter().all(|h| !is_scoped_variable_name(&h.symbol)),
            "a scope-qualified variable name leaked into results: {:?}",
            hits.iter().map(|h| &h.symbol).collect::<Vec<_>>()
        );
    }

    #[test]
    fn name_match_preview_prefers_documentation_over_term_format() {
        // A symbol carrying both a terse termFormat and a real documentation
        // string: the name-match snippet must show the documentation, not the
        // one-word label.
        let kb = kb_from(
            r#"
            (instance Triangle Class)
            (termFormat EnglishLanguage Triangle "triangle")
            (documentation Triangle EnglishLanguage "A three-sided polygon.")
            "#,
        );
        let hits = kb.search("Triangle", &SearchOpts::default());
        let hit = hits.iter().find(|h| h.symbol == "Triangle").expect("Triangle hit");
        assert_eq!(hit.source, SearchSource::Documentation, "preview should cite the documentation");
        assert_eq!(hit.text, "A three-sided polygon.");
    }

    #[test]
    fn preview_prefers_the_wanted_language_documentation() {
        let kb = kb_from(
            r#"
            (instance Triangle Class)
            (documentation Triangle EnglishLanguage "A three-sided polygon.")
            (documentation Triangle FrenchLanguage "Un polygone a trois cotes.")
            "#,
        );
        let opts = SearchOpts { language: Some("FrenchLanguage"), ..SearchOpts::default() };
        let hits = kb.search("Triangle", &opts);
        let hit = hits.iter().find(|h| h.symbol == "Triangle").expect("Triangle hit");
        assert_eq!(hit.language, "FrenchLanguage");
        assert_eq!(hit.text, "Un polygone a trois cotes.");
    }

    #[test]
    fn language_filter_never_shows_an_off_filter_snippet() {
        // The wanted-language termFormat matches the query; the only
        // documentation is English. The preview must not "upgrade" the hit to
        // the excluded English documentation — source tier never outranks the
        // language filter.
        let kb = kb_from(
            r#"
            (instance Triangle Class)
            (termFormat FrenchLanguage Triangle "triangle")
            (documentation Triangle EnglishLanguage "A three-sided polygon.")
            "#,
        );
        let opts = SearchOpts { language: Some("FrenchLanguage"), ..SearchOpts::default() };
        let hits = kb.search("triangle", &opts);
        let hit = hits.iter().find(|h| h.symbol == "Triangle").expect("Triangle hit");
        assert_eq!(hit.language, "FrenchLanguage", "snippet language must respect the filter");
        assert_eq!(hit.text, "triangle");
        assert_eq!(hit.source, SearchSource::TermFormat);
    }

    #[test]
    fn language_filtered_name_match_survives_other_language_documentation() {
        // Query matches only the symbol NAME. The wanted-language termFormat
        // must back the hit; the English-only documentation must neither back
        // it nor knock the symbol out of the results.
        let kb = kb_from(
            r#"
            (instance Foo Class)
            (termFormat FrenchLanguage Foo "fou")
            (documentation Foo EnglishLanguage "An English-only description.")
            "#,
        );
        let opts = SearchOpts { language: Some("FrenchLanguage"), ..SearchOpts::default() };
        let hits = kb.search("Foo", &opts);
        let hit = hits.iter().find(|h| h.symbol == "Foo")
            .expect("name match must not be dropped by off-language documentation");
        assert_eq!(hit.language, "FrenchLanguage");
        assert_eq!(hit.text, "fou");
    }

    #[test]
    fn query_matching_several_text_fields_yields_one_row() {
        // "polygon" matches both the termFormat and the documentation of
        // Triangle; the unified preview would render two byte-identical rows.
        let kb = kb_from(
            r#"
            (instance Triangle Class)
            (termFormat EnglishLanguage Triangle "the polygon")
            (documentation Triangle EnglishLanguage "A polygon with three sides.")
            "#,
        );
        let hits = kb.search("polygon", &SearchOpts::default());
        let rows: Vec<_> = hits.iter().filter(|h| h.symbol == "Triangle").collect();
        assert_eq!(rows.len(), 1, "one row per symbol, got {rows:?}");
        assert_eq!(rows[0].text, "A polygon with three sides.");
    }
}
