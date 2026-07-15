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

/// One match in the documentation index.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// The SUMO symbol whose documentation/termFormat/format axiom matched.
    pub symbol:   String,
    /// Classification labels for the symbol (mirrors `ManPage::kinds`).
    pub kinds:    Vec<ManKind>,
    /// Which predicate produced the hit.
    pub source:   SearchSource,
    /// The language tag of the matching axiom (e.g. `"EnglishLanguage"`).
    pub language: String,
    /// The full matching string, surrounding quotes stripped.
    pub text:     String,
    /// SentenceId of the matching axiom (for follow-on tooling).
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
    /// Substring search across SUMO's natural-language fields.
    ///
    /// Returns every documentation / termFormat / format axiom whose
    /// payload string contains `query` (case-insensitive), paired
    /// with the symbol it describes and the symbol's kind.
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
                let text_lc = text.to_lowercase();
                let Some(match_idx) = text_lc.find(&q) else { continue };

                let sym_id: SymbolId = match sent.elements.get(sym_pos) {
                    Some(Element::Symbol(sym)) => sym.id(),
                    _ => continue,
                };

                let lang = match sent.elements.get(lang_pos) {
                    Some(Element::Symbol(sym)) => sym.to_string(),
                    _ => continue,
                };
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

/// Strips a single pair of surrounding double quotes from `s`, if present.
fn strip_quotes(s: &str) -> String {
    let mut s = s.to_string();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s.remove(0);
        s.pop();
    }
    s
}
