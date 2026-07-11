// crates/core/src/saturate/kbo.rs
//
// The Knuth–Bendix reduction ordering (Knuth & Bendix 1970) on the
// prover's interned atoms — the keystone the equality machinery
// (demodulation, ordered superposition, redundancy) will stand on.
//
// WHY THIS IS A SEPARATE VALUE, NOT PART OF THE FINGERPRINT.  A
// reduction ordering needs `s > t ⟹ u[s] > u[t]` (stability under
// context).  The algebraic fingerprint is built to be the OPPOSITE —
// a tiny structural change must produce an uncorrelated hash — and its
// carrier group (XOR over GF(2)^n) is 2-torsion, which by Levi's
// theorem admits NO compatible total order at all.  So the order can
// never be read off the coins; it is computed independently here.
//
// WHAT THE ALGEBRAIC MACHINERY *DOES* BUY US, and is used below:
//
//   * Weight is the one clean monotone homomorphism a term algebra
//     admits — a sum into (ℕ, +).  We compute it on the SAME kind of
//     content-addressed, memoized bottom-up walk as `fingerprint.rs`
//     (`KboInfo`, keyed by `AtomId`): a background term is weighed
//     once, ever, across every problem.
//   * Content addressing collapses the lexicographic tie-break's
//     "are these two subterms equal?" to an O(1) id compare (an
//     `Element::Sub` carries the subterm's content hash), so the lex
//     scan jumps straight to the first discriminating argument.
//   * A per-atom variable-presence bitmask (the structural sibling of
//     `leaf_sig`) is a one-AND necessary pre-filter for the variable
//     condition, which is the case that most often makes two terms
//     incomparable.
//
// ADMISSIBILITY.  We require every symbol weight ≥ 1 and the variable
// weight `w0 = 1`, so every constant weighs ≥ `w0`.  Under that
// restriction the classic special case (a weight-0 unary symbol) never
// arises and the definition collapses to: variable condition, then
// weight, then precedence/lexicographic — implemented faithfully in
// `compare`.  Any admissible weight table and ANY total precedence
// yield a sound, well-founded ordering; the particular choice is a
// tuning axis (a future `Strategy` knob), not a correctness concern.

use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use smallvec::SmallVec;

use crate::syntactic::SyntacticLayer;
use crate::types::{Element, Literal, SymbolId};

use super::clause::{AtomId, AtomTable};
use super::hash64::BuildContentHasher;

/// The result of comparing two terms under the ordering.  `Incomparable`
/// is a first-class outcome — KBO is a partial order, and "we cannot
/// orient these" is exactly the signal demodulation/superposition need
/// (don't restrict; keep both directions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KboCmp {
    Greater,
    Less,
    Equal,
    Incomparable,
}

/// Per-atom data the ordering precomputes, memoized by content hash.
/// Pure function of the atom's structure and the weight table — so the
/// memo is permanent for a fixed ordering, like `AtomInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KboInfo {
    /// KBO weight: Σ of symbol weights over every occurrence (variables
    /// contribute `w0`).  The monotone scalar; comparison's hot path.
    pub(crate) weight: u64,
    /// Variable occurrence multiset: `(var symbol id, count)`.  Small —
    /// canonical clauses use few distinct vars.
    pub(crate) vars: SmallVec<[(SymbolId, u32); 4]>,
    /// Bit `id & 63` set for every variable present — the necessary
    /// pre-filter for the variable condition (one AND rejects the
    /// common failing case before the multiset walk).
    pub(crate) var_mask: u64,
}

impl KboInfo {
    #[inline]
    fn count_of(&self, v: SymbolId) -> u32 {
        self.vars.iter().find(|(s, _)| *s == v).map_or(0, |(_, c)| *c)
    }
}

