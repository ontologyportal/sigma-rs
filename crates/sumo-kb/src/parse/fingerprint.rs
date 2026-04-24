// crates/sumo-kb/src/parse/fingerprint.rs
//
// Stable per-sentence hash over an [`AstNode`] tree.  Independent of
// source whitespace, span information, and `SentenceId` / `SymbolId`
// allocations -- two syntactically-identical sentences always hash
// the same, even if one moved to a different line in the file.
//
// Non-LSP uses: content-addressed dedup, test-snapshot hashing,
// sentence-level diffing against an on-disk version of a file,
// incremental file-watcher workflows.  The existing clause-level
// canonical hash in `canonical.rs` is TFF/FOF-abstract and
// AC-reducing; this one is structural.  Both coexist because the
// use cases are different (clause dedup vs. source-level diff).
//
// The byte layout is deliberately terse -- one tag byte per element
// kind, then the payload.  `xxhash_rust::xxh64` is already a direct
// dep of this crate via the clause-canonical module.

// `HashMap` is only needed by the canonical (alpha-equivalent)
// walkers, which live behind `ask`.
#[cfg(feature = "ask")]
use std::collections::HashMap;

use xxhash_rust::xxh64::Xxh64;

use crate::parse::ast::{AstNode, OpKind};

// Store-side fingerprint walkers.  Previously `ask`-gated because
// their only consumers (reconcile / axiom_source) were themselves
// `ask`-gated.  Now unconditionally available — reconcile is no
// longer `ask`-gated, and the walkers are feature-neutral anyway.
use crate::kif_store::KifStore;
use crate::types::{Element, Literal, SentenceId};

const SEED: u64 = 0xC0DE_5F5F_5F5F_5F5Fu64;

// Tag bytes distinguish variants so two different element kinds
// with the same payload hash differently.
const TAG_LIST: u8 = b'L';
const TAG_SYM:  u8 = b'S';
const TAG_VAR:  u8 = b'V';
const TAG_ROW:  u8 = b'R';
const TAG_NUM:  u8 = b'N';
const TAG_STR:  u8 = b'T';
const TAG_OP:   u8 = b'O';

// -- Leaf emitters ------------------------------------------------------------
//
// Every fingerprint walker — AST-side / store-side × plain / canonical —
// writes the same `tag-byte + payload + 0-terminator` sequence for each
// leaf kind.  Factoring the four combinations' shared bookkeeping into
// these helpers cuts ~80 LoC of duplication and makes a future change
// to the byte layout a single-site edit.
//
// The 0-byte terminator on strings guards against payload-concatenation
// collisions: `"ab"+"c"` and `"a"+"bc"` hash identically without it.

#[inline]
fn emit_list_header(h: &mut Xxh64, len: usize) {
    h.update(&[TAG_LIST]);
    h.update(&(len as u32).to_be_bytes());
}

#[inline]
fn emit_symbol(h: &mut Xxh64, name: &str) {
    h.update(&[TAG_SYM]);
    h.update(name.as_bytes());
    h.update(&[0]);
}

#[inline]
fn emit_number(h: &mut Xxh64, value: &str) {
    h.update(&[TAG_NUM]);
    h.update(value.as_bytes());
    h.update(&[0]);
}

#[inline]
fn emit_str(h: &mut Xxh64, value: &str) {
    h.update(&[TAG_STR]);
    h.update(value.as_bytes());
    h.update(&[0]);
}

#[inline]
fn emit_op(h: &mut Xxh64, op: &OpKind) {
    h.update(&[TAG_OP]);
    h.update(op_byte(op));
}

/// Plain (preserves original name) variable / row-variable leaf.
#[inline]
fn emit_variable_plain(h: &mut Xxh64, name: &str, is_row: bool) {
    let tag = if is_row { TAG_ROW } else { TAG_VAR };
    h.update(&[tag]);
    h.update(name.as_bytes());
    h.update(&[0]);
}

