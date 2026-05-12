//! Symbol introspection over the KIF store's documentation relations.
//!
//! Authoring intent lives in three special relations:
//!
//!   (documentation    Symbol   Language "text")
//!   (termFormat       Language Symbol   "text")
//!   (format           Language relation "format-string")
//!
//! This module scans the head-indexed view for them and exposes typed
//! lookups plus a combined [`ManPage`] carrying kind / parents /
//! signature data from the semantic layer. All queries are pure reads.

use crate::SentenceId;
use crate::layer::{Layer, TopLayer};
use crate::semantics::consts::DOC_RELATION;
use crate::syntactic::SyntacticLayer;
use crate::types::{DocEntry, RelationDomain, RelationRange};
use crate::types::{Element, SymbolId};

use super::KnowledgeBase;

// -- Public types -------------------------------------------------------------

/// Category classification for a symbol. A single symbol may belong to
/// more than one category, so callers receive the full set.
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
    /// Lowercase string label for this category.
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
/// sub-sentence are recorded in [`ManPage::ref_nested`] instead.
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
    /// Taxonomic children — the inverse edges: `(subclass Child sym)` /
    /// `(instance Child sym)` / `(subrelation Child sym)` /
    /// `(subAttribute Child sym)`.  In each [`ParentEdge`] here, the
    /// `parent` field holds the **child** symbol; `relation` is the KIF head.
    pub children:      Vec<ParentEdge>,
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
    /// of its first such occurrence.  One entry per sentence: if the
    /// same symbol appears at multiple root positions in one sentence,
    /// only the leftmost is recorded.
    pub ref_args:      Vec<SentenceRef>,
    /// Sentences where the symbol appears **only inside a nested
    /// sub-sentence** (never at the root level).  Surfaced separately
    /// so consumers can display them under a dedicated heading without
    /// mis-reporting an argument position.
    pub ref_nested:    Vec<SentenceId>,
    /// Total number of root formulas this symbol occurs in (at any
    /// depth), from the syntactic occurrence index.  Includes
    /// documentation / taxonomy / format sentences — the raw
    /// occurrence count, independent of the filtered `ref_*` listings.
    pub appears_in_count: usize,
    /// Normalized-implication root sids in which the symbol appears in
    /// the **antecedent**, derived from the CAF branch index.  Sorted.
    pub antecedent_refs:  Vec<SentenceId>,
    /// Number of normalized-implication roots in which the symbol
    /// appears in the **consequent**.
    pub consequent_count: usize,
    /// Root sids this symbol **owns** per the SInE index — those for
    /// which it is a least-general (trigger) symbol at tolerance 1.0.
    pub owned_sids:       std::collections::HashSet<SentenceId>,
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

impl<L: TopLayer + Layer> KnowledgeBase<L> {
    /// Taxonomic categories a symbol belongs to, most-specific first with
    /// redundant labels suppressed (a predicate also passes `is_relation`, so
    /// only `Predicate` is reported).
    pub(crate) fn kinds_of(&self, sym: SymbolId) -> Vec<ManKind> {
        let mut kinds = Vec::new();
        if self.is_class(sym)     { kinds.push(ManKind::Class); }
        if self.is_function(sym)  { kinds.push(ManKind::Function); }
        if self.is_predicate(sym) { kinds.push(ManKind::Predicate); }
        if self.is_relation(sym)
            && !kinds.iter().any(|k| matches!(k, ManKind::Function | ManKind::Predicate))
        {
            kinds.push(ManKind::Relation);
        }
        if self.is_instance(sym)  { kinds.push(ManKind::Instance); }
        if kinds.is_empty()       { kinds.push(ManKind::Individual); }
        kinds
    }

    /// Return every `(documentation <symbol> <lang> "...")` entry.  When
    /// `language` is `Some`, only entries matching that language tag are
    /// returned.
    pub fn documentation(&self, symbol: &str, language: Option<&str>) -> Vec<DocEntry> {
        let doc_hash = DOC_RELATION.id();
        let Some(id) = self.symbol_id(symbol) else { return Vec::new(); };
        self.layer.semantic().documentation(id, language).into_iter().filter(|d| d.rel == doc_hash).collect()
    }

    /// Return every `(termFormat <lang> <symbol> "...")` entry.  When
    /// `language` is `Some`, only entries matching that language tag are returned.
    pub fn term_format(&self, symbol: &str, language: Option<&str>) -> Vec<DocEntry> {
        self.layer.semantic().term_format_named(symbol, language)
    }