/// A Knuth–Bendix ordering: weight table + total precedence + the
/// content-addressed per-atom memo.  One lives on each `ProverLayer`
/// (the ordering is layer-fixed, so the memo is sound to share).
#[derive(Debug)]
pub(crate) struct KboOrdering {
    /// Variable weight (`w0`); also the floor for every symbol weight.
    w0: u64,
    /// Weight for symbols/literals/operators absent from `sym_weight`.
    default_weight: u64,
    /// Per-symbol weight overrides (clamped to ≥ `w0` on read).
    sym_weight: HashMap<SymbolId, u64>,
    /// Per-symbol precedence rank overrides (higher = greater).  A
    /// symbol with no override ranks by its own id — any total order is
    /// admissible, so the default is the (deterministic) id order.
    sym_prec: HashMap<SymbolId, u64>,
    /// Precedence permutation seed.  `0` ⇒ id-order (the default total
    /// order).  Non-zero ⇒ symbols rank by `hash(id, seed)` — a different
    /// but still total (hence admissible) precedence.  The weight memo is
    /// precedence-independent, so this only reshapes orientation, not the
    /// cached weights.
    prec_seed: u64,
    /// Content-addressed memo, exactly the `AtomInfos` pattern.
    memo: DashMap<AtomId, Arc<KboInfo>, BuildContentHasher>,
}

impl Default for KboOrdering {
    /// Unit weights everywhere (`w(f) = w0 = 1`), id-order precedence.
    /// With unit weights the KBO weight equals an atom's leaf count —
    /// i.e. the `AtomInfo.size` measure — but kept independent so a
    /// non-uniform weight table can diverge without disturbing the
    /// fingerprint memo.
    fn default() -> Self {
        Self {
            w0: 1,
            default_weight: 1,
            sym_weight: HashMap::new(),
            sym_prec: HashMap::new(),
            prec_seed: 0,
            memo: DashMap::with_hasher(BuildContentHasher::default()),
        }
    }
}

/// One element of a literal's term-multiset (literal ordering): a real
/// term-side element, or the minimal `⊤` placeholder standing for the
/// right-hand side of a predicate atom's implicit equation `A ≈ ⊤`.
#[derive(Clone)]
enum LitTerm {
    Top,
    Elem(Element),
}

impl KboOrdering {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A KBO with a permuted symbol precedence (same uniform weights).
    /// `seed == 0` is just the default id-order.
    pub(crate) fn with_prec_seed(seed: u64) -> Self {
        Self { prec_seed: seed, ..Self::default() }
    }

    /// Install a symbol weight (clamped to ≥ `w0` to preserve
    /// admissibility).  Mutating the table after atoms have been
    /// weighed would desync the memo, so this is construction-time only.
    #[cfg(test)]
    pub(crate) fn set_weight(&mut self, sym: SymbolId, w: u64) {
        self.sym_weight.insert(sym, w.max(self.w0));
        self.memo.clear();
    }

    /// Install a precedence rank (higher = greater in precedence).
    #[cfg(test)]
    pub(crate) fn set_precedence(&mut self, sym: SymbolId, rank: u64) {
        self.sym_prec.insert(sym, rank);
    }

    #[inline]
    fn weight_of_sym(&self, s: SymbolId) -> u64 {
        self.sym_weight.get(&s).copied().unwrap_or(self.default_weight).max(self.w0)
    }

    /// The KBO weight of one LEAF slot-form term — the weight-memo hook
    /// the ground-term facts walk (`saturate::terms`) sums over.  Mirrors
    /// [`Self::compute`]'s per-element arms exactly: symbol → its table
    /// weight, literal/operator → the default (≥ `w0`), variable → `w0`
    /// (unused by the ground-only facts walk, kept for totality).
    /// Weights are precedence-independent, so every `prec_seed` lane
    /// reads the same value — the reason the facts table can be
    /// layer-shared without a per-seed key.
    #[inline]
    pub(crate) fn term_leaf_weight(&self, t: &super::clause::Term) -> u64 {
        use super::clause::Term;
        match t {
            Term::Sym(s) => self.weight_of_sym(s.id()),
            Term::Lit(_) | Term::Op(_) => self.default_weight.max(self.w0),
            Term::Var(_) => self.w0,
            Term::App(_) => 0, // compounds sum their children; no own weight
        }
    }

    /// The KBO weight of a SLOT-form atom term, computed transiently
    /// from the tree — equals `self.info(id).weight` for the interned
    /// counterpart (same leaf-weight table, same saturating fold in the
    /// same traversal order; property-tested in `prover/make.rs`).
    /// The hash-before-intern feature-vector path reads OPEN literals'
    /// weights through this instead of `info` (which would need the
    /// atom resident to resolve).
    pub(crate) fn term_weight(&self, t: &super::clause::Term) -> u64 {
        use super::clause::Term;
        match t {
            Term::App(elems) => elems
                .iter()
                .fold(0u64, |w, e| w.saturating_add(self.term_weight(e))),
            leaf => self.term_leaf_weight(leaf),
        }
    }

