// crates/core/src/saturate/fingerprint.rs
//
// The residue algebra (prototype §3): coins, fingerprints, residues.
//
// Every (seat, ground-term) pair has a pseudo-random 64-bit *coin*.
// An atom's *fingerprint* is the XOR of its ground seats' coins plus an
// arity tag; variables contribute nothing (they live in the out-of-band
// *mask* — the set of non-ground seats).  THE KEY EQUATION: for every
// atom `a` that a pattern `p` matches,
//
//     residue_under(a, mask(p)) == fingerprint(p)
//
// because removing the seats `p` leaves open makes the remaining ground
// seats coincide.  XOR's self-inverse property is what makes residues
// *cancellable*: moving between masks is one coin-XOR per seat changed
// (the index's lazy union views ride exactly this).
//
// Per-atom facts are memoized in [`AtomInfos`] keyed by content hash —
// SUMO-scale background clauses are fingerprinted once, ever, across
// every problem.

use std::sync::Arc;

use dashmap::DashMap;
use smallvec::SmallVec;

use xxhash_rust::xxh64::xxh64;

use crate::syntactic::SyntacticLayer;
use crate::types::{Element, Literal, Symbol};

use super::super::clause::{AtomId, AtomTable, Term};

/// Seed for the coin PRF — its own keyspace, disjoint from sentence
/// content hashes and clause keys.
const COIN_SEED: u64 = 0xC0_1A_C0_1A_C0_1A_C0_1A;

/// Seats at or beyond this index never carry coins and are treated as
/// permanently masked.  Probing stays sound (the candidate set is a
/// superset; unification verifies) — only selectivity degrades, and no
/// real SUMO atom is this wide.
pub(crate) const MAX_SEATS: usize = 64;

/// The coin for term-key `key` sitting in `seat`.  Seat 0 is the head —
/// a predicate variable is just an open seat 0 (the second-order door).
#[inline]
pub(crate) fn coin(seat: usize, key: u64) -> u64 {
    let mut buf = [0u8; 12];
    buf[..4].copy_from_slice(&(seat as u32).to_be_bytes());
    buf[4..].copy_from_slice(&key.to_be_bytes());
    xxh64(&buf, COIN_SEED)
}

/// The arity tag mixed into every fingerprint, so `(p a)` and `(p a b)`
/// can never collide via an empty XOR pile.
#[inline]
pub(crate) fn arity_tag(arity: usize) -> u64 {
    xxh64(&(arity as u32).to_be_bytes(), COIN_SEED ^ 0xA117)
}

/// Everything the index needs to know about one atom, precomputed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AtomInfo {
    /// Number of seats (sentence elements, head included).
    pub(crate) arity: u8,
    /// Bit `i` set ⇔ seat `i` is non-ground (a variable, or a subterm
    /// containing one).  Seats ≥ [`MAX_SEATS`] are implicitly masked.
    pub(crate) mask: u64,
    /// The fingerprint: arity tag ⊕ coins of the ground seats — i.e.
    /// the residue under the atom's own mask.
    pub(crate) base_residue: u64,
    /// Coin per seat; `0` for masked seats (so XOR-ing a masked seat is
    /// a no-op, which keeps [`Self::residue_under`] branch-free).
    pub(crate) seat_coins: SmallVec<[u64; 6]>,
    /// Sum of CUBES of the ground-seat coins (GF(2^64)) — the second
    /// power-sum word.  Residuals of two same-arity atoms are decodable
    /// up to two unknowns (see `gf64::decode`).  No arity tag (it would
    /// cancel in every residual anyway).
    pub(crate) s3: u64,
    /// Term depth of the whole atom (leaf = 0, so a flat atom is 1).
    pub(crate) depth: u8,
    /// Leaf count of the whole atom.
    pub(crate) size: u16,
    /// 64-bit Bloom signature of the atom's GROUND LEAVES (head symbol,
    /// argument symbols/literals, recursively through compound seats —
    /// variables contribute nothing).  One bit per leaf, keyed by the
    /// same content keys the coins use.  Backs the conjecture-distance
    /// queue factor (Liu & Xu-style structural relevance): two atoms
    /// that share ground leaves are few substitution steps apart, and
    /// `popcount(sig_a & sig_b)` measures exactly that, in one AND.
    pub(crate) leaf_sig: u64,
}

impl AtomInfo {
    #[inline]
    pub(crate) fn is_ground(&self) -> bool { self.mask == 0 }

