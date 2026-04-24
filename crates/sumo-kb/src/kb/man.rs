// crates/sumo-kb/src/kb/man.rs
//
// Symbol introspection -- the ontology-native equivalent of doc comments.
// KIF has no syntactic doc-comment; authoring intent lives in three
// special relations:
//
//   (documentation    Symbol   Language "text")
//   (termFormat       Language Symbol   "text")
//   (format           Language relation "format-string")
//
// All three are ordinary binary/ternary relations stored as root
// sentences in the KIF store.  This module scans the head-indexed
// view for them and exposes typed lookups plus a combined
// [`ManPage`] that also carries kind / parents / signature data
// from the semantic layer.  All queries are pure reads.
//
// The `KnowledgeBase::man*` methods return plain `Vec<DocEntry>` so
// multi-language KBs (English + other) can be rendered without loss.

use crate::SentenceId;
use crate::kif_store::KifStore;
use crate::semantic::RelationDomain;
use crate::types::{Element, SymbolId};

use super::KnowledgeBase;

// -- Public types -------------------------------------------------------------

/// A single documentation blurb as authored in the ontology.
///
/// `text` has the surrounding quotes of the KIF string literal stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocEntry {
    pub language: String,
    pub text:     String,
}

/// Category classification for a symbol.  A single symbol may belong to
/// more than one category (e.g. a relation that is also an instance of
/// `BinaryPredicate`); callers receive the full set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManKind {
    Class,
    Relation,
    Function,
    Predicate,
    Instance,
    /// No ancestry or relation declaration found -- a bare constant.
    Individual,
}

impl ManKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Class      => "class",
            Self::Relation   => "relation",
            Self::Function   => "function",
            Self::Predicate  => "predicate",
            Self::Instance   => "instance",
            Self::Individual => "individual",
        }
    }
}

/// Signature expectation for one argument or return value of a relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortSig {
    pub class:    String,
    /// True for `(domainSubclass …)` / `(rangeSubclass …)` declarations --
    /// the argument is itself a *class* (not an instance of it).
    pub subclass: bool,
}

/// One (position, sid) reference to a sentence where the symbol
/// appears at the sentence's **root** level.
///
/// - `position == 0`  — the symbol is the head of the root list
///   (e.g. `(Human X)` references `Human` at position 0).
/// - `position >= 1`  — the symbol is an argument at that 1-based
///   position counting from the head (e.g. `(instance X Human)`
///   references `Human` at position 2).
///
/// Sentences where the symbol only appears inside a nested
/// sub-sentence are recorded in [`ManPage::ref_nested`] instead —
/// see that field for the rationale.
#[derive(Debug, Clone)]
pub struct SentenceRef(pub usize, pub SentenceId);

/// Everything a man-page view needs for one symbol.
#[derive(Debug, Clone)]
pub struct ManPage {
    pub name:          String,
    pub kinds:         Vec<ManKind>,
    /// All `(documentation sym _ _)` hits in KB order.
    pub documentation: Vec<DocEntry>,
    /// All `(termFormat _ sym _)` hits in KB order.
    pub term_format:   Vec<DocEntry>,
    /// All `(format _ sym _)` hits in KB order.  Empty for non-relation symbols.
    pub format:        Vec<DocEntry>,
    /// Taxonomic parents: `(subclass sym Parent)` / `(instance sym Parent)` / ...
    pub parents:       Vec<ParentEdge>,
    /// Declared arity (from the `BinaryRelation` / `TernaryRelation` / ...
    /// ancestry).  `None` when unknown; `Some(-1)` for variable-arity
    /// relations.
    pub arity:         Option<i32>,
    /// Positional domains indexed by 1-based argument position.
    /// Arguments with no explicit declaration are elided.
    pub domains:       Vec<(usize, SortSig)>,
    /// Declared range (functions and relations that declare one).
    pub range:         Option<SortSig>,
    /// Sentences where the symbol appears at the **root level** of
    /// the sentence's element list, along with the 0-based position
    /// of its first such occurrence.  One entry per sentence (first
    /// position wins) — if the same symbol appears at multiple root
    /// positions in one sentence, only the leftmost is recorded.
    pub ref_args:      Vec<SentenceRef>,
    /// Sentences where the symbol appears **only inside a nested
    /// sub-sentence** (never at the root level).  Common for
    /// quantified axioms like `(forall (?X) (instance ?X Human))`:
    /// `Human` is buried inside the sub-sentence, so the root's
    /// elements are `[forall, vars, Sub]` and none of them is
    /// literally `Human`.  These references are surfaced separately
    /// so consumers can display them under a dedicated heading
    /// without mis-reporting an argument position.
    pub ref_nested:    Vec<SentenceId>,
}

