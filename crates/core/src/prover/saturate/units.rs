// crates/core/src/saturate/units.rs
//
// Active unit-clause stores (prototype Prover state): the O(1)
// subsumption/simplification machinery the `make` step rides.
//
// - `ground`: (polarity, AtomId) -> clause id.  Content addressing makes
//   ground-unit lookup literally one hash probe — same-polarity hit
//   subsumes the new clause; opposite-polarity hit deletes the literal
//   (unit simplification), citing the unit as a parent.
// - `open`:   (polarity, head key, arity) -> non-ground unit atoms.
//   Checked by one-way match (the unit is the pattern), so `(p ?X)`
//   subsumes `(p a)` in any derived clause.
// - `equals`: active unit `(equal l r)` clauses, both orientations —
//   the paramodulation source set.

use std::sync::Arc;

use crate::syntactic::SyntacticLayer;
use crate::types::Element;

use super::clause::{AtomId, AtomTable, Term};
use super::AtomInfos;
use super::hash64::Map64;
use super::unify::{match_one_way, slot_atom, term_slots, Subst};

/// Key of an atom's head seat, for the open-unit buckets.  `None` when
/// the head is a variable (predicate-variable units are not bucketed —
/// they would match everything; the prototype skips them identically).
fn head_key(atoms: &AtomTable, syn: &SyntacticLayer, atom: AtomId) -> Option<(u64, u8)> {
    let s = atoms.resolve(atom, syn)?;
    let arity = s.elements.len().min(255) as u8;
    match s.elements.first()? {
        Element::Symbol(sym) => Some((sym.id(), arity)),
        Element::Op(op)      => Some((u64::from(op_tag(op)), arity)),
        _ => None,
    }
}

pub(crate) fn op_tag(op: &crate::parse::OpKind) -> u8 {
    use crate::parse::OpKind::*;
    match op {
        And => b'a', Or => b'o', Not => b'n', Implies => b'i',
        Iff => b'f', Equal => b'e', ForAll => b'A', Exists => b'E',
    }
}

/// What a unit lookup found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitHit {
    /// A same-polarity unit covers the literal — the clause is subsumed.
    Subsumes(u32),
    /// An opposite-polarity unit refutes the literal — drop it, citing
    /// the unit clause as a parent.
    Refutes(u32),
}

/// One open (non-ground) unit in its bucket, with the pattern term
/// lifted ONCE at registration — `make`'s simplification loop matches
/// against these patterns hundreds of thousands of times per run, and
/// re-lifting per attempt (slot_atom → term_of → reslot) was a
/// measured profile hotspot.
#[derive(Debug, Clone)]
pub(crate) struct OpenUnit {
    pub(crate) atom:    AtomId,
    pub(crate) clause:  u32,
    pub(crate) pattern: Arc<Term>,
    /// Slot-variable count of the owning clause — the caller's scratch
    /// substitution only needs slots `0..=nvars` cleared per attempt.
    pub(crate) nvars:   u32,
}

/// How one target seat participates in the open-unit residue lookup.
#[derive(Debug, Clone, Copy)]
pub(crate) enum SeatK {
    /// A variable: a pattern with a constant here can never match.
    Var,
    /// A compound, with its SHAPE coin (head symbol + length) — `None`
    /// when the compound is variable-headed.  Groups whose mask leaves
    /// this seat closed fall back to a scan; groups that SHAPE this
    /// seat key on the shape coin.
    Compound(Option<u64>),
    /// A leaf with its coin.
    Leaf(u64),
}