    /// Fingerprint with the seats in `u` (a bitmask) removed: XOR off
    /// the ground coins at those seats.  `u ⊇ mask` is the caller's
    /// contract (a union mask); masked seats have zero coins so they
    /// cost nothing.  THE KEY EQUATION's left-hand side.
    #[inline]
    pub(crate) fn residue_under(&self, u: u64) -> u64 {
        let mut r = self.base_residue;
        let mut extra = u & !self.mask;
        while extra != 0 {
            let i = extra.trailing_zeros() as usize;
            if i >= self.seat_coins.len() { break; }
            r ^= self.seat_coins[i];
            extra &= extra - 1;
        }
        r
    }
}

/// Layer-wide memo: `AtomId -> AtomInfo`.  Content addressing makes the
/// memo permanent — an atom's info can never go stale, only unreferenced.
#[derive(Debug, Default)]
pub(crate) struct AtomInfos {
    map: DashMap<AtomId, Arc<AtomInfo>, super::super::hash64::BuildContentHasher>,
    /// coin → (seat, filler): the algebraic-extraction phone book.
    dict: DashMap<u64, (u8, CoinVal), super::super::hash64::BuildContentHasher>,
}

/// What a coin encodes, materializable back into a [`Term`].
#[derive(Debug, Clone)]
pub(crate) enum CoinVal {
    Sym(Symbol),
    Sub(AtomId),
    Lit(Literal),
    Op(crate::parse::OpKind),
}

/// The dictionary payload for a ground element.
fn coin_val(el: &Element) -> CoinVal {
    match el {
        Element::Symbol(s) => CoinVal::Sym(s.0.clone()),
        Element::Sub(sid) => CoinVal::Sub(*sid),
        Element::Literal(l) => CoinVal::Lit(l.clone()),
        Element::Op(op) => CoinVal::Op(op.clone()),
        Element::Variable { .. } => unreachable!("variables never coin"),
    }
}

/// What one *seat term* contributes to its enclosing atom.
struct SeatMeta {
    ground: bool,
    depth:  u8,
    size:   u16,
    /// Coin key — meaningful only when `ground`.
    key:    u64,
    /// Ground-leaf signature of the seat (leaf: its own bit; compound:
    /// the subterm's accumulated signature — ground leaves under an
    /// open compound still count; variable: 0).
    leaf_sig: u64,
}

impl AtomInfos {
    /// The (memoized) info for atom `id`.  Resolves through the prover's
    /// `AtomTable` with store fall-back, recursing into subterms (whose
    /// info memoizes under their own ids on the way).
    pub(crate) fn info(
        &self,
        id:    AtomId,
        atoms: &AtomTable,
        syn:   &SyntacticLayer,
    ) -> Arc<AtomInfo> {
        if let Some(hit) = self.map.get(&id) {
            return hit.value().clone();
        }
        let computed = Arc::new(self.compute(id, atoms, syn));
        self.map.entry(id).or_insert(computed).value().clone()
    }

    fn compute(&self, id: AtomId, atoms: &AtomTable, syn: &SyntacticLayer) -> AtomInfo {
        let Some(sent) = atoms.resolve(id, syn) else {
            // Unresolvable atom: treat as a fully-masked zero-arity husk;
            // probes degrade to "verify everything", never to wrong answers.
            return AtomInfo {
                arity: 0, mask: 0, base_residue: arity_tag(0),
                seat_coins: SmallVec::new(), s3: 0, depth: 0, size: 0,
                leaf_sig: 0,
            };
        };
        let n = sent.elements.len();
        let mut mask = 0u64;
        let mut residue = arity_tag(n);
        let mut s3 = 0u64;
        let mut coins: SmallVec<[u64; 6]> = SmallVec::with_capacity(n);
        let mut depth = 0u8;
        let mut size = 0u16;
        let mut leaf_sig = 0u64;
        for (i, el) in sent.elements.iter().enumerate() {
            let m = self.seat_meta(el, atoms, syn);
            depth = depth.max(m.depth);
            size = size.saturating_add(m.size);
            leaf_sig |= m.leaf_sig;
            if i >= MAX_SEATS || !m.ground {
                if !m.ground && i < MAX_SEATS {
                    mask |= 1u64 << i;
                } else if i >= MAX_SEATS {
                    // Out-of-range seats are "masked" without a bit to
                    // set; both sides of the equation skip them alike.
                }
                coins.push(0);
            } else {
                let c = coin(i, m.key);
                residue ^= c;
                s3 ^= crate::gf64::cube(c);
                coins.push(c);
                // The decode phone book: coin → (seat, filler).  Pure
                // function of its inputs — append-only, never stale.
                self.dict.entry(c).or_insert_with(|| (i as u8, coin_val(el)));
            }
        }
        AtomInfo {
            arity: n.min(255) as u8,
            mask,
            base_residue: residue,
            seat_coins: coins,
            s3,
            depth: depth.saturating_add(1), // the atom itself is one level
            size,
            leaf_sig,
        }
    }