/// One taxonomic parent edge from the symbol.  `relation` is the KIF
/// head that introduced it (`subclass`, `instance`, `subrelation`,
/// `subAttribute`); `parent` is the other side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentEdge {
    pub relation: String,
    pub parent:   String,
}

// -- KnowledgeBase API --------------------------------------------------------

impl KnowledgeBase {
    /// Return every `(documentation <symbol> <lang> "...")` entry.  When
    /// `language` is `Some`, only entries matching that language tag are
    /// returned.  Result preserves KB insertion order.
    ///
    /// Backed by the `SemanticCache`: the first call per symbol scans
    /// the store's head-indexed view once; subsequent calls (and every
    /// language variant thereof) are HashMap hits.
    pub fn documentation(&self, symbol: &str, language: Option<&str>) -> Vec<DocEntry> {
        let Some(id) = self.symbol_id(symbol) else { return Vec::new(); };
        self.layer.documentation(id, language)
    }

    /// Return every `(termFormat <lang> <symbol> "...")` entry.
    pub fn term_format(&self, symbol: &str, language: Option<&str>) -> Vec<DocEntry> {
        let Some(id) = self.symbol_id(symbol) else { return Vec::new(); };
        self.layer.term_format(id, language)
    }

    /// Return every `(format <lang> <relation> "...")` entry.  Semantically
    /// meaningful only for relation symbols, but the scan is symmetric.
    pub fn format_string(&self, relation: &str, language: Option<&str>) -> Vec<DocEntry> {
        let Some(id) = self.symbol_id(relation) else { return Vec::new(); };
        self.layer.format(id, language)
    }

    /// Build a full man-page view for `symbol`.  `None` if the symbol
    /// is not interned in the KB.  All fields are best-effort: the
    /// query succeeds even when some data is missing, in which case
    /// the corresponding field is empty / `None`.
    pub fn manpage(&self, symbol: &str) -> Option<ManPage> {
        let sym_id = self.symbol_id(symbol)?;
        Some(build_manpage(self, sym_id, symbol))
    }
}

fn build_manpage(kb: &KnowledgeBase, sym_id: SymbolId, name: &str) -> ManPage {
    // Classification: ordering is chosen for human readability -- the
    // most specific label first.  Redundant labels are suppressed
    // (e.g. a predicate also passes `is_relation`; report only predicate).
    let mut kinds = Vec::new();
    if kb.is_class(sym_id)     { kinds.push(ManKind::Class); }
    if kb.is_function(sym_id)  { kinds.push(ManKind::Function); }
    if kb.is_predicate(sym_id) { kinds.push(ManKind::Predicate); }
    if kb.is_relation(sym_id)
        && !kinds.iter().any(|k| matches!(k, ManKind::Function | ManKind::Predicate))
    {
        kinds.push(ManKind::Relation);
    }
    if kb.is_instance(sym_id)  { kinds.push(ManKind::Instance); }
    if kinds.is_empty()        { kinds.push(ManKind::Individual); }

    let parents = collect_parents(&kb.layer.store, sym_id);
    let (arity, domains, range) = signature(kb, sym_id);
    let (ref_args, ref_nested) = collect_refs(&kb.layer.store, sym_id);

    ManPage {
        name: name.to_string(),
        kinds,
        documentation: kb.documentation(name, None),
        term_format:   kb.term_format(name, None),
        format:        kb.format_string(name, None),
        parents,
        arity,
        domains,
        range,
        ref_args,
        ref_nested,
    }
}

