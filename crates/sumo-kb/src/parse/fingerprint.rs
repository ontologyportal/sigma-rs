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

use xxhash_rust::xxh64::Xxh64;

use crate::parse::ast::{AstNode, OpKind};

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

/// Hash a root AST node (expected to be an [`AstNode::List`]) into a
/// stable 64-bit fingerprint.  For non-list roots returns a hash of
/// the node in-place, which is still a valid fingerprint but seldom
/// produced by a well-formed KIF document.
pub fn sentence_fingerprint(node: &AstNode) -> u64 {
    let mut h = Xxh64::new(SEED);
    hash_node(&mut h, node);
    h.digest()
}

fn hash_node(h: &mut Xxh64, node: &AstNode) {
    match node {
        AstNode::List { elements, .. } => {
            h.update(&[TAG_LIST]);
            let len = elements.len() as u32;
            h.update(&len.to_be_bytes());
            for el in elements {
                hash_node(h, el);
            }
        }
        AstNode::Symbol { name, .. } => {
            h.update(&[TAG_SYM]);
            h.update(name.as_bytes());
            h.update(&[0]);  // terminator -- prevents "ab"+"c" hashing like "a"+"bc"
        }
        AstNode::Variable { name, .. } => {
            h.update(&[TAG_VAR]);
            h.update(name.as_bytes());
            h.update(&[0]);
        }
        AstNode::RowVariable { name, .. } => {
            h.update(&[TAG_ROW]);
            h.update(name.as_bytes());
            h.update(&[0]);
        }
        AstNode::Number { value, .. } => {
            h.update(&[TAG_NUM]);
            h.update(value.as_bytes());
            h.update(&[0]);
        }
        AstNode::Str { value, .. } => {
            h.update(&[TAG_STR]);
            h.update(value.as_bytes());
            h.update(&[0]);
        }
        AstNode::Operator { op, .. } => {
            h.update(&[TAG_OP]);
            h.update(op_byte(op));
        }
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

    #[test]
    fn many_sentences_have_independent_hashes() {
        let src = "(instance A B)\n(instance A B)\n(instance C D)";
        let hs  = hash_of(src);
        assert_eq!(hs.len(), 3);
        assert_eq!(hs[0], hs[1]);    // dup
        assert_ne!(hs[0], hs[2]);
    }
}
