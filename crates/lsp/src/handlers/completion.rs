//! `textDocument/completion` handler.
//!
//! Context-aware completion over the shared KB. Three cases are handled by
//! tracking the paren stack up to the cursor:
//!
//!   * `(<CURSOR>` — sentence-head position: suggest operators plus every
//!     relation that appears as a sentence head.
//!   * `(head <args> <CURSOR>` — argument position: when `head`'s domain for
//!     this arg is declared, filter symbols to instances/members of that
//!     class; otherwise offer every symbol.
//!   * Anywhere else (between forms, whitespace, inside a string) — return
//!     an empty response.

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionList, CompletionParams, CompletionResponse,
    Documentation, MarkupContent, MarkupKind,
};

use sigmakee_rs_sdk::{
    KnowledgeBase, ManKind, OpTok, RelationDomain, SearchOpts, TaxConstraint, Token, TokenKind,
    TopLayer,
};

use crate::conv::position_to_offset;
use crate::state::GlobalState;

// -- Public entry point ------------------------------------------------------

/// Handle a `textDocument/completion` request, returning context-aware
/// completion items or `None` when the document or KB is unavailable.
pub fn handle_completion<L: TopLayer>(
    state: &GlobalState<L>,
    params: CompletionParams,
) -> Option<CompletionResponse> {
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;

    log::debug!(
        target: "sumo_lsp",
        "completion requested '{}' {}:{}",
        uri, position.line, position.character
    );
    let t0 = sigmakee_rs_sdk::Instant::now();

    let docs = state.docs.read().ok()?;
    let doc = docs.get(&uri)?;
    let offset = position_to_offset(&doc.rope, position);

    let session = state.session.read().ok()?;
    let kb = session.kb();

    let text = String::from(&doc.rope);

    // The partial word ending at the cursor.
    let prefix = word_prefix_at(&text, offset);

    let ctx = classify_cursor_context(&doc.tokens, offset);
    let (items, truncated) = match ctx {
        CompletionCtx::FormulaHead => suggest_formula_heads(kb, &prefix),
        CompletionCtx::SentenceHead {
            parent_rel,
            self_idx,
        } => suggest_term_heads(kb, parent_rel, self_idx, &prefix),
        CompletionCtx::ArgPosition { head, arg_idx } => suggest_args(kb, head, arg_idx, &prefix),
        CompletionCtx::Free => (Vec::new(), false),
    };

    log::debug!(
        target: "sumo_lsp",
        "completion answered '{}' {} items ({}) in {:?}",
        uri,
        items.len(),
        if truncated { "truncated" } else { "complete" },
        t0.elapsed()
    );

    // A truncated list is marked incomplete so the client re-queries as the
    // user types more of the word (narrowing the prefix), instead of
    // filtering the truncated list locally and never seeing the tail.
    Some(CompletionResponse::List(CompletionList {
        is_incomplete: truncated,
        items,
    }))
}

/// Candidate budget for completion: both the [`SearchOpts::limit`] passed to
/// `search` (bounding how many candidates it reads, not just returns -- see
/// that field's doc comment) and the cap on the final response, past which
/// items are cut and the response marked incomplete. Smaller than `search`'s
/// own `DEFAULT_CANDIDATE_LIMIT` (200): interactive completion pays a
/// per-candidate cost building each `CompletionItem` (a documentation
/// lookup), so a tighter budget trades a handful of items for latency
/// directly, on every keystroke.
const COMPLETION_CANDIDATES: usize = 50;