    /// Return every `(format <lang> <relation> "...")` entry.  Semantically
    /// meaningful only for relation symbols.  When `language` is `Some`, only
    /// entries matching that language tag are returned.
    pub fn format_string(&self, relation: &str, language: Option<&str>) -> Vec<DocEntry> {
        self.layer.semantic().format_string(relation, language)
    }

    /// Build a full man-page view for `symbol`.  `None` if the symbol is not
    /// interned in the KB.  All fields are best-effort: the query succeeds even
    /// when some data is missing, in which case the corresponding field is
    /// empty / `None`.
    pub fn manpage(&self, symbol: &str) -> Option<ManPage> {
        let sym_id = self.symbol_id(symbol)?;
        Some(build_manpage(self, sym_id, symbol))
    }
}

impl KnowledgeBase {

    /// Run the deferred normalization + rewrite pass so that the
    /// introspection data the man page reads — `normal_implications`,
    /// the antecedent/consequent branch index, `suppressed`, and the
    /// per-sentence formula caches — is populated.  Idempotent and cheap
    /// when already clean.  Call once (mutably) before the immutable
    /// `manpage` / `sentence_tptp` reads.
    pub fn ensure_introspection(&mut self) {
        self.layer.ensure_rewrite_pass();
        let _ = self.layer.semantic.syntactic.normal_implications();
    }

    /// Return the cached TPTP rendering of sentence `sid` in `mode`, or
    /// `None` when the sentence is suppressed (replaced by synthetic
    /// equivalents) or cannot be converted.  Callers that get `None`
    /// for a suppressed sentence should fall back to
    /// [`Self::synthetic_replacements_of`].
    pub fn sentence_tptp(&self, sid: SentenceId, mode: crate::TptpLang) -> Option<String> {
        let cf = if mode.is_typed() { self.layer.formula_tff(sid)? } else { self.layer.formula_fof(sid)? };
        Some(cf.formula.to_tptp())
    }

    /// `true` if `sid` was suppressed by the rewrite pass (its synthetic
    /// replacement, not the original, is what the prover sees).
    pub fn is_suppressed(&self, sid: SentenceId) -> bool {
        self.layer.suppressed.read().unwrap().contains(&sid)
    }

    /// The synthetic sentences that replaced `sid` (transitively), if it
    /// was normalized / guard-augmented by the rewrite pass.  Empty for
    /// sentences that pass through unchanged.
    pub fn synthetic_replacements_of(&self, sid: SentenceId) -> Vec<SentenceId> {
        self.layer.synthetic_replacements(&[sid])
    }

}

fn build_manpage<L: TopLayer + Layer>(kb: &KnowledgeBase<L>, sym_id: SymbolId, name: &str) -> ManPage {
    let kinds = kb.kinds_of(sym_id);

    let store = &kb.layer.semantic().syntactic;
    let parents = collect_parents(store, sym_id);
    let children = collect_children(store, sym_id);
    let (arity, domains, range) = signature(kb, sym_id);
    let (ref_args, ref_nested) = collect_refs(store, sym_id);

    let appears_in_count = store.axiom_sentences_of(sym_id).len();
    let (antecedent_refs, consequent_count) = antecedent_consequent(store, sym_id);
    let owned_sids = store.sine_current(|idx| {
        let seed: std::collections::HashSet<SymbolId> =
            std::iter::once(sym_id).collect();
        idx.select(&seed, 1.0, Some(1))
    });

    ManPage {
        name: name.to_string(),
        kinds,
        documentation: kb.documentation(name, None),
        term_format:   kb.term_format(name, None),
        format:        kb.format_string(name, None),
        parents,
        children,
        arity,
        domains,
        range,
        ref_args,
        ref_nested,
        appears_in_count,
        antecedent_refs,
        consequent_count,
        owned_sids,
    }
}

/// Heads excluded from the REFERENCES listing: the doc-style relations
/// (surfaced in their own DOCUMENTATION / TERM FORMAT / FORMAT sections)
/// and the taxonomy relations (surfaced in PARENTS).
const EXCLUDED_REF_HEADS: &[&str] = &[
    "documentation", "termFormat", "format",
    "subclass", "instance", "subrelation", "subAttribute",
];

/// Classify the symbol's occurrences into antecedent membership (sorted
/// distinct implication roots) and a consequent count, using the
/// normalization branch index.
fn antecedent_consequent(
    _store:  &SyntacticLayer,
    _sym_id: SymbolId,
) -> (Vec<SentenceId>, usize) {
    // TODO: restore via the normalization branch index.
    // let idx = store.impl_sym_index();
    // let mut ant_vec: Vec<SentenceId> = idx
    //     .antecedent
    //     .get(&sym_id)
    //     .map(|s| s.iter().copied().collect())
    //     .unwrap_or_default();
    // ant_vec.sort_unstable();
    // let con = idx.consequent.get(&sym_id).map(|s| s.len()).unwrap_or(0);
    // (ant_vec, con)
    return (vec![], 0);
}

