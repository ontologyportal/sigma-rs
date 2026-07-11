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
use super::super::parked;

/// Seed for the coin PRF — its own keyspace, disjoint from sentence
/// content hashes and clause keys.
const COIN_SEED: u64 = 0xC0_1A_C0_1A_C0_1A_C0_1A;

/// Seed for the seat SHAPE words (`AtomInfo::seat_shapes` /
/// `AtomInfo::self_shape`) — a third keyspace, disjoint from both the
/// coin PRF and the content hashes, so a leaf's coin fold and a
/// compound's (head, len) fold can only collide accidentally (which
/// weakens the filter, never falsely rejects).
const SHAPE_SEED: u64 = 0x5EA7_5EA7_5EA7_5EA7;

/// Seats at or beyond this index never carry coins and are treated as
/// permanently masked.  Probing stays sound (the candidate set is a
/// superset; unification verifies) — only selectivity degrades, and no
/// real SUMO atom is this wide.
pub(crate) const MAX_SEATS: usize = 64;

/// The coin for term-key `key` sitting in `seat`.  Seat 0 is the head —
/// a predicate variable is just an open seat 0 (the second-order door).
///
/// COMMUTATIVE-CANCELLATION AUDIT (phase-0 item 3, documented at the
/// derivation site): every path-weighted accumulation in this scheme
/// hashes position and content JOINTLY before accumulating — a coin is
/// `xxh64(seat ‖ content-key)`, never `f(seat) · g(content)` for
/// symbol-independent per-position multipliers.  In characteristic 2
/// (XOR piles), a product of independent per-position multipliers along
/// paths would make permuted paths collide as a FAMILY (carryless
/// commuting products cancel systematically); a joint PRF image cannot
/// be factored that way, so permuted seats collide only at the generic
/// 2⁻⁶⁴ rate.  The same discipline holds for every derived channel:
/// `s3` cubes the joint coin (GF(2⁶⁴) power sum of PRF images), the
/// shape words hash `(seat/head, len)` jointly under [`SHAPE_SEED`],
/// `leaf_sig` is an OR-accumulated (idempotent, cancellation-free)
/// Bloom of content-only bits, and the content hashes themselves
/// (`ElementHasher`) feed a sequential byte stream, not a commutative
/// product.  No symbol-independent per-position multiplier products
/// exist in this codepath.
#[inline]
pub(crate) fn coin(seat: usize, key: u64) -> u64 {
    let mut buf = [0u8; 12];
    buf[..4].copy_from_slice(&(seat as u32).to_be_bytes());
    buf[4..].copy_from_slice(&key.to_be_bytes());
    xxh64(&buf, COIN_SEED)
}

/// Fold a 64-bit shape hash to the 32-bit seat-shape word, reserving
/// `0` as the WILDCARD class (bare variable / var-headed compound /
/// seat ≥ [`MAX_SEATS`]).  Equal inputs fold equally (so a true shape
/// agreement can never be rejected); a fold collision between distinct
/// shapes only weakens the filter (false pass), never falsifies it.
#[inline]
fn fold_shape(h: u64) -> u32 {
    let f = ((h >> 32) as u32) ^ (h as u32);
    if f == 0 { 1 } else { f }
}