fn collect_parents(store: &KifStore, sym_id: SymbolId) -> Vec<ParentEdge> {
    const TAX_RELATIONS: &[&str] = &["subclass", "instance", "subrelation", "subAttribute"];
    let mut out = Vec::new();
    for &rel_head in TAX_RELATIONS {
        for &sid in store.by_head(rel_head) {
            let sent = &store.sentences[store.sent_idx(sid)];
            // Shape: (rel CHILD PARENT) -- child at elements[1], parent at [2].
            let child_ok = matches!(
                sent.elements.get(1),
                Some(Element::Symbol { id, .. }) if *id == sym_id
            );
            if !child_ok { continue; }
            if let Some(Element::Symbol { id: parent, .. }) = sent.elements.get(2) {
                out.push(ParentEdge {
                    relation: rel_head.to_string(),
                    parent:   store.sym_name(*parent).to_string(),
                });
            }
        }
    }
    out
}

fn signature(
    kb:     &KnowledgeBase,
    sym_id: SymbolId,
) -> (Option<i32>, Vec<(usize, SortSig)>, Option<SortSig>) {
    let arity = kb.layer.arity(sym_id);
    let range = kb.layer.range(sym_id).ok().flatten().map(|rd| sort_sig(kb, &rd));
    let domains_raw = kb.layer.domain(sym_id);
    let domains: Vec<(usize, SortSig)> = domains_raw.into_iter().enumerate()
        .filter_map(|(i, rd)| {
            // The semantic layer uses `u64::MAX` to represent
            // "no explicit domain declared for this argument".
            if rd.id() == u64::MAX { return None; }
            Some((i + 1, sort_sig(kb, &rd)))
        })
        .collect();
    (arity, domains, range)
}

fn sort_sig(kb: &KnowledgeBase, rd: &RelationDomain) -> SortSig {
    let id = rd.id();
    SortSig {
        class:    kb.layer.store.sym_name(id).to_string(),
        subclass: matches!(rd, RelationDomain::DomainSubclass(_)),
    }
}

// -- Reference collection ----------------------------------------------------

/// Classify each sentence in which `sym_id` occurs into one of two
/// buckets:
///
/// - **`ref_args`** — the symbol appears at the root level of the
///   sentence's element list.  Record the 0-based position of its
///   first such occurrence (position 0 = head; position ≥ 1 =
///   argument slot).
/// - **`ref_nested`** — the symbol appears only inside a nested
///   sub-sentence, never at the root level.
///
/// Both lists are sorted by sid for deterministic output across
/// runs, and deduplicated (one entry per root sid even if the
/// symbol occurs multiple times in that sentence).
///
/// `KifStore::axiom_sentences_of` already recurses through
/// sub-sentences during registration, so every root sid in which
/// `sym_id` appears (at any depth) is in the scan.  The classifier
/// below decides *where* the occurrence lives per root.
fn collect_refs(
    store:  &KifStore,
    sym_id: SymbolId,
) -> (Vec<SentenceRef>, Vec<SentenceId>) {
    let mut args:   Vec<SentenceRef> = Vec::new();
    let mut nested: Vec<SentenceId>  = Vec::new();
    let mut sids: Vec<SentenceId> = store.axiom_sentences_of(sym_id).to_vec();
    sids.sort_unstable();
    sids.dedup();

    for sid in sids {
        let sent = &store.sentences[store.sent_idx(sid)];
        // Scan root-level elements first.  A direct Symbol match at
        // depth 0 is the common case — record its position.
        let root_hit = sent.elements.iter().enumerate().find_map(|(i, el)| {
            match el {
                Element::Symbol { id, .. } if *id == sym_id => Some(i),
                _ => None,
            }
        });
        if let Some(pos) = root_hit {
            args.push(SentenceRef(pos, sid));
            continue;
        }
        // Not at root — recurse into any Subs.  Since
        // `axiom_sentences_of` told us the symbol IS somewhere in
        // this sentence, at least one sub must contain it.  If not
        // (shouldn't happen, but defensive), we drop the sid rather
        // than emit a bogus nested reference.
        let appears_nested = sent.elements.iter().any(|el| match el {
            Element::Sub { sid: sub_sid, .. } => subtree_contains_symbol(store, *sub_sid, sym_id),
            _ => false,
        });
        if appears_nested {
            nested.push(sid);
        }
    }

    (args, nested)
}

