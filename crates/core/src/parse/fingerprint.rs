//! Stable per-sentence hash over an [`AstNode`] tree, independent of source
//! whitespace, span information, and `SentenceId` / `SymbolId` allocations, so
//! two syntactically-identical sentences always hash the same.

#[cfg(any(feature = "ask", feature = "native-prover"))]
use std::collections::HashMap;

use xxhash_rust::xxh64::Xxh64;

use crate::parse::ast::{AstNode, OpKind};

const SEED: u64 = 0xC0DE_5F5F_5F5F_5F5Fu64;

// Tag bytes distinguish variants so two element kinds with the same payload
// hash differently.
const TAG_LIST: u8 = b'L';
const TAG_SYM:  u8 = b'S';
const TAG_VAR:  u8 = b'V';
const TAG_ROW:  u8 = b'R';
const TAG_NUM:  u8 = b'N';
const TAG_STR:  u8 = b'T';
const TAG_OP:   u8 = b'O';

impl AstNode {
    /// Non-canonical fingerprint of this node (variable names affect the hash).
    pub(crate) fn fingerprint(&self) -> u64 {
        sentence_fingerprint(self)
    }

    /// Canonical fingerprint of this node (variable names are irrelevant;
    /// tracked by first-occurrence index).
    #[cfg(any(feature = "ask", feature = "native-prover"))]
    #[allow(dead_code)]
    pub(crate) fn canonical_fingerprint(&self) -> u64 {
        canonical_sentence_fingerprint(self)
    }
} 

impl std::hash::Hash for AstNode {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u64(self.fingerprint());
    }
}

// The 0-byte terminator on strings guards against payload-concatenation
// collisions: `"ab"+"c"` and `"a"+"bc"` hash identically without it.

/// Update a hash with the header byte for an [`AstNode::List`]
#[inline]
fn emit_list_header(h: &mut Xxh64, len: usize) {
    h.update(&[TAG_LIST]);
    h.update(&(len as u32).to_be_bytes());
}

/// Update a hash with the name of a [`AstNode::Symbol`]
/// and its name (terminated with a null byte)
#[inline]
fn emit_symbol(h: &mut Xxh64, name: &str) {
    h.update(&[TAG_SYM]);
    h.update(name.as_bytes());
    h.update(&[0]);
}

/// Update a hash with the value of a literal [`AstNode::Number`]
#[inline]
fn emit_number(h: &mut Xxh64, value: &str) {
    h.update(&[TAG_NUM]);
    h.update(value.as_bytes());
    h.update(&[0]);
}

/// Update a hash with the value of a literal [`AstNode::Str`]
#[inline]
fn emit_str(h: &mut Xxh64, value: &str) {
    h.update(&[TAG_STR]);
    h.update(value.as_bytes());
    h.update(&[0]);
}

/// Update a hash with a literal [`OpKind`]
#[inline]
fn emit_op(h: &mut Xxh64, op: &OpKind) {
    h.update(&[TAG_OP]);
    h.update(op_byte(op));
}

/// Update a hash with the name of a [`AstNode::Variable`] and whether it is a
/// row, hashing the name literally (not canonicalized).
#[inline]
fn emit_variable_plain(h: &mut Xxh64, name: &str, is_row: bool) {
    let tag = if is_row { TAG_ROW } else { TAG_VAR };
    h.update(&[tag]);
    h.update(name.as_bytes());
    h.update(&[0]);
}

/// Emit a canonical (renumbered in first-occurrence order) [`AstNode::Variable`].
/// Two variables with the same canonical index hash identically regardless of
/// their surface names.
#[cfg(any(feature = "ask", feature = "native-prover"))]
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

/// Hash a root AST node (expected to be an [`AstNode::List`]) into a stable
/// 64-bit fingerprint.  For non-list roots, hashes the node in place, which is
/// still a valid fingerprint but seldom produced by a well-formed KIF document.
pub fn sentence_fingerprint(node: &AstNode) -> u64 {
    let mut h = Xxh64::new(SEED);
    hash_node(&mut h, node);
    h.digest()
}

/// Generic hashing of an [`AstNode`]
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
        // `Annotated` is stripped before a sentence is built and must never
        // reach the content hash.
        AstNode::Annotated   { .. }        => unreachable!(
            "Annotated statements are stripped before fingerprinting"),
    }
}

