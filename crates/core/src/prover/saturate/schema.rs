// crates/core/src/saturate/schema.rs
//
// The SCHEMA CHANNEL: algebraic recognition of theory-rule clause
// shapes (symmetry, transitivity, antisymmetry, irreflexivity, inverse
// pairs, and the second-order metaschemas that generate them).
//
// A clause's *schema fingerprint* is a GF(2^64) power-sum sketch
// ⟨S₁, S₃⟩ over per-literal coins, with the head symbol factored
// MULTIPLICATIVELY: coin = k_lit ⊗ H(head), where k_lit hashes only the
// literal's skeleton (polarity, arity, canonical variable slots, fixed
// symbols in argument seats).  For a clause whose symbol heads are all
// one relation R, dividing the symbol-headed partial sums by H(R)
// cancels R entirely:
//
//     S₁ = (⊕ᵢ kᵢ) ⊗ H(R)   ⇒   S₁ ⊗ H(R)⁻¹ = ⊕ᵢ kᵢ   (a CONSTANT)
//     S₃ = (⊕ᵢ kᵢ³) ⊗ H(R)³ ⇒   S₃ ⊗ H(R)⁻³ = ⊕ᵢ kᵢ³  (its checksum)
//
// so every symmetry rule — for ANY relation — collapses to the same
// precomputed key, probed in one Map64 lookup.  Equality- and
// variable-headed literals (the metaschema family) carry fixed head
// factors and ride in a separate fixed partial sum that needs no
// normalization.  Hash equality is necessary, never sufficient: every
// probe hit runs a structural verifier before anything acts on it —
// the same probe-superset-then-verify discipline as the residue index.
//
// The table is built GENERATIVELY: each pattern's source literals are
// pushed through the real `canonical_clause`, and the fingerprint of
// whatever comes out is what gets registered.  The recognizer can never
// drift from the canonicalizer, because the canonicalizer defines it.

use smallvec::SmallVec;

use xxhash_rust::xxh64::xxh64;

use crate::gf64;
use crate::parse::OpKind;
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, Symbol, SymbolId};

use super::canon::{canonical_clause, canonical_slot};
use super::clause::{AtomTable, PLit, Term};
use super::hash64::Map64;

/// Seed for literal-skeleton coins — its own keyspace, disjoint from
/// sentence hashes, clause keys, and residue-index coins.
const SCHEMA_SEED: u64 = 0x5C_E3_A5_C4_E3_A5_C4_E3;
/// Seed for head-symbol factors H(sym).
const HEAD_SEED: u64 = SCHEMA_SEED ^ 0x4EAD;

/// Multiplicative head factor for a symbol head.  Forced odd so it is
/// never zero (zero is not invertible in GF(2^64)); costs one bit of
/// uniformity, which the structural verify makes irrelevant.
fn h_sym(id: SymbolId) -> u64 {
    xxh64(&id.to_be_bytes(), HEAD_SEED) | 1
}

/// Fixed head factors for non-symbol heads.
const H_EQ: u64 = 0xE9_0A_11_7E_9A_11_7E_01;
const H_VAR: u64 = 0x7A_4E_AD_5C_0F_FE_E0_11;

