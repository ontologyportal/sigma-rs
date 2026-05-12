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
    /// Sort key — lower is more relevant.
    fn rank(self) -> u8 {
        match self {
            Self::TermFormat    => 0,
            Self::Documentation => 1,
            Self::Format        => 2,
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
    /// Hits are sorted by source relevance (termFormat → documentation
    /// → format) and then alphabetically by symbol name within each
    /// source.  Apply [`SearchOpts::kind`] / [`SearchOpts::language`]
    /// for narrowing; pass `SearchOpts::default()` for no filtering.
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
                if !text.to_lowercase().contains(&q) {
                    continue;
                }

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
                hits.push(SearchHit {
                    symbol,
                    kinds,
                    source,
                    language: lang,
                    text:     strip_quotes(text),
                    sid,
                });
            }
        }

        // Stable sort keeps KB insertion order within a (source, symbol) cluster.
        hits.sort_by(|a, b| {
            a.source.rank().cmp(&b.source.rank())
                .then_with(|| a.symbol.cmp(&b.symbol))
        });

        if let Some(n) = opts.limit {
            hits.truncate(n);
        }
        hits
    }
}

// -- Helpers -----------------------------------------------------------------

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
