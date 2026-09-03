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
//! (independent of the text scan above) -- see [`KnowledgeBase::search`]'s
//! doc comment for why this exists.

use std::collections::{HashMap, HashSet};

use super::KnowledgeBase;
use crate::kb::man::ManKind;
use crate::layer::{Layer, TopLayer};
use crate::types::{Element, Literal, SymbolId};
use crate::SentenceId;

// -- Public types ------------------------------------------------------------

/// Which of the three documentation predicates produced a match.
///
/// Used by the CLI to render a label ("doc" / "term" / "format") and to sort
/// hits by source relevance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchSource {
    /// Hit was in the third arg of `(termFormat …)`
    TermFormat,
    /// Hit was in the third arg of `(documentation …)``
    Documentation,
    /// Hit was in the third arg of `(format …)`
    Format,
    /// Hit came from the WordNet lexicon (see `SearchOpts::lexicon`, only
    /// ever produced with the `lexicon` feature)
    /// `SearchHit::sense` carries the sense tag; `SearchHit::text` carries
    /// the synset gloss.
    WordNet,
}

impl SearchSource {
    /// Short label for this source (`"term"`, `"doc"`, `"format"`, or `"wn"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TermFormat => "term",
            Self::Documentation => "doc",
            Self::Format => "format",
            Self::WordNet => "wn",
        }
    }
}

/// One match: either a documentation/termFormat/format axiom whose text
/// contains the query, or (see [`KnowledgeBase::search`]) a symbol whose own
/// *name* matches the query but which has no such axiom to cite -- the latter
/// carries an empty `language`/`text` and `sid == SentenceId::MAX` as a
/// "no backing axiom" sentinel.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// The SUMO symbol whose documentation/termFormat/format axiom matched,
    /// or whose own name matched the query directly.
    pub symbol: String,
    /// Classification labels for the symbol (mirrors `ManPage::kinds`).
    pub kinds: Vec<ManKind>,
    /// Which predicate produced the hit (best-effort -- `Documentation` when
    /// the hit came from the unsourced name-match pass).
    pub source: SearchSource,
    /// The language tag of the matching axiom (e.g. `"EnglishLanguage"`), or
    /// `""` for an unsourced name-match hit.
    pub language: String,
    /// The full matching string, surrounding quotes stripped, or `""` for an
    /// unsourced name-match hit.
    pub text: String,
    /// SentenceId of the matching axiom, or `SentenceId::MAX` for an
    /// unsourced name-match hit (no backing axiom to cite).
    pub sid: SentenceId,
    /// For [`SearchSource::WordNet`] hits: the matched sense plus the
    /// mapping-kind suffix in the mappings files' own notation
    /// For example: `"dog#n#1+"` (`=` equivalent, `+` subsuming, `@` instance).
    /// This field is empty for all other hit sources
    pub sense: String,
    /// Relevance score, higher = better.  Combines symbol-name match quality
    /// (exact > prefix > substring > name doesn't contain the query), the
    /// source tier (termFormat > documentation > format), and how early the
    /// query appears in the matched text.  Hits are returned sorted by this
    /// descending (ties broken by symbol name, then `sid`).  The sum of
    /// [`rank_breakdown`](Self::rank_breakdown)'s values.
    pub rank: f32,
    /// The named contributions [`rank`](Self::rank) is the sum of
    pub rank_breakdown: Vec<RankComponent>,
}

/// One labeled contribution to a [`SearchHit::rank`] score.
#[derive(Debug, Clone, PartialEq)]
pub struct RankComponent {
    /// Human-readable name of this contribution, e.g. `"exact name match"`.
    pub label: &'static str,
    /// This contribution's share of [`SearchHit::rank`].  Every component
    /// that could apply to this hit's tier is always present, even at
    /// `0.0` -- e.g. a `format`-sourced hit still lists its source-tier
    /// component (worth `0.0`), so the breakdown is a complete accounting
    /// of the formula, not just the nonzero terms.
    pub value: f32,
}

/// A constraint on search hits, checked as a symbol-name membership test
/// against a precomputed allow-set.  [`SearchOpts::taxonomy`] takes a list of
/// these, ANDed together (a hit must satisfy every constraint in the list).
///
/// `SubclassOf`/`InstanceOf` are also expressible inline in the query string
/// as `-subclass->Class` / `-instance->Class` tokens (see
/// [`KnowledgeBase::search`]); a non-empty explicit [`SearchOpts::taxonomy`]
/// wins over the inline form rather than combining with it.
///
/// `Serialize`/`Deserialize` use serde's default externally-tagged newtype
/// representation (e.g. `{"subclassOf": "Animal"}`) -- the shape the wasm
/// search binding's `taxonomy` parameter takes from JS (see
/// `crates/wasm/src/session/views.rs`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaxConstraint {
    /// Transitive subclasses of the class (the class itself excluded):
    /// with `(subclass A B)` `(subclass B C)`, `SubclassOf("C")` yields
    /// both `B` and `A`.
    SubclassOf(String),
    /// Instances of the class or of any of its transitive subclasses:
    /// with `(instance Z A)` `(subclass A C)`, `InstanceOf("C")` yields `Z`.
    InstanceOf(String),
    /// Relations whose declared `range` is the class or a transitive
    /// `subclass` descendant of it -- e.g. `RangeOf("Human")` yields every
    /// function whose result is declared to be a `Human` (or narrower).
    /// A schema-probe over `range` declarations (see
    /// [`KnowledgeBase::relations_with_range`]), not a subclass/instance
    /// walk over the relation symbols themselves.
    RangeOf(String),
    /// Like [`RangeOf`](Self::RangeOf), but over `rangeSubclass` instead of
    /// `range` -- SUMO's convention for a function that itself returns a
    /// class-denoting term.
    RangeSubclassOf(String),
}

/// Default [`SearchOpts::limit`] -- the candidate budget a caller gets
/// unless it overrides `limit` explicitly. Bounds both how many hits
/// `search` returns AND, via `name_match_hits`'s fallback-scan
/// short-circuit, how much internal scanning work a broad query does: a
/// smaller budget means less work, not just a smaller response. Interactive
/// callers with a tighter latency budget than plain discovery search (e.g.
/// LSP completion, which pays a per-candidate cost building each
/// `CompletionItem`) should pass a smaller `Some(n)` explicitly -- see
/// `crates/lsp/src/handlers/completion.rs`.
pub const DEFAULT_CANDIDATE_LIMIT: usize = 200;

