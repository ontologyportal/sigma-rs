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
    let mut h = ElementHasher::new(elements.len());
    for el in elements {
        h.element(el);
    }
    h.finish()
}

/// Incremental view of [`content_hash`]'s byte scheme, one emit per element.
///
/// This is THE single definition of the content-address byte layout: both the
/// sentence store (`content_hash` above, via [`Self::element`]) and the
/// prover's derived-term key computation (`saturate::terms`, via the typed
/// emitters) drive the same hasher, so a derived ground `Term` hashes to
/// exactly the id `AtomTable::intern_atom` / the store would assign the same
/// content — one shared 64-bit keyspace across tiers.
pub(crate) struct ElementHasher(Xxh64);

impl ElementHasher {
    /// Start a hash for an element list of length `len` (the length is part
    /// of the content, mixed in first).
    pub(crate) fn new(len: usize) -> Self {
        let mut h = Xxh64::new(SEED);
        h.update(&(len as u32).to_be_bytes());
        Self(h)
    }

    /// A ground symbol, by its content id.
    pub(crate) fn symbol(&mut self, id: u64) {
        self.0.update(&[TAG_SYM]);
        self.0.update(&id.to_be_bytes());
    }

    /// A variable, by its scope-qualified symbol id.
    pub(crate) fn variable(&mut self, id: u64, is_row: bool) {
        self.0.update(&[if is_row { TAG_ROW } else { TAG_VAR }]);
        self.0.update(&id.to_be_bytes());
    }

    /// A string or numeric literal.
    pub(crate) fn literal(&mut self, lit: &Literal) {
        match lit {
            Literal::Str(s) => {
                self.0.update(&[TAG_STR]);
                self.0.update(s.as_bytes());
                self.0.update(&[0]); // terminator: guards "ab"+"c" vs "a"+"bc"
            }
            Literal::Number(n) => {
                self.0.update(&[TAG_NUM]);
                self.0.update(n.as_bytes());
                self.0.update(&[0]);
            }
        }
    }

    /// A sub-sentence, by its content id.
    pub(crate) fn sub(&mut self, sid: SentenceId) {
        self.0.update(&[TAG_SUB]);
        self.0.update(&sid.to_be_bytes());
    }

    /// An operator head.
    pub(crate) fn op(&mut self, op: &OpKind) {
        self.0.update(&[TAG_OP]);
        self.0.update(op_byte(op));
    }

    /// One whole element (the store-side walk).
    pub(crate) fn element(&mut self, el: &Element) {
        match el {
            Element::Symbol(sym) => self.symbol(sym.id()),
            Element::Variable { id, is_row, .. } => self.variable(*id, *is_row),
            Element::Literal(lit) => self.literal(lit),
            Element::Sub(sid) => self.sub(*sid),
            Element::Op(op) => self.op(op),
        }
    }

    /// The finished content hash.
    pub(crate) fn finish(self) -> SentenceId {
        self.0.digest()
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