/// The symbol-shaped word ending at byte `offset` (possibly empty).
fn word_prefix_at(text: &str, offset: usize) -> String {
    let end = offset.min(text.len());
    let bytes = text.as_bytes();
    let mut start = end;
    while start > 0
        && matches!(bytes[start - 1], b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-')
    {
        start -= 1;
    }
    text[start..end].to_string()
}

/// Case-insensitive prefix test; an empty prefix matches everything.
fn matches_prefix(name: &str, prefix_lc: &str) -> bool {
    prefix_lc.is_empty() || name.to_ascii_lowercase().starts_with(prefix_lc)
}

/// True for symbols a user would recognise as ontology terms: KIF variables
/// (`?x` / `@row`), the store's scope-qualified variable interns
/// (`HUMAN__3724`), and CNF Skolem constants are all machine bookkeeping and
/// must never appear as suggestions.  (Same policy the stats view applies.)
fn is_completable_symbol<L: TopLayer>(kb: &KnowledgeBase<L>, name: &str) -> bool {
    !name.starts_with('?')
        && !name.starts_with('@')
        && !kb.symbol_is_variable(name)
        && !kb.symbol_is_skolem(name)
}

/// Relevance-ranked candidate symbols for a non-empty `prefix`, via the KB's
/// [`search`](KnowledgeBase::search) index -- the same engine the discovery
/// UIs use.  Its rank orders exact > prefix > substring name matches and
/// prefers termFormat/documentation-backed symbols. Hits are deduped by
/// symbol (search returns them rank-descending, so first wins), filtered to
/// real ontology terms, and -- unlike the discovery UIs, which want them --
/// filtered to require the *symbol's own name* to actually relate to
/// `prefix`. `search` also surfaces symbols matched only because some
/// documentation/termFormat/format *text* contains the query, with no
/// relation to the symbol's own name (their `search_rank` name component is
/// 0, its lowest tier). Those are legitimate discovery-search results, but a
/// completion item's `label` is the symbol's bare name, and every standard
/// LSP client (Monaco included) filters its displayed suggestion list
/// against each item's label vs. the currently-typed text -- so a
/// name-unrelated hit is never actually shown no matter what's sent, and
/// silently spends a slot in the `COMPLETION_CANDIDATES` budget on something
/// invisible to the user.
///
/// `taxonomy` layers on any ancestor/range constraint the caller has already
/// derived (declared domain/range class); an empty `prefix` combined with a
/// non-empty `taxonomy` still returns results -- `search` enumerates the
/// whole constrained closure when there's no text query -- which is exactly
/// how a domain/range-narrowed suggestion list can appear before the user
/// types anything, while an unconstrained empty prefix (no `taxonomy`
/// either) naturally returns nothing rather than the whole KB.
fn ranked_candidates<L: TopLayer>(
    kb: &KnowledgeBase<L>,
    prefix: &str,
    kind: Option<ManKind>,
    taxonomy: Vec<TaxConstraint>,
) -> Vec<String> {
    let opts = SearchOpts {
        kind,
        language: None,
        limit: Some(COMPLETION_CANDIDATES + 1),
        taxonomy,
        // NOTE: WordNet matching is not used here
        ..SearchOpts::default()
    };
    let prefix_lc = prefix.to_ascii_lowercase();
    let mut seen = std::collections::HashSet::new();
    kb.search(prefix, &opts)
        .into_iter()
        .filter(|h| {
            seen.insert(h.symbol.clone())
                && is_completable_symbol(kb, &h.symbol)
                && (prefix_lc.is_empty() || h.symbol.to_ascii_lowercase().contains(&prefix_lc))
        })
        .map(|h| h.symbol)
        .collect()
}

/// Attach a rank-preserving `sort_text` (LSP clients otherwise re-sort
/// alphabetically / by their own fuzzy score) to each item in order.
fn apply_rank_order(items: &mut [CompletionItem]) {
    for (i, item) in items.iter_mut().enumerate() {
        item.sort_text = Some(format!("{i:05}"));
    }
}

// -- Context classification --------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum CompletionCtx {
    /// The cursor sits at the same position as a `SentenceHead` but
    /// its context means it definately is a formula (e.g. arg to an operator
    /// or top level S expression)
    FormulaHead,
    /// Cursor sits inside an opening paren that does not (syntactically) require
    /// a Formula. Track the rel token from the parent frame and the current
    /// sentence index
    SentenceHead { parent_rel: Token, self_idx: usize },
    /// Inside a list whose head is already determined, at argument
    /// position `arg_idx` (1-based element index: head is 0,
    /// first arg is 1, etc.).
    ArgPosition { head: Token, arg_idx: usize },
    /// Top-level (between forms), inside a string, or any other
    /// non-completable position.
    Free,
}

