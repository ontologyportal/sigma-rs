// crates/sumo-kb/src/fingerprint.rs
//
// Canonical formula fingerprinting for deduplication.
//
// Two formulas are considered equal if and only if they are alpha-equivalent:
// identical up to consistent renaming of variables.  We normalise all
// variables to positional names (Var_0, Var_1, ...) in DFS visitation order
// and hash the resulting byte sequence with xxHash64.

use std::collections::HashMap;
use xxhash_rust::xxh64::Xxh64;

use crate::kif_store::KifStore;
use crate::types::{Element, OpKind, SentenceId, SymbolId};

// -- Discriminant bytes --------------------------------------------------------
const D_SYMBOL:   u8 = 0x01;
const D_VARIABLE: u8 = 0x02;
const D_LITERAL:  u8 = 0x03;
const D_OP:       u8 = 0x04;
const D_SUB:      u8 = 0x05;
const D_ROWVAR:   u8 = 0x06;
const D_SEP:      u8 = 0x00; // NUL terminator after string bytes

fn op_byte(op: &OpKind) -> u8 {
    match op {
        OpKind::And     => 0,
        OpKind::Or      => 1,
        OpKind::Not     => 2,
        OpKind::Implies => 3,
        OpKind::Iff     => 4,
        OpKind::Equal   => 5,
        OpKind::ForAll  => 6,
        OpKind::Exists  => 7,
    }
}

// -- Internal DFS hasher -------------------------------------------------------

/// Recursively hash `sid` into `hasher`, normalising variables via `var_map`.
/// `var_counter` increments each time a new variable is encountered.
fn hash_sentence(
    store:       &KifStore,
    sid:         SentenceId,
    hasher:      &mut Xxh64,
    var_map:     &mut HashMap<SymbolId, u32>,
    var_counter: &mut u32,
) {
    let sentence = &store.sentences[store.sent_idx(sid)];
    for el in &sentence.elements {
        match el {
            Element::Symbol(id) => {
                hasher.update(&[D_SYMBOL]);
                hasher.update(store.sym_name(*id).as_bytes());
                hasher.update(&[D_SEP]);
            }
            Element::Variable { id, is_row, .. } => {
                let discriminant = if *is_row { D_ROWVAR } else { D_VARIABLE };
                let idx = var_map.entry(*id).or_insert_with(|| {
                    let n = *var_counter;
                    *var_counter += 1;
                    n
                });
                let canonical = format!("Var_{}", idx);
                hasher.update(&[discriminant]);
                hasher.update(canonical.as_bytes());
                hasher.update(&[D_SEP]);
            }
            Element::Literal(lit) => {
                hasher.update(&[D_LITERAL]);
                hasher.update(lit.to_string().as_bytes());
                hasher.update(&[D_SEP]);
            }
            Element::Op(op) => {
                hasher.update(&[D_OP, op_byte(op)]);
            }
            Element::Sub(sub_sid) => {
                hasher.update(&[D_SUB]);
                hash_sentence(store, *sub_sid, hasher, var_map, var_counter);
            }
        }
    }
}

// -- Public API ----------------------------------------------------------------

/// Compute a canonical xxHash64 fingerprint for the formula rooted at `sid`.
///
/// Two formulas are alpha-equivalent if and only if their fingerprints match.
pub(crate) fn fingerprint(store: &KifStore, sid: SentenceId) -> u64 {
    let mut hasher      = Xxh64::new(0);
    let mut var_map     = HashMap::new();
    let mut var_counter = 0u32;
    hash_sentence(store, sid, &mut hasher, &mut var_map, &mut var_counter);
    let hash = hasher.digest();
    log::trace!(target: "sumo_kb::fingerprint",
        "fingerprint sid={} hash={:016x}", sid, hash);
    hash
}

/// Returns the fingerprint for `sid` plus fingerprints for each direct
/// `Element::Sub` child of `sid`.  Used by `tell()` to check both the
/// top-level formula and its immediate sub-formulas for duplicates.
pub(crate) fn fingerprint_depth1(store: &KifStore, sid: SentenceId) -> Vec<u64> {
    let mut result = vec![fingerprint(store, sid)];
    let sentence = &store.sentences[store.sent_idx(sid)];
    for el in &sentence.elements {
        if let Element::Sub(sub_sid) = el {
            result.push(fingerprint(store, *sub_sid));
        }
    }
    result
}

// -- Tests ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kif_store::{load_kif, KifStore};

    fn store_with(src: &str) -> (KifStore, Vec<SentenceId>) {
        let mut store = KifStore::default();
        load_kif(&mut store, src, "test");
        let roots = store.roots.clone();
        (store, roots)
    }

    /// Alpha-equivalent formulas must have the same fingerprint.
    #[test]
    fn alpha_equivalence() {
        let (s1, r1) = store_with("(=> (instance ?X Human) (instance ?X Animal))");
        let (s2, r2) = store_with("(=> (instance ?Y Human) (instance ?Y Animal))");
        assert_eq!(fingerprint(&s1, r1[0]), fingerprint(&s2, r2[0]));
    }

    /// Structurally different formulas must have different fingerprints.
    #[test]
    fn distinctness() {
        let (s1, r1) = store_with("(subclass Dog Animal)");
        let (s2, r2) = store_with("(subclass Cat Animal)");
        assert_ne!(fingerprint(&s1, r1[0]), fingerprint(&s2, r2[0]));
    }

    /// The same formula loaded twice must be identical.
    #[test]
    fn deterministic() {
        let src = "(=> (instance ?X Human) (subclass ?X Mammal))";
        let (s1, r1) = store_with(src);
        let (s2, r2) = store_with(src);
        assert_eq!(fingerprint(&s1, r1[0]), fingerprint(&s2, r2[0]));
    }

    /// fingerprint_depth1 returns >=1 entry (at least the root).
    #[test]
    fn depth1_includes_root() {
        let (s, r) = store_with("(=> (instance ?X Human) (instance ?X Animal))");
        let fps = fingerprint_depth1(&s, r[0]);
        assert!(!fps.is_empty());
        assert_eq!(fps[0], fingerprint(&s, r[0]));
    }
}