/// Optional filters for [`KnowledgeBase::search`].  All fields except
/// `limit` are best-effort: unknown kinds simply match nothing, unknown
/// languages match nothing.
#[derive(Debug, Clone)]
pub struct SearchOpts<'a> {
    /// Filter to only hits of this kind (e.g. only `Class`).
    /// `None` accepts any kind.
    pub kind: Option<ManKind>,
    /// Filter to only axioms tagged with this language.
    /// E.g. `Some("EnglishLanguage")`.  `None` accepts any language.
    pub language: Option<&'a str>,
    /// Cap on the number of candidates `search` reads and returns.
    /// `Some(n)` also bounds internal scanning work (see
    /// [`DEFAULT_CANDIDATE_LIMIT`]'s doc comment); `None` = no cap, every
    /// matching candidate is read and returned. Defaults to
    /// `Some(`[`DEFAULT_CANDIDATE_LIMIT`]`)`, not `None` -- an explicit,
    /// unbounded discovery search needs `limit: None` set deliberately.
    pub limit: Option<usize>,
    /// Restrict hits to symbols satisfying every constraint in this list
    /// (AND).  Empty accepts everything (unless the query string carries an
    /// inline constraint).
    pub taxonomy: Vec<TaxConstraint>,
    /// Synonym expansion: when set, the query is also looked up as a word in
    /// this WordNet lexicon and every SUMO term its synsets are anchored to
    /// becomes a [`SearchSource::WordNet`] hit. IF a hit is on a symbol not
    /// currently present in the KB, it is filtered out. All other filters still
    /// apply
    #[cfg(feature = "lexicon")]
    pub lexicon: Option<&'a crate::lexicon::WordNet>,
    /// WordNet-only mode: skip the documentation/termFormat/format text scan
    /// and the symbol-name pass entirely; return only lexicon hits. With no
    /// [`SearchOpts::lexicon`] set this will always result in an empty array.
    #[cfg(feature = "lexicon")]
    pub wordnet_only: bool,
}

impl<'a> Default for SearchOpts<'a> {
    fn default() -> Self {
        Self {
            kind: None,
            language: None,
            limit: Some(DEFAULT_CANDIDATE_LIMIT),
            taxonomy: Vec::new(),
            #[cfg(feature = "lexicon")]
            lexicon: None,
            #[cfg(feature = "lexicon")]
            wordnet_only: false,
        }
    }
}

// -- KB method ---------------------------------------------------------------

impl<L: TopLayer + Layer> KnowledgeBase<L> {
    /// The primary search API for symbols in the `KnowledgeBase`. It takes
    /// a variety of parameters and sorts outputs based on a relevance metric.
    /// The search looks for the search query using the following strategies:
    ///
    /// 1. Returns every `documentation` / `termFormat` / `format` axiom whose
    ///    payload string contains `query` (case-insensitive), paired
    ///    with the symbol it describes and the symbol's kind.
    ///
    /// 2. (see [`name_match_hits`]) Matches `query` directly against every
    ///    symbol's own name, independent of what its documentation says.
    ///
    /// 3. When the `lexicon` parameter is set, the search will also attempt
    ///    to find WordNet synsets that match the query and return the
    ///    corresponding SUMO symbol mapping as defined by the WordNet lexicon.
    ///    Importantly, WordNet filtering happens independent of text based
    ///    search
    ///
    /// Hits are sorted by [`SearchHit::rank`] (relevance, descending): a
    /// symbol whose *name* matches the query (exact > prefix > substring)
    /// outranks one that only matched inside a documentation blurb, with the
    /// source tier (termFormat -> documentation -> format) and match position as
    /// tie-breakers, then symbol name and `sid` for determinism.  
    ///
    /// See [`SearchOpts`] for additional options that can be used to control
    /// and filter results.
    pub fn search(&self, query: &str, opts: &SearchOpts) -> Vec<SearchHit> {
        // Inline taxonomy syntax: `-subclass->Class` / `-instance->Class`
        // tokens anywhere in the query restrict hits to that transitive
        // closure (see [`TaxConstraint`]); the remaining tokens are the text
        // query.  A taxonomy constraint with NO text query enumerates the
        // whole closure (an empty needle matches every candidate below).
        let (text_query, inline_tax) = split_taxonomy_query(query);
        let mut taxonomy = opts.taxonomy.clone();
        if taxonomy.is_empty() {
            taxonomy.extend(inline_tax);
        }
        if text_query.is_empty() && taxonomy.is_empty() {
            return Vec::new();
        }
        // The allow-set for the constraints, computed once: symbol names
        // satisfying every constraint in `taxonomy` (intersection).  `None`
        // = unconstrained.
        let tax_allow: Option<HashSet<String>> = taxonomy.iter().fold(None, |acc, t| {
            let set: HashSet<String> = match t {
                TaxConstraint::SubclassOf(c) => self.subclasses_of(c).into_iter().collect(),
                TaxConstraint::InstanceOf(c) => self.instances_of(c).into_iter().collect(),
                TaxConstraint::RangeOf(c) => {
                    self.relations_with_range(c, false).into_iter().collect()
                }
                TaxConstraint::RangeSubclassOf(c) => {
                    self.relations_with_range(c, true).into_iter().collect()
                }
            };
            Some(match acc {
                None => set,
                Some(prev) => prev.intersection(&set).cloned().collect(),
            })
        });
        let q = text_query.to_lowercase();

        // Potentially skip the naive text searches
        #[cfg(feature = "lexicon")]
        let text_passes = !opts.wordnet_only;
        #[cfg(not(feature = "lexicon"))]
        let text_passes = true;

        let mut hits = if text_passes {
            self.text_and_name_hits(&q, opts, &tax_allow)
        } else {
            Vec::new()
        };

        // WordNet synonym expansion: added AFTER the text/name passes (so it
        // can dedup against their symbols) but BEFORE the taxonomy filter
        // below, so a WordNet hit outside an active taxonomy constraint's
        // closure is excluded exactly like any other hit.
        #[cfg(feature = "lexicon")]
        if let Some(wn) = opts.lexicon {
            self.apply_wordnet(&q, wn, opts, &mut hits);
        }

        // Taxonomy filter: one choke point AFTER every pass, BEFORE
        // rank-sort and the limit -- so the cap counts only in-closure hits.
        if let Some(allow) = &tax_allow {
            hits.retain(|h| allow.contains(&h.symbol));
        }

        // Sort by relevance (descending), then deterministic tie-breaks. The
        // stable sort preserves KB order for hits with an identical key.
        hits.sort_by(|a, b| {
            b.rank
                .partial_cmp(&a.rank)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.symbol.cmp(&b.symbol))
                .then_with(|| a.sid.cmp(&b.sid))
        });