/// Recursive helper: does the sentence tree rooted at `sid` contain
/// any direct `Element::Symbol { id: sym_id }` at any depth?
///
/// Complements [`collect_refs`]'s root-level scan — used when the
/// target sid's root doesn't contain the symbol but the
/// axiom-occurrence index says it's somewhere in the tree.
fn subtree_contains_symbol(store: &KifStore, sid: SentenceId, sym_id: SymbolId) -> bool {
    if !store.has_sentence(sid) { return false; }
    let sent = &store.sentences[store.sent_idx(sid)];
    for el in &sent.elements {
        match el {
            Element::Symbol { id, .. } if *id == sym_id => return true,
            Element::Sub { sid: sub_sid, .. } => {
                if subtree_contains_symbol(store, *sub_sid, sym_id) { return true; }
            }
            _ => {}
        }
    }
    false
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn kb_from(kif: &str) -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let r = kb.load_kif(kif, "test.kif", None);
        assert!(r.ok, "load failed: {:?}", r.errors);
        kb
    }

    #[test]
    fn documentation_round_trip() {
        let kb = kb_from(r#"
            (documentation Human EnglishLanguage "A &%Human being.")
            (documentation Human FrenchLanguage  "Un être &%humain.")
        "#);
        let all = kb.documentation("Human", None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].language, "EnglishLanguage");
        assert_eq!(all[0].text,     "A &%Human being.");
        let en = kb.documentation("Human", Some("EnglishLanguage"));
        assert_eq!(en.len(), 1);
        let fr = kb.documentation("Human", Some("FrenchLanguage"));
        assert_eq!(fr.len(), 1);
        assert_eq!(fr[0].text, "Un être &%humain.");
    }

    #[test]
    fn term_format_argument_order() {
        // termFormat reverses the order: (termFormat LANG SYM TEXT)
        let kb = kb_from(r#"
            (termFormat EnglishLanguage Human "human")
        "#);
        let tf = kb.term_format("Human", None);
        assert_eq!(tf.len(), 1);
        assert_eq!(tf[0].language, "EnglishLanguage");
        assert_eq!(tf[0].text,     "human");
    }

    #[test]
    fn format_string_for_relation() {
        let kb = kb_from(r#"
            (format EnglishLanguage subclass "%1 is a subclass of %2")
        "#);
        let fmts = kb.format_string("subclass", None);
        assert_eq!(fmts.len(), 1);
        assert_eq!(fmts[0].text, "%1 is a subclass of %2");
    }

    #[test]
    fn missing_symbol_returns_none() {
        let kb = kb_from("(subclass Human Animal)");
        assert!(kb.manpage("DoesNotExist").is_none());
        assert!(kb.documentation("DoesNotExist", None).is_empty());
    }

    /// `kb_from` loads into a session but doesn't promote to axiom
    /// status — and `sym_refs` (backing the man-page reference
    /// scanner) only tracks promoted sentences.  `kb_promoted_from`
    /// promotes the loaded session so reference-collection tests
    /// see the data.  In the real CLI, `open_or_build_kb` calls
    /// `make_session_axiomatic(BASE)` on every load, so this
    /// mirrors production.
    fn kb_promoted_from(kif: &str) -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let r = kb.load_kif(kif, "test.kif", None);
        assert!(r.ok, "load failed: {:?}", r.errors);
        kb.make_session_axiomatic("test.kif");
        kb
    }

    #[test]
    fn refs_split_root_level_and_nested_occurrences() {
        // Three sentences that mention `Human`:
        //   1. `(subclass Human Hominid)`     — root pos 1 (arg 1)
        //   2. `(instance Socrates Human)`     — root pos 2 (arg 2)
        //   3. `(forall (?X) (instance ?X Human))` — only buried in the Sub
        let kb = kb_promoted_from(r#"
            (subclass Human Hominid)
            (instance Socrates Human)
            (forall (?X) (instance ?X Human))
        "#);
        let man = kb.manpage("Human").expect("Human must resolve");
        // Root-level refs: positions 1 and 2 (both are argument
        // slots; no head occurrence in this fixture).
        let positions: std::collections::BTreeSet<usize> =
            man.ref_args.iter().map(|r| r.0).collect();
        assert!(
            positions.contains(&1) && positions.contains(&2),
            "expected arg-1 and arg-2 occurrences, got {:?}", man.ref_args,
        );
        // The quantified sentence must show up as nested.
        assert_eq!(
            man.ref_nested.len(), 1,
            "expected exactly one nested ref for the forall axiom, got {:?}",
            man.ref_nested,
        );
    }

    #[test]
    fn refs_one_per_sentence_when_symbol_appears_twice() {
        // `Human` appears at root position 1 AND inside the nested
        // sub-sentence — should be counted as a single root-level
        // ref (the leftmost position wins), NOT as both a root ref
        // AND a nested ref.
        let kb = kb_promoted_from("(=> (instance Human Agent) (instance Human Entity))");
        let man = kb.manpage("Human").expect("Human must resolve");
        assert_eq!(
            man.ref_args.len() + man.ref_nested.len(), 1,
            "Human referenced once per sentence: ref_args={:?}, ref_nested={:?}",
            man.ref_args, man.ref_nested,
        );
    }

    #[test]
    fn refs_position_records_arg_slot() {
        // `Hominid` appears at root position 2 in the subclass axiom.
        let kb = kb_promoted_from("(subclass Human Hominid)");
        let man = kb.manpage("Hominid").expect("Hominid must resolve");
        assert!(
            man.ref_args.iter().any(|r| r.0 == 2),
            "expected arg-2 position, got {:?}", man.ref_args,
        );
        assert!(man.ref_nested.is_empty());
    }

    #[test]
    fn manpage_collects_parents() {
        let kb = kb_from(r#"
            (subclass Human  Hominid)
            (subclass Hominid Primate)
            (documentation Human EnglishLanguage "A member of species homo sapiens.")
        "#);
        let man = kb.manpage("Human").expect("Human must resolve");
        // Parents include only the direct edge(s).
        assert!(
            man.parents.iter().any(|p| p.relation == "subclass" && p.parent == "Hominid"),
            "expected direct parent Hominid, got {:?}", man.parents
        );
        assert_eq!(man.documentation.len(), 1);
        assert_eq!(man.documentation[0].text, "A member of species homo sapiens.");
    }

    #[test]
    fn non_string_slot_does_not_panic() {
        // `documentation` with a non-string third arg should be ignored,
        // not blow up the scan.
        let kb = kb_from(r#"
            (documentation Human EnglishLanguage 42)
            (documentation Human EnglishLanguage "real doc")
        "#);
        let docs = kb.documentation("Human", None);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].text, "real doc");
    }

    // -- Caching behaviour ---------------------------------------------------

    #[test]
    fn documentation_result_is_cached_on_second_call() {
        // The backing scan is O(N_doc_entries) on the first call.  The
        // cache means subsequent calls are HashMap hits.  We can't directly
        // observe the cache HashMap from the test (it's private), but we
        // can check that two successive calls return identical vectors and
        // work on large KBs cheaply.  More importantly: the invalidation
        // test below confirms the cache IS populated (otherwise invalidation
        // would be a no-op).
        let kb = kb_from(r#"
            (documentation Human EnglishLanguage "first")
            (documentation Human FrenchLanguage  "le premier")
        "#);
        let a = kb.documentation("Human", None);
        let b = kb.documentation("Human", None);
        assert_eq!(a.len(), 2);
        assert_eq!(a, b);
    }

    #[test]
    fn language_filter_hits_cache() {
        let kb = kb_from(r#"
            (documentation Human EnglishLanguage "en")
            (documentation Human FrenchLanguage  "fr")
        "#);
        // Fill the cache via an unfiltered call.
        let _all = kb.documentation("Human", None);
        // A subsequent filtered call must still filter correctly.
        let en = kb.documentation("Human", Some("EnglishLanguage"));
        assert_eq!(en.len(), 1);
        assert_eq!(en[0].text, "en");
    }

    #[test]
    fn invalidate_symbols_evicts_cached_doc_entries() {
        use std::collections::HashSet;

        let kb = kb_from(r#"
            (documentation Human EnglishLanguage "stale")
        "#);
        // Warm the cache.
        let a = kb.documentation("Human", None);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].text, "stale");

        // Invalidate the Human symbol specifically.
        let human_id = kb.symbol_id("Human").expect("Human interned");
        let mut set  = HashSet::new();
        set.insert(human_id);
        // SemanticLayer is behind `kb.layer` (pub(crate) field inside this
        // module tree).
        kb.layer.invalidate_symbols(&set);

        // A subsequent lookup must re-populate from the store.  We haven't
        // changed the underlying sentences, so the result is the same --
        // the test confirms the invalidation doesn't corrupt the lookup.
        let b = kb.documentation("Human", None);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].text, "stale");
    }

    #[test]
    fn invalidate_preserves_unrelated_symbols() {
        use std::collections::HashSet;

        let kb = kb_from(r#"
            (documentation Human  EnglishLanguage "h")
            (documentation Animal EnglishLanguage "a")
        "#);
        let _ = kb.documentation("Human", None);
        let _ = kb.documentation("Animal", None);

        let human_id = kb.symbol_id("Human").unwrap();
        let mut set  = HashSet::new();
        set.insert(human_id);
        kb.layer.invalidate_symbols(&set);

        // Animal's entry must still return correctly -- the invalidation
        // only evicted Human's key.
        let animal_docs = kb.documentation("Animal", None);
        assert_eq!(animal_docs.len(), 1);
        assert_eq!(animal_docs[0].text, "a");
    }

    #[test]
    fn parse_error_does_not_poison_subsequent_lookups() {
        // The critical invariant: one bad sentence in one file must
        // not take the rest of the KB out of service.  Before the
        // fix, `ingest()` early-returned on any parse error, which
        // skipped the semantic-cache update; queries on symbols in
        // the broken file returned None even though the store had
        // their sentences.  That cascaded into `manpage()` failing
        // for unrelated files because the taxonomy layer was in an
        // inconsistent state.
        let mut kb = KnowledgeBase::new();

        // File A: clean, loads cleanly.
        let r_a = kb.load_kif(r#"
            (subclass Hominid Primate)
            (documentation Hominid EnglishLanguage "Great apes.")
        "#, "a.kif", None);
        assert!(r_a.ok);

        // File B: has a trailing incomplete sentence (common when
        // a user is mid-edit) plus a valid sentence.  The parse
        // surfaces an error but the recovered valid sentence must
        // still be queryable, and A must remain untouched.
        let r_b = kb.load_kif(r#"
            (subclass Human Hominid)
            (documentation Human EnglishLanguage
        "#, "b.kif", None);
        assert!(!r_b.ok, "expected parse error");
        assert!(!r_b.errors.is_empty());

        // Both files' manpages resolve.
        let hominid = kb.manpage("Hominid").expect("Hominid still present");
        assert!(hominid.documentation.iter().any(|d| d.text.contains("Great apes")));

        let human = kb.manpage("Human").expect("Human recovered despite parse error");
        // Human inherits from Hominid via the recovered subclass edge.
        assert!(human.parents.iter().any(|p| p.parent == "Hominid" && p.relation == "subclass"));
    }
}