/// Canonical (renumbered in first-occurrence order) variable /
/// row-variable leaf.  Two variables with the same canonical index
/// hash identically regardless of their surface names, giving the
/// alpha-equivalent fingerprint property.
///
/// Used only by the `ask`-gated canonical walkers.
#[cfg(feature = "ask")]
#[inline]
fn emit_variable_canonical(
    h:    &mut Xxh64,
    name: &str,
    is_row: bool,
    vars: &mut HashMap<String, u32>,
    rows: &mut HashMap<String, u32>,
) {
    let (tag, map) = if is_row {
        (TAG_ROW, rows)
    } else {
        (TAG_VAR, vars)
    };
    let next = map.len() as u32;
    let idx = *map.entry(name.to_owned()).or_insert(next);
    h.update(&[tag]);
    h.update(&idx.to_be_bytes());
}

/// Hash a root AST node (expected to be an [`AstNode::List`]) into a
/// stable 64-bit fingerprint.  For non-list roots returns a hash of
/// the node in-place, which is still a valid fingerprint but seldom
/// produced by a well-formed KIF document.
pub fn sentence_fingerprint(node: &AstNode) -> u64 {
    let mut h = Xxh64::new(SEED);
    hash_node(&mut h, node);
    h.digest()
}

/// Plain (non-alpha-equivalent) fingerprint of a `Sentence` stored
/// in the `KifStore`.  Produces the **same hash bytes** as
/// [`sentence_fingerprint`] on the equivalent `AstNode`, so stored
/// sentences can be compared against freshly-parsed AST nodes.
///
/// Used by the disk-to-memory reconcile path: when a DB is opened,
/// only `file_roots` is rehydrated (not `file_hashes`), so the
/// reconciler recomputes hashes for rehydrated sentences on demand
/// before diffing against the fresh on-disk text.
pub(crate) fn sentence_fingerprint_from_store(sid: SentenceId, store: &KifStore) -> u64 {
    let mut h = Xxh64::new(SEED);
    hash_sentence_plain(&mut h, sid, store);
    h.digest()
}

fn hash_sentence_plain(h: &mut Xxh64, sid: SentenceId, store: &KifStore) {
    let sentence = &store.sentences[store.sent_idx(sid)];
    emit_list_header(h, sentence.elements.len());
    for elem in &sentence.elements {
        hash_element_plain(h, elem, store);
    }
}

fn hash_element_plain(h: &mut Xxh64, elem: &Element, store: &KifStore) {
    match elem {
        Element::Symbol   { id, .. }              => emit_symbol(h, store.sym_name(*id)),
        Element::Variable { name, is_row, .. }    => emit_variable_plain(h, name, *is_row),
        Element::Literal  { lit: Literal::Number(n), .. } => emit_number(h, n),
        Element::Literal  { lit: Literal::Str(s), .. }    => emit_str(h, s),
        Element::Sub      { sid: child, .. }      => hash_sentence_plain(h, *child, store),
        Element::Op       { op, .. }              => emit_op(h, op),
    }
}

fn hash_node(h: &mut Xxh64, node: &AstNode) {
    match node {
        AstNode::List { elements, .. } => {
            emit_list_header(h, elements.len());
            for el in elements { hash_node(h, el); }
        }
        AstNode::Symbol      { name, .. }  => emit_symbol(h, name),
        AstNode::Variable    { name, .. }  => emit_variable_plain(h, name, false),
        AstNode::RowVariable { name, .. }  => emit_variable_plain(h, name, true),
        AstNode::Number      { value, .. } => emit_number(h, value),
        AstNode::Str         { value, .. } => emit_str(h, value),
        AstNode::Operator    { op, .. }    => emit_op(h, op),
    }
}