/// The shape coin of a compound seat: seat index + the compound's head
/// key + its length.  One-way matching demands the pattern's and
/// target's compound seats agree on exactly this triple before any
/// recursion, so equal shape coins are a NECESSARY condition (and
/// different ones an algebraic refutation).  `None` for var-headed
/// compounds (shapeless — a var head still only matches another
/// concrete head equal to itself one-way, but patterns don't shape it).
pub(crate) fn seat_shape_coin(seat: usize, t: &Term) -> Option<u64> {
    let Term::App(elems) = t else { return None };
    let head_key = match elems.first()? {
        Term::Sym(s) => xxhash_rust::xxh64::xxh64(&s.id().to_be_bytes(), u64::from(b'S')),
        Term::Op(op) => xxhash_rust::xxh64::xxh64(&[op_tag(op)], u64::from(b'O')),
        _ => return None,
    };
    let mut buf = [0u8; 20];
    buf[..4].copy_from_slice(&(seat as u32).to_be_bytes());
    buf[4..12].copy_from_slice(&head_key.to_be_bytes());
    buf[12..20].copy_from_slice(&(elems.len() as u64).to_be_bytes());
    Some(xxhash_rust::xxh64::xxh64(&buf, u64::from(b'P')))
}

#[derive(Debug, Default, Clone)]
pub(crate) struct UnitStores {
    /// (polarity, atom) → owning unit clause id.  Ground atoms only.
    ground: Map64<(bool, AtomId), u32>,
    /// (polarity, head key, arity) → (pattern mask, shaped-seat mask) →
    /// (pattern residue ⊕ shape coins) → open units.  THE KEY EQUATION
    /// as an index, extended with SHAPE coins: a masked seat holding a
    /// concrete-headed compound (the skolem shape `(parent ?X (sk ?X))`
    /// that dominates these buckets) contributes a (head,len) coin, so
    /// targets with a leaf or a different function there never see the
    /// pattern.  Without shapes the all-open group was reached by every
    /// target — 10M+ dead match walks per run.
    open: Map64<(bool, u64, u8), Map64<(u64, u64), Map64<u64, Vec<OpenUnit>>>>,
    /// Active positive `(equal l r)` units as slot-form term pairs,
    /// both orientations (l→r and r→l), with the owning clause id.
    pub(crate) equals: Vec<(u32, Term, Term)>,
}

impl UnitStores {
    /// Register an *activated* unit clause's single literal.  `nvars`
    /// is the owning clause's slot-variable count.
    pub(crate) fn add_unit(
        &mut self,
        clause_id: u32,
        pos:       bool,
        atom:      AtomId,
        nvars:     u32,
        infos:     &AtomInfos,
        atoms:     &AtomTable,
        syn:       &SyntacticLayer,
    ) {
        let info = infos.info(atom, atoms, syn);
        if info.is_ground() {
            self.ground.entry((pos, atom)).or_insert(clause_id);
        } else if let Some((h, ar)) = head_key(atoms, syn, atom) {
            if let Some(pattern) = slot_atom(atoms, syn, atom, 0) {
                // Shape coins for masked compound seats with a concrete head.
                let mut shaped = 0u64;
                let mut key = info.base_residue;
                if let Term::App(elems) = &pattern {
                    for (i, el) in elems.iter().enumerate() {
                        if i < 64 && (info.mask >> i) & 1 == 1 {
                            if let Some(c) = seat_shape_coin(i, el) {
                                shaped |= 1 << i;
                                key ^= c;
                            }
                        }
                    }
                }
                self.open
                    .entry((pos, h, ar))
                    .or_default()
                    .entry((info.mask, shaped))
                    .or_default()
                    .entry(key)
                    .or_default()
                    .push(OpenUnit {
                        atom, clause: clause_id, pattern: Arc::new(pattern), nvars,
                    });
            }
        }
        // Equality units feed paramodulation in both orientations.
        if pos {
            if let Some(s) = atoms.resolve(atom, syn) {
                if s.elements.len() == 3
                    && matches!(s.elements.first(),
                        Some(Element::Op(crate::parse::OpKind::Equal)))
                {
                    if let Some(Term::App(elems)) = slot_atom(atoms, syn, atom, 0) {
                        let l = elems[1].clone();
                        let r = elems[2].clone();
                        if l != r {
                            self.equals.push((clause_id, l.clone(), r.clone()));
                            self.equals.push((clause_id, r, l));
                        }
                    }
                }
            }
        }
    }