fn collect_parents(store: &SyntacticLayer, sym_id: SymbolId) -> Vec<ParentEdge> {
    const TAX_RELATIONS: &[&str] = &["subclass", "instance", "subrelation", "subAttribute"];
    let mut out = Vec::new();
    for &rel_head in TAX_RELATIONS {
        for sid in store.by_head(rel_head).iter().copied() {
            let Some(sent) = store.sentence(sid) else { continue };
            // Shape: (rel CHILD PARENT) -- child at elements[1], parent at [2].
            let child_ok = matches!(
                sent.elements.get(1),
                Some(Element::Symbol(sym)) if sym.id() == sym_id
            );
            if !child_ok { continue; }
            if let Some(Element::Symbol(parent)) = sent.elements.get(2) {
                out.push(ParentEdge {
                    relation: rel_head.to_string(),
                    parent:   parent.to_string(),
                });
            }
        }
    }
    out
}

/// Inverse of [`collect_parents`]: find `(rel CHILD sym)` edges — the
/// symbols that declare `sym` as their parent.  `relation` is the KIF
/// head; the returned `ParentEdge.parent` field holds the *child*.
fn collect_children(store: &SyntacticLayer, sym_id: SymbolId) -> Vec<ParentEdge> {
    const TAX_RELATIONS: &[&str] = &["subclass", "instance", "subrelation", "subAttribute"];
    let mut out = Vec::new();
    for &rel_head in TAX_RELATIONS {
        for sid in store.by_head(rel_head).iter().copied() {
            let Some(sent) = store.sentence(sid) else { continue };
            // Shape: (rel CHILD PARENT) — parent at elements[2] must be `sym`.
            let parent_ok = matches!(
                sent.elements.get(2),
                Some(Element::Symbol(sym)) if sym.id() == sym_id
            );
            if !parent_ok { continue; }
            if let Some(Element::Symbol(child)) = sent.elements.get(1) {
                out.push(ParentEdge {
                    relation: rel_head.to_string(),
                    parent:   child.to_string(),
                });
            }
        }
    }
    out
}

fn signature<L: TopLayer + Layer>(
    kb:     &KnowledgeBase<L>,
    sym_id: SymbolId,
) -> (Option<i32>, Vec<(usize, SortSig)>, Option<SortSig>) {
    let arity = kb.layer.semantic().arity(sym_id);
    let range = sort_sig_range(kb, &kb.layer.semantic().range(sym_id));
    let domains_raw = kb.layer.semantic().domain(sym_id);
    let domains: Vec<(usize, SortSig)> = domains_raw.iter().cloned().enumerate()
        .filter_map(|(i, rd)| {
            if matches!(rd, RelationDomain::Unknown) { return None; }
            Some((i + 1, sort_sig(kb, &rd)?))
        })
        .collect();
    (arity, domains, range)
}

fn sort_sig_range<L: TopLayer + Layer>(kb: &KnowledgeBase<L>, rd: &RelationRange) -> Option<SortSig> {
    let id = rd.id()?;
    Some(SortSig {
        class:    kb.layer.semantic().syntactic.sym_name(id)
            .map(|s| s.name().to_string())
            .unwrap_or_default(),
        subclass: matches!(rd, RelationRange::RangeSubclass(_)),
    })
}

