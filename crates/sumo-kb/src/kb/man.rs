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

use crate::kif_store::KifStore;
use crate::semantic::RelationDomain;
use crate::types::{Element, Literal, SymbolId};

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
    pub fn documentation(&self, symbol: &str, language: Option<&str>) -> Vec<DocEntry> {
        // Argument layout: (documentation SYM LANG TEXT)
        //   elements[0] = head,  elements[1] = SYM,
        //   elements[2] = LANG,  elements[3] = TEXT
        doc_entries_for(&self.layer.store, "documentation", symbol, 1, 2, 3, language)
    }

    /// Return every `(termFormat <lang> <symbol> "...")` entry.
    pub fn term_format(&self, symbol: &str, language: Option<&str>) -> Vec<DocEntry> {
        // (termFormat LANG SYM TEXT)
        doc_entries_for(&self.layer.store, "termFormat", symbol, 2, 1, 3, language)
    }

    /// Return every `(format <lang> <relation> "...")` entry.  Semantically
    /// meaningful only for relation symbols, but the scan is symmetric.
    pub fn format_string(&self, relation: &str, language: Option<&str>) -> Vec<DocEntry> {
        // (format LANG REL TEXT)
        doc_entries_for(&self.layer.store, "format", relation, 2, 1, 3, language)
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

// -- Internal helpers ---------------------------------------------------------

/// Scan root sentences with head `head` and extract (language, text) pairs.
///
/// `target_idx` / `lang_idx` / `text_idx` are the 0-based element
/// positions of the target symbol, the language tag, and the string
/// literal within the sentence's `elements` vector (index 0 is the
/// head itself).  When `language` is `Some(l)`, only entries with that
/// language tag are returned.
fn doc_entries_for(
    store:      &KifStore,
    head:       &str,
    symbol:     &str,
    target_idx: usize,
    lang_idx:   usize,
    text_idx:   usize,
    language:   Option<&str>,
) -> Vec<DocEntry> {
    let mut out = Vec::new();
    let target_id = match store.sym_id(symbol) {
        Some(id) => id,
        None     => return out,
    };
    for &sid in store.by_head(head) {
        let sent = &store.sentences[store.sent_idx(sid)];
        // Target slot must be a bare symbol equal to the one requested.
        let tgt = match sent.elements.get(target_idx) {
            Some(Element::Symbol(id)) => *id,
            _ => continue,
        };
        if tgt != target_id { continue; }
        // Language slot.
        let lang = match sent.elements.get(lang_idx) {
            Some(Element::Symbol(id)) => store.sym_name(*id).to_string(),
            _ => continue,
        };
        if let Some(want) = language {
            if lang != want { continue; }
        }
        // Text slot must be a string literal.
        let text = match sent.elements.get(text_idx) {
            Some(Element::Literal(Literal::Str(s))) => strip_quotes(s),
            _ => continue,
        };
        out.push(DocEntry { language: lang, text });
    }
    out
}

/// Strip the surrounding `"..."` that the KIF tokenizer preserves on
/// string literals.  Safe to call on unquoted strings -- it is a no-op
/// when the bounds don't match.
fn strip_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
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
                Some(Element::Symbol(id)) if *id == sym_id
            );
            if !child_ok { continue; }
            if let Some(Element::Symbol(parent)) = sent.elements.get(2) {
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
}