    /// The unit clause id holding exactly (pos, atom), if active.
    pub(crate) fn ground_unit(&self, pos: bool, atom: AtomId) -> Option<u32> {
        self.ground.get(&(pos, atom)).copied()
    }

    /// Open units that could one-way match a target with seat classes
    /// `seats` (cloned out — callers iterate while mutating other
    /// prover state; pattern terms are shared `Arc`s).  Per pattern-
    /// mask group: a target VARIABLE under a pattern-closed seat kills
    /// the whole group; leaf coins at the remaining closed seats form
    /// the residue key (one hash probe); a target COMPOUND under a
    /// closed seat degrades that group to a scan (match verifies).
    pub(crate) fn open_candidates(
        &self,
        pos:   bool,
        head:  u64,
        arity: u8,
        n_elems: usize,
        seats: &[SeatK],
    ) -> Vec<OpenUnit> {
        let Some(groups) = self.open.get(&(pos, head, arity)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        'group: for (&(mp, shaped), residues) in groups.iter() {
            let mut key = super::arity_tag(n_elems);
            let mut scan_all = false;
            for (i, sk) in seats.iter().enumerate() {
                if (mp >> i) & 1 == 1 {
                    // Pattern-open seat.  If the group SHAPES it, the
                    // target must be a compound with that exact
                    // (head, len) — anything else can't match.
                    if (shaped >> i) & 1 == 1 {
                        match sk {
                            SeatK::Compound(Some(c)) => key ^= c,
                            _ => continue 'group,
                        }
                    }
                    continue;
                }
                match sk {
                    SeatK::Var => continue 'group,
                    SeatK::Compound(_) => scan_all = true,
                    SeatK::Leaf(c) => key ^= c,
                }
            }
            if scan_all {
                for v in residues.values() { out.extend(v.iter().cloned()); }
            } else if let Some(v) = residues.get(&key) {
                out.extend(v.iter().cloned());
            }
        }
        out
    }

    /// Check a (canonical) literal against the active units: ground
    /// table first (one probe each way), then the head bucket of open
    /// units by one-way match.  `nvars_lit` is the number of canonical
    /// variables in the literal's own clause (slot-space sizing).
    pub(crate) fn check(
        &self,
        pos:        bool,
        atom:       AtomId,
        nvars_lit:  u32,
        infos:      &AtomInfos,
        atoms:      &AtomTable,
        syn:        &SyntacticLayer,
    ) -> Option<UnitHit> {
        let info = infos.info(atom, atoms, syn);
        if info.is_ground() {
            if let Some(cid) = self.ground_unit(pos, atom) {
                return Some(UnitHit::Subsumes(cid));
            }
            if let Some(cid) = self.ground_unit(!pos, atom) {
                return Some(UnitHit::Refutes(cid));
            }
        }
        // Open units: the literal's head must be concrete to bucket.
        let (h, ar) = head_key(atoms, syn, atom)?;
        // The unit pattern gets slots [0, nvars_unit); the target
        // literal's variables sit above at offset nvars-of-pattern...
        // — but one-way match never binds target vars, so the target
        // only needs slots distinct from the pattern's.  Offset by the
        // largest pattern var count we may see in this bucket scan.
        const PATTERN_SLOTS: u32 = 256;
        let target = slot_atom(atoms, syn, atom, PATTERN_SLOTS)?;
        for &same_pol in &[pos, !pos] {
            if let Some(groups) = self.open.get(&(same_pol, h, ar)) {
                for residues in groups.values() {
                    for bucket in residues.values() {
                        for u in bucket {
                            let mut s: Subst =
                                vec![None; (PATTERN_SLOTS + nvars_lit) as usize];
                            if match_one_way(&u.pattern, &target, &mut s) {
                                return Some(if same_pol == pos {
                                    UnitHit::Subsumes(u.clause)
                                } else {
                                    UnitHit::Refutes(u.clause)
                                });
                            }
                        }
                    }
                }
            }
        }
        None
    }

    pub(crate) fn ground_len(&self) -> usize { self.ground.len() }
}