/// Walk `tokens` up to `cursor_offset` and classify the completion context.
///
/// Tracks a paren stack where each frame records the head name (`None` until
/// the first non-paren token is consumed) and the argument count seen so far;
/// the topmost frame at the cursor determines the context.
fn classify_cursor_context(
    tokens: &[sigmakee_rs_sdk::Token],
    cursor_offset: usize,
) -> CompletionCtx {
    #[derive(Default)]
    struct Frame {
        head: Option<Token>,
        arg_count: usize,
    }
    let mut stack: Vec<Frame> = Vec::new();

    for tok in tokens {
        // A token starting at the cursor offset is "at" the cursor and excluded.
        if tok.span.offset >= cursor_offset {
            break;
        }

        // A word-shaped token the cursor touches (inside it, or right at its
        // end) is the word BEING COMPLETED -- counting it as a consumed
        // element would classify `(instance R|` as argument position 2 and
        // filter by the wrong domain.  Parens are exempt: `(|` must still
        // push its frame for head position to classify.
        let wordish = matches!(
            tok.kind,
            TokenKind::Symbol(_)
                | TokenKind::Operator(_)
                | TokenKind::Variable(_)
                | TokenKind::RowVariable(_)
                | TokenKind::Number(_)
        );
        if wordish && tok.span.end_offset >= cursor_offset {
            break;
        }

        match &tok.kind {
            // Lexical trivia: neither a head nor an argument.
            TokenKind::Comment(_) => {}
            TokenKind::LParen => stack.push(Frame::default()),
            TokenKind::RParen => {
                stack.pop();
            }
            TokenKind::Operator(_)
            | TokenKind::Symbol(_)
            | TokenKind::Variable(_)
            | TokenKind::RowVariable(_)
            | TokenKind::Str(_)
            | TokenKind::Number(_) => {
                if let Some(top) = stack.last_mut() {
                    if top.head.is_none() {
                        // A variable or literal in head position is a parse
                        // error; the empty-string sentinel keeps subsequent
                        // tokens counting as args.
                        top.head = Some(tok.clone());
                    } else {
                        top.arg_count += 1;
                    }
                }
            }
        }
    }

    let mut s = stack.into_iter();
    match (s.next_back(), s.next_back()) {
        (None, _) => CompletionCtx::Free,
        // At the top level, the frame is a formula
        (Some(f), None) if f.head.is_none() => CompletionCtx::FormulaHead,
        // Otherwise, the frame is a formula if the outer frame's head is a non-equals operator
        (Some(Frame { head, .. }), Some(prev)) if head.is_none() => {
            if let Some(prev_head) = prev.head {
                if matches!(&prev_head.kind, TokenKind::Operator(op) if !matches!(op, OpTok::Equal))
                {
                    CompletionCtx::FormulaHead
                } else {
                    CompletionCtx::SentenceHead {
                        parent_rel: prev_head,
                        self_idx: prev.arg_count + 1,
                    }
                }
            } else {
                // A non symbol in the head is a parse error; offer no completion for
                // nauty devs
                CompletionCtx::Free
            }
        }
        (Some(f), _) => CompletionCtx::ArgPosition {
            head: f.head.unwrap_or_default(),
            arg_idx: f.arg_count + 1,
        },
    }
}

// -- Head suggestions --------------------------------------------------------

/// Offer the logical operators plus every predicate name matching `prefix`,
/// via [`ranked_candidates`].  With an empty `prefix` and no other
/// constraint, `search` naturally returns no predicate names -- only the
/// (prefix-matched, so also empty-prefix-inclusive) operator keywords appear
/// until the user starts typing.  Capped; the bool is `true` when candidates
/// were cut at [`COMPLETION_CANDIDATES`].
fn suggest_formula_heads<L: TopLayer>(
    kb: &KnowledgeBase<L>,
    prefix: &str,
) -> (Vec<CompletionItem>, bool) {
    let prefix_lc = prefix.to_ascii_lowercase();
    let mut out: Vec<CompletionItem> = OP_KEYWORDS
        .iter()
        .filter(|op| matches_prefix(op, &prefix_lc))
        .map(|op| CompletionItem {
            label: op.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("logical operator".to_string()),
            ..Default::default()
        })
        .collect();

    let mut names = ranked_candidates(kb, prefix, Some(ManKind::Predicate), Vec::new());

    let budget = COMPLETION_CANDIDATES.saturating_sub(out.len());
    let truncated = names.len() > budget;
    names.truncate(budget);
    out.extend(names.iter().map(|n| item_for_symbol(kb, n)));
    apply_rank_order(&mut out);
    (out, truncated)
}