/// What the recognizer found.  `rel` is the schema's relation (for the
/// metaschemas — which quantify over the relation — it is `None`);
/// `rel2` is the second relation of an inverse pair.
#[derive(Debug, Clone)]
pub(crate) struct SchemaHit {
    pub(crate) kind: SchemaKind,
    pub(crate) rel:  Option<Symbol>,
    pub(crate) rel2: Option<Symbol>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchemaKind {
    /// `¬R(x,y) ∨ R(y,x)` — rule-stated symmetry.
    Symmetry,
    /// `¬R(x,y) ∨ ¬R(y,z) ∨ R(x,z)` — rule-stated transitivity.
    Transitivity,
    /// `¬R(x,y) ∨ ¬R(y,x) ∨ x=y` — rule-stated antisymmetry.
    Antisymmetry,
    /// `¬R(x,x)` — rule-stated irreflexivity.
    Irreflexivity,
    /// `¬R1(x,y) ∨ R2(y,x)` — one direction of an inverse pair.
    Inverse,
    /// `¬(instance ?r SymmetricRelation) ∨ ¬?r(x,y) ∨ ?r(y,x)` —
    /// SUMO's second-order symmetry schema.
    SymMetaschema,
    /// `¬(x=y) ∨ ¬R(..x..) ∨ R(..y..)` — a Leibniz
    /// substitution-of-equals schema (Merge states these for
    /// `instance` and `property`, both argument positions, both
    /// directions).  Its open equality literal resolves against EVERY
    /// equality unit the run derives — the dominant flood in
    /// equality-heavy problems — while paramodulation and the
    /// ground-equality congruence closure already provide the
    /// substitution behavior.
    EqSubstitution,
    /// The 4-literal TransitiveRelation analogue (recognized for
    /// statistics; NOT absorbed — see prover.rs for why).
    TransMetaschema,
}

/// One literal's shape, the recognizer's working form.  Extracted from
/// canonical-clause atoms (runtime) or source `Term`s (table build);
/// the fingerprint and verifiers see only this.
#[derive(Debug, Clone)]
struct LitShape {
    pos:  bool,
    head: HeadK,
    args: SmallVec<[ArgK; 2]>,
}

#[derive(Debug, Clone, PartialEq)]
enum HeadK {
    Sym(Symbol),
    Eq,
    /// Variable head (the second-order door), by canonical slot.
    Var(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ArgK {
    /// Canonical variable, by slot.
    Slot(u32),
    /// Fixed symbol (e.g. `SymmetricRelation` in a metaschema guard).
    Sym(SymbolId),
}

/// The precomputed pattern table.  Lives on the `ProverLayer`, built
/// once per layer.
#[derive(Debug, Default)]
pub(crate) struct SchemaTable {
    map: Map64<u64, SchemaKind>,
}

// -- shape extraction ---------------------------------------------------------------

/// Shape of one canonical-clause literal, or `None` when the literal
/// does not fit the schema family (compound/literal arguments, arity
/// other than 3, non-canonical variables).
fn shape_of_sentence(
    pos:   bool,
    atom:  super::clause::AtomId,
    atoms: &AtomTable,
    syn:   &SyntacticLayer,
) -> Option<LitShape> {
    let sent = atoms.resolve(atom, syn)?;
    if sent.elements.len() != 3 {
        return None;
    }
    let head = match sent.elements.first()? {
        Element::Symbol(s) => HeadK::Sym(s.0.clone()),
        Element::Op(OpKind::Equal) => HeadK::Eq,
        Element::Variable { id, .. } => HeadK::Var(canonical_slot(*id)?),
        _ => return None,
    };
    let mut args: SmallVec<[ArgK; 2]> = SmallVec::new();
    for el in &sent.elements[1..] {
        args.push(match el {
            Element::Variable { id, .. } => ArgK::Slot(canonical_slot(*id)?),
            Element::Symbol(s) => ArgK::Sym(s.id()),
            _ => return None,
        });
    }
    Some(LitShape { pos, head, args })
}

/// [`shape_of_sentence`]'s slot-term twin: the same shape, read off the
/// SLOT-form literal term (canonical variables already carry their slot
/// numbers there, so `canonical_slot` is the identity map).  `None` for
/// exactly the literals the sentence reader rejects — wrong arity,
/// compound or literal-valued arguments, non-`Equal` operator heads.
fn shape_of_term(pos: bool, t: &Term) -> Option<LitShape> {
    let Term::App(elems) = t else { return None };
    if elems.len() != 3 {
        return None;
    }
    let head = match &elems[0] {
        Term::Sym(s) => HeadK::Sym(s.clone()),
        Term::Op(OpKind::Equal) => HeadK::Eq,
        Term::Var(v) => HeadK::Var(u32::try_from(*v).ok()?),
        _ => return None,
    };
    let mut args: SmallVec<[ArgK; 2]> = SmallVec::new();
    for el in &elems[1..] {
        args.push(match el {
            Term::Var(v) => ArgK::Slot(u32::try_from(*v).ok()?),
            Term::Sym(s) => ArgK::Sym(s.id()),
            _ => return None,
        });
    }
    Some(LitShape { pos, head, args })
}

// -- the fingerprint ----------------------------------------------------------------

/// Skeleton coin for one literal: everything EXCEPT the symbol-head
/// identity.  Fixed argument symbols (metaschema guard classes) are
/// part of the skeleton — `(instance ?r SymmetricRelation)` and
/// `(instance ?r TransitiveRelation)` must coin differently.
fn k_lit(s: &LitShape) -> u64 {
    let mut buf: SmallVec<[u8; 32]> = SmallVec::new();
    buf.push(s.pos as u8);
    match &s.head {
        HeadK::Sym(_) => buf.push(b'S'),
        HeadK::Eq => buf.push(b'E'),
        HeadK::Var(slot) => {
            buf.push(b'V');
            buf.extend_from_slice(&slot.to_be_bytes());
        }
    }
    for a in &s.args {
        match a {
            ArgK::Slot(k) => {
                buf.push(b'v');
                buf.extend_from_slice(&k.to_be_bytes());
            }
            ArgK::Sym(id) => {
                buf.push(b's');
                buf.extend_from_slice(&id.to_be_bytes());
            }
        }
    }
    xxh64(&buf, SCHEMA_SEED) | 1
}

/// The clause's schema fingerprint: the four partial-sum words plus
/// the distinct symbol heads encountered.  `key()` collapses the words
/// into the table-probe key after normalizing the symbol-headed sums
/// by the (single) relation's head factor.
struct Fingerprint {
    /// ⟨S₁, S₃⟩ over symbol-headed literal coins.
    sym1: u64,
    sym3: u64,
    /// ⟨S₁, S₃⟩ over eq-/var-headed literal coins (fixed head factors —
    /// already relation-independent, no normalization needed).
    fix1: u64,
    fix3: u64,
    heads: SmallVec<[Symbol; 2]>,
}

fn fingerprint(shapes: &[LitShape]) -> Fingerprint {
    let mut fp = Fingerprint {
        sym1: 0, sym3: 0, fix1: 0, fix3: 0,
        heads: SmallVec::new(),
    };
    for s in shapes {
        let k = k_lit(s);
        match &s.head {
            HeadK::Sym(sym) => {
                let c = gf64::mul(k, h_sym(sym.id()));
                fp.sym1 ^= c;
                fp.sym3 ^= gf64::cube(c);
                if !fp.heads.iter().any(|h| h.id() == sym.id()) {
                    fp.heads.push(sym.clone());
                }
            }
            HeadK::Eq => {
                let c = gf64::mul(k, H_EQ);
                fp.fix1 ^= c;
                fp.fix3 ^= gf64::cube(c);
            }
            HeadK::Var(_) => {
                let c = gf64::mul(k, H_VAR);
                fp.fix1 ^= c;
                fp.fix3 ^= gf64::cube(c);
            }
        }
    }
    fp
}

impl Fingerprint {
    /// The table key: symbol sums divided by the single head factor
    /// (relation cancels; the S₃ word rides along as a built-in
    /// checksum), mixed with the fixed sums.  `None` unless exactly
    /// one distinct symbol head is present.
    fn key(&self) -> Option<u64> {
        if self.heads.len() != 1 {
            return None;
        }
        let ih = gf64::inv(h_sym(self.heads[0].id()));
        let n1 = gf64::mul(self.sym1, ih);
        let n3 = gf64::mul(self.sym3, gf64::cube(ih));
        let mut buf = [0u8; 32];
        buf[..8].copy_from_slice(&n1.to_be_bytes());
        buf[8..16].copy_from_slice(&n3.to_be_bytes());
        buf[16..24].copy_from_slice(&self.fix1.to_be_bytes());
        buf[24..].copy_from_slice(&self.fix3.to_be_bytes());
        Some(xxh64(&buf, SCHEMA_SEED ^ 0x6E9))
    }
}

// -- structural verifiers -----------------------------------------------------------

/// Slot pair of a 2-slot-argument literal, or `None`.
fn two_slots(s: &LitShape) -> Option<(u32, u32)> {
    match (s.args.first()?, s.args.get(1)?) {
        (ArgK::Slot(a), ArgK::Slot(b)) => Some((*a, *b)),
        _ => None,
    }
}

fn head_sym(s: &LitShape) -> Option<&Symbol> {
    match &s.head { HeadK::Sym(sym) => Some(sym), _ => None }
}

/// Verify a probe hit structurally.  Hash equality routed us here;
/// this is the ground truth.
fn verify(kind: SchemaKind, shapes: &[LitShape]) -> bool {
    match kind {
        SchemaKind::Symmetry => {
            let [a, b] = shapes else { return false };
            let (neg, pos) = if a.pos { (b, a) } else { (a, b) };
            if neg.pos || !pos.pos { return false; }
            let (Some(hn), Some(hp)) = (head_sym(neg), head_sym(pos)) else { return false };
            if hn.id() != hp.id() { return false; }
            let (Some((x, y)), Some((u, v))) = (two_slots(neg), two_slots(pos)) else { return false };
            x != y && u == y && v == x
        }
        SchemaKind::Transitivity => {
            let [a, b, c] = shapes else { return false };
            let mut negs: SmallVec<[&LitShape; 2]> = SmallVec::new();
            let mut pos = None;
            for s in [a, b, c] {
                if s.pos { pos = Some(s); } else { negs.push(s); }
            }
            let (Some(p), [n1, n2]) = (pos, negs.as_slice()) else { return false };
            let heads: Option<Vec<&Symbol>> =
                [p, n1, n2].iter().map(|s| head_sym(s)).collect();
            let Some(heads) = heads else { return false };
            if heads[0].id() != heads[1].id() || heads[1].id() != heads[2].id() {
                return false;
            }
            let (Some((px, pz)), Some(s1), Some(s2)) =
                (two_slots(p), two_slots(n1), two_slots(n2)) else { return false };
            if px == pz { return false; }
            // ¬R(px,m) ∧ ¬R(m,pz) in either literal order, m fresh.
            let chains = |(ax, ay): (u32, u32), (bx, by): (u32, u32)| {
                ax == px && by == pz && ay == bx && ay != px && ay != pz
            };
            chains(s1, s2) || chains(s2, s1)
        }
        SchemaKind::Antisymmetry => {
            let [a, b, c] = shapes else { return false };
            let mut negs: SmallVec<[&LitShape; 2]> = SmallVec::new();
            let mut pos = None;
            for s in [a, b, c] {
                if s.pos { pos = Some(s); } else { negs.push(s); }
            }
            let (Some(p), [n1, n2]) = (pos, negs.as_slice()) else { return false };
            if !matches!(p.head, HeadK::Eq) { return false; }
            let (Some(h1), Some(h2)) = (head_sym(n1), head_sym(n2)) else { return false };
            if h1.id() != h2.id() { return false; }
            let (Some((ex, ey)), Some((x, y)), Some((u, v))) =
                (two_slots(p), two_slots(n1), two_slots(n2)) else { return false };
            x != y && u == y && v == x
                && ((ex == x && ey == y) || (ex == y && ey == x))
        }
        SchemaKind::Irreflexivity => {
            let [s] = shapes else { return false };
            if s.pos || head_sym(s).is_none() { return false; }
            matches!(two_slots(s), Some((x, y)) if x == y)
        }
        SchemaKind::Inverse => {
            let [a, b] = shapes else { return false };
            let (neg, pos) = if a.pos { (b, a) } else { (a, b) };
            if neg.pos || !pos.pos { return false; }
            let (Some(hn), Some(hp)) = (head_sym(neg), head_sym(pos)) else { return false };
            if hn.id() == hp.id() { return false; }
            let (Some((x, y)), Some((u, v))) = (two_slots(neg), two_slots(pos)) else { return false };
            x != y && u == y && v == x
        }
        SchemaKind::EqSubstitution => {
            let [a, b, c] = shapes else { return false };
            let mut eq = None;
            let mut neg_r = None;
            let mut pos_r = None;
            for s in [a, b, c] {
                match (&s.head, s.pos) {
                    (HeadK::Eq, false) if eq.is_none() => eq = Some(s),
                    (HeadK::Sym(_), false) if neg_r.is_none() => neg_r = Some(s),
                    (HeadK::Sym(_), true) if pos_r.is_none() => pos_r = Some(s),
                    _ => return false,
                }
            }
            let (Some(eq), Some(nr), Some(pr)) = (eq, neg_r, pos_r) else { return false };
            let Some((ea, eb)) = two_slots(eq) else { return false };
            if ea == eb { return false; }
            let (Some(hn), Some(hp)) = (head_sym(nr), head_sym(pr)) else { return false };
            if hn.id() != hp.id() || nr.args.len() != pr.args.len() {
                return false;
            }
            // Exactly one seat differs, and it swaps between the
            // equality's two variables; every other seat is the same
            // variable slot.
            let mut diff = None;
            for (i, (na, pa)) in nr.args.iter().zip(pr.args.iter()).enumerate() {
                match (na, pa) {
                    (ArgK::Slot(nv), ArgK::Slot(pv)) if nv == pv => {}
                    // A partially-instantiated derived copy: the
                    // unchanged seat may be the same ground symbol.
                    (ArgK::Sym(ns), ArgK::Sym(ps)) if ns == ps => {}
                    (ArgK::Slot(nv), ArgK::Slot(pv)) => {
                        if diff.replace((i, *nv, *pv)).is_some() {
                            return false; // more than one differing seat
                        }
                    }
                    _ => return false,
                }
            }
            matches!(diff, Some((_, nv, pv))
                if nv != pv && (nv == ea && pv == eb || nv == eb && pv == ea))
        }
        SchemaKind::SymMetaschema | SchemaKind::TransMetaschema => {
            // One guard ¬(instance ?r <Class>); the rest are ?r-headed
            // literals forming the symmetry (or transitivity) shape over
            // the SAME head slot.
            let class = Symbol::hash_name(match kind {
                SchemaKind::SymMetaschema => "SymmetricRelation",
                _ => "TransitiveRelation",
            });
            let instance = Symbol::hash_name("instance");
            let mut rel_slot = None;
            let mut body: SmallVec<[&LitShape; 3]> = SmallVec::new();
            for s in shapes {
                match &s.head {
                    HeadK::Sym(h) if h.id() == instance && !s.pos => {
                        let (Some(ArgK::Slot(r)), Some(ArgK::Sym(c))) =
                            (s.args.first(), s.args.get(1)) else { return false };
                        if *c != class || rel_slot.replace(*r).is_some() {
                            return false;
                        }
                    }
                    HeadK::Var(_) => body.push(s),
                    _ => return false,
                }
            }
            let Some(r) = rel_slot else { return false };
            if !body.iter().all(|s| matches!(s.head, HeadK::Var(v) if v == r)) {
                return false;
            }
            // Re-verify the body as the corresponding first-order shape
            // (head identity already established above).
            let dummy = Symbol::from("schema-body");
            let body_shapes: Vec<LitShape> = body
                .iter()
                .map(|s| LitShape {
                    pos: s.pos,
                    head: HeadK::Sym(dummy.clone()),
                    args: s.args.clone(),
                })
                .collect();
            let inner = match kind {
                SchemaKind::SymMetaschema => SchemaKind::Symmetry,
                _ => SchemaKind::Transitivity,
            };
            verify(inner, &body_shapes)
        }
    }
}

// -- the table ----------------------------------------------------------------------

impl SchemaTable {
    /// Build the pattern table by pushing each pattern's source
    /// literals through the REAL `canonical_clause` and fingerprinting
    /// what comes out — self-calibrating against the canonicalizer.
    /// Patterns whose canonical literal order is emission-dependent
    /// (blank-key ties between same-head negative literals) register
    /// one key per ordering.
    pub(crate) fn build(atoms: &AtomTable, syn: &SyntacticLayer) -> Self {
        let r = || Term::Sym(Symbol::from("schema-rel"));
        let v = |n: &str| Term::Var(Symbol::hash_name(n));
        let app = |elems: Vec<Term>| Term::App(elems);
        let (x, y, z, rv) = (|| v("?SX"), || v("?SY"), || v("?SZ"), || v("?SR"));
        let inst = || Term::Sym(Symbol::from("instance"));
        let symc = || Term::Sym(Symbol::from("SymmetricRelation"));
        let trac = || Term::Sym(Symbol::from("TransitiveRelation"));
        let eq = || Term::Op(OpKind::Equal);

        let mut variants: Vec<(SchemaKind, Vec<(bool, Term)>)> = vec![
            (SchemaKind::Symmetry, vec![
                (false, app(vec![r(), x(), y()])),
                (true,  app(vec![r(), y(), x()])),
            ]),
            (SchemaKind::Irreflexivity, vec![
                (false, app(vec![r(), x(), x()])),
            ]),
            (SchemaKind::SymMetaschema, vec![
                (false, app(vec![inst(), rv(), symc()])),
                (false, app(vec![rv(), x(), y()])),
                (true,  app(vec![rv(), y(), x()])),
            ]),
            // Leibniz substitution: both argument positions, both
            // substitution directions.
            (SchemaKind::EqSubstitution, vec![
                (false, app(vec![eq(), x(), y()])),
                (false, app(vec![r(), x(), z()])),
                (true,  app(vec![r(), y(), z()])),
            ]),
            (SchemaKind::EqSubstitution, vec![
                (false, app(vec![eq(), x(), y()])),
                (false, app(vec![r(), y(), z()])),
                (true,  app(vec![r(), x(), z()])),
            ]),
            (SchemaKind::EqSubstitution, vec![
                (false, app(vec![eq(), x(), y()])),
                (false, app(vec![r(), z(), x()])),
                (true,  app(vec![r(), z(), y()])),
            ]),
            (SchemaKind::EqSubstitution, vec![
                (false, app(vec![eq(), x(), y()])),
                (false, app(vec![r(), z(), y()])),
                (true,  app(vec![r(), z(), x()])),
            ]),
        ];
        // Emission-order variants: same-polarity same-head literals tie
        // on the blank key, so the stable sort preserves their input
        // order — every ordering is a distinct canonical form.
        for negs in [[0usize, 1], [1, 0]] {
            let tn = |i: usize| match i {
                0 => app(vec![r(), x(), y()]),
                _ => app(vec![r(), y(), z()]),
            };
            variants.push((SchemaKind::Transitivity, vec![
                (false, tn(negs[0])),
                (false, tn(negs[1])),
                (true,  app(vec![r(), x(), z()])),
            ]));
            let mn = |i: usize| match i {
                0 => app(vec![rv(), x(), y()]),
                _ => app(vec![rv(), y(), z()]),
            };
            variants.push((SchemaKind::TransMetaschema, vec![
                (false, app(vec![inst(), rv(), trac()])),
                (false, mn(negs[0])),
                (false, mn(negs[1])),
                (true,  app(vec![rv(), x(), z()])),
            ]));
            let an = |i: usize| match i {
                0 => app(vec![r(), x(), y()]),
                _ => app(vec![r(), y(), x()]),
            };
            for eq_args in [[0usize, 1], [1, 0]] {
                let ea = |i: usize| if i == 0 { x() } else { y() };
                variants.push((SchemaKind::Antisymmetry, vec![
                    (false, an(negs[0])),
                    (false, an(negs[1])),
                    (true,  app(vec![eq(), ea(eq_args[0]), ea(eq_args[1])])),
                ]));
            }
        }

        let mut map: Map64<u64, SchemaKind> = Map64::default();
        for (kind, lits) in variants {
            let pc = canonical_clause(lits, atoms);
            let shapes: Option<Vec<LitShape>> = pc
                .lits
                .iter()
                .map(|l| shape_of_sentence(l.pos, l.atom, atoms, syn))
                .collect();
            let Some(shapes) = shapes else {
                debug_assert!(false, "schema table: {kind:?} failed shape extraction");
                continue;
            };
            let Some(key) = fingerprint(&shapes).key() else {
                debug_assert!(false, "schema table: {kind:?} has no single-head key");
                continue;
            };
            debug_assert!(verify(kind, &shapes), "schema table: {kind:?} fails own verify");
            let prev = map.insert(key, kind);
            debug_assert!(
                prev.is_none() || prev == Some(kind),
                "schema table: key collision {prev:?} vs {kind:?}"
            );
        }
        Self { map }
    }

    /// Probe a canonical clause against the table.  Cheap gates first;
    /// the fingerprint runs only on clauses whose every literal fits
    /// the schema family; structural verify runs only on table hits.
    pub(crate) fn probe(
        &self,
        lits:  &[PLit],
        atoms: &AtomTable,
        syn:   &SyntacticLayer,
    ) -> Option<SchemaHit> {
        if lits.is_empty() || lits.len() > 4 {
            return None;
        }
        let mut shapes: SmallVec<[LitShape; 4]> = SmallVec::new();
        for l in lits {
            shapes.push(shape_of_sentence(l.pos, l.atom, atoms, syn)?);
        }
        self.probe_shapes(shapes)
    }

    /// [`Self::probe`] fed from SLOT-form literal terms instead of
    /// resident atom sentences (hash-before-intern: `make` probes
    /// clauses whose atoms may never be interned).  The shape reader
    /// was the only part that touched the table; same shapes, same
    /// verdicts (twin test below).
    pub(crate) fn probe_terms(
        &self,
        lits:  &[PLit],
        terms: &[(bool, Term)],
    ) -> Option<SchemaHit> {
        if lits.is_empty() || lits.len() > 4 {
            return None;
        }
        let mut shapes: SmallVec<[LitShape; 4]> = SmallVec::new();
        for (l, (_, t)) in lits.iter().zip(terms) {
            shapes.push(shape_of_term(l.pos, t)?);
        }
        self.probe_shapes(shapes)
    }

    /// The shared probe tail: fingerprint → table hit → verify.
    fn probe_shapes(&self, shapes: SmallVec<[LitShape; 4]>) -> Option<SchemaHit> {
        let fp = fingerprint(&shapes);

        // Two distinct symbol heads on a 2-literal clause: the inverse
        // pair — no relation cancels, so it is verified directly.
        if fp.heads.len() == 2 && shapes.len() == 2 {
            if verify(SchemaKind::Inverse, &shapes) {
                let (neg, pos) = if shapes[0].pos {
                    (&shapes[1], &shapes[0])
                } else {
                    (&shapes[0], &shapes[1])
                };
                return Some(SchemaHit {
                    kind: SchemaKind::Inverse,
                    rel:  head_sym(neg).cloned(),
                    rel2: head_sym(pos).cloned(),
                });
            }
            return None;
        }

        let kind = *self.map.get(&fp.key()?)?;
        if !verify(kind, &shapes) {
            return None;
        }
        let rel = match kind {
            SchemaKind::SymMetaschema | SchemaKind::TransMetaschema => None,
            _ => Some(fp.heads[0].clone()),
        };
        Some(SchemaHit { kind, rel, rel2: None })
    }

    pub(crate) fn len(&self) -> usize { self.map.len() }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::kif_layer;

    use super::super::ProverLayer;
    use super::*;

    /// Probe the single clause of a single-root KIF fixture.
    fn probe_kif(kif: &str) -> Option<SchemaHit> {
        let layer = ProverLayer::new(kif_layer(kif));
        let roots = layer.semantic.syntactic.file_root_sids("base");
        assert_eq!(roots.len(), 1, "fixture must hold exactly one root");
        let cls = layer.clauses_for(roots[0]);
        if cls.is_empty() {
            return None; // tautology — dropped by the clausifier
        }
        assert_eq!(cls.len(), 1, "fixture must clausify to one clause");
        layer
            .schema
            .probe(&cls[0].lits, &layer.atoms, &layer.semantic.syntactic)
    }

    fn rel_name(hit: &SchemaHit) -> String {
        hit.rel.as_ref().expect("hit carries a relation").name().to_string()
    }

    // Hash-before-intern twin: the term-shape probe (`probe_terms`, fed
    // from slot-form terms) must agree with the sentence-shape probe on
    // every fixture in this module's family — hits AND misses, kind and
    // relation identity included.
    #[test]
    fn probe_terms_agrees_with_sentence_probe_on_the_fixture_family() {
        use super::super::unify::slot_atom;
        let fixtures = [
            "(=> (connectedTo ?A ?B) (connectedTo ?B ?A))",
            "(=> (and (above ?X ?Y) (above ?Y ?Z)) (above ?X ?Z))",
            "(=> (and (above ?Y ?Z) (above ?X ?Y)) (above ?X ?Z))",
            "(=> (and (larger ?A ?B) (larger ?B ?A)) (equal ?A ?B))",
            "(=> (before ?A ?B) (not (before ?B ?A)))",
            "(not (properPart ?X ?X))",
            "(=> (husband ?H ?W) (wife ?W ?H))",
            "(=> (instance ?R SymmetricRelation) (=> (?R ?A ?B) (?R ?B ?A)))",
            "(=> (and (instance ?R TransitiveRelation) (?R ?X ?Y) (?R ?Y ?Z)) (?R ?X ?Z))",
            "(=> (and (equal ?A ?B) (parent ?A ?C)) (parent ?B ?C))",
            // Near-misses / non-schema shapes must MISS through both readers.
            "(=> (parent ?A ?B) (ancestor ?A ?B))",
            "(instance dummy Entity)",
            "(=> (and (larger ?A ?B) (larger ?B ?C)) (equal ?A ?C))",
        ];
        for kif in fixtures {
            let layer = ProverLayer::new(kif_layer(kif));
            let roots = layer.semantic.syntactic.file_root_sids("base");
            assert_eq!(roots.len(), 1, "fixture must hold exactly one root: {kif}");
            let cls = layer.clauses_for(roots[0]);
            for pc in cls.iter() {
                let syn = &layer.semantic.syntactic;
                let by_sentence = layer.schema.probe(&pc.lits, &layer.atoms, syn);
                let terms: Vec<(bool, Term)> = pc
                    .lits
                    .iter()
                    .map(|l| (l.pos, slot_atom(&layer.atoms, syn, l.atom, 0).expect("liftable")))
                    .collect();
                let by_terms = layer.schema.probe_terms(&pc.lits, &terms);
                match (&by_sentence, &by_terms) {
                    (None, None) => {}
                    (Some(a), Some(b)) => {
                        assert_eq!(a.kind, b.kind, "kind diverged for {kif}");
                        assert_eq!(
                            a.rel.as_ref().map(|s| s.id()),
                            b.rel.as_ref().map(|s| s.id()),
                            "rel diverged for {kif}",
                        );
                        assert_eq!(
                            a.rel2.as_ref().map(|s| s.id()),
                            b.rel2.as_ref().map(|s| s.id()),
                            "rel2 diverged for {kif}",
                        );
                    }
                    _ => panic!("probe divergence for {kif}: {by_sentence:?} vs {by_terms:?}"),
                }
            }
        }
    }

    #[test]
    fn table_builds_all_patterns() {
        let layer = ProverLayer::new(kif_layer("(instance dummy Entity)"));
        // symmetry + irrefl + sym-metaschema + 2×transitivity +
        // 2×trans-metaschema + antisymmetry (4 emission variants
        // collapsing pairwise onto 2 canonical forms) +
        // 4×eq-substitution = 13 keys.
        assert_eq!(layer.schema.len(), 13);
    }

    #[test]
    fn recognizes_symmetry_rule() {
        let hit = probe_kif("(=> (connectedTo ?A ?B) (connectedTo ?B ?A))").unwrap();
        assert_eq!(hit.kind, SchemaKind::Symmetry);
        assert_eq!(rel_name(&hit), "connectedTo");
    }

    #[test]
    fn recognizes_transitivity_both_emission_orders() {
        for kif in [
            "(=> (and (above ?X ?Y) (above ?Y ?Z)) (above ?X ?Z))",
            "(=> (and (above ?Y ?Z) (above ?X ?Y)) (above ?X ?Z))",
        ] {
            let hit = probe_kif(kif).unwrap_or_else(|| panic!("no hit for {kif}"));
            assert_eq!(hit.kind, SchemaKind::Transitivity, "{kif}");
            assert_eq!(rel_name(&hit), "above");
        }
    }

    #[test]
    fn recognizes_antisymmetry() {
        let hit = probe_kif(
            "(=> (and (covers ?X ?Y) (covers ?Y ?X)) (equal ?X ?Y))",
        ).unwrap();
        assert_eq!(hit.kind, SchemaKind::Antisymmetry);
        assert_eq!(rel_name(&hit), "covers");
    }

    #[test]
    fn recognizes_irreflexivity() {
        let hit = probe_kif("(not (properPartX ?X ?X))").unwrap();
        assert_eq!(hit.kind, SchemaKind::Irreflexivity);
        assert_eq!(rel_name(&hit), "properPartX");
    }

    #[test]
    fn recognizes_inverse_pair() {
        let hit = probe_kif("(=> (smallerThanX ?X ?Y) (largerThanX ?Y ?X))").unwrap();
        assert_eq!(hit.kind, SchemaKind::Inverse);
        assert_eq!(rel_name(&hit), "smallerThanX");
        assert_eq!(&*hit.rel2.as_ref().unwrap().name(), "largerThanX");
    }

    #[test]
    fn recognizes_symmetry_metaschema() {
        let hit = probe_kif(
            "(=> (and (instance ?REL SymmetricRelation) (?REL ?I1 ?I2)) (?REL ?I2 ?I1))",
        ).unwrap();
        assert_eq!(hit.kind, SchemaKind::SymMetaschema);
        assert!(hit.rel.is_none());
    }

    #[test]
    fn recognizes_real_merge_symmetry_metaschema() {
        // Merge.kif:2377, verbatim shape (nested forall, no `and`).
        let hit = probe_kif(
            "(=> (instance ?REL SymmetricRelation) \
                 (forall (?INST1 ?INST2) \
                    (=> (?REL ?INST1 ?INST2) (?REL ?INST2 ?INST1))))",
        ).unwrap();
        assert_eq!(hit.kind, SchemaKind::SymMetaschema);
    }

    #[test]
    fn recognizes_transitivity_metaschema() {
        let hit = probe_kif(
            "(=> (and (instance ?REL TransitiveRelation) (?REL ?I1 ?I2) (?REL ?I2 ?I3)) \
                 (?REL ?I1 ?I3))",
        ).unwrap();
        assert_eq!(hit.kind, SchemaKind::TransMetaschema);
    }

    #[test]
    fn recognizes_eq_substitution_schemas() {
        // Merge.kif:254-280, verbatim shapes.  Each <=> splits into two
        // clauses; both directions of both schemas must hit.
        let layer = ProverLayer::new(kif_layer(
            "(=> (equal ?THING1 ?THING2) \
                 (forall (?CLASS) \
                    (<=> (instance ?THING1 ?CLASS) (instance ?THING2 ?CLASS))))",
        ));
        let roots = layer.semantic.syntactic.file_root_sids("base");
        assert_eq!(roots.len(), 1);
        let cls = layer.clauses_for(roots[0]);
        assert_eq!(cls.len(), 2, "<=> must split into two clauses");
        for pc in cls.iter() {
            let hit = layer
                .schema
                .probe(&pc.lits, &layer.atoms, &layer.semantic.syntactic)
                .expect("eq-substitution clause must hit");
            assert_eq!(hit.kind, SchemaKind::EqSubstitution);
        }
        // Second argument position (the property-attribute spelling).
        let hit = probe_kif(
            "(=> (and (equal ?A1 ?A2) (property ?T ?A1)) (property ?T ?A2))",
        ).unwrap();
        assert_eq!(hit.kind, SchemaKind::EqSubstitution);
    }

    #[test]
    fn rejects_near_misses() {
        // Guarded symmetry is NOT unconditional symmetry.
        assert!(probe_kif(
            "(=> (and (instance ?X Human) (likes ?X ?Y)) (likes ?Y ?X))"
        ).is_none());
        // Subrelation shape: same argument order, different heads.
        assert!(probe_kif("(=> (relA ?X ?Y) (relB ?X ?Y))").is_none());
        // Reflexive-ish tautology shape: same head, same order.
        assert!(probe_kif("(=> (relC ?X ?Y) (relC ?X ?Y))").is_none());
        // Ternary relation: arity outside the schema family.
        assert!(probe_kif("(=> (relD ?X ?Y ?Z) (relD ?Y ?X ?Z))").is_none());
    }
}