/// One forward demodulator `l → r`: a positive unit equality whose
/// left side is STRICTLY KBO-greater than its right.  The orientation
/// is decided ONCE, at registration — KBO is stable under substitution
/// (`l >ₖ r ⟹ lσ >ₖ rσ` for every σ), so a registration-time check
/// licenses rewriting every matched instance without re-comparing.
/// Terms are slot-form at base 0 (the owning clause's canonical slots).
#[derive(Debug, Clone)]
pub(crate) struct Demod {
    /// Owning unit-equality clause id (a proof-DAG parent of every
    /// clause it rewrites).
    pub(crate) clause: u32,
    /// The KBO-larger side — the rewrite pattern.
    pub(crate) l: Term,
    /// The KBO-smaller side — the replacement.  Its variables are a
    /// subset of `l`'s (KBO's variable condition), so a successful
    /// match binds everything `r` mentions.
    pub(crate) r: Term,
    /// `max_slot(l) + 1` — the pattern's slot-space size, precomputed
    /// so the matcher's substitution vector sizes without a walk.
    pub(crate) nslots: u32,
}

/// The forward-demodulation index: oriented unit equations bucketed by
/// their left side's top shape, so a rewrite probe for a target
/// subterm is one hash lookup instead of a scan of every active
/// equation (the un-indexed scan was the measured 7% TPTP regression
/// that kept `Strategy.demod` off — see `strategy.rs`).
///
/// Buckets:
/// - `app`:  `(head key, arity)` for compound left sides — one-way
///   matching demands the pattern's and target's head + length agree
///   exactly, so the bucket key is a NECESSARY condition.
/// - `leaf`: bare-constant left sides (symbol id).  These only match
///   the identical constant.
///
/// Variable-headed and literal-valued left sides are not indexed
/// (skipping a demodulator is always sound — demodulation is optional
/// simplification; both shapes are vanishingly rare as KBO-oriented
/// lhs and ground constant equalities are already collapsed by the
/// oracle's `normalize_eq`).
///
/// NOT part of the background snapshot: orientation depends on the
/// run's KBO (`prec_seed` permutes precedence, and `bg_fingerprint`
/// deliberately excludes search knobs), so a hydrated prover rebuilds
/// this from the arena — exactly the superposition indexes' contract.
#[derive(Debug, Default, Clone)]
pub(crate) struct DemodIndex {
    app: Map64<(u64, u8), Vec<Demod>>,
    leaf: Map64<u64, Vec<Demod>>,
    len: usize,
}

impl DemodIndex {
    pub(crate) fn is_empty(&self) -> bool { self.len == 0 }

    pub(crate) fn clear(&mut self) {
        self.app = Map64::default();
        self.leaf = Map64::default();
        self.len = 0;
    }

    /// Register the oriented demodulator `l → r` (caller has already
    /// verified `l >ₖ r`).  Unindexable left-side shapes are dropped.
    pub(crate) fn add(&mut self, clause: u32, l: Term, r: Term) {
        let mut slots = std::collections::BTreeSet::new();
        term_slots(&l, &mut slots);
        let nslots = slots.iter().max().map_or(0, |m| *m as u32 + 1);
        match &l {
            Term::App(elems) => {
                let key = match elems.first() {
                    Some(Term::Sym(s)) => s.id(),
                    Some(Term::Op(op)) => u64::from(op_tag(op)),
                    // Variable-headed pattern: not bucketable (it would
                    // have to probe on every arity match) — skip.
                    _ => return,
                };
                let ar = elems.len().min(255) as u8;
                self.app
                    .entry((key, ar))
                    .or_default()
                    .push(Demod { clause, l, r, nslots });
            }
            Term::Sym(s) => {
                let id = s.id();
                self.leaf.entry(id).or_default().push(Demod { clause, l, r, nslots });
            }
            _ => return,
        }
        self.len += 1;
    }