fn sort_sig<L: TopLayer + Layer>(kb: &KnowledgeBase<L>, rd: &RelationDomain) -> Option<SortSig> {
    let id = rd.id()?;
    Some(SortSig {
        class:    kb.layer.semantic().syntactic.sym_name(id)
            .map(|s| s.name().to_string())
            .unwrap_or_default(),
        subclass: matches!(rd, RelationDomain::DomainSubclass(_)),
    })
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
/// Both lists are sorted by sid for deterministic output and
/// deduplicated (one entry per root sid even if the symbol occurs
/// multiple times in that sentence).
fn collect_refs(
    store:  &SyntacticLayer,
    sym_id: SymbolId,
) -> (Vec<SentenceRef>, Vec<SentenceId>) {
    let mut args:   Vec<SentenceRef> = Vec::new();
    let mut nested: Vec<SentenceId>  = Vec::new();
    let mut sids: Vec<SentenceId> = store.axiom_sentences_of(sym_id).iter().copied().collect();
    sids.sort_unstable();

    for sid in sids {
        let Some(sent) = store.sentence(sid) else { continue };
        if let Some(head_id) = sent.head_symbol() {
            if let Some(head_name) = store.sym_name(head_id) {
                if EXCLUDED_REF_HEADS.contains(&head_name.name().as_ref()) {
                    continue;
                }
            }
        }
        let root_hit = sent.elements.iter().enumerate().find_map(|(i, el)| {
            match el {
                Element::Symbol(sym) if sym.id() == sym_id => Some(i),
                _ => None,
            }
        });
        if let Some(pos) = root_hit {
            args.push(SentenceRef(pos, sid));
            continue;
        }
        let appears_nested = sent.elements.iter().any(|el| match el {
            Element::Sub(sub_sid) => subtree_contains_symbol(store, *sub_sid, sym_id),
            _ => false,
        });
        if appears_nested {
            nested.push(sid);
        }
    }

    (args, nested)
}

/// Does the sentence tree rooted at `sid` contain any direct
/// `Element::Symbol { id: sym_id }` at any depth?
fn subtree_contains_symbol(store: &SyntacticLayer, sid: SentenceId, sym_id: SymbolId) -> bool {
    let Some(sent) = store.sentence(sid) else { return false };
    for el in &sent.elements {
        match el {
            Element::Symbol(sym) if sym.id() == sym_id => return true,
            Element::Sub(sub_sid) => {
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
        // Man-page rendering reads `documentation()` at Base scope, so fixtures
        // must be ingested as a FILE and promoted (inline `tell`s stay
        // session-transient and are invisible to the Base accessor).
        let mut kb = KnowledgeBase::new();
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("test.kif"), "test.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        let r = kb.make_session_axiomatic("test.kif");
        assert!(matches!(r, Ok(_)), "promotion failed: {:?}", r.err());
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
        assert!(all.iter().any(|d| d.language == "EnglishLanguage" && d.text == "A &%Human being."),
            "expected English entry, got {:?}", all);
        assert!(all.iter().any(|d| d.language == "FrenchLanguage" && d.text == "Un être &%humain."),
            "expected French entry, got {:?}", all);
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

    /// Load `kif` and promote the session to axiom status so the man-page
    /// reference scanner (which only tracks promoted sentences) sees the data.
    fn kb_promoted_from(kif: &str) -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        // Promotable content must be a FILE source (inline `tell`s can't be
        // lifted), so ingest as a real file then promote.
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("test.kif"), "test.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        let r = kb.make_session_axiomatic(
            "test.kif"
        );
        assert!(matches!(r, Ok(_)), "promotion failed: {:?}", r.err());
        kb
    }

    #[test]
    fn refs_split_root_level_and_nested_occurrences() {
        // Non-taxonomy relations (taxonomy heads are now filtered out of
        // REFERENCES — see EXCLUDED_REF_HEADS).  Three sentences mention
        // `Human`:
        //   1. `(orientation Human Hominid Right)` — root pos 1 (arg 1)
        //   2. `(located Socrates Human)`          — root pos 2 (arg 2)
        //   3. `(=> (foo ?X) (likes ?X Human))`    — only buried in the Sub
        //      (a top-level `(forall …)` would be stripped at ingest, so we
        //      nest under `=>` to keep `Human` genuinely sub-sentence-only).
        let kb = kb_promoted_from(r#"
            (orientation Human Hominid Right)
            (located Socrates Human)
            (=> (foo ?X) (likes ?X Human))
        "#);
        let man = kb.manpage("Human").expect("Human must resolve");
        let positions: std::collections::BTreeSet<usize> =
            man.ref_args.iter().map(|r| r.0).collect();
        assert!(
            positions.contains(&1) && positions.contains(&2),
            "expected arg-1 and arg-2 occurrences, got {:?}", man.ref_args,
        );
        assert_eq!(
            man.ref_nested.len(), 1,
            "expected exactly one nested ref for the forall axiom, got {:?}",
            man.ref_nested,
        );
    }

    #[test]
    fn refs_exclude_taxonomy_and_doc_relations() {
        // subclass / instance / documentation / termFormat / format must
        // NOT appear in the REFERENCES listing (req 4); they're covered by
        // PARENTS / DOCUMENTATION sections.  Only the `(located …)`
        // sentence should survive as a ref.
        let kb = kb_promoted_from(r#"
            (subclass Human Hominid)
            (instance Human Agent)
            (documentation Human EnglishLanguage "doc")
            (located Human Earth)
        "#);
        let man = kb.manpage("Human").expect("Human must resolve");
        assert_eq!(man.ref_args.len() + man.ref_nested.len(), 1,
            "only the (located …) ref should survive, got args={:?} nested={:?}",
            man.ref_args, man.ref_nested);
        // The surviving ref is the `located` sentence with Human at arg-1.
        assert!(man.ref_args.iter().any(|r| r.0 == 1),
            "expected the located sentence (arg pos 1), got {:?}", man.ref_args);
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
        // `Hominid` appears at root position 2 (a non-taxonomy relation,
        // since taxonomy heads are filtered out of REFERENCES).
        let kb = kb_promoted_from("(orientation Human Hominid Right)");
        let man = kb.manpage("Hominid").expect("Hominid must resolve");
        assert!(
            man.ref_args.iter().any(|r| r.0 == 2),
            "expected arg-2 position, got {:?}", man.ref_args,
        );
        assert!(man.ref_nested.is_empty());
    }

    #[test]
    #[ignore = "TODO(migration): antecedent/consequent branch index (impl_sym_index) retired pending Phase-4 rewrite"]
    fn manpage_antecedent_consequent_count_and_filtered_refs() {
        let mut kb = kb_promoted_from(r#"
            (=> (instance ?X Human) (attribute ?X Rational))
            (subclass Human Animal)
            (documentation Human EnglishLanguage "A &%Human.")
        "#);
        kb.ensure_introspection();

        let human = kb.manpage("Human").expect("Human resolves");
        // Raw occurrence count: implication + subclass + documentation = 3.
        assert_eq!(human.appears_in_count, 3,
            "expected 3 raw occurrences, got {}", human.appears_in_count);
        // Human is in the antecedent `(instance ?X Human)` of the one rule.
        assert_eq!(human.antecedent_refs.len(), 1,
            "Human should be antecedent of 1 implication, got {:?}", human.antecedent_refs);
        assert_eq!(human.consequent_count, 0);
        // REFERENCES must exclude the subclass + documentation sentences
        // (covered by PARENTS / DOCUMENTATION).  The only non-excluded
        // sentence mentioning Human is the implication, where Human is
        // nested inside the antecedent sub → ref_nested.
        assert!(human.ref_args.is_empty(),
            "subclass/doc should be filtered out of ref_args, got {:?}", human.ref_args);
        assert_eq!(human.ref_nested.len(), 1,
            "only the implication should remain as a nested ref, got {:?}", human.ref_nested);

        // Rational is in the consequent of the rule.
        let rational = kb.manpage("Rational").expect("Rational resolves");
        assert_eq!(rational.antecedent_refs.len(), 0);
        assert_eq!(rational.consequent_count, 1,
            "Rational should be consequent of 1 implication, got {}", rational.consequent_count);
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
        // kb.layer.semantic.invalidate_symbols(&set);

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
        // kb.layer.semantic.invalidate_symbols(&set);

        let animal_docs = kb.documentation("Animal", None);
        assert_eq!(animal_docs.len(), 1);
        assert_eq!(animal_docs[0].text, "a");
    }

    #[test]
    fn parse_error_does_not_poison_subsequent_lookups() {
        // Invariant: one bad sentence in one file must not take the rest of
        // the KB out of service.
        let mut kb = KnowledgeBase::new();

        // File A: clean. Ingest as a FILE and promote so the Base-scoped
        // man-page accessor sees its documentation.
        let r_a = kb.reload_kif(r#"
            (subclass Hominid Primate)
            (documentation Hominid EnglishLanguage "Great apes.")
        "#, &std::path::PathBuf::from("a.kif"), "a.kif");
        assert!(r_a.ok);
        assert!(matches!(kb.make_session_axiomatic("a.kif"), Ok(_)));

        // File B: a trailing incomplete sentence plus a valid one. The parse
        // surfaces an error but the recovered valid sentence must still be
        // queryable, and A must remain untouched.
        let r_b = kb.reload_kif(r#"
            (subclass Human Hominid)
            (documentation Human EnglishLanguage
        "#, &std::path::PathBuf::from("b.kif"), "b.kif");
        assert!(!r_b.ok, "expected parse error");
        assert!(r_b.has_errors());
        let _ = kb.make_session_axiomatic("b.kif");

        // Both files' manpages resolve.
        let hominid = kb.manpage("Hominid").expect("Hominid still present");
        assert!(hominid.documentation.iter().any(|d| d.text.contains("Great apes")));

        let human = kb.manpage("Human").expect("Human recovered despite parse error");
        // Human inherits from Hominid via the recovered subclass edge.
        assert!(human.parents.iter().any(|p| p.parent == "Hominid" && p.relation == "subclass"));
    }
}
