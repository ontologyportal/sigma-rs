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

        let mut hits: Vec<SearchHit> = Vec::new();
        // Per-symbol first-seen (sid, source, language, text) from the scan
        // below, kept regardless of whether `q` matched — the name-match pass
        // uses this to give a symbol with no text hit of its own a real
        // citation + preview instead of a bare, unsourced row.  SCHEMAS is
        // scanned termFormat-first, so "first-seen" already prefers the
        // higher source tier.
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

                backing.entry(sym_id)
                    .or_insert_with(|| (sid, source, lang.clone(), strip_quotes(text)));

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
                hits.push(SearchHit {
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

        let already_hit: HashSet<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        hits.extend(self.name_match_hits(&q, opts, &backing, &already_hit));

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
    /// A symbol with no backing text has no natural language tag, so
    /// [`SearchOpts::language`] only filters it out when it *does* have
    /// backing text in a different language; language-less name hits always
    /// pass through.
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
            if let Some(want) = opts.language {
                if !language.is_empty() && language != want { return; }
            }

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
fn is_scoped_variable_name(name: &str) -> bool {
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
}