// -- Alpha-equivalent fingerprint ---------------------------------------------
//
// Same tag scheme as `sentence_fingerprint`, but variable names are
// replaced with a per-hash counter indexed by first occurrence.  Two
// formulas that differ only in the names of their bound / free
// variables — e.g. `(=> (P ?X) (Q ?X))` vs. `(=> (P ?Y) (Q ?Y))` —
// produce the same hash.  That's exactly the equivalence Vampire's
// proof steps need: it renames user variables to `X0`, `X1`, …
// internally, and we want to map those back to the source axioms
// they came from.
//
// `canonical_sentence_fingerprint` operates on a parsed `AstNode`
// (proof-side path) and `sentence_canonical_fingerprint` operates on
// a `Sentence` in the KifStore (KB-side path).  They produce
// **identical** hashes for alpha-equivalent formulas — this is
// tested below — so the lookup works symmetrically.
//
// Gated on `ask` because the sole consumer is `axiom_source`'s
// proof-step-to-source matcher, which only compiles under `ask`.
// The plain (non-canonical) walkers above stay un-gated because
// `reconcile_file` uses them unconditionally.

/// Alpha-equivalent fingerprint of a parsed KIF formula.  Variable
/// names are renumbered in first-occurrence order so that
/// alpha-variants collapse to the same hash.
///
/// Outer-universal normalization: any chain of leading `(forall …)`
/// wrappers is stripped before hashing.  KIF treats free variables in
/// a top-level sentence as implicitly universally quantified, while
/// Vampire's TPTP output always makes them explicit, so the two
/// spellings of `(=> (P ?X) (Q ?X))` and
/// `(forall (?X) (=> (P ?X) (Q ?X)))` must hash identically for
/// proof-to-source matching to work.  This strip is outer-only —
/// nested `(forall …)` inside a body (e.g. under an implication's
/// consequent) stays structural.
#[cfg(feature = "ask")]
pub fn canonical_sentence_fingerprint(node: &AstNode) -> u64 {
    let mut h = Xxh64::new(SEED);
    let mut vars: HashMap<String, u32> = HashMap::new();
    let mut rows: HashMap<String, u32> = HashMap::new();
    hash_node_canonical(&mut h, strip_leading_forall(node), &mut vars, &mut rows);
    h.digest()
}

/// Same as [`canonical_sentence_fingerprint`] but walks a `Sentence`
/// in the `KifStore` directly, without a round-trip to AST.  Produces
/// identical hash bytes when the underlying formulas are
/// alpha-equivalent.  Applies the same outer-forall stripping.
#[cfg(feature = "ask")]
pub(crate) fn sentence_canonical_fingerprint(sid: SentenceId, store: &KifStore) -> u64 {
    let mut h = Xxh64::new(SEED);
    let mut vars: HashMap<String, u32> = HashMap::new();
    let mut rows: HashMap<String, u32> = HashMap::new();
    let inner = strip_leading_forall_sid(sid, store);
    hash_sentence_canonical(&mut h, inner, store, &mut vars, &mut rows);
    h.digest()
}

/// Peel outer `(forall (?vars…) body)` wrappers off an AST node and
/// return a reference to the innermost body.  Non-forall nodes are
/// returned unchanged.  Used only by the `ask`-gated
/// `canonical_sentence_fingerprint`.
#[cfg(feature = "ask")]
fn strip_leading_forall(mut node: &AstNode) -> &AstNode {
    loop {
        let AstNode::List { elements, .. } = node else { return node; };
        // Shape: [Operator(ForAll), List(vars), body]
        if elements.len() != 3 { return node; }
        let is_forall = matches!(
            &elements[0],
            AstNode::Operator { op: OpKind::ForAll, .. }
        );
        if !is_forall { return node; }
        node = &elements[2];
    }
}

/// Store-side twin of [`strip_leading_forall`]: walks through outer
/// `(forall …)` sentences via `Element::Sub` links until the body
/// sentence is reached.
#[cfg(feature = "ask")]
fn strip_leading_forall_sid(mut sid: SentenceId, store: &KifStore) -> SentenceId {
    loop {
        let s = &store.sentences[store.sent_idx(sid)];
        // Shape: [Op(ForAll), Sub(vars), Sub(body)]
        if s.elements.len() != 3 { return sid; }
        let is_forall = matches!(
            s.elements.first(),
            Some(Element::Op { op: OpKind::ForAll, .. })
        );
        if !is_forall { return sid; }
        match &s.elements[2] {
            Element::Sub { sid: body, .. } => sid = *body,
            _ => return sid,
        }
    }
}