    /// [`Self::term_leaf_weight`]'s store-side twin, over [`Element`]s.
    #[inline]
    pub(crate) fn element_leaf_weight(&self, el: &Element) -> u64 {
        match el {
            Element::Symbol(s) => self.weight_of_sym(s.id()),
            Element::Literal(_) | Element::Op(_) => self.default_weight.max(self.w0),
            Element::Variable { .. } => self.w0,
            Element::Sub(_) => 0, // compounds sum their children; no own weight
        }
    }

    /// Precedence key for a leaf/head element, as a `(class, key)` tuple
    /// compared lexicographically.  Distinct constants get distinct keys
    /// (the value is folded in), so precedence is total on ground leaves.
    fn prec_key(&self, el: &Element) -> (u8, u64) {
        match el {
            Element::Symbol(s) => {
                let id = s.id();
                let rank = self.sym_prec.get(&id).copied().unwrap_or_else(|| {
                    if self.prec_seed == 0 {
                        id
                    } else {
                        xxhash_rust::xxh64::xxh64(&id.to_le_bytes(), self.prec_seed)
                    }
                });
                (3, rank)
            }
            Element::Op(op) => (2, u64::from(op_byte(op))),
            Element::Literal(Literal::Str(v)) =>
                (1, xxhash_rust::xxh64::xxh64(v.as_bytes(), 0x5712)),
            Element::Literal(Literal::Number(v)) =>
                (0, xxhash_rust::xxh64::xxh64(v.as_bytes(), 0x5713)),
            // A variable has no precedence; callers treat a variable head
            // as conservatively incomparable before reaching here.
            Element::Variable { id, .. } => (4, *id),
            Element::Sub(sid) => (5, *sid),
        }
    }

    /// The memoized info for an interned atom or subterm.
    pub(crate) fn info(
        &self,
        id: AtomId,
        atoms: &AtomTable,
        syn: &SyntacticLayer,
    ) -> Arc<KboInfo> {
        if let Some(hit) = self.memo.get(&id) {
            return hit.value().clone();
        }
        let computed = Arc::new(self.compute(id, atoms, syn));
        self.memo.entry(id).or_insert(computed).value().clone()
    }

    fn compute(&self, id: AtomId, atoms: &AtomTable, syn: &SyntacticLayer) -> KboInfo {
        let Some(sent) = atoms.resolve(id, syn) else {
            return KboInfo { weight: 0, vars: SmallVec::new(), var_mask: 0 };
        };
        let mut weight = 0u64;
        let mut vars: SmallVec<[(SymbolId, u32); 4]> = SmallVec::new();
        let mut var_mask = 0u64;
        let bump = |v: SymbolId, vars: &mut SmallVec<[(SymbolId, u32); 4]>| {
            match vars.iter_mut().find(|(s, _)| *s == v) {
                Some((_, c)) => *c += 1,
                None => vars.push((v, 1)),
            }
        };
        for el in sent.elements.iter() {
            match el {
                Element::Variable { id, .. } => {
                    weight = weight.saturating_add(self.w0);
                    bump(*id, &mut vars);
                    var_mask |= 1u64 << (id & 63);
                }
                Element::Symbol(s) =>
                    weight = weight.saturating_add(self.weight_of_sym(s.id())),
                Element::Op(_) | Element::Literal(_) =>
                    weight = weight.saturating_add(self.default_weight.max(self.w0)),
                Element::Sub(sid) => {
                    let ci = self.info(*sid, atoms, syn);
                    weight = weight.saturating_add(ci.weight);
                    var_mask |= ci.var_mask;
                    for (v, c) in ci.vars.iter() {
                        match vars.iter_mut().find(|(s, _)| s == v) {
                            Some((_, cc)) => *cc += *c,
                            None => vars.push((*v, *c)),
                        }
                    }
                }
            }
        }
        KboInfo { weight, vars, var_mask }
    }