    /// Demodulators whose left side could match `t`, by top shape —
    /// the one-hash-probe prefilter (match verifies).
    pub(crate) fn candidates(&self, t: &Term) -> Option<&[Demod]> {
        match t {
            Term::App(elems) => {
                let key = match elems.first() {
                    Some(Term::Sym(s)) => s.id(),
                    Some(Term::Op(op)) => u64::from(op_tag(op)),
                    _ => return None,
                };
                let ar = elems.len().min(255) as u8;
                self.app.get(&(key, ar)).map(Vec::as_slice)
            }
            Term::Sym(s) => self.leaf.get(&s.id()).map(Vec::as_slice),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Symbol;

    fn sym(name: &str) -> Term {
        Term::Sym(Symbol::from(name))
    }

    #[test]
    fn demod_index_buckets_by_head_and_arity() {
        let mut idx = DemodIndex::default();
        assert!(idx.is_empty());
        // sideKick(?0) -> ?0  (non-ground pattern, slot form).
        let l = Term::App(vec![sym("sideKick"), Term::Var(0)]);
        idx.add(7, l.clone(), Term::Var(0));
        assert!(!idx.is_empty());

        // Same head + arity: candidates found.
        let t = Term::App(vec![sym("sideKick"), sym("Clark")]);
        let hits = idx.candidates(&t).expect("head bucket must hit");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].clause, 7);
        assert_eq!(hits[0].nslots, 1);

        // Different head: no candidates (the prefilter's whole point).
        let other = Term::App(vec![sym("nemesis"), sym("Clark")]);
        assert!(idx.candidates(&other).is_none());
        // Different arity, same head: no candidates.
        let wide = Term::App(vec![sym("sideKick"), sym("a"), sym("b")]);
        assert!(idx.candidates(&wide).is_none());
        // A bare variable target is never probed.
        assert!(idx.candidates(&Term::Var(3)).is_none());
    }

    #[test]
    fn demod_index_leaf_bucket_and_clear() {
        let mut idx = DemodIndex::default();
        idx.add(3, sym("aliasOf"), sym("real"));
        let hits = idx.candidates(&sym("aliasOf")).expect("leaf bucket must hit");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].clause, 3);
        assert!(idx.candidates(&sym("real")).is_none());
        idx.clear();
        assert!(idx.is_empty());
        assert!(idx.candidates(&sym("aliasOf")).is_none());
    }

    #[test]
    fn demod_index_skips_unindexable_patterns() {
        let mut idx = DemodIndex::default();
        // Variable-headed compound lhs: dropped (sound — demod is
        // optional simplification).
        idx.add(1, Term::App(vec![Term::Var(0), sym("a")]), sym("b"));
        assert!(idx.is_empty());
    }

    #[test]
    fn non_ground_demodulator_matches_instance_and_rewrites() {
        use super::super::unify::{apply, shift_slots};
        // sideKick(?0) → ?0 against the ground instance sideKick(Clark):
        // the match binds ?0 = Clark and the replacement is Clark.
        let mut idx = DemodIndex::default();
        idx.add(9, Term::App(vec![sym("sideKick"), Term::Var(0)]), Term::Var(0));

        let target = Term::App(vec![sym("sideKick"), sym("Clark")]);
        let d = &idx.candidates(&target).expect("bucket")[0];
        let mut s: Subst = vec![None; d.nslots as usize];
        assert!(match_one_way(&d.l, &target, &mut s));
        assert_eq!(apply(&d.r, &s), sym("Clark"));

        // Slot-collision soundness: a NON-ground target whose own slot
        // ids overlap the pattern's.  The pattern is shifted above the
        // target's slots, so its ?0 must never be confused with the
        // target's ?0 — the rewrite yields the target's variable intact.
        let open_target = Term::App(vec![sym("sideKick"), Term::Var(0)]);
        let off = 1u64; // max target slot + 1
        let l2 = shift_slots(&d.l, off);
        let mut s2: Subst = vec![None; (off + u64::from(d.nslots)) as usize + 1];
        assert!(match_one_way(&l2, &open_target, &mut s2));
        assert_eq!(apply(&shift_slots(&d.r, off), &s2), Term::Var(0));
    }
}
