//! Content hash of a built sentence — the basis for content-addressed
//! `SentenceId`s (`SentenceId == content_hash(elements)`).
//!
//! Variables hash on their scope-qualified `SymbolId`, not on `var_index`, so
//! only concrete (variable-free) sentences share a scope-independent hash and
//! dedup.  `var_index` and `span` are excluded from both the hash and the
//! structural-equality check.

use xxhash_rust::xxh64::Xxh64;

use crate::parse::OpKind;
use crate::types::{Element, Literal, SentenceId};

const SEED: u64 = 0x5E27_E27E_5E27_E27E;

// One tag byte per element kind so two kinds with the same payload bytes hash
// differently.
const TAG_SYM: u8 = b'S';
const TAG_VAR: u8 = b'V';
const TAG_ROW: u8 = b'R';
const TAG_NUM: u8 = b'N';
const TAG_STR: u8 = b'T';
const TAG_SUB: u8 = b'L';
const TAG_OP:  u8 = b'O';

/// Content hash of a sentence's element list — its `SentenceId`.
pub(super) fn content_hash(elements: &[Element]) -> SentenceId {
    let mut h = Xxh64::new(SEED);
    h.update(&(elements.len() as u32).to_be_bytes());
    for el in elements {
        emit_element(&mut h, el);
    }
    h.digest()
}

fn emit_element(h: &mut Xxh64, el: &Element) {
    match el {
        Element::Symbol(sym) => {
            h.update(&[TAG_SYM]);
            h.update(&sym.id().to_be_bytes());
        }
        Element::Variable { id, is_row, .. } => {
            h.update(&[if *is_row { TAG_ROW } else { TAG_VAR }]);
            h.update(&id.to_be_bytes());
        }
        Element::Literal(lit) => match lit {
            Literal::Str(s) => {
                h.update(&[TAG_STR]);
                h.update(s.as_bytes());
                h.update(&[0]); // terminator: guards "ab"+"c" vs "a"+"bc"

            }
            Literal::Number(n) => {
                h.update(&[TAG_NUM]);
                h.update(n.as_bytes());
                h.update(&[0]);
            }
        },
        Element::Sub(sid) => {
            h.update(&[TAG_SUB]);
            h.update(&sid.to_be_bytes());
        }
        Element::Op(op) => {
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

/// Structural equality of two element lists, ignoring `span` and `var_index`
/// (the two fields excluded from [`content_hash`]).  Used only to confirm a
/// content-hash match is a genuine duplicate rather than a 64-bit collision.
pub(super) fn elements_eq(a: &[Element], b: &[Element]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| element_eq(x, y))
}

fn element_eq(a: &Element, b: &Element) -> bool {
    match (a, b) {
        (Element::Symbol(x), Element::Symbol(y)) => x == y,
        (
            Element::Variable { id: xi, is_row: xr, .. },
            Element::Variable { id: yi, is_row: yr, .. },
        ) => xi == yi && xr == yr,
        (Element::Literal(x), Element::Literal(y)) => x == y,
        (Element::Sub(x), Element::Sub(y)) => x == y,
        (Element::Op(x), Element::Op(y)) => x == y,
        _ => false,
    }
}