    /// The variable condition `a ⊒ b`: every variable occurs in `a` at
    /// least as often as in `b`.  The bitmask is a one-AND necessary
    /// pre-check — if `b` has a variable bit `a` lacks, it fails outright.
    #[inline]
    fn var_dominates(a: &KboInfo, b: &KboInfo) -> bool {
        if b.var_mask & !a.var_mask != 0 {
            return false;
        }
        b.vars.iter().all(|(v, cb)| a.count_of(*v) >= *cb)
    }

    /// Compare two interned atoms (or subterms) under the ordering.
    pub(crate) fn compare(
        &self,
        a: AtomId,
        b: AtomId,
        atoms: &AtomTable,
        syn: &SyntacticLayer,
    ) -> KboCmp {
        // Content addressing: identical ids are identical terms.
        if a == b {
            return KboCmp::Equal;
        }
        let ia = self.info(a, atoms, syn);
        let ib = self.info(b, atoms, syn);
        let vc_ab = Self::var_dominates(&ia, &ib);
        let vc_ba = Self::var_dominates(&ib, &ia);

        // Weight first — the monotone scalar decides most comparisons.
        if ia.weight > ib.weight {
            return if vc_ab { KboCmp::Greater } else { KboCmp::Incomparable };
        }
        if ia.weight < ib.weight {
            return if vc_ba { KboCmp::Less } else { KboCmp::Incomparable };
        }
        // Equal weight: structural tie-break.
        self.struct_cmp(a, b, vc_ab, vc_ba, atoms, syn)
    }

    /// Equal-weight structural comparison: precedence on heads, then
    /// lexicographic on arguments (each a full recursive `compare`).
    fn struct_cmp(
        &self,
        a: AtomId,
        b: AtomId,
        vc_ab: bool,
        vc_ba: bool,
        atoms: &AtomTable,
        syn: &SyntacticLayer,
    ) -> KboCmp {
        let (Some(sa), Some(sb)) = (atoms.resolve(a, syn), atoms.resolve(b, syn)) else {
            return KboCmp::Incomparable;
        };
        let (ea, eb) = (&sa.elements, &sb.elements);
        let (Some(ha), Some(hb)) = (ea.first(), eb.first()) else {
            return KboCmp::Incomparable;
        };
        // A variable head (second-order / predicate variable) is not
        // ordered — stay conservative (incomparable ⇒ no restriction).
        if matches!(ha, Element::Variable { .. }) || matches!(hb, Element::Variable { .. }) {
            return KboCmp::Incomparable;
        }
        // Heads differ in precedence ⇒ that direction, gated by varcond.
        let (pa, pb) = (self.prec_key(ha), self.prec_key(hb));
        if pa != pb {
            return if pa > pb {
                if vc_ab { KboCmp::Greater } else { KboCmp::Incomparable }
            } else if vc_ba {
                KboCmp::Less
            } else {
                KboCmp::Incomparable
            };
        }
        // Same head; same arity required to lex (variadic relations with
        // differing arity are conservatively incomparable here).
        if ea.len() != eb.len() {
            return KboCmp::Incomparable;
        }
        for (la, lb) in ea.iter().zip(eb.iter()).skip(1) {
            // Content-address equality skip: identical arguments cost an
            // id compare (Sub) or value compare (leaf), no descent.
            if elem_eq(la, lb) {
                continue;
            }
            return match self.cmp_elem(la, lb, atoms, syn) {
                KboCmp::Equal => continue,
                KboCmp::Greater =>
                    if vc_ab { KboCmp::Greater } else { KboCmp::Incomparable },
                KboCmp::Less =>
                    if vc_ba { KboCmp::Less } else { KboCmp::Incomparable },
                KboCmp::Incomparable => KboCmp::Incomparable,
            };
        }
        KboCmp::Equal
    }