/// The (head, len) SHAPE hash of a compound with a concrete head —
/// `0` when the head is not a symbol/operator (variable-headed or
/// nested-compound-headed: shapeless, wildcard).  `head_key` is the
/// same per-leaf content key the coins use (`b'S'` / `b'O'` streams),
/// so the Element- and Term-side derivations agree byte for byte.
#[inline]
fn shape_hash(head_key: u64, len: usize) -> u64 {
    let mut buf = [0u8; 16];
    buf[..8].copy_from_slice(&head_key.to_be_bytes());
    buf[8..].copy_from_slice(&(len as u64).to_be_bytes());
    xxh64(&buf, SHAPE_SEED)
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
    /// Per-seat SHAPE class word (the phase-0 upgrade — Schulz's
    /// IJCAR-2012 sample-class distinction ported to seat grain).  The
    /// plain `mask` collapses TWO different wildcard sources into one
    /// bit: a bare variable at the seat (Schulz's class **A** — unifies
    /// with anything) and a variable-CONTAINING compound with a rigid
    /// head (what Schulz would sample as the rigid head symbol class at
    /// this position, the variable surfacing only at deeper positions).
    /// The latter is NOT a wildcard for either retrieval relation:
    /// `f(X)` can never unify with `g(a)`, and can never one-way match
    /// a bare-variable target.  `seat_shapes[i]` restores the
    /// distinction:
    ///
    ///   * `0` — true wildcard: bare variable, variable-headed /
    ///     compound-headed compound, or seat ≥ [`MAX_SEATS`];
    ///   * compound with a concrete (symbol/operator) head, ground or
    ///     open — `fold_shape(shape_hash(head key, len))`;
    ///   * ground leaf, or ground compound with a shapeless head —
    ///     `fold_shape(coin(seat, content key))` (its exact-content
    ///     class; the coin and shape streams use disjoint seeds, so a
    ///     leaf class never equals a (head, len) class except by a
    ///     filter-weakening 2⁻³² accident).
    ///
    /// Consumed by [`Self::seats_match_onto`] /
    /// [`Self::seats_unifiable_with`] — necessary-condition prefilters
    /// in front of unchanged exact checks (one-way match/unification).
    pub(crate) seat_shapes: SmallVec<[u32; 8]>,
    /// This atom's OWN (head, len) shape hash — `shape_hash(head key,
    /// arity)` when the head seat is a concrete symbol/operator, else
    /// `0`.  A parent's compound seat derives its shape word from the
    /// child's `self_shape` (one field read, no re-resolve).
    pub(crate) self_shape: u64,
}

impl AtomInfo {
    #[inline]
    pub(crate) fn is_ground(&self) -> bool { self.mask == 0 }

    /// MATCHING-direction seat filter: can `self` (the PATTERN — its
    /// variables may bind) possibly one-way match `other` (the
    /// candidate instance)?  NECESSARY condition for `selfσ = other`:
    /// a pattern seat's non-wildcard shape survives every σ (ground
    /// seats are untouched; a concrete-headed open compound keeps its
    /// head and length under substitution), so the instance seat must
    /// carry the IDENTICAL shape word — in particular a bare-variable
    /// instance seat (word 0) refutes any rigid pattern seat, which
    /// the mask alone cannot see.  `false` is a sound rejection;
    /// `true` says nothing (the exact matcher verifies).
    #[inline]
    pub(crate) fn seats_match_onto(&self, other: &AtomInfo) -> bool {
        let n = self.seat_shapes.len().min(other.seat_shapes.len());
        self.seat_shapes[..n]
            .iter()
            .zip(&other.seat_shapes[..n])
            .all(|(&a, &b)| a == 0 || a == b)
    }

    /// UNIFIABILITY-direction seat filter (variables wildcard on BOTH
    /// sides): two rigid seat classes are compatible only when equal —
    /// `f(X)` vs `g(a)` is an algebraic refutation the union-mask
    /// residue never sees (both seats vanish under the union).
    /// NECESSARY: unifiable seats are either wildcard on a side or
    /// agree on (head, len) / exact leaf content, and equal true
    /// shapes always fold equally.  `false` soundly rejects.
    #[inline]
    pub(crate) fn seats_unifiable_with(&self, other: &AtomInfo) -> bool {
        let n = self.seat_shapes.len().min(other.seat_shapes.len());
        self.seat_shapes[..n]
            .iter()
            .zip(&other.seat_shapes[..n])
            .all(|(&a, &b)| a == 0 || b == 0 || a == b)
    }