/// Offer any relation. It attempts to determine the correct relations to offer based on the following
/// factors. Note, by this point, operator arguments have been handled
/// 1. If the outer frame's head symbol's domain at the given index is `Formula` short circuit to
///    suggest formula heads
/// 2. If the outer frame's head symbol's has a domain defined at the given index, find all functions
///    whose range matches said domain
/// 3. If the domain is unknown, offer all relations
fn suggest_term_heads<L: TopLayer>(
    kb: &KnowledgeBase<L>,
    parent_rel: Token,
    self_idx: usize,
    prefix: &str,
) -> (Vec<CompletionItem>, bool) {
    // Determine what the domain of the parent symbol is (at all), expressed
    // as a search taxonomy constraint over the RANGE of candidate relations
    // (not their own subclass/instance position -- a term head here must be
    // a FUNCTION whose result conforms to the domain class).  Empty means
    // the domain couldn't be determined, so every relation is a candidate.
    let taxonomy: Vec<TaxConstraint> = if let Token {
        kind: TokenKind::Symbol(parent_rel_sym),
        ..
    } = &parent_rel
    {
        match kb.domain(parent_rel_sym, self_idx) {
            Some(RelationDomain::Domain(sym_id)) if kb.is_formula_type(sym_id) => {
                return suggest_formula_heads(kb, prefix);
            }
            Some(RelationDomain::Domain(sym_id)) => {
                // The argument must be an INSTANCE of this class, so a
                // nested function call must return one -- `range`, not
                // `rangeSubclass` (which denotes a class-valued result).
                kb.sym_name(sym_id)
                    .map(|name| vec![TaxConstraint::RangeOf(name)])
                    .unwrap_or_default()
            }
            Some(RelationDomain::DomainSubclass(sym_id)) => {
                // The argument must itself be a class that's a SUBCLASS
                // of this one, so a nested function call must return a
                // class-valued term -- `rangeSubclass`, not `range`.
                kb.sym_name(sym_id)
                    .map(|name| vec![TaxConstraint::RangeSubclassOf(name)])
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };

    // No operator keywords until the user starts typing (see suggest_args'
    // "nothing until typing" rule); once typing starts they're prefix-matched
    // as usual.
    let prefix_lc = prefix.to_ascii_lowercase();
    let mut out: Vec<CompletionItem> = if prefix.is_empty() {
        Vec::new()
    } else {
        OP_KEYWORDS
            .iter()
            .filter(|op| matches_prefix(op, &prefix_lc))
            .map(|op| CompletionItem {
                label: op.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some("logical operator".to_string()),
                ..Default::default()
            })
            .collect()
    };

    // A non-empty `taxonomy` (domain known) still returns candidates with an
    // empty prefix -- `search` enumerates the whole constrained closure with
    // no text query. With no domain known AND no prefix typed, this is
    // exactly `ranked_candidates(kb, "", Some(Relation), [])`, which
    // `search`'s own empty-query-and-no-taxonomy guard turns into nothing.
    let mut names = ranked_candidates(kb, prefix, Some(ManKind::Relation), taxonomy);

    let budget = COMPLETION_CANDIDATES.saturating_sub(out.len());
    let truncated = names.len() > budget;
    names.truncate(budget);
    out.extend(names.iter().map(|n| item_for_symbol(kb, n)));
    apply_rank_order(&mut out);
    (out, truncated)
}

const OP_KEYWORDS: &[&str] = &["and", "or", "not", "=>", "<=>", "equal", "forall", "exists"];

// -- Argument suggestions ----------------------------------------------------

/// Offer symbols satisfying the declared domain at this argument position,
/// prefix-filtered and capped.  Falls back to every interned symbol when no
/// domain is declared; Skolem symbols are filtered out.  The bool is `true`
/// when candidates were cut at [`COMPLETION_CANDIDATES`].
fn suggest_args<L: TopLayer>(
    kb: &KnowledgeBase<L>,
    head: Token,
    arg_idx: usize,
    prefix: &str,
) -> (Vec<CompletionItem>, bool) {
    let expected = match head.kind {
        TokenKind::Symbol(head_str) => kb.domain(&head_str, arg_idx),
        _ => None,
    };
    // Domain-conformance constraint, pushed into `search`'s taxonomy list
    // where expressible:
    //   * `Domain(Class)` -- SUMO's `Class` is the implicit superclass of
    //     every class-denoting symbol, whether or not it's explicitly
    //     declared `(instance X Class)` (most classes are instances of a
    //     narrower subclass of `Class`, e.g. `SetOrClass`, instead) --
    //     `TaxConstraint::InstanceOf("Class")` would miss those, so this
    //     case still needs the broad `is_class` classifier as a post-filter
    //     instead of a taxonomy constraint.
    //   * `Domain(class)` -- the argument must be an INSTANCE of `class`.
    //   * `DomainSubclass(class)` -- the argument must itself be a class
    //     that's a SUBCLASS of `class`.
    //   * `Unknown` / no domain declared -- no constraint, everything
    //     conforms (and, with no prefix either, `search` returns nothing).
    let class_type_fallback = matches!(
        expected,
        Some(RelationDomain::Domain(sym_id)) if kb.is_class_type(sym_id)
    );
    let taxonomy: Vec<TaxConstraint> = match expected {
        Some(RelationDomain::Domain(sym_id)) if kb.is_class_type(sym_id) => Vec::new(),
        Some(RelationDomain::Domain(sym_id)) => kb
            .sym_name(sym_id)
            .map(|name| vec![TaxConstraint::InstanceOf(name)])
            .unwrap_or_default(),
        Some(RelationDomain::DomainSubclass(sym_id)) => kb
            .sym_name(sym_id)
            .map(|name| vec![TaxConstraint::SubclassOf(name)])
            .unwrap_or_default(),
        Some(RelationDomain::Unknown) | None => Vec::new(),
    };

    // Relevance-ranked through the search index; rank order is preserved
    // into the items via `sort_text`.  A non-empty `taxonomy` still returns
    // candidates with an empty prefix (the whole constrained closure); with
    // no domain known and nothing typed, this is nothing.
    let mut names: Vec<String> = ranked_candidates(kb, prefix, None, taxonomy)
        .into_iter()
        .filter(|n| !class_type_fallback || kb.symbol_id(n).is_some_and(|id| kb.is_class(id)))
        .collect();

    let truncated = names.len() > COMPLETION_CANDIDATES;
    names.truncate(COMPLETION_CANDIDATES);
    // Item construction (documentation lookup, taxonomy classification) only
    // for the survivors -- this is what bounds the request.
    let mut items: Vec<CompletionItem> = names.iter().map(|n| item_for_symbol(kb, n)).collect();
    apply_rank_order(&mut items);
    (items, truncated)
}

// -- Shared helpers ----------------------------------------------------------

fn item_for_symbol<L: TopLayer>(kb: &KnowledgeBase<L>, name: &str) -> CompletionItem {
    let kind = classify_completion_kind(kb, name);
    let documentation = kb
        .documentation(name, Some("EnglishLanguage"))
        .into_iter()
        .next()
        .map(|d| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: d.text,
            })
        });
    CompletionItem {
        label: name.to_string(),
        kind: Some(kind),
        documentation,
        ..Default::default()
    }
}