#[cfg(feature = "ask")]
fn hash_node_canonical(
    h:    &mut Xxh64,
    node: &AstNode,
    vars: &mut HashMap<String, u32>,
    rows: &mut HashMap<String, u32>,
) {
    match node {
        AstNode::List { elements, .. } => {
            emit_list_header(h, elements.len());
            for el in elements { hash_node_canonical(h, el, vars, rows); }
        }
        AstNode::Symbol      { name, .. }  => emit_symbol(h, name),
        AstNode::Variable    { name, .. }  => emit_variable_canonical(h, name, false, vars, rows),
        AstNode::RowVariable { name, .. }  => emit_variable_canonical(h, name, true,  vars, rows),
        AstNode::Number      { value, .. } => emit_number(h, value),
        AstNode::Str         { value, .. } => emit_str(h, value),
        AstNode::Operator    { op, .. }    => emit_op(h, op),
    }
}

#[cfg(feature = "ask")]
fn hash_sentence_canonical(
    h:    &mut Xxh64,
    sid:  SentenceId,
    store: &KifStore,
    vars: &mut HashMap<String, u32>,
    rows: &mut HashMap<String, u32>,
) {
    let sentence = &store.sentences[store.sent_idx(sid)];
    emit_list_header(h, sentence.elements.len());
    for elem in &sentence.elements {
        hash_element_canonical(h, elem, store, vars, rows);
    }
}

#[cfg(feature = "ask")]
fn hash_element_canonical(
    h:    &mut Xxh64,
    elem: &Element,
    store: &KifStore,
    vars: &mut HashMap<String, u32>,
    rows: &mut HashMap<String, u32>,
) {
    match elem {
        Element::Symbol   { id, .. }                        => emit_symbol(h, store.sym_name(*id)),
        Element::Variable { name, is_row, .. }              => emit_variable_canonical(h, name, *is_row, vars, rows),
        Element::Literal  { lit: Literal::Number(n), .. }   => emit_number(h, n),
        Element::Literal  { lit: Literal::Str(s), .. }      => emit_str(h, s),
        Element::Sub      { sid: child, .. }                => hash_sentence_canonical(h, *child, store, vars, rows),
        Element::Op       { op, .. }                        => emit_op(h, op),
    }
}