    /// Compare two argument elements.  Compounds (`Sub`) recurse through
    /// the memoized atom comparison; leaves/variables compare by weight
    /// then precedence (the same logic as `compare`, at element grain).
    fn cmp_elem(
        &self,
        ea: &Element,
        eb: &Element,
        atoms: &AtomTable,
        syn: &SyntacticLayer,
    ) -> KboCmp {
        if let (Element::Sub(sa), Element::Sub(sb)) = (ea, eb) {
            return self.compare(*sa, *sb, atoms, syn);
        }
        let (wa, va) = self.elem_profile(ea, atoms, syn);
        let (wb, vb) = self.elem_profile(eb, atoms, syn);
        let vc_ab = Self::var_dominates(&va, &vb);
        let vc_ba = Self::var_dominates(&vb, &va);
        if wa > wb {
            return if vc_ab { KboCmp::Greater } else { KboCmp::Incomparable };
        }
        if wa < wb {
            return if vc_ba { KboCmp::Less } else { KboCmp::Incomparable };
        }
        // Equal weight at a leaf/var position.  A variable on either side
        // (and not structurally equal, already handled by the caller's
        // skip) is incomparable under positive weights; two ground leaves
        // decide by precedence.
        if matches!(ea, Element::Variable { .. }) || matches!(eb, Element::Variable { .. })
            || matches!(ea, Element::Sub(_)) || matches!(eb, Element::Sub(_))
        {
            return KboCmp::Incomparable;
        }
        let (pa, pb) = (self.prec_key(ea), self.prec_key(eb));
        match pa.cmp(&pb) {
            std::cmp::Ordering::Greater => KboCmp::Greater,
            std::cmp::Ordering::Less => KboCmp::Less,
            std::cmp::Ordering::Equal => KboCmp::Equal,
        }
    }

    /// Weight + variable profile of a single element (Sub recurses to the
    /// memo; leaves are weight-only; a variable is `w0` with itself).
    fn elem_profile(
        &self,
        el: &Element,
        atoms: &AtomTable,
        syn: &SyntacticLayer,
    ) -> (u64, KboInfo) {
        match el {
            Element::Sub(sid) => {
                let ci = self.info(*sid, atoms, syn);
                (ci.weight, (*ci).clone())
            }
            Element::Variable { id, .. } => {
                let mut vars: SmallVec<[(SymbolId, u32); 4]> = SmallVec::new();
                vars.push((*id, 1));
                (self.w0, KboInfo { weight: self.w0, vars, var_mask: 1u64 << (id & 63) })
            }
            Element::Symbol(s) => (
                self.weight_of_sym(s.id()),
                KboInfo { weight: self.weight_of_sym(s.id()), vars: SmallVec::new(), var_mask: 0 },
            ),
            Element::Op(_) | Element::Literal(_) => {
                let w = self.default_weight.max(self.w0);
                (w, KboInfo { weight: w, vars: SmallVec::new(), var_mask: 0 })
            }
        }
    }

    // -- literal ordering (superposition maximality) ----------------------

    /// Compare two literals under the KBO-induced literal ordering — the
    /// multiset extension of the term order over each literal's encoding
    /// (Bachmair–Ganzinger): a positive equality `s≈t` is `{s,t}`, a
    /// negative `s≉t` is the doubled `{s,s,t,t}` (so a negative outweighs
    /// the positive with the same terms), and a non-equality atom `A` is
    /// the equation `A ≈ ⊤` → `{A, ⊤}` with `⊤` minimal — so two predicate
    /// literals compare by their atoms.
    pub(crate) fn compare_lits(
        &self,
        a_pos: bool, a_atom: AtomId,
        b_pos: bool, b_atom: AtomId,
        atoms: &AtomTable, syn: &SyntacticLayer,
    ) -> KboCmp {
        let ma = self.literal_multiset(a_pos, a_atom, atoms, syn);
        let mb = self.literal_multiset(b_pos, b_atom, atoms, syn);
        self.multiset_cmp(&ma, &mb, atoms, syn)
    }

    fn literal_multiset(
        &self, pos: bool, atom: AtomId, atoms: &AtomTable, syn: &SyntacticLayer,
    ) -> SmallVec<[LitTerm; 4]> {
        let mut m: SmallVec<[LitTerm; 4]> = SmallVec::new();
        let eq_sides = atoms.resolve(atom, syn).filter(|s| {
            s.elements.len() == 3
                && matches!(s.elements.first(),
                    Some(Element::Op(crate::parse::OpKind::Equal)))
        });
        match eq_sides {
            Some(sent) => {
                let s = LitTerm::Elem(sent.elements[1].clone());
                let t = LitTerm::Elem(sent.elements[2].clone());
                m.push(s.clone());
                m.push(t.clone());
                if !pos { m.push(s); m.push(t); }
            }
            None => {
                let a = LitTerm::Elem(Element::Sub(atom));
                m.push(a.clone());
                m.push(LitTerm::Top);
                if !pos { m.push(a); m.push(LitTerm::Top); }
            }
        }
        m
    }