fn classify_completion_kind<L: TopLayer>(kb: &KnowledgeBase<L>, name: &str) -> CompletionItemKind {
    let Some(id) = kb.symbol_id(name) else {
        return CompletionItemKind::TEXT;
    };
    if kb.is_class(id) {
        return CompletionItemKind::CLASS;
    }
    if kb.is_function(id) {
        return CompletionItemKind::FUNCTION;
    }
    if kb.is_predicate(id) {
        return CompletionItemKind::INTERFACE;
    }
    if kb.is_relation(id) {
        return CompletionItemKind::INTERFACE;
    }
    if kb.is_instance(id) {
        return CompletionItemKind::CONSTANT;
    }
    CompletionItemKind::VARIABLE
}

// -- Tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;
    use sigmakee_rs_sdk::tokenize_kif;

    fn tokens_for(src: &str) -> Vec<sigmakee_rs_sdk::Token> {
        let (toks, _errs) = tokenize_kif(src, "t.kif");
        toks
    }

    fn kb_from(kif: &str) -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let report = kb.load(
            sigmakee_rs_sdk::SourceFile::kif(std::path::PathBuf::from("t.kif"), kif.to_string()),
            "t.kif",
        );
        assert!(report.ok, "load failed: {:?}", report.diagnostics);
        kb.make_session_axiomatic("t.kif").expect("promotion");
        kb
    }

    #[test]
    fn ranked_candidates_excludes_name_unrelated_documentation_matches() {
        // "BoneStructure" is a legitimate name match for "Bon". "Skeleton"'s
        // documentation happens to contain "bon" ("...composed of bones...")
        // but its own name has nothing to do with the typed prefix -- a
        // standard LSP client filters its displayed list against each
        // item's label vs. the typed text, so a name-unrelated hit like this
        // is never actually shown no matter what's sent (see the doc
        // comment on `ranked_candidates`).
        let kb = kb_from(
            r#"
            (subclass BoneStructure Entity)
            (subclass Skeleton Entity)
            (documentation Skeleton EnglishLanguage "A structure composed of bones.")
            "#,
        );
        let names = ranked_candidates(&kb, "Bon", None, Vec::new());
        assert!(
            names.contains(&"BoneStructure".to_string()),
            "a real name match must survive: {names:?}"
        );
        assert!(
            !names.contains(&"Skeleton".to_string()),
            "a hit that matched only via documentation text, with a name \
             unrelated to the prefix, must be filtered out: {names:?}"
        );
    }

    #[test]
    fn cursor_right_after_open_paren_is_formula_head() {
        let src = "(";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, 1);
        assert_eq!(ctx, CompletionCtx::FormulaHead);
    }

    #[test]
    fn cursor_in_operator_is_formula_head() {
        let src = "(or (";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, src.len());
        assert_eq!(ctx, CompletionCtx::FormulaHead);
    }

    #[test]
    fn cursor_in_operator_second_is_formula_head() {
        let src = "(or (something) (";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, src.len());
        assert_eq!(ctx, CompletionCtx::FormulaHead);
    }

    #[test]
    fn cursor_in_arg_is_sentence_head() {
        let src = "(something (";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, src.len());
        assert_matches!(ctx, CompletionCtx::SentenceHead { parent_rel, self_idx } if parent_rel.to_string() == "something" && self_idx == 1);
    }

    #[test]
    fn cursor_after_head_and_space_is_arg_1() {
        let src = "(subclass ";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, src.len());
        match ctx {
            CompletionCtx::ArgPosition { head, arg_idx } => {
                assert_eq!(head.to_string(), "subclass");
                assert_eq!(arg_idx, 1);
            }
            other => panic!("expected ArgPosition, got {:?}", other),
        }
    }

    #[test]
    fn cursor_after_two_args_is_arg_3() {
        let src = "(subclass Human Animal ";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, src.len());
        match ctx {
            CompletionCtx::ArgPosition { head, arg_idx } => {
                assert_eq!(head.to_string(), "subclass");
                assert_eq!(arg_idx, 3);
            }
            other => panic!("expected ArgPosition arg 3, got {:?}", other),
        }
    }

    #[test]
    fn cursor_at_top_level_is_free() {
        let src = "(subclass Human Animal) ";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, src.len());
        assert_eq!(ctx, CompletionCtx::Free);
    }

    #[test]
    fn nested_list_picks_innermost_frame() {
        let src = "(=> (instance ?X ";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, src.len());
        match ctx {
            CompletionCtx::ArgPosition { head, arg_idx } => {
                assert_eq!(head.to_string(), "instance");
                assert_eq!(arg_idx, 2);
            }
            other => panic!("expected inner ArgPosition, got {:?}", other),
        }
    }

    #[test]
    fn operator_head_recognised() {
        let src = "(forall ";
        let toks = tokens_for(src);
        let ctx = classify_cursor_context(&toks, src.len());
        match ctx {
            CompletionCtx::ArgPosition { head, arg_idx } => {
                assert_eq!(head.to_string(), "forall");
                assert_eq!(arg_idx, 1);
            }
            other => panic!("expected forall ArgPosition, got {:?}", other),
        }
    }
}