/// Alpha-equivalent fingerprint of a parsed KIF formula.  Variable names are
/// renumbered in first-occurrence order so that alpha-variants collapse to the
/// same hash.
///
/// Any chain of leading `(forall …)` wrappers is stripped before hashing, so
/// `(=> (P ?X) (Q ?X))` and `(forall (?X) (=> (P ?X) (Q ?X)))` hash identically.
/// The strip is outer-only: a nested `(forall …)` inside a body stays
/// structural.
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub(crate) fn canonical_sentence_fingerprint(node: &AstNode) -> u64 {
    let mut h = Xxh64::new(SEED);
    let mut vars: HashMap<String, u32> = HashMap::new();
    let mut rows: HashMap<String, u32> = HashMap::new();
    hash_node_canonical(&mut h, strip_leading_forall(node), &mut vars, &mut rows);
    h.digest()
}

/// Peel all outer `(forall (?vars…) body)` wrappers off an AST node and
/// return a reference to the innermost body.  Non-forall nodes are
/// returned unchanged.
#[cfg(any(feature = "ask", feature = "native-prover"))]
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
        node = strip_leading_forall(&elements[2]);
    }
}

#[cfg(any(feature = "ask", feature = "native-prover"))]
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
        // `Annotated` is stripped before a sentence is built and must never
        // reach the content hash.
        AstNode::Annotated   { .. }        => unreachable!(
            "Annotated statements are stripped before fingerprinting"),
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
        ast.iter().map(|d| sentence_fingerprint(&d.as_stmt().cloned().unwrap())).collect()
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
        let a = hash_of("(=> (P ?X) (Q ?X))");
        let b = hash_of("(=> (P ?Y) (Q ?Y))");
        assert_ne!(a, b);
    }

    #[test]
    fn tag_separation_prevents_symbol_vs_variable_collision() {
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

    #[cfg(any(feature = "ask", feature = "native-prover"))]
    fn canon_hash_of(src: &str) -> Vec<u64> {
        let (ast, errs) = Parser::Kif.parse(src, "test");
        assert!(errs.is_empty(), "parse errors: {:?}", errs);
        ast.iter().map(|d| canonical_sentence_fingerprint(&d.as_stmt().cloned().unwrap())).collect()
    }

    #[cfg(any(feature = "ask", feature = "native-prover"))]
    #[test]
    fn canonical_collapses_variable_renames() {
        let a = canon_hash_of("(=> (P ?X) (Q ?X))");
        let b = canon_hash_of("(=> (P ?Y) (Q ?Y))");
        let c = canon_hash_of("(=> (P ?HUMAN) (Q ?HUMAN))");
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[cfg(any(feature = "ask", feature = "native-prover"))]
    #[test]
    fn canonical_preserves_distinct_variable_positions() {
        let a = canon_hash_of("(=> (P ?X) (Q ?X))");
        let b = canon_hash_of("(=> (P ?X) (Q ?Y))");
        assert_ne!(a, b);
    }

    #[cfg(any(feature = "ask", feature = "native-prover"))]
    #[test]
    fn canonical_preserves_symbol_names() {
        let a = canon_hash_of("(=> (P ?X) (Q ?X))");
        let b = canon_hash_of("(=> (Q ?X) (R ?X))");
        assert_ne!(a, b);
    }

    #[cfg(any(feature = "ask", feature = "native-prover"))]
    #[test]
    fn canonical_strips_leading_forall() {
        let implicit = canon_hash_of("(=> (P ?X) (Q ?X))");
        let single   = canon_hash_of("(forall (?X) (=> (P ?X) (Q ?X)))");
        let renamed  = canon_hash_of("(forall (?HUMAN) (=> (P ?HUMAN) (Q ?HUMAN)))");
        assert_eq!(implicit, single);
        assert_eq!(implicit, renamed);
    }

    #[cfg(any(feature = "ask", feature = "native-prover"))]
    #[test]
    fn canonical_strips_multiple_outer_foralls_but_not_nested() {
        let source = canon_hash_of("(=> (R ?A ?B ?C) (S ?A ?B ?C))");
        let vampire_style = canon_hash_of(
            "(forall (?X1) (forall (?X2) (forall (?X3) (=> (R ?X1 ?X2 ?X3) (S ?X1 ?X2 ?X3)))))"
        );
        assert_eq!(source, vampire_style);
    }

    #[cfg(any(feature = "ask", feature = "native-prover"))]
    #[test]
    fn canonical_preserves_inner_foralls() {
        let outer = canon_hash_of("(forall (?X) (=> (P ?X) (Q ?X)))");
        let inner = canon_hash_of("(=> (forall (?X) (P ?X)) (Q ?X))");
        assert_ne!(outer, inner);
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