        if let Some(n) = opts.limit {
            hits.truncate(n);
        }
        hits
    }

    /// The text-field scan plus the name-match pass (everything
    /// [`Self::search`] does except WordNet expansion). Skipped entirely in
    /// `SearchOpts::wordnet_only` mode. `tax_allow` is read only to decide
    /// whether the name-match fallback scan's limit short-circuit is safe
    /// (see [`Self::name_match_hits`]'s doc comment)
    fn text_and_name_hits(
        &self,
        q: &str,
        opts: &SearchOpts,
        tax_allow: &Option<HashSet<String>>,
    ) -> Vec<SearchHit> {
        let syn = &self.layer.semantic().syntactic;

        // Best text hit per symbol -- one row per symbol, keeping the
        // highest-ranked match when the query hits several of its text fields
        // (the displayed snippet is unified from `backing` below, so extra
        // rows would render as duplicates).
        let mut text_hits: HashMap<SymbolId, SearchHit> = HashMap::new();
        // Per-symbol (sid, source, language, text) from the scan below, kept
        // regardless of whether `q` matched -- the name-match pass uses this to
        // give a symbol with no text hit of its own a real citation + preview
        // instead of a bare, unsourced row.  The preview prefers the
        // *documentation* string (the real description) over the terse
        // `termFormat` label, then `format`; ties within a tier keep first-seen.
        // When a language filter is set, off-language entries never enter the
        // map: the preview must respect the same filter the matches do.
        let mut backing: HashMap<SymbolId, (SentenceId, SearchSource, String, String)> =
            HashMap::new();

        // (head_name, symbol_arg_index, lang_arg_index, text_arg_index, source).
        // Arg indices are into `Sentence.elements`, where `elements[0]` is the
        // head and arguments start at `elements[1]`.
        const SCHEMAS: &[(&str, usize, usize, usize, SearchSource)] = &[
            ("termFormat", 2, 1, 3, SearchSource::TermFormat),
            ("documentation", 1, 2, 3, SearchSource::Documentation),
            ("format", 2, 1, 3, SearchSource::Format),
        ];

        for &(head, sym_pos, lang_pos, text_pos, source) in SCHEMAS {
            for sid in syn.by_head(head).iter().copied() {
                let Some(sent) = syn.sentence(sid) else {
                    continue;
                };

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
                    let better = backing
                        .get(&sym_id)
                        .is_none_or(|cur| source_preview_rank(source) < source_preview_rank(cur.1));
                    if better {
                        backing.insert(sym_id, (sid, source, lang.clone(), strip_quotes(text)));
                    }
                }

                let text_lc = text.to_lowercase();
                let Some(match_idx) = text_lc.find(q) else {
                    continue;
                };

                if let Some(want) = opts.language {
                    if lang != want {
                        continue;
                    }
                }

                let kinds = self.kinds_of(sym_id);
                if let Some(want) = opts.kind {
                    if !kind_matches(&kinds, want) {
                        continue;
                    }
                }

                let symbol = match syn.sym_name(sym_id) {
                    Some(s) => s.name().to_string(),
                    None => continue,
                };
                let occurrence = syn.sine_current(|idx| idx.generality(sym_id));
                let rank_breakdown = search_rank(q, &symbol, source, match_idx, occurrence);
                let rank = sum_rank(&rank_breakdown);
                let keep = text_hits.get(&sym_id).is_none_or(|cur| rank > cur.rank);
                if keep {
                    text_hits.insert(
                        sym_id,
                        SearchHit {
                            symbol,
                            kinds,
                            source,
                            language: lang,
                            text: strip_quotes(text),
                            sid,
                            sense: String::new(),
                            rank,
                            rank_breakdown,
                        },
                    );
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
        // The fallback substring scan inside `name_match_hits` may be
        // skipped once enough higher-ranked hits already exist, but ONLY
        // when no taxonomy constraint is active. `tax_allow` filters `hits`
        // (including whatever `name_match_hits` itself finds via its own
        // prefix fast path) later in `search`, so with a constraint set, no
        // count taken now -- of `hits` or of the fast path's own results --
        // can predict how many will actually survive that later filter.
        let short_circuit_limit = if tax_allow.is_none() {
            opts.limit
        } else {
            None
        };
        let name_hits =
            self.name_match_hits(q, opts, &backing, &already_hit, &hits, short_circuit_limit);
        hits.extend(name_hits);
        hits
    }

    /// The name-match pass described in [`Self::search`]'s doc comment: every
    /// interned, non-Skolem, non-variable symbol whose own name matches `q`
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
    ///
    /// Two passes:
    ///   1. **Prefix fast path**, via `syn.symbols_with_prefix` (a sorted-name
    ///      index maintained by `syntactic::symbols` -- O(log n + matches)
    ///      instead of a full-table scan). Covers exact and prefix name
    ///      matches: `search_rank`'s two highest name tiers.
    ///   2. **Substring fallback**, a full scan for names that *contain* `q`
    ///      without starting with it (the remaining, lowest name tier) -- no
    ///      sorted index can accelerate an arbitrary substring query. Skipped
    ///      entirely when `already_high_tier` (the caller's already-computed
    ///      text-field hits) plus the fast path's own results already total
    ///      at least `short_circuit_limit`: the prefix tier's rank floor (60)
    ///      strictly exceeds the substring tier's ceiling (name 40 + source
    ///      12 + position 4 + occurrence bonus <= 3 = 59), so no
    ///      substring-only hit could survive the caller's final sort +
    ///      truncate regardless. `short_circuit_limit` must be `None`
    ///      whenever a taxonomy constraint is active (passed by `search`) --
    ///      `tax_allow` filters hits, including this pass's own prefix
    ///      results, *after* this function returns, so no count taken here
    ///      can predict what survives that later filter.
    fn name_match_hits(
        &self,
        q: &str,
        opts: &SearchOpts,
        backing: &HashMap<SymbolId, (SentenceId, SearchSource, String, String)>,
        already_hit: &HashSet<&str>,
        already_high_tier: &[SearchHit],
        short_circuit_limit: Option<usize>,
    ) -> Vec<SearchHit> {
        let syn = &self.layer.semantic().syntactic;
        let mut out = Vec::new();
        let mut seen: HashSet<SymbolId> = HashSet::new();

        let build = |sym_id: SymbolId, name: &str, out: &mut Vec<SearchHit>| {
            // `?X`/`@X` variables are interned into this same table under a
            // scope-qualified key (`"<name>__<scope-id>"`, e.g. `X__3` -- see
            // `Element::from_node`'s `Variable` arm) so that two distinct
            // quantifier scopes don't alias to one symbol. That's an
            // interning detail, not KB vocabulary, and must never surface as
            // a search result -- e.g. a KB axiom binding `?Human` would
            // otherwise show up as a hit named `Human__15551`.
            if is_scoped_variable_name(name) {
                return;
            }
            if already_hit.contains(name) {
                return;
            }

            let kinds = self.kinds_of(sym_id);
            if let Some(want) = opts.kind {
                if !kind_matches(&kinds, want) {
                    return;
                }
            }

            let (sid, source, language, text) = match backing.get(&sym_id) {
                Some((sid, source, lang, text)) => (*sid, *source, lang.clone(), text.clone()),
                None => (
                    SentenceId::MAX,
                    SearchSource::Documentation,
                    String::new(),
                    String::new(),
                ),
            };

            let occurrence = syn.sine_current(|idx| idx.generality(sym_id));
            let rank_breakdown = search_rank(q, name, source, 0, occurrence);
            let rank = sum_rank(&rank_breakdown);
            out.push(SearchHit {
                symbol: name.to_string(),
                kinds,
                source,
                language,
                text,
                sid,
                sense: String::new(),
                rank,
                rank_breakdown,
            });
        };

        for (sym, sym_id) in syn.symbols_with_prefix(q) {
            seen.insert(sym_id);
            build(sym_id, &sym.name(), &mut out);
        }

        if let Some(limit) = short_circuit_limit {
            let high_tier = out.len() + already_high_tier.iter().filter(|h| h.rank >= 60.0).count();
            if high_tier >= limit {
                return out;
            }
        }

        syn.symbols.entries().for_each(|(&sym_id, sym)| {
            if seen.contains(&sym_id) || syn.is_skolem(sym_id) {
                return;
            }
            let name = sym.name();
            if !name.to_lowercase().contains(q) {
                return;
            }
            build(sym_id, &name, &mut out);
        });
        out
    }

    /// WordNet synonym expansion (see [`SearchOpts::lexicon`]): every SUMO
    /// term `q`'s senses anchor to, applied in place to `hits`.
    ///
    /// A term can match on both the normal doc string name search and the
    /// WordNet-sourced row for the same query. Its WordNet
    /// evidence is folded into the existing hit's `rank_breakdown`/`rank` as
    /// an extra component
    ///
    /// Two graceful-degradation points, both silent (never an error):
    ///   - a synset may anchor to a SUMO term the *loaded* KB doesn't
    ///     currently have (e.g. only `Merge.kif` is loaded, but the synset
    ///     anchors to a `Mid-level-ontology.kif` term) -- `symbol_id` returns
    ///     `None` and that anchor is skipped, so results are filtered to
    ///     terms that actually exist in the loaded KB;
    ///   - every anchor kind is included regardless of strength (`=`, `+`,
    ///     `@`, or the rare negated forms) -- none are dropped, only ranked
    ///     lower (see [`wordnet_rank`]).
    #[cfg(feature = "lexicon")]
    fn apply_wordnet(
        &self,
        q: &str,
        wn: &crate::lexicon::WordNet,
        opts: &SearchOpts,
        hits: &mut Vec<SearchHit>,
    ) {
        let mut existing_idx: HashMap<String, usize> = HashMap::new();
        for (i, h) in hits.iter().enumerate() {
            existing_idx.entry(h.symbol.clone()).or_insert(i);
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut new_hits = Vec::new();
        for sense in wn.senses(q) {
            for anchor in &sense.synset.sumo {
                if !seen.insert(anchor.term.clone()) {
                    continue;
                }
                let Some(sym_id) = self.symbol_id(&anchor.term) else {
                    continue;
                };
                let kinds = self.kinds_of(sym_id);
                if let Some(want) = opts.kind {
                    if !kind_matches(&kinds, want) {
                        continue;
                    }
                }
                let rank_breakdown = wordnet_rank(anchor.kind, sense.sense_no);
                if let Some(&i) = existing_idx.get(&anchor.term) {
                    let existing = &mut hits[i];
                    existing.rank += sum_rank(&rank_breakdown);
                    existing.rank_breakdown.extend(rank_breakdown);
                    if existing.sense.is_empty() {
                        existing.sense = format!("{}{}", sense.label(), anchor.kind.suffix());
                    }
                    continue;
                }
                new_hits.push(SearchHit {
                    symbol: anchor.term.clone(),
                    kinds,
                    source: SearchSource::WordNet,
                    language: String::new(),
                    text: sense.synset.gloss.clone(),
                    sid: SentenceId::MAX,
                    sense: format!("{}{}", sense.label(), anchor.kind.suffix()),
                    rank: sum_rank(&rank_breakdown),
                    rank_breakdown,
                });
            }
        }
        hits.extend(new_hits);
    }
}

// -- Helpers -----------------------------------------------------------------

/// Relevance breakdown for a search hit -- [`SearchHit::rank`] is the sum of
/// these components' values (higher = better).
///
/// `query_lc` and the compared symbol are lowercased; `match_idx` is the byte
/// offset of the query within the (already lowercased) matched text.  The
/// symbol-name term dominates so an exact/prefix name match outranks a hit that
/// only matched deep inside a documentation blurb; the source tier and match
/// position are secondary nudges.
fn search_rank(
    query_lc: &str,
    symbol: &str,
    source: SearchSource,
    match_idx: usize,
    occurrence: usize,
) -> Vec<RankComponent> {
    let sym_lc = symbol.to_lowercase();
    let (name_label, name_value) = if sym_lc == query_lc {
        ("exact name match", 100.0)
    } else if sym_lc.starts_with(query_lc) {
        ("prefix name match", 60.0)
    } else if sym_lc.contains(query_lc) {
        ("substring name match", 40.0)
    } else {
        ("no name match", 0.0)
    };
    let (src_label, src_value) = match source {
        SearchSource::TermFormat => ("termFormat source", 12.0),
        SearchSource::Documentation => ("documentation source", 6.0),
        SearchSource::Format => ("format source", 0.0),
        // WordNet hits are scored by `wordnet_rank`, never routed here; the
        // arm only exists for match correctness.
        SearchSource::WordNet => ("wordnet source", 0.0),
    };
    // Earlier matches score a little higher; a match at the very start gets a
    // small flat bonus.
    let pos_value = if match_idx == 0 {
        4.0
    } else {
        2.0 / (1.0 + match_idx as f32)
    };
    vec![
        RankComponent {
            label: name_label,
            value: name_value,
        },
        RankComponent {
            label: src_label,
            value: src_value,
        },
        RankComponent {
            label: "match position",
            value: pos_value,
        },
        RankComponent {
            label: "usage frequency",
            value: occurrence_bonus(occurrence),
        },
    ]
}

/// Diminishing-returns nudge toward symbols that appear in more axioms
/// (`SineIndex::generality`, exposed via `SyntacticLayer::sine_current`) --
/// a proxy for how central/commonly-used a symbol is. Capped well below a
/// single name-match tier step (40 -> 60 -> 100), so it only breaks ties
/// within a tier (e.g. among several substring matches) and never promotes a
/// weaker name match over a stronger one.
fn occurrence_bonus(occurrence: usize) -> f32 {
    (occurrence as f32).ln_1p().min(3.0)
}

/// Preview preference for the name-match snippet: lower is preferred. The
/// documentation string is the real description, so it wins over the terse
/// `termFormat` label and the `format` template. (Distinct from `search_rank`'s
/// source tier, which scores *content* relevance.)
fn source_preview_rank(source: SearchSource) -> u8 {
    match source {
        SearchSource::Documentation => 0,
        SearchSource::TermFormat => 1,
        SearchSource::Format => 2,
        // Exhaustiveness only.
        SearchSource::WordNet => 3,
    }
}

/// Relevance score for a WordNet synonym hit. The base tier tracks how
/// strong the anchor is:
/// 1. An `=` (equivalent) mapping is the best possible synonym evidence
///    and lands just above a termFormat text hit
/// 2. A `+` (subsuming) sits between termFormat and documentation tiers
///    and a most-frequent-sense bonus decays hyperbolically so `dog#n#1`
///    outranks `dog#n#7`.
///
/// NOTE: Exact/prefix *name* matches (100/60) always outrank any
/// WordNet hit, by design.
#[cfg(feature = "lexicon")]
fn wordnet_rank(kind: crate::lexicon::MappingKind, sense_no: usize) -> Vec<RankComponent> {
    use crate::lexicon::MappingKind::*;
    let (label, base) = match kind {
        Equivalent => ("equivalent WordNet anchor", 20.0),
        Subsuming => ("subsuming WordNet anchor", 8.0),
        Instance => ("instance WordNet anchor", 6.0),
        Other(_) => ("weak WordNet anchor", 2.0),
    };
    vec![
        RankComponent { label, value: base },
        RankComponent {
            label: "most-frequent-sense bonus",
            value: 4.0 / sense_no.max(1) as f32,
        },
    ]
}

/// Sum a rank breakdown's components into the flat [`SearchHit::rank`] score.
fn sum_rank(components: &[RankComponent]) -> f32 {
    components.iter().map(|c| c.value).sum()
}

/// Split inline taxonomy tokens (`-subclass->Class` / `-instance->Class`)
/// out of a raw query string.  Remaining whitespace-separated tokens rejoin
/// as the text query; the LAST taxonomy token wins when several appear.
fn split_taxonomy_query(query: &str) -> (String, Option<TaxConstraint>) {
    let mut text: Vec<&str> = Vec::new();
    let mut tax = None;
    for tok in query.split_whitespace() {
        if let Some(class) = tok.strip_prefix("-subclass->") {
            if !class.is_empty() {
                tax = Some(TaxConstraint::SubclassOf(class.to_string()));
                continue;
            }
        }
        if let Some(class) = tok.strip_prefix("-instance->") {
            if !class.is_empty() {
                tax = Some(TaxConstraint::InstanceOf(class.to_string()));
                continue;
            }
        }
        text.push(tok);
    }
    (text.join(" "), tax)
}

/// Kind-filter matcher.  `--kind relation` matches the broad sense (any of
/// Relation, Predicate, Function); all other kinds require an exact match.
fn kind_matches(have: &[ManKind], want: ManKind) -> bool {
    if want == ManKind::Relation {
        have.iter().any(|k| {
            matches!(
                k,
                ManKind::Relation | ManKind::Predicate | ManKind::Function
            )
        })
    } else {
        have.contains(&want)
    }
}

/// `true` if `name` is a quantifier/free-variable's scope-qualified interning
/// key rather than real KB vocabulary -- i.e. matches `"<base>__<scope-id>"`
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
        assert!(r.is_ok(), "promotion failed: {:?}", r.err());
        kb
    }

    #[test]
    fn default_opts_limit_to_the_documented_candidate_budget() {
        assert_eq!(
            SearchOpts::default().limit,
            Some(DEFAULT_CANDIDATE_LIMIT),
            "SearchOpts::default() must not be unbounded -- an explicit \
             discovery search opts in via `limit: None` deliberately"
        );
        assert_eq!(
            DEFAULT_CANDIDATE_LIMIT, 200,
            "a change here is a behavior change for every caller that \
             relies on the default (e.g. `sumo search`'s CLI default)"
        );
    }

    #[test]
    fn search_truncates_to_the_configured_limit() {
        let kb = kb_from(
            "(subclass FooA Entity)\n(subclass FooB Entity)\n(subclass FooC Entity)\n\
             (subclass FooD Entity)\n(subclass FooE Entity)\n",
        );
        let opts = SearchOpts {
            limit: Some(2),
            ..SearchOpts::default()
        };
        let hits = kb.search("Foo", &opts);
        assert_eq!(
            hits.len(),
            2,
            "an explicit smaller limit overrides the default and caps output: {hits:?}"
        );
    }

    #[test]
    fn taxonomy_subclass_constraint_is_transitive() {
        let kb = kb_from(
            "(subclass A B)\n(subclass B C)\n(subclass Unrelated Entity)\n(instance Z A)\n",
        );
        // Enumeration: constraint alone, no text query.
        let hits = kb.search("-subclass->C", &SearchOpts::default());
        let syms: Vec<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        assert!(syms.contains(&"A"), "transitive subclass A: {syms:?}");
        assert!(syms.contains(&"B"), "direct subclass B: {syms:?}");
        assert!(
            !syms.contains(&"C"),
            "the class itself is excluded: {syms:?}"
        );
        assert!(
            !syms.contains(&"Unrelated"),
            "outside the closure: {syms:?}"
        );
        assert!(
            !syms.contains(&"Z"),
            "instances are not subclasses: {syms:?}"
        );
    }

    #[test]
    fn taxonomy_instance_constraint_walks_the_subclass_closure() {
        let kb = kb_from(
            "(subclass A B)\n(subclass B C)\n(instance Z A)\n(instance Y C)\n(instance X Entity)\n",
        );
        let hits = kb.search("-instance->C", &SearchOpts::default());
        let syms: Vec<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        assert!(
            syms.contains(&"Z"),
            "instance of transitive subclass: {syms:?}"
        );
        assert!(syms.contains(&"Y"), "direct instance: {syms:?}");
        assert!(
            !syms.contains(&"X"),
            "instance outside the closure: {syms:?}"
        );
        assert!(!syms.contains(&"A"), "classes are not instances: {syms:?}");
    }

    #[test]
    fn taxonomy_constraint_combines_with_a_text_query() {
        let kb = kb_from(concat!(
            "(subclass Dog Animal)\n(subclass Cat Animal)\n(subclass Rock Entity)\n",
            "(documentation Dog EnglishLanguage \"A furry companion.\")\n",
            "(documentation Rock EnglishLanguage \"A furry-looking mineral, allegedly.\")\n",
        ));
        // Text matches BOTH Dog and Rock; the constraint keeps only Dog.
        let hits = kb.search("furry -subclass->Animal", &SearchOpts::default());
        let syms: Vec<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        assert_eq!(
            syms,
            vec!["Dog"],
            "constraint must intersect the text query"
        );
    }

    #[test]
    fn explicit_taxonomy_opt_beats_inline_syntax() {
        let kb = kb_from("(subclass A B)\n(subclass B C)\n(subclass D E)\n");
        let opts = SearchOpts {
            taxonomy: vec![TaxConstraint::SubclassOf("E".into())],
            ..SearchOpts::default()
        };
        let hits = kb.search("-subclass->C", &opts);
        let syms: Vec<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        assert_eq!(syms, vec!["D"], "explicit opt wins over inline: {syms:?}");
    }

    #[test]
    fn range_of_constraint_finds_relations_by_declared_range() {
        let kb = kb_from(
            r#"
            (subclass ChildOfEntity Entity)
            (range subclass ChildOfEntity)
            (range instance ChildOfEntity)
            (subclass NaturalLanguage ChildOfEntity)
            (range documentation NaturalLanguage)
            (rangeSubclass UnionFn ChildOfEntity)
        "#,
        );
        let opts = SearchOpts {
            taxonomy: vec![TaxConstraint::RangeOf("ChildOfEntity".into())],
            ..SearchOpts::default()
        };
        // Constraint alone (no text query) enumerates the whole set.
        let hits = kb.search("", &opts);
        let syms: Vec<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        for want in ["subclass", "instance", "documentation"] {
            assert!(syms.contains(&want), "missing {want} in {syms:?}");
        }
        assert!(
            !syms.contains(&"UnionFn"),
            "rangeSubclass must not satisfy RangeOf: {syms:?}"
        );
    }

    #[test]
    fn range_subclass_of_constraint_is_distinct_from_range_of() {
        let kb = kb_from(
            r#"
            (subclass ChildOfEntity Entity)
            (range instance ChildOfEntity)
            (rangeSubclass UnionFn ChildOfEntity)
        "#,
        );
        let opts = SearchOpts {
            taxonomy: vec![TaxConstraint::RangeSubclassOf("ChildOfEntity".into())],
            ..SearchOpts::default()
        };
        let hits = kb.search("", &opts);
        let syms: Vec<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        assert_eq!(
            syms,
            vec!["UnionFn"],
            "only the rangeSubclass hit: {syms:?}"
        );
    }

    #[test]
    fn multiple_taxonomy_constraints_are_anded() {
        // B and D are both subclasses of Root, but only D is also an
        // instance of Marked -- a single-constraint search would return
        // both; combining constraints must narrow to just D.
        let kb = kb_from(
            "(subclass B Root)\n(subclass D Root)\n(instance D Marked)\n(instance E Marked)\n",
        );
        let opts = SearchOpts {
            taxonomy: vec![
                TaxConstraint::SubclassOf("Root".into()),
                TaxConstraint::InstanceOf("Marked".into()),
            ],
            ..SearchOpts::default()
        };
        let hits = kb.search("", &opts);
        let syms: Vec<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        assert_eq!(syms, vec!["D"], "AND of both constraints: {syms:?}");
    }

    #[test]
    fn substring_only_name_match_still_found_via_fallback() {
        // "man" is a substring of HumanDoll but not a prefix -- must still
        // be found via the substring fallback scan, not just the prefix
        // fast path.
        let kb = kb_from("(instance HumanDoll Class)");
        let hits = kb.search("man", &SearchOpts::default());
        let syms: Vec<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        assert!(
            syms.contains(&"HumanDoll"),
            "substring-only name match must still be found: {syms:?}"
        );
    }

    #[test]
    fn taxonomy_constraint_disables_the_limit_short_circuit() {
        // Enough prefix-tier hits (AAA1..AAA3) exist to satisfy `limit`
        // alone -- but none of them are `Container` instances, so `tax_allow`
        // filters all three out AFTER name_match_hits runs. Only
        // XAAContainer (a substring-only match, outside the prefix fast
        // path) is in the taxonomy closure. If the fallback's limit
        // short-circuit didn't also check for an active taxonomy constraint,
        // it would count the 3 (soon-to-be-filtered) prefix hits as "enough",
        // skip scanning for XAAContainer entirely, and the final result
        // would wrongly come back empty.
        let kb = kb_from(
            "(instance Container Class)\n(instance Other Class)\n\
             (instance AAA1 Other)\n(instance AAA2 Other)\n(instance AAA3 Other)\n\
             (instance XAAContainer Container)\n",
        );
        let opts = SearchOpts {
            limit: Some(2),
            taxonomy: vec![TaxConstraint::InstanceOf("Container".into())],
            ..SearchOpts::default()
        };
        let hits = kb.search("AA", &opts);
        let syms: Vec<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        assert_eq!(
            syms,
            vec!["XAAContainer"],
            "a substring-only match must still be found when a taxonomy \
             constraint filters out all the prefix-tier hits: {syms:?}"
        );
    }

    #[test]
    fn scoped_variable_name_detection() {
        assert!(is_scoped_variable_name("X__3"));
        assert!(is_scoped_variable_name("Human__15551"));
        assert!(!is_scoped_variable_name("HumanDoll"));
        assert!(!is_scoped_variable_name("subordinateInOrganization"));
        assert!(!is_scoped_variable_name("w__chase_12")); // skolem naming, non-digit suffix
        assert!(!is_scoped_variable_name("__3")); // no base name before the scope
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
        let r = kb.reload_kif(
            "(documentation Dog EnglishLanguage \"a real dog\")",
            &std::path::PathBuf::from("t.kif"),
            "t.kif",
        );
        assert!(r.ok, "seed load failed: {:?}", r.diagnostics);

        kb.tell(
            "(documentation ZzGhost EnglishLanguage \"half typed thing\")",
            "__scratch__",
        );
        kb.flush_session("__scratch__");

        let opts = SearchOpts {
            kind: None,
            language: None,
            limit: None,
            taxonomy: Vec::new(),
            ..SearchOpts::default()
        };
        let hits = kb.search("half typed", &opts);
        assert!(
            hits.is_empty(),
            "flushed scratch content must not be searchable, got {:?}",
            hits.iter().map(|h| h.symbol.clone()).collect::<Vec<_>>()
        );
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
        assert!(
            hits.iter().any(|h| h.symbol == "Human"),
            "expected an exact `Human` hit, got {:?}",
            hits.iter().map(|h| &h.symbol).collect::<Vec<_>>()
        );
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
        let hit = hits
            .iter()
            .find(|h| h.symbol == "Triangle")
            .expect("Triangle hit");
        assert_eq!(
            hit.source,
            SearchSource::Documentation,
            "preview should cite the documentation"
        );
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
        let opts = SearchOpts {
            language: Some("FrenchLanguage"),
            ..SearchOpts::default()
        };
        let hits = kb.search("Triangle", &opts);
        let hit = hits
            .iter()
            .find(|h| h.symbol == "Triangle")
            .expect("Triangle hit");
        assert_eq!(hit.language, "FrenchLanguage");
        assert_eq!(hit.text, "Un polygone a trois cotes.");
    }

    #[test]
    fn language_filter_never_shows_an_off_filter_snippet() {
        // The wanted-language termFormat matches the query; the only
        // documentation is English. The preview must not "upgrade" the hit to
        // the excluded English documentation -- source tier never outranks the
        // language filter.
        let kb = kb_from(
            r#"
            (instance Triangle Class)
            (termFormat FrenchLanguage Triangle "triangle")
            (documentation Triangle EnglishLanguage "A three-sided polygon.")
            "#,
        );
        let opts = SearchOpts {
            language: Some("FrenchLanguage"),
            ..SearchOpts::default()
        };
        let hits = kb.search("triangle", &opts);
        let hit = hits
            .iter()
            .find(|h| h.symbol == "Triangle")
            .expect("Triangle hit");
        assert_eq!(
            hit.language, "FrenchLanguage",
            "snippet language must respect the filter"
        );
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
        let opts = SearchOpts {
            language: Some("FrenchLanguage"),
            ..SearchOpts::default()
        };
        let hits = kb.search("Foo", &opts);
        let hit = hits
            .iter()
            .find(|h| h.symbol == "Foo")
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

    #[test]
    fn rank_prefers_the_more_frequently_used_symbol_within_a_tier() {
        // FooA and FooZ both prefix-match "Foo" (same name-match tier), and
        // neither has any documentation/termFormat text, so without an
        // occurrence signal they'd tie-break alphabetically (FooA first).
        // FooZ appears in far more axioms; the occurrence bonus must place
        // it first despite losing the alphabetical tie-break.
        let kb = kb_from(
            "(subclass FooA Entity)\n(subclass FooZ Entity)\n(instance a FooZ)\n\
             (instance b FooZ)\n(instance c FooZ)\n(instance d FooZ)\n",
        );
        let hits = kb.search("Foo", &SearchOpts::default());
        let syms: Vec<&str> = hits.iter().map(|h| h.symbol.as_str()).collect();
        let pos_a = syms.iter().position(|&s| s == "FooA").expect("FooA hit");
        let pos_z = syms.iter().position(|&s| s == "FooZ").expect("FooZ hit");
        assert!(
            pos_z < pos_a,
            "more frequently used FooZ must outrank FooA: {syms:?}"
        );
    }

    #[cfg(feature = "lexicon")]
    fn fixture_lexicon() -> crate::lexicon::WordNet {
        crate::lexicon::WordNet::from_texts(
            [(
                "02084071 05 n 03 dog 0 domestic_dog 0 Canis_familiaris 0 001 @ 02083346 n 0000 | a domesticated canine &%Canine+\n\
                 02121620 05 n 01 cat 0 001 @ 02120997 n 0000 | feline mammal &%Feline+\n",
                crate::lexicon::Pos::Noun,
            )],
            None,
            None,
        )
    }

    /// The synonym-expansion payoff: no KB string contains "dog", yet the
    /// query surfaces `Canine` via the lexicon -- while `Feline`, whose
    /// anchoring synset doesn't match the query, and any anchored term *not
    /// interned in the KB* stay absent.
    #[cfg(feature = "lexicon")]
    #[test]
    fn wordnet_expansion_surfaces_anchored_terms_present_in_kb() {
        let kb = kb_from(
            r#"
            (documentation Canine EnglishLanguage "A carnivorous mammal of the family Canidae.")
            (subclass Canine Mammal)
            "#,
        );
        let wn = fixture_lexicon();

        let plain = kb.search("dog", &SearchOpts::default());
        assert!(
            plain.is_empty(),
            "no lexicon -> no hits, got {:?}",
            plain.iter().map(|h| &h.symbol).collect::<Vec<_>>()
        );

        let opts = SearchOpts {
            lexicon: Some(&wn),
            ..Default::default()
        };
        let hits = kb.search("dog", &opts);
        assert_eq!(
            hits.len(),
            1,
            "got {:?}",
            hits.iter().map(|h| &h.symbol).collect::<Vec<_>>()
        );
        let h = &hits[0];
        assert_eq!(h.symbol, "Canine");
        assert_eq!(h.source, SearchSource::WordNet);
        assert_eq!(h.sense, "dog#n#1+");
        assert_eq!(h.text, "a domesticated canine");
        assert_eq!(h.sid, SentenceId::MAX);

        // `cat` anchors to `Feline`, which is not interned in this KB.
        assert!(
            kb.search("cat", &opts).is_empty(),
            "an anchored term missing from the KB must not be recommended"
        );
    }

    /// A symbol already surfaced by the text/name passes must not appear a
    /// second time as a WordNet row (never-twice invariant) -- but its
    /// WordNet evidence still counts: the anchor's rank components are folded
    /// into the existing hit instead of being dropped, and its `sense` tag is
    /// filled in.
    #[cfg(feature = "lexicon")]
    #[test]
    fn wordnet_hits_deduplicate_against_other_passes() {
        let kb = kb_from(r#"(documentation Canine EnglishLanguage "The dog family.")"#);
        let wn = fixture_lexicon();
        let opts = SearchOpts {
            lexicon: Some(&wn),
            ..Default::default()
        };
        // "dog" text-matches Canine's documentation AND anchors to Canine.
        let hits = kb.search("dog", &opts);
        let canine: Vec<_> = hits.iter().filter(|h| h.symbol == "Canine").collect();
        assert_eq!(
            canine.len(),
            1,
            "duplicate rows: {:?}",
            hits.iter()
                .map(|h| (&h.symbol, h.source))
                .collect::<Vec<_>>()
        );
        assert_eq!(canine[0].source, SearchSource::Documentation);
        assert_eq!(canine[0].sense, "dog#n#1+");
        assert!(
            canine[0]
                .rank_breakdown
                .iter()
                .any(|c| c.label == "subsuming WordNet anchor"),
            "expected a folded-in WordNet rank component, got {:?}",
            canine[0].rank_breakdown
        );
        assert_eq!(canine[0].rank, sum_rank(&canine[0].rank_breakdown));
    }

    /// `wordnet_only` suppresses the text and name passes: the same KB where
    /// "dog" text-matches Canine's documentation yields exactly one hit, and
    /// it is the WordNet row (which, with no other passes to dedup against,
    /// now carries the gloss + sense tag).
    #[cfg(feature = "lexicon")]
    #[test]
    fn wordnet_only_suppresses_text_and_name_passes() {
        let kb = kb_from(r#"(documentation Canine EnglishLanguage "The dog family.")"#);
        let wn = fixture_lexicon();
        let opts = SearchOpts {
            lexicon: Some(&wn),
            wordnet_only: true,
            ..Default::default()
        };
        let hits = kb.search("dog", &opts);
        assert_eq!(
            hits.len(),
            1,
            "got {:?}",
            hits.iter()
                .map(|h| (&h.symbol, h.source))
                .collect::<Vec<_>>()
        );
        assert_eq!(hits[0].symbol, "Canine");
        assert_eq!(hits[0].source, SearchSource::WordNet);
        assert_eq!(hits[0].sense, "dog#n#1+");
    }

    /// `wordnet_only` with no lexicon supplied yields nothing -- never a
    /// silent fall-back to the text passes.
    #[cfg(feature = "lexicon")]
    #[test]
    fn wordnet_only_without_lexicon_is_empty() {
        let kb = kb_from(r#"(documentation Canine EnglishLanguage "The dog family.")"#);
        let opts = SearchOpts {
            wordnet_only: true,
            ..Default::default()
        };
        assert!(kb.search("dog", &opts).is_empty());
    }

    /// New reconciliation behavior versus the branch this was ported from:
    /// an active taxonomy constraint filters WordNet hits exactly like any
    /// other hit -- a synonym-expansion result outside the constrained
    /// closure must not leak through.
    #[cfg(feature = "lexicon")]
    #[test]
    fn wordnet_hits_are_filtered_by_an_active_taxonomy_constraint() {
        let kb = kb_from(
            r#"
            (subclass Canine Mammal)
            (subclass Rock Entity)
            "#,
        );
        let wn = fixture_lexicon();
        let opts = SearchOpts {
            lexicon: Some(&wn),
            taxonomy: vec![TaxConstraint::SubclassOf("Entity".into())],
            ..Default::default()
        };
        // "dog" anchors to Canine, which is a Mammal, not a subclass of
        // Entity via this KB's (deliberately disconnected) taxonomy.
        let hits = kb.search("dog", &opts);
        assert!(
            hits.iter().all(|h| h.symbol != "Canine"),
            "WordNet hit outside the taxonomy closure must be filtered: {:?}",
            hits.iter().map(|h| &h.symbol).collect::<Vec<_>>()
        );
    }

    /// The wasm search binding deserializes `TaxConstraint` from a JS array
    /// of externally-tagged newtype objects (`{"subclassOf": "Animal"}`);
    /// this is the plain-JSON shape that decodes into, one variant per test.
    #[test]
    fn tax_constraint_deserializes_from_externally_tagged_json() {
        let cases = [
            (
                r#"{"subclassOf":"Animal"}"#,
                TaxConstraint::SubclassOf("Animal".into()),
            ),
            (
                r#"{"instanceOf":"Human"}"#,
                TaxConstraint::InstanceOf("Human".into()),
            ),
            (
                r#"{"rangeOf":"Human"}"#,
                TaxConstraint::RangeOf("Human".into()),
            ),
            (
                r#"{"rangeSubclassOf":"Human"}"#,
                TaxConstraint::RangeSubclassOf("Human".into()),
            ),
        ];
        for (json, want) in cases {
            let got: TaxConstraint = serde_json::from_str(json).expect("deserializes");
            assert_eq!(got, want, "for {json}");
        }
    }

    #[test]
    fn rank_breakdown_sums_to_rank_for_a_text_hit() {
        let kb = kb_from(r#"(documentation Triangle EnglishLanguage "A three-sided polygon.")"#);
        let hits = kb.search("Triangle", &SearchOpts::default());
        let hit = hits
            .iter()
            .find(|h| h.symbol == "Triangle")
            .expect("Triangle hit");
        let summed: f32 = hit.rank_breakdown.iter().map(|c| c.value).sum();
        assert!(
            (hit.rank - summed).abs() < 1e-4,
            "rank {} != breakdown sum {} ({:?})",
            hit.rank,
            summed,
            hit.rank_breakdown
        );
        // Exact name match -> its labeled component is present and dominant.
        assert!(
            hit.rank_breakdown
                .iter()
                .any(|c| c.label == "exact name match" && c.value == 100.0),
            "{:?}",
            hit.rank_breakdown
        );
    }

    #[cfg(feature = "lexicon")]
    #[test]
    fn rank_breakdown_sums_to_rank_for_a_wordnet_hit() {
        let kb = kb_from(r#"(instance Canine SetOrClass)"#);
        let wn = fixture_lexicon();
        let opts = SearchOpts {
            lexicon: Some(&wn),
            ..Default::default()
        };
        let hits = kb.search("dog", &opts);
        let hit = hits
            .iter()
            .find(|h| h.symbol == "Canine")
            .expect("Canine hit");
        let summed: f32 = hit.rank_breakdown.iter().map(|c| c.value).sum();
        assert!(
            (hit.rank - summed).abs() < 1e-4,
            "rank {} != breakdown sum {} ({:?})",
            hit.rank,
            summed,
            hit.rank_breakdown
        );
        assert!(
            hit.rank_breakdown
                .iter()
                .any(|c| c.label == "subsuming WordNet anchor" && c.value == 8.0),
            "{:?}",
            hit.rank_breakdown
        );
        assert!(
            hit.rank_breakdown
                .iter()
                .any(|c| c.label == "most-frequent-sense bonus"),
            "{:?}",
            hit.rank_breakdown
        );
    }
}