    fn cmp_litterm(
        &self, a: &LitTerm, b: &LitTerm, atoms: &AtomTable, syn: &SyntacticLayer,
    ) -> KboCmp {
        match (a, b) {
            (LitTerm::Top, LitTerm::Top) => KboCmp::Equal,
            (LitTerm::Top, _) => KboCmp::Less,
            (_, LitTerm::Top) => KboCmp::Greater,
            (LitTerm::Elem(ea), LitTerm::Elem(eb)) => self.cmp_elem(ea, eb, atoms, syn),
        }
    }

    /// Dershowitz–Manna multiset extension of [`Self::cmp_litterm`].
    fn multiset_cmp(
        &self, ma: &[LitTerm], mb: &[LitTerm], atoms: &AtomTable, syn: &SyntacticLayer,
    ) -> KboCmp {
        let mut a: Vec<&LitTerm> = ma.iter().collect();
        let mut b: Vec<&LitTerm> = mb.iter().collect();
        // Strip pairwise-equal elements (the multiset difference).
        let mut i = 0;
        while i < a.len() {
            if let Some(j) = b.iter()
                .position(|y| self.cmp_litterm(a[i], y, atoms, syn) == KboCmp::Equal)
            {
                b.swap_remove(j);
                a.swap_remove(i);
            } else {
                i += 1;
            }
        }
        match (a.is_empty(), b.is_empty()) {
            (true, true) => KboCmp::Equal,
            (false, true) => KboCmp::Greater,
            (true, false) => KboCmp::Less,
            (false, false) => {
                let a_dom = b.iter().all(|n|
                    a.iter().any(|m| self.cmp_litterm(m, n, atoms, syn) == KboCmp::Greater));
                if a_dom {
                    return KboCmp::Greater;
                }
                let b_dom = a.iter().all(|m|
                    b.iter().any(|n| self.cmp_litterm(n, m, atoms, syn) == KboCmp::Greater));
                if b_dom { KboCmp::Less } else { KboCmp::Incomparable }
            }
        }
    }
}

/// Structural element equality — manual because `Element` doesn't derive
/// `PartialEq`.  `Sub` compares by content hash, the O(1) skip.
fn elem_eq(a: &Element, b: &Element) -> bool {
    match (a, b) {
        (Element::Variable { id: x, .. }, Element::Variable { id: y, .. }) => x == y,
        (Element::Symbol(x), Element::Symbol(y)) => x.id() == y.id(),
        (Element::Op(x), Element::Op(y)) => op_byte(x) == op_byte(y),
        (Element::Literal(Literal::Str(x)), Element::Literal(Literal::Str(y))) => x == y,
        (Element::Literal(Literal::Number(x)), Element::Literal(Literal::Number(y))) => x == y,
        (Element::Sub(x), Element::Sub(y)) => x == y,
        _ => false,
    }
}