    /// [`Self::seats_unifiable_with`] with the ARGUMENT-SWAP tolerance
    /// the resolution path requires: `resolve` retries a failed
    /// unification with the given literal's arguments swapped when its
    /// head is a known symmetric relation (`resolve_sym`), and a
    /// candidate can be direct-probe-retrieved yet unify ONLY swapped
    /// (e.g. `(r (f ?X) (g ?Y))` against `(r (g a) (f b))` — the
    /// union residue ignores both open compound seats).  Symmetry is
    /// oracle- and epoch-dependent, invisible at this depth, so every
    /// arity-3 atom gets the crossed comparison as well — the filter
    /// stays a necessary condition for "some resolve path exists".
    #[inline]
    pub(crate) fn seats_unifiable_mod_swap(&self, other: &AtomInfo) -> bool {
        if self.seats_unifiable_with(other) {
            return true;
        }
        if self.seat_shapes.len() == 3 && other.seat_shapes.len() == 3 {
            let c = |a: u32, b: u32| a == 0 || b == 0 || a == b;
            return c(self.seat_shapes[0], other.seat_shapes[0])
                && c(self.seat_shapes[1], other.seat_shapes[2])
                && c(self.seat_shapes[2], other.seat_shapes[1]);
        }
        false
    }

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
    /// The seat's OWN (head, len) shape hash when it is a compound
    /// with a concrete head (`AtomInfo::self_shape` of the subterm) —
    /// `0` for leaves, variables, and shapeless compounds.  Feeds the
    /// enclosing atom's `seat_shapes`.
    head_shape: u64,
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
                leaf_sig: 0, seat_shapes: SmallVec::new(), self_shape: 0,
            };
        };
        let n = sent.elements.len();
        let mut mask = 0u64;
        let mut residue = arity_tag(n);
        let mut s3 = 0u64;
        let mut coins: SmallVec<[u64; 6]> = SmallVec::with_capacity(n);
        let mut shapes: SmallVec<[u32; 8]> = SmallVec::with_capacity(n);
        let mut depth = 0u8;
        let mut size = 0u16;
        let mut leaf_sig = 0u64;
        for (i, el) in sent.elements.iter().enumerate() {
            let m = self.seat_meta(el, atoms, syn);
            depth = depth.max(m.depth);
            size = size.saturating_add(m.size);
            leaf_sig |= m.leaf_sig;
            // Seat SHAPE word (see the field docs): concrete-headed
            // compounds fold their (head, len) shape; ground leaves /
            // shapeless ground compounds fold their content coin;
            // everything else is wildcard.
            shapes.push(if i >= MAX_SEATS {
                0
            } else if m.head_shape != 0 {
                fold_shape(m.head_shape)
            } else if m.ground {
                fold_shape(coin(i, m.key))
            } else {
                0
            });
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
            seat_shapes: shapes,
            self_shape: elements_self_shape(&sent.elements),
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
                ground: false, depth: 0, size: 1, key: 0, leaf_sig: 0, head_shape: 0,
            },
            Element::Symbol(s) => {
                let key = xxh64(&s.id().to_be_bytes(), u64::from(b'S'));
                SeatMeta { ground: true, depth: 0, size: 1, key, leaf_sig: leaf_bit(key), head_shape: 0 }
            }
            Element::Literal(Literal::Str(v)) => {
                let key = xxh64(v.as_bytes(), u64::from(b'T'));
                SeatMeta { ground: true, depth: 0, size: 1, key, leaf_sig: leaf_bit(key), head_shape: 0 }
            }
            Element::Literal(Literal::Number(v)) => {
                let key = xxh64(v.as_bytes(), u64::from(b'N'));
                SeatMeta { ground: true, depth: 0, size: 1, key, leaf_sig: leaf_bit(key), head_shape: 0 }
            }
            Element::Op(op) => SeatMeta {
                ground: true, depth: 0, size: 1,
                key: xxh64(&[op_byte(op)], u64::from(b'O')),
                leaf_sig: 0,
                head_shape: 0,
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
                    head_shape: info.self_shape,
                }
            }
        }
    }

    parked! {
        pub(crate) fn len(&self) -> usize { self.map.len() }
    }

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

/// TRANSIENT [`AtomInfo`] of a SLOT-form atom term, computed from the
/// tree alone — no `AtomTable` residency, no [`AtomInfos`] memo entry,
/// and (deliberately) no decode phone-book registration.  Field-for-
/// field equal to `AtomInfos::info(intern_slot_atom(t))` (property test
/// in `prover/make.rs`; live debug twin in `forward_subsumed`).
///
/// This is the hash-before-intern query side: a not-yet-accepted
/// clause's literals are probed against the index / feature-vector /
/// bloom channels with THIS info, and only an accepted clause interns
/// its atoms and warms the shared memos (the phone book then registers
/// its coins on the atom's first memoized compute — decode residual
/// coins are always partner-seat coins, and partners are accepted
/// clauses, so no decode ever depends on a dead clause's registration).
///
/// A non-`App` term is treated as the single-element sentence
/// `intern_atom` would wrap it into (canonicalized literal terms are
/// always `App`-wrapped, so this arm is defensive parity).
pub(crate) fn term_atom_info(t: &Term) -> AtomInfo {
    let one;
    let elems: &[Term] = match t {
        Term::App(e) => e,
        other => {
            one = [other.clone()];
            &one
        }
    };
    let n = elems.len();
    let mut mask = 0u64;
    let mut residue = arity_tag(n);
    let mut s3 = 0u64;
    let mut coins: SmallVec<[u64; 6]> = SmallVec::with_capacity(n);
    let mut shapes: SmallVec<[u32; 8]> = SmallVec::with_capacity(n);
    let mut depth = 0u8;
    let mut size = 0u16;
    let mut leaf_sig = 0u64;
    for (i, el) in elems.iter().enumerate() {
        let m = term_seat_meta(el);
        depth = depth.max(m.depth);
        size = size.saturating_add(m.size);
        leaf_sig |= m.leaf_sig;
        // Seat SHAPE word — the exact twin of `AtomInfos::compute`'s
        // derivation, seat by seat.
        shapes.push(if i >= MAX_SEATS {
            0
        } else if m.head_shape != 0 {
            fold_shape(m.head_shape)
        } else if m.ground {
            fold_shape(coin(i, m.key))
        } else {
            0
        });
        if i >= MAX_SEATS || !m.ground {
            if !m.ground && i < MAX_SEATS {
                mask |= 1u64 << i;
            }
            coins.push(0);
        } else {
            let c = coin(i, m.key);
            residue ^= c;
            s3 ^= crate::gf64::cube(c);
            coins.push(c);
            // NO `dict` entry — transient by design (see above).
        }
    }
    AtomInfo {
        arity: n.min(255) as u8,
        mask,
        base_residue: residue,
        seat_coins: coins,
        s3,
        depth: depth.saturating_add(1),
        size,
        leaf_sig,
        seat_shapes: shapes,
        self_shape: terms_self_shape(elems),
    }
}