fn op_byte(op: &OpKind) -> &'static [u8] {
    match op {
        OpKind::And     => b"a",
        OpKind::Or      => b"o",
        OpKind::Not     => b"n",
        OpKind::Implies => b"i",
        OpKind::Iff     => b"f",
        OpKind::Equal   => b"e",
        OpKind::ForAll  => b"A",
        OpKind::Exists  => b"E",
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Parser;

    fn hash_of(src: &str) -> Vec<u64> {
        let (ast, errs) = Parser::Kif.parse(src, "test");
        assert!(errs.is_empty(), "parse errors: {:?}", errs);
        ast.iter().map(sentence_fingerprint).collect()
    }

    #[test]
    fn identical_sentences_hash_same() {
        let a = hash_of("(subclass Human Animal)");
        let b = hash_of("(subclass Human Animal)");
        assert_eq!(a, b);
    }

    #[test]
    fn whitespace_does_not_affect_hash() {
        let a = hash_of("(subclass Human Animal)");
        let b = hash_of("(subclass  Human\n  Animal)");
        assert_eq!(a, b);
    }

    #[test]
    fn line_position_does_not_affect_hash() {
        let a = hash_of("(subclass Human Animal)");
        let b = hash_of("\n\n\n(subclass Human Animal)");
        assert_eq!(a, b);
    }

    #[test]
    fn comment_lines_do_not_affect_hash() {
        let a = hash_of("(subclass Human Animal)");
        let b = hash_of("; doc\n; more doc\n(subclass Human Animal)");
        assert_eq!(a, b);
    }

    #[test]
    fn different_sentences_hash_differently() {
        let a = hash_of("(subclass Human Animal)");
        let b = hash_of("(subclass Human Hominid)");
        assert_ne!(a, b);
    }

    #[test]
    fn variable_rename_changes_hash() {
        // The fingerprint is syntactic -- it deliberately does NOT do
        // alpha-equivalence.  `?X` and `?Y` are different sentences.
        let a = hash_of("(=> (P ?X) (Q ?X))");
        let b = hash_of("(=> (P ?Y) (Q ?Y))");
        assert_ne!(a, b);
    }

    #[test]
    fn tag_separation_prevents_symbol_vs_variable_collision() {
        // `?Foo` and `Foo` would collide if we didn't tag-byte them.
        let a = hash_of("(P ?Foo)");
        let b = hash_of("(P Foo)");
        assert_ne!(a, b);
    }

    #[test]
    fn number_and_string_with_same_text_hash_differently() {
        let a = hash_of("(P 42)");
        let b = hash_of("(P \"42\")");
        assert_ne!(a, b);
    }

    #[test]
    fn nested_list_structure_affects_hash() {
        let a = hash_of("(=> (P ?X) (Q ?X))");
        let b = hash_of("(=> (Q ?X) (P ?X))");
        assert_ne!(a, b);
    }

    // -- Canonical (alpha-equivalent) fingerprint ----------------------------
    //
    // All canonical-fingerprint tests gated on `ask` because the
    // canonical walkers themselves are gated on `ask` (their only
    // non-test consumer is `axiom_source`'s proof-step matcher).

    #[cfg(feature = "ask")]
    fn canon_hash_of(src: &str) -> Vec<u64> {
        let (ast, errs) = Parser::Kif.parse(src, "test");
        assert!(errs.is_empty(), "parse errors: {:?}", errs);
        ast.iter().map(canonical_sentence_fingerprint).collect()
    }

    #[cfg(feature = "ask")]
    #[test]
    fn canonical_collapses_variable_renames() {
        // The whole point: alpha-variants hash identically under
        // `canonical_sentence_fingerprint`, so Vampire's `?X0` maps
        // back to the source's `?HUMAN`.
        let a = canon_hash_of("(=> (P ?X) (Q ?X))");
        let b = canon_hash_of("(=> (P ?Y) (Q ?Y))");
        let c = canon_hash_of("(=> (P ?HUMAN) (Q ?HUMAN))");
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[cfg(feature = "ask")]
    #[test]
    fn canonical_preserves_distinct_variable_positions() {
        // ?X appearing twice is NOT the same shape as ?X then ?Y.
        let a = canon_hash_of("(=> (P ?X) (Q ?X))");
        let b = canon_hash_of("(=> (P ?X) (Q ?Y))");
        assert_ne!(a, b);
    }

    #[cfg(feature = "ask")]
    #[test]
    fn canonical_preserves_symbol_names() {
        // Non-variable symbols still matter — renaming `P` to `Q`
        // is not alpha equivalence.
        let a = canon_hash_of("(=> (P ?X) (Q ?X))");
        let b = canon_hash_of("(=> (Q ?X) (R ?X))");
        assert_ne!(a, b);
    }

    #[cfg(feature = "ask")]
    #[test]
    fn canonical_strips_leading_forall() {
        // KIF leaves free variables implicitly universally quantified
        // at the top level.  Vampire's TPTP output makes them
        // explicit.  The two must hash identically so
        // proof-step-to-source matching works.
        let implicit = canon_hash_of("(=> (P ?X) (Q ?X))");
        let single   = canon_hash_of("(forall (?X) (=> (P ?X) (Q ?X)))");
        let renamed  = canon_hash_of("(forall (?HUMAN) (=> (P ?HUMAN) (Q ?HUMAN)))");
        assert_eq!(implicit, single);
        assert_eq!(implicit, renamed);
    }

    #[cfg(feature = "ask")]
    #[test]
    fn canonical_strips_multiple_outer_foralls_but_not_nested() {
        // Nested single-var foralls — Vampire's preferred layout —
        // must collapse onto the flat multi-var spelling found in
        // most SUMO source axioms.
        let source = canon_hash_of("(=> (R ?A ?B ?C) (S ?A ?B ?C))");
        let vampire_style = canon_hash_of(
            "(forall (?X1) (forall (?X2) (forall (?X3) (=> (R ?X1 ?X2 ?X3) (S ?X1 ?X2 ?X3)))))"
        );
        assert_eq!(source, vampire_style);
    }

    #[cfg(feature = "ask")]
    #[test]
    fn canonical_preserves_inner_foralls() {
        // Only the outermost chain is stripped — a forall embedded
        // inside an implication's body is semantically load-bearing
        // and must stay structural.
        let outer = canon_hash_of("(forall (?X) (=> (P ?X) (Q ?X)))");
        let inner = canon_hash_of("(=> (forall (?X) (P ?X)) (Q ?X))");
        assert_ne!(outer, inner);
    }

    #[test]
    fn plain_matches_between_ast_and_store() {
        // Same as the canonical cross-check but for the
        // non-alpha-equivalent `sentence_fingerprint_from_store` —
        // used by the reconcile fallback when `file_hashes` is
        // empty after a DB reload.  The variable-name-preserving
        // hash must agree across both sides so the file-diff
        // retained/removed/added classification is stable.
        use crate::KnowledgeBase;
        let mut kb = KnowledgeBase::new();
        const SRC: &str = "(subclass Dog Mammal)";
        let r = kb.load_kif(SRC, "t.kif", Some("t.kif"));
        assert!(r.ok, "load failed: {:?}", r.errors);

        let (ast, _) = Parser::Kif.parse(SRC, "test-ast");
        let ast_hash = sentence_fingerprint(&ast[0]);

        let sid = kb.file_roots("t.kif")[0];
        let store_hash = sentence_fingerprint_from_store(sid, kb.store_for_testing());

        assert_eq!(
            ast_hash, store_hash,
            "AST-side and store-side plain fingerprints must agree"
        );
    }

    #[cfg(feature = "ask")]
    #[test]
    fn canonical_matches_between_ast_and_store() {
        // The two canonical-hash entry points must produce identical
        // bytes: one walks an `AstNode`, the other walks a
        // `Sentence` in the store.  This is the property the
        // proof-source lookup depends on.
        use crate::KnowledgeBase;
        const SRC: &str = "(=> (instance ?A Agent) (instance ?A Entity))";
        let mut kb = KnowledgeBase::new();
        let r = kb.load_kif(SRC, "test.kif", Some("test.kif"));
        assert!(r.ok, "load failed: {:?}", r.errors);

        // Renumbered version: should still hash equal under alpha-equivalence.
        let (ast, _) = Parser::Kif.parse(
            "(=> (instance ?X0 Agent) (instance ?X0 Entity))",
            "test-ast",
        );
        let ast_hash = canonical_sentence_fingerprint(&ast[0]);

        // Store-side hash of the loaded sentence.  Reaching into
        // the private `layer.store` via the crate-internal
        // `store_for_testing` accessor — a sibling-module test
        // doesn't need a wider API surface for this.
        let sid = kb.file_roots("test.kif")[0];
        let store_hash = sentence_canonical_fingerprint(sid, kb.store_for_testing());

        assert_eq!(
            ast_hash, store_hash,
            "AstNode-side and Sentence-side canonical hashes must agree"
        );
    }

    #[test]
    fn many_sentences_have_independent_hashes() {
        let src = "(instance A B)\n(instance A B)\n(instance C D)";
        let hs  = hash_of(src);
        assert_eq!(hs.len(), 3);
        assert_eq!(hs[0], hs[1]);    // dup
        assert_ne!(hs[0], hs[2]);
    }
}