fn op_byte(op: &crate::parse::OpKind) -> u8 {
    use crate::parse::OpKind::*;
    match op {
        And => b'a', Or => b'o', Not => b'n', Implies => b'i',
        Iff => b'f', Equal => b'e', ForAll => b'A', Exists => b'E',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::clause::Term;
    use crate::types::Symbol;

    /// Intern `(head args...)` and return its atom id, building it
    /// through the same `AtomTable` path the prover uses.
    fn atom(atoms: &AtomTable, t: &Term) -> AtomId {
        atoms.intern_atom(t)
    }

    fn sym(name: &str) -> Term {
        Term::Sym(Symbol::from(name))
    }
    fn var(name: &str) -> Term {
        Term::Var(Symbol::hash_name(name))
    }
    fn app(parts: Vec<Term>) -> Term {
        Term::App(parts)
    }

    // We need a SyntacticLayer for `resolve`'s store fall-back, but every
    // atom here is interned locally, so a throwaway empty one suffices.
    fn syn() -> SyntacticLayer {
        SyntacticLayer::default()
    }

    fn eq(s: Term, t: Term) -> Term {
        app(vec![Term::Op(crate::parse::OpKind::Equal), s, t])
    }

    #[test]
    fn negative_literal_outweighs_positive_same_atom() {
        let atoms = AtomTable::default();
        let syn = syn();
        let kbo = KboOrdering::new();
        // (p a) as a literal: negative {A,A,⊤,⊤} ≻ positive {A,⊤}.
        let pa = atom(&atoms, &app(vec![sym("p"), sym("a")]));
        assert_eq!(kbo.compare_lits(false, pa, true, pa, &atoms, &syn), KboCmp::Greater);
        assert_eq!(kbo.compare_lits(true, pa, false, pa, &atoms, &syn), KboCmp::Less);
        // Same literal: equal.
        assert_eq!(kbo.compare_lits(true, pa, true, pa, &atoms, &syn), KboCmp::Equal);
    }

    #[test]
    fn predicate_literals_compare_by_their_atoms() {
        let atoms = AtomTable::default();
        let syn = syn();
        let kbo = KboOrdering::new();
        // (p (f a)) ≻ (p a) by weight — the ⊤ placeholders cancel, so the
        // literal order reduces to the atom order.
        let heavy = atom(&atoms, &app(vec![sym("p"), app(vec![sym("f"), sym("a")])]));
        let light = atom(&atoms, &app(vec![sym("p"), sym("a")]));
        assert_eq!(kbo.compare_lits(true, heavy, true, light, &atoms, &syn), KboCmp::Greater);
        assert_eq!(kbo.compare_lits(true, light, true, heavy, &atoms, &syn), KboCmp::Less);
    }

    #[test]
    fn equality_literal_sides_form_the_multiset() {
        let atoms = AtomTable::default();
        let syn = syn();
        let kbo = KboOrdering::new();
        // (equal (f a) b) {f(a), b} ≻ (equal a b) {a, b}: f(a) ≻ a dominates,
        // b cancels.
        let big = atom(&atoms, &eq(app(vec![sym("f"), sym("a")]), sym("b")));
        let small = atom(&atoms, &eq(sym("a"), sym("b")));
        assert_eq!(kbo.compare_lits(true, big, true, small, &atoms, &syn), KboCmp::Greater);
    }

    #[test]
    fn subterm_is_smaller_f_of_x_greater_than_x() {
        let atoms = AtomTable::default();
        let syn = syn();
        let kbo = KboOrdering::new();
        // f(x) vs x : f(x) is heavier and still contains x ⇒ greater.
        let fx = atom(&atoms, &app(vec![sym("f"), var("x")]));
        let x = atom(&atoms, &var("x"));
        assert_eq!(kbo.compare(fx, x, &atoms, &syn), KboCmp::Greater);
        assert_eq!(kbo.compare(x, fx, &atoms, &syn), KboCmp::Less);
    }

    #[test]
    fn commutativity_is_incomparable() {
        let atoms = AtomTable::default();
        let syn = syn();
        let kbo = KboOrdering::new();
        // f(x,y) vs f(y,x): equal weight, same head, lex hits x|y then
        // y|x — variables, incomparable.  KBO correctly REFUSES to orient
        // commutativity (it would loop) — the reason symmetric relations
        // need the schema channel, not a deficiency of the ordering.
        let l = atom(&atoms, &app(vec![sym("f"), var("x"), var("y")]));
        let r = atom(&atoms, &app(vec![sym("f"), var("y"), var("x")]));
        assert_eq!(kbo.compare(l, r, &atoms, &syn), KboCmp::Incomparable);
    }

    #[test]
    fn variable_and_constant_are_always_incomparable() {
        let atoms = AtomTable::default();
        let syn = syn();
        // Even with the constant weighted heavier: c >ₖ x needs x to
        // occur in c (it can't), so the variable condition forbids it
        // both ways.  A variable is never oriented against a term that
        // doesn't contain it.
        let mut kbo = KboOrdering::new();
        let c = atom(&atoms, &sym("c"));
        let x = atom(&atoms, &var("x"));
        assert_eq!(kbo.compare(c, x, &atoms, &syn), KboCmp::Incomparable);
        kbo.set_weight(Symbol::hash_name("c"), 9);
        let c2 = atom(&atoms, &sym("c"));
        assert_eq!(kbo.compare(c2, x, &atoms, &syn), KboCmp::Incomparable);
    }

    #[test]
    fn lexicographic_by_precedence_on_shared_head() {
        let atoms = AtomTable::default();
        let syn = syn();
        let mut kbo = KboOrdering::new();
        // g(a) vs g(b): equal weight, same head g, lex compares a|b by
        // precedence.  Force a ≻ b.
        kbo.set_precedence(Symbol::hash_name("a"), 100);
        kbo.set_precedence(Symbol::hash_name("b"), 1);
        let ga = atom(&atoms, &app(vec![sym("g"), sym("a")]));
        let gb = atom(&atoms, &app(vec![sym("g"), sym("b")]));
        assert_eq!(kbo.compare(ga, gb, &atoms, &syn), KboCmp::Greater);
        assert_eq!(kbo.compare(gb, ga, &atoms, &syn), KboCmp::Less);
    }

    #[test]
    fn heavier_outranks_regardless_of_precedence() {
        let atoms = AtomTable::default();
        let syn = syn();
        let kbo = KboOrdering::new();
        // f(a,b) (weight 3) vs c (weight 1): pure weight decision, both
        // ground so the variable condition is vacuous.
        let fab = atom(&atoms, &app(vec![sym("f"), sym("a"), sym("b")]));
        let c = atom(&atoms, &sym("c"));
        assert_eq!(kbo.compare(fab, c, &atoms, &syn), KboCmp::Greater);
        assert_eq!(kbo.compare(c, fab, &atoms, &syn), KboCmp::Less);
    }

    #[test]
    fn orientation_is_stable_under_instantiation() {
        // The demodulator contract: a registration-time `l ≻ r` licenses
        // rewriting EVERY matched instance `lσ → rσ` without re-comparing,
        // because KBO is stable under substitution.  Exercise it on the
        // two shapes registration sees:
        let atoms = AtomTable::default();
        let syn = syn();
        let kbo = KboOrdering::new();

        // (1) f(g(x)) ≻ g(x); instance σ = {x → h(h(c))}.
        let l = app(vec![sym("f"), app(vec![sym("g"), var("x")])]);
        let r = app(vec![sym("g"), var("x")]);
        assert_eq!(
            kbo.compare(atom(&atoms, &l), atom(&atoms, &r), &atoms, &syn),
            KboCmp::Greater);
        let big = app(vec![sym("h"), app(vec![sym("h"), sym("c")])]);
        let li = app(vec![sym("f"), app(vec![sym("g"), big.clone()])]);
        let ri = app(vec![sym("g"), big.clone()]);
        assert_eq!(
            kbo.compare(atom(&atoms, &li), atom(&atoms, &ri), &atoms, &syn),
            KboCmp::Greater);

        // (2) The duplicated-variable case — where a naive "weight of the
        // pattern" argument would go wrong if stability failed: f(x,x) ≻
        // g(x) (weight 3 > 2, x-count 2 ≥ 1).  Under σ = {x → h(h(c))}
        // the left side's weight grows TWICE as fast — still greater.
        let l2 = app(vec![sym("f"), var("x"), var("x")]);
        let r2 = app(vec![sym("g"), var("x")]);
        assert_eq!(
            kbo.compare(atom(&atoms, &l2), atom(&atoms, &r2), &atoms, &syn),
            KboCmp::Greater);
        let l2i = app(vec![sym("f"), big.clone(), big.clone()]);
        let r2i = app(vec![sym("g"), big]);
        assert_eq!(
            kbo.compare(atom(&atoms, &l2i), atom(&atoms, &r2i), &atoms, &syn),
            KboCmp::Greater);
    }

    #[test]
    fn identical_atoms_are_equal_and_memo_is_stable() {
        let atoms = AtomTable::default();
        let syn = syn();
        let kbo = KboOrdering::new();
        let t = app(vec![sym("h"), var("x"), app(vec![sym("g"), var("x")])]);
        let a = atom(&atoms, &t);
        let b = atom(&atoms, &t); // same content ⇒ same id
        assert_eq!(a, b);
        assert_eq!(kbo.compare(a, b, &atoms, &syn), KboCmp::Equal);
        // Memo returns a stable value across calls.
        let w1 = kbo.info(a, &atoms, &syn).weight;
        let w2 = kbo.info(a, &atoms, &syn).weight;
        assert_eq!(w1, w2);
        // x occurs twice (once bare, once under g) ⇒ recorded multiplicity.
        assert_eq!(kbo.info(a, &atoms, &syn).count_of(Symbol::hash_name("x")), 2);
    }
}