/// [`AtomInfo::self_shape`] of an Element-form sentence: (head key,
/// len) under [`SHAPE_SEED`] for concrete symbol/operator heads, `0`
/// otherwise.  [`terms_self_shape`] is its byte-for-byte `Term` twin.
fn elements_self_shape(elements: &[Element]) -> u64 {
    match elements.first() {
        Some(Element::Symbol(s)) => {
            shape_hash(xxh64(&s.id().to_be_bytes(), u64::from(b'S')), elements.len())
        }
        Some(Element::Op(op)) => {
            shape_hash(xxh64(&[op_byte(op)], u64::from(b'O')), elements.len())
        }
        _ => 0,
    }
}

/// [`elements_self_shape`]'s slot-`Term` twin.
fn terms_self_shape(elems: &[Term]) -> u64 {
    match elems.first() {
        Some(Term::Sym(s)) => {
            shape_hash(xxh64(&s.id().to_be_bytes(), u64::from(b'S')), elems.len())
        }
        Some(Term::Op(op)) => {
            shape_hash(xxh64(&[op_byte(op)], u64::from(b'O')), elems.len())
        }
        _ => 0,
    }
}

/// [`AtomInfos::seat_meta`]'s slot-`Term` twin — same keys, same leaf
/// bits, same groundness/depth/size semantics, seat by seat.
fn term_seat_meta(el: &Term) -> SeatMeta {
    let leaf_bit = |key: u64| 1u64 << (key & 63);
    match el {
        Term::Var(_) => SeatMeta { ground: false, depth: 0, size: 1, key: 0, leaf_sig: 0, head_shape: 0 },
        Term::Sym(s) => {
            let key = xxh64(&s.id().to_be_bytes(), u64::from(b'S'));
            SeatMeta { ground: true, depth: 0, size: 1, key, leaf_sig: leaf_bit(key), head_shape: 0 }
        }
        Term::Lit(Literal::Str(v)) => {
            let key = xxh64(v.as_bytes(), u64::from(b'T'));
            SeatMeta { ground: true, depth: 0, size: 1, key, leaf_sig: leaf_bit(key), head_shape: 0 }
        }
        Term::Lit(Literal::Number(v)) => {
            let key = xxh64(v.as_bytes(), u64::from(b'N'));
            SeatMeta { ground: true, depth: 0, size: 1, key, leaf_sig: leaf_bit(key), head_shape: 0 }
        }
        Term::Op(op) => SeatMeta {
            ground: true,
            depth: 0,
            size: 1,
            key: xxh64(&[op_byte(op)], u64::from(b'O')),
            leaf_sig: 0,
            head_shape: 0,
        },
        Term::App(_) => {
            let info = term_atom_info(el);
            let ground = info.is_ground();
            SeatMeta {
                ground,
                depth: info.depth,
                size: info.size,
                // The subterm's identity IS its would-be content hash —
                // the id `Element::Sub` would carry after interning.
                // Only meaningful (and only computed) when ground.
                key: if ground { super::super::clause::slot_atom_content_id(el) } else { 0 },
                leaf_sig: info.leaf_sig,
                head_shape: info.self_shape,
            }
        }
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
