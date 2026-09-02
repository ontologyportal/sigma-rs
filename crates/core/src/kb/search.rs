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
    /// Hit was in the third arg of `(termFormat …)` -- the symbol's
    /// short natural-language name.
    TermFormat,
    /// Hit was in the third arg of `(documentation …)` -- the long
    /// English description.
    Documentation,
    /// Hit was in the third arg of `(format …)` -- a relation's
    /// natural-language template.
    Format,
}

impl SearchSource {
    /// Short label for this source (`"term"`, `"doc"`, or `"format"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TermFormat => "term",
            Self::Documentation => "doc",
            Self::Format => "format",
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
    /// Relevance score, higher = better.  Combines symbol-name match quality
    /// (exact > prefix > substring > name doesn't contain the query), the
    /// source tier (termFormat > documentation > format), and how early the
    /// query appears in the matched text.  Hits are returned sorted by this
    /// descending (ties broken by symbol name, then `sid`).
    pub rank: f32,
}

/// A constraint on search hits, checked as a symbol-name membership test
/// against a precomputed allow-set.  [`SearchOpts::taxonomy`] takes a list of
/// these, ANDed together (a hit must satisfy every constraint in the list).
///
/// `SubclassOf`/`InstanceOf` are also expressible inline in the query string
/// as `-subclass->Class` / `-instance->Class` tokens (see
/// [`KnowledgeBase::search`]); a non-empty explicit [`SearchOpts::taxonomy`]
/// wins over the inline form rather than combining with it.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

impl<'a> Default for SearchOpts<'a> {
    fn default() -> Self {
        Self {
            kind: None,
            language: None,
            limit: Some(DEFAULT_CANDIDATE_LIMIT),
            taxonomy: Vec::new(),
        }
    }
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
    /// repeats their name -- e.g. SUMO's `Human` class is glossed as "Modern
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
    /// source tier (termFormat -> documentation -> format) and match position as
    /// tie-breakers, then symbol name and `sid` for determinism.  Apply
    /// [`SearchOpts::kind`] / [`SearchOpts::language`] for narrowing; pass
    /// `SearchOpts::default()` for no filtering.
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
                let Some(match_idx) = text_lc.find(&q) else {
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
                let rank = search_rank(&q, &symbol, source, match_idx, occurrence);
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
                            rank,
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
        // skipped once enough higher-ranked hits already exist -- but ONLY
        // when no taxonomy constraint is active. `tax_allow` filters `hits`
        // (including whatever `name_match_hits` itself finds via its own
        // prefix fast path) *after* this point, so with a constraint set, no
        // count taken now -- of `hits` or of the fast path's own results --
        // can predict how many will actually survive that later filter.
        let short_circuit_limit = if tax_allow.is_none() {
            opts.limit
        } else {
            None
        };
        let name_hits =
            self.name_match_hits(&q, opts, &backing, &already_hit, &hits, short_circuit_limit);
        hits.extend(name_hits);

        // Taxonomy filter: one choke point AFTER both passes, BEFORE
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
            let rank = search_rank(q, name, source, 0, occurrence);
            out.push(SearchHit {
                symbol: name.to_string(),
                kinds,
                source,
                language,
                text,
                sid,
                rank,
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
}

// -- Helpers -----------------------------------------------------------------

/// Relevance score for a search hit (higher = better).
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
) -> f32 {
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
        SearchSource::TermFormat => 12.0,
        SearchSource::Documentation => 6.0,
        SearchSource::Format => 0.0,
    };
    // Earlier matches score a little higher; a match at the very start gets a
    // small flat bonus.
    let pos = if match_idx == 0 {
        4.0
    } else {
        2.0 / (1.0 + match_idx as f32)
    };
    name + src + pos + occurrence_bonus(occurrence)
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
    }
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
}