    /// One seat's metadata.  Compound subterms recurse through
    /// [`Self::info`], so their facts memoize under their own ids.
    fn seat_meta(&self, el: &Element, atoms: &AtomTable, syn: &SyntacticLayer) -> SeatMeta {
        // One Bloom bit per leaf, derived from the same content key the
        // coin uses.  Operator leaves (`Equal`) are excluded: the bit
        // would be shared by EVERY equality literal, which is overlap
        // noise, not relevance.
        let leaf_bit = |key: u64| 1u64 << (key & 63);
        match el {
            Element::Variable { .. } => SeatMeta {
                ground: false, depth: 0, size: 1, key: 0, leaf_sig: 0,
            },
            Element::Symbol(s) => {
                let key = xxh64(&s.id().to_be_bytes(), u64::from(b'S'));
                SeatMeta { ground: true, depth: 0, size: 1, key, leaf_sig: leaf_bit(key) }
            }
            Element::Literal(Literal::Str(v)) => {
                let key = xxh64(v.as_bytes(), u64::from(b'T'));
                SeatMeta { ground: true, depth: 0, size: 1, key, leaf_sig: leaf_bit(key) }
            }
            Element::Literal(Literal::Number(v)) => {
                let key = xxh64(v.as_bytes(), u64::from(b'N'));
                SeatMeta { ground: true, depth: 0, size: 1, key, leaf_sig: leaf_bit(key) }
            }
            Element::Op(op) => SeatMeta {
                ground: true, depth: 0, size: 1,
                key: xxh64(&[op_byte(op)], u64::from(b'O')),
                leaf_sig: 0,
            },
            Element::Sub(sid) => {
                let info = self.info(*sid, atoms, syn);
                SeatMeta {
                    ground: info.is_ground(),
                    depth:  info.depth,
                    size:   info.size,
                    // The subterm's identity IS its content hash — the
                    // structural-equality coin key, like the prototype's
                    // whole-term tuples.
                    key:    *sid,
                    // Leaves under an open compound still count: the
                    // signature tracks CONTENT overlap, not groundness.
                    leaf_sig: info.leaf_sig,
                }
            }
        }
    }

    pub(crate) fn len(&self) -> usize { self.map.len() }

    /// Decode-phone-book lookup: the (seat, filler) a coin encodes,
    /// materialized as a [`Term`] (compound subterms lift through the
    /// atom table).  `None` for unknown coins (collision ⇒ caller
    /// falls back to unification).
    pub(crate) fn coin_term(
        &self,
        c: u64,
        atoms: &AtomTable,
        syn: &SyntacticLayer,
    ) -> Option<(u8, Term)> {
        let entry = self.dict.get(&c)?;
        let (seat, val) = entry.value();
        let term = match val {
            CoinVal::Sym(s) => Term::Sym(s.clone()),
            CoinVal::Sub(sid) => atoms.term_of(*sid, syn)?,
            CoinVal::Lit(l) => Term::Lit(l.clone()),
            CoinVal::Op(op) => Term::Op(op.clone()),
        };
        Some((*seat, term))
    }
}

/// Coin a slot-`Term` leaf at `seat` exactly as [`AtomInfos::seat_meta`]
/// coins the matching `Element` — the bridge that lets un-interned
/// derived terms participate in THE KEY EQUATION.  `None` for
/// variables and compounds (no cheap coin without interning).
pub(crate) fn slot_term_seat_coin(seat: usize, t: &Term) -> Option<u64> {
    let key = match t {
        Term::Sym(s) => xxh64(&s.id().to_be_bytes(), u64::from(b'S')),
        Term::Lit(Literal::Str(v)) => xxh64(v.as_bytes(), u64::from(b'T')),
        Term::Lit(Literal::Number(v)) => xxh64(v.as_bytes(), u64::from(b'N')),
        Term::Op(op) => xxh64(&[op_byte(op)], u64::from(b'O')),
        Term::Var(_) | Term::App(_) => return None,
    };
    Some(coin(seat, key))
}

fn op_byte(op: &crate::parse::OpKind) -> u8 {
    use crate::parse::OpKind::*;
    match op {
        And => b'a', Or => b'o', Not => b'n', Implies => b'i',
        Iff => b'f', Equal => b'e', ForAll => b'A', Exists => b'E',
    }
}
