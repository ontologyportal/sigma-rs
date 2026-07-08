// crates/core/src/saturate/prover/fvi.rs
//
// Feature-vector prefilter for clause subsumption (E-style FVI, v1: a
// per-candidate check, not yet a trie index — see module docs on
// `NativeProver::forward_subsumed`).
//
// A clause `sub` can only subsume `sup` (one-way match of every `sub`
// literal onto a distinct `sup` literal, same polarity — i.e. `sub·σ` is
// a sub-multiset of `sup`'s literals for some substitution σ over
// `sub`'s variables only) if every MONOTONE feature of `sub` is `<=` the
// corresponding feature of `sup`: matching only ever INSTANTIATES a
// `sub` variable with a `sup`-side subterm, which never shrinks a
// literal's size or weight, and `sub`'s (whole, positive, negative)
// literal counts each inject into `sup`'s.
//
// So `fv(sub) <= fv(sup)` pointwise is NECESSARY for `sub` to subsume
// `sup` — a cheap, sound REJECTION test: if any channel violates it,
// `clause_subsumes` would have returned `false`, and we skip the
// expensive one-way-matching search entirely.  It is not sufficient
// (passing the prefilter says nothing; `clause_subsumes` still verifies).
//
// Channels (all `u16`, saturating — a clause with >65535 in any channel
// just floors the check to "can't reject," which only costs a redundant
// `clause_subsumes` call, never a wrong answer):
//   0. #literals
//   1. #positive literals
//   2. #negative literals
//   3. term size (leaf count, summed over literals)
//   4. KBO weight (summed over literals, via the memoized per-atom KBO
//      info — the same weight table the queue/demodulation already use)
//
// REJECTED CHANNEL — #distinct variables (task item (e)) is NOT a valid
// monotone channel and is deliberately NOT implemented here.  Worked
// counterexample: sub = (p ?0), sup = (p a).  `sub` subsumes `sup` via
// σ={?0↦a}, yet #vars(sub)=1 > #vars(sup)=0 — so `#vars(sub) <=
// #vars(sup)` is false even on the simplest possible instance pair.  The
// reverse inequality fails too: sub = (p ?0), sup = (p (f ?1 ?2))
// subsumes with σ={?0↦(f ?1 ?2)}, giving #vars(sub)=1 < #vars(sup)=2.
// So raw variable COUNT moves in neither direction under substitution
// (a match can merge distinct sub-variables onto one ground subterm,
// shrinking the count, or instantiate one sub-variable with a term
// carrying several of sup's own variables, growing it) — matches the
// standard FVI literature (Schulz, "Simple and Efficient Clause
// Subsumption with Feature Vector Indexing"), which uses per-symbol
// occurrence counts and term-size/weight measures, never a bare
// distinct-variable tally, for exactly this reason.  KBO weight (channel
// 4) already captures the part of "variable-ness" that IS monotone: each
// variable OCCURRENCE contributes >= w0 to weight, and substitution can
// only grow that — so weight subsumes (pun intended) the useful half of
// what a variable-count channel would have offered.
//
// Symbol-count channels (g/h in the task write-up) are skipped: cheap
// only with a precomputed "globally most frequent symbols" table, which
// does not exist yet and would need a whole-KB pass to build — not
// justified before measuring whether the 5 structural channels already
// reject enough candidates (see the task's measurement gate).

use std::sync::Arc;

use crate::syntactic::SyntacticLayer;

use super::super::clause::{AtomId, AtomTable, PLit, Term};
use super::super::kbo::KboOrdering;
use super::super::terms::TermFactsTable;
use super::super::AtomInfo;

/// Number of feature channels.
pub(crate) const FV_LEN: usize = 5;

/// A clause's feature-vector: monotone summaries used to REJECT
/// impossible subsumption attempts before the expensive exact check.
/// Cheap to compute once (at clause creation) and cheap to compare
/// (five `u16` comparisons, no branching on clause shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ClauseFv(pub(crate) [u16; FV_LEN]);

impl ClauseFv {
    /// Computed from a literal list (atom-id form: `pos`/`atom` pairs) —
    /// the shape both an arena `ClauseRec` and a not-yet-pushed candidate
    /// share, so one function serves both call sites in
    /// `forward_subsumed`.  Leaf-count and KBO weight both come from the
    /// SAME per-atom memos the rest of the prover already warms
    /// (`AtomInfos::info` / `KboOrdering::info`), via the two injected
    /// lookups — this function never re-walks a `Term` tree.
    ///
    /// `ground_weights` (Part 3.3 of the ground-term identity design,
    /// passed only under `Strategy.demod`): a GROUND literal's weight is
    /// read from the layer-shared ground-term facts memo instead of the
    /// per-`KboOrdering` memo — the same value (weights are
    /// `prec_seed`-independent; debug-asserted), but the layer table
    /// stays warm across prec-seeded portfolio lanes whose fresh
    /// `KboOrdering` memos start cold.
    pub(crate) fn compute(
        lits: &[PLit],
        kbo: &KboOrdering,
        atom_info: impl Fn(AtomId) -> Arc<AtomInfo>,
        atoms: &AtomTable,
        syn: &SyntacticLayer,
        ground_weights: Option<&TermFactsTable>,
    ) -> Self {
        let mut n_lits = 0u32;
        let mut n_pos = 0u32;
        let mut n_neg = 0u32;
        let mut size = 0u64;
        let mut weight = 0u64;
        for l in lits {
            n_lits += 1;
            if l.pos { n_pos += 1; } else { n_neg += 1; }
            let info = atom_info(l.atom);
            let w = ground_weights
                .filter(|_| info.is_ground())
                .and_then(|tbl| tbl.facts_for_atom(l.atom, atoms, syn, kbo))
                .map(|f| f.kbo_weight);
            #[cfg(any(test, debug_assertions))]
            if let Some(w) = w {
                debug_assert_eq!(
                    w, kbo.info(l.atom, atoms, syn).weight,
                    "ground facts weight diverged from the KBO memo for atom {:#x}",
                    l.atom,
                );
            }
            weight = weight
                .saturating_add(w.unwrap_or_else(|| kbo.info(l.atom, atoms, syn).weight));
            size = size.saturating_add(u64::from(info.size));
        }
        Self([
            sat_u16(n_lits as u64),
            sat_u16(n_pos as u64),
            sat_u16(n_neg as u64),
            sat_u16(size),
            sat_u16(weight),
        ])
    }

    /// [`Self::compute`] fed from PRE-ACCEPT transient data
    /// (hash-before-intern): per-literal `AtomInfo`s computed from the
    /// slot terms (`term_atom_info`) and weights from the tree walk /
    /// ground-facts memo — no `AtomTable` residency needed, no memo
    /// side effects beyond the (content-addressed, value-identical)
    /// ground-facts entries the eager path also wrote.  Byte-equal to
    /// `compute` on the interned counterpart (debug twin at the
    /// `forward_subsumed` call site; property test in `make.rs`).
    pub(crate) fn compute_from_terms(
        lits: &[PLit],
        terms: &[(bool, Term)],
        infos: &[AtomInfo],
        kbo: &KboOrdering,
        ground_weights: Option<&TermFactsTable>,
    ) -> Self {
        debug_assert_eq!(lits.len(), terms.len());
        debug_assert_eq!(lits.len(), infos.len());
        let mut n_lits = 0u32;
        let mut n_pos = 0u32;
        let mut n_neg = 0u32;
        let mut size = 0u64;
        let mut weight = 0u64;
        for ((l, (_, t)), info) in lits.iter().zip(terms).zip(infos) {
            n_lits += 1;
            if l.pos { n_pos += 1; } else { n_neg += 1; }
            // Mirrors `compute`'s ground-weight arm: the facts walk only
            // answers for truly ground terms (`compute`'s
            // `facts_for_atom` bails the same way on the mask-blind
            // "ground" of a >MAX_SEATS seat), and the fallback is the
            // same leaf-weight fold `KboOrdering::info` runs.
            let w = ground_weights
                .filter(|_| info.is_ground())
                .and_then(|tbl| tbl.ground_facts(t, kbo))
                .map(|f| f.kbo_weight);
            weight = weight.saturating_add(w.unwrap_or_else(|| kbo.term_weight(t)));
            size = size.saturating_add(u64::from(info.size));
        }
        Self([
            sat_u16(n_lits as u64),
            sat_u16(n_pos as u64),
            sat_u16(n_neg as u64),
            sat_u16(size),
            sat_u16(weight),
        ])
    }

    /// Pointwise `<=`: necessary condition for `self` to subsume a
    /// clause with feature vector `other`.  `false` here is a SOUND,
    /// cheap rejection; `true` means "still possible," not "subsumes."
    #[inline]
    pub(crate) fn le(&self, other: &Self) -> bool {
        self.0.iter().zip(other.0.iter()).all(|(a, b)| a <= b)
    }
}

#[inline]
fn sat_u16(v: u64) -> u16 {
    v.min(u64::from(u16::MAX)) as u16
}

/// One Bloom bit per GROUND literal for [`ClauseBlooms::glit`], keyed by
/// the literal's canonical atom id (already a uniform 64-bit content
/// hash — same trust model as every `hash64` key) with polarity mixed
/// into the bit index: `atom ^ pos` flips the low index bit, so the
/// positive and negative literal over one atom land on sibling bits —
/// free polarity selectivity.  Mirrors `fingerprint.rs`'s house style
/// (`leaf_bit = 1 << (key & 63)`).  Both sides of every probe MUST use
/// this one function — the subset test is only sound if C-side and
/// D-side bits are derived identically.
#[inline]
pub(crate) fn glit_bit(pos: bool, atom: AtomId) -> u64 {
    1u64 << ((atom ^ u64::from(pos)) & 63)
}

/// Bloom-filter subsumption prefilter words: two 64-bit signatures per
/// clause, computed once at clause birth from the SAME memoized
/// per-atom data (`AtomInfos::info`) every probe's other side uses.
/// Stored beside [`ClauseFv`] on `ClauseRec` and consulted by
/// `forward_subsumed` BEFORE the feature-vector channels — each test is
/// one AND + compare, cheaper than the five `u16` comparisons.
///
/// Like the feature vector, both words are NECESSARY-condition filters:
/// a violated subset test soundly REJECTS "C subsumes D" (the exact
/// check would have said no); a passed test says nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ClauseBlooms {
    /// OR of `AtomInfo::leaf_sig` over the clause's literal atoms: one
    /// content-keyed bit per GROUND LEAF anywhere in the clause — head
    /// symbols, argument symbols / string / numeric literals,
    /// recursively through compound seats, INCLUDING ground leaves
    /// under open (variable-containing) compounds; variables and
    /// operator leaves (`Equal`) contribute nothing (see
    /// `fingerprint.rs::seat_meta`, which fixes those semantics for
    /// both sides of every probe).
    ///
    /// SOUNDNESS (leaf channel): if C subsumes D there is a σ with
    /// Cσ's literals a sub-multiset of D's.  Substitution only ever
    /// replaces VARIABLES, so every ground leaf of C survives verbatim
    /// into Cσ: ground-leaves(C) ⊆ ground-leaves(Cσ) ⊆
    /// ground-leaves(D).  `leaf_sig` is a superset signature of exactly
    /// those leaves, with bit derivation shared through the layer's
    /// `AtomInfos` memo — so leaf-subset implies bit-subset, and
    /// `leaf(C) & !leaf(D) == 0` is NECESSARY for subsumption.
    /// A set difference (`!= 0`) is therefore a sound rejection.
    pub(crate) leaf: u64,
    /// OR of [`glit_bit`] over the clause's FULLY GROUND literals
    /// (groundness decided exactly on the slot-form term — see
    /// [`Self::compute`] for why not `AtomInfo::is_ground`).  `0` when
    /// the clause has no ground literals — the channel is then
    /// inapplicable and the subset test passes vacuously.
    ///
    /// SOUNDNESS (ground-literal channel): a fully ground literal L of
    /// C is its own σ-image, so if C subsumes D then L appears VERBATIM
    /// among D's literals (same polarity, same canonical atom).  A
    /// verbatim-appearing ground literal is one of D's ground literals,
    /// so its `glit_bit` is set in D's word.  Hence every bit of C's
    /// word must appear in D's: `glit(C) & !glit(D) == 0` is NECESSARY;
    /// `!= 0` rejects soundly.
    pub(crate) glit: u64,
}

impl ClauseBlooms {
    /// Computed from the canonical literal list plus the parallel
    /// slot-form terms (`lits[i]` ↔ `terms[i]` — the invariant `make`
    /// establishes and debug-asserts).  Groundness for the `glit` word
    /// is decided on the TERM (`Term::is_ground`, exact) rather than
    /// `AtomInfo::is_ground`, whose seat mask silently treats seats
    /// ≥ `MAX_SEATS` as ground — a fine approximation for retrieval
    /// heuristics, but a (however astronomically rare) soundness hole
    /// for a rejection channel.
    pub(crate) fn compute(
        lits: &[PLit],
        terms: &[(bool, Term)],
        atom_info: impl Fn(AtomId) -> Arc<AtomInfo>,
    ) -> Self {
        debug_assert_eq!(lits.len(), terms.len());
        let mut leaf = 0u64;
        let mut glit = 0u64;
        for (l, (_, t)) in lits.iter().zip(terms) {
            leaf |= atom_info(l.atom).leaf_sig;
            if t.is_ground() {
                glit |= glit_bit(l.pos, l.atom);
            }
        }
        Self { leaf, glit }
    }

    /// [`Self::compute`] with the per-atom infos supplied directly
    /// (the hash-before-intern query side: transient infos computed
    /// from the slot terms, no residency).  Same bit derivation —
    /// `leaf_sig` from the info, groundness from the term.
    pub(crate) fn compute_from_infos(
        lits: &[PLit],
        terms: &[(bool, Term)],
        infos: &[AtomInfo],
    ) -> Self {
        debug_assert_eq!(lits.len(), terms.len());
        debug_assert_eq!(lits.len(), infos.len());
        let mut leaf = 0u64;
        let mut glit = 0u64;
        for ((l, (_, t)), info) in lits.iter().zip(terms).zip(infos) {
            leaf |= info.leaf_sig;
            if t.is_ground() {
                glit |= glit_bit(l.pos, l.atom);
            }
        }
        Self { leaf, glit }
    }
}

/// Packed per-clause record for `forward_subsumed`'s candidate scan —
/// the ONLY home of a clause's bloom words + feature vector + literal
/// count (moved off `ClauseRec`, not mirrored).  The scan gathers
/// candidates by id from the literal index, and with these four small
/// fields packed into one 32-byte record the whole
/// cheapest-first filter chain (length → leaf bloom → glit bloom → FV)
/// reads ONE cache line per candidate instead of pointer-chasing into
/// the ~200-byte `ClauseRec` (measured: candidate iteration was the
/// remaining forward-subsumption cost after the filters landed).
/// Lives in `NativeProver::subs`, indexed by clause id in lockstep with
/// the arena (`subs.len() == clauses.len()`, debug-asserted at every
/// write site).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SubsRec {
    /// Bloom prefilter words (leaf + ground-literal channels).
    pub(crate) blooms: ClauseBlooms,
    /// Feature-vector channels (#lits/#pos/#neg/size/KBO-weight).
    pub(crate) fv: ClauseFv,
    /// Exact literal count (`lits.len()`), NOT the saturated `fv[0]`
    /// channel — the `c.lits.len() > d.len()` pre-attempt filter must
    /// stay exact for clauses wider than `u16::MAX` literals (byte-
    /// identity discipline; width is practically capped far below, but
    /// the filter gates a stats counter, so no approximation here).
    pub(crate) nlits: u32,
}

/// The packing IS the point: two records per cache line (16 blooms +
/// 10 fv + 4 nlits, padded to 32).  If a field addition ever grows
/// this past 32 bytes, reconsider the layout instead of silently
/// spilling into a second line per candidate.
const _: () = assert!(std::mem::size_of::<SubsRec>() == 32);

/// Test-only: build a `ClauseFv` directly from slot-form terms (the
/// working-tree shape used by `clause_subsumes`/the subsumption unit
/// tests), for exercising monotonicity without standing up a full
/// `AtomTable`/`SyntacticLayer`/`KboOrdering` trio.
#[cfg(test)]
pub(crate) fn fv_from_terms(terms: &[(bool, Term)]) -> ClauseFv {
    let mut n_lits = 0u32;
    let mut n_pos = 0u32;
    let mut n_neg = 0u32;
    let mut size = 0u64;
    let mut weight = 0u64;
    fn term_size_and_weight(t: &Term) -> (u64, u64) {
        match t {
            Term::App(elems) => elems.iter().map(term_size_and_weight)
                .fold((0, 0), |(sa, wa), (sb, wb)| (sa + sb, wa + wb)),
            _ => (1, 1),
        }
    }
    for (pos, t) in terms {
        n_lits += 1;
        if *pos { n_pos += 1; } else { n_neg += 1; }
        let (s, w) = term_size_and_weight(t);
        size = size.saturating_add(s);
        weight = weight.saturating_add(w);
    }
    ClauseFv([
        sat_u16(n_lits as u64),
        sat_u16(n_pos as u64),
        sat_u16(n_neg as u64),
        sat_u16(size),
        sat_u16(weight),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Symbol;

    fn s(n: &str) -> Term { Term::Sym(Symbol::from(n)) }
    fn app(v: Vec<Term>) -> Term { Term::App(v) }
    fn v(n: u64) -> Term { Term::Var(n) }

    #[test]
    fn subset_instance_clause_pair_is_pointwise_le() {
        // sub = (p ?0)  vs  sup = (p a) — a single ground instance.
        // Every channel of `sub` must be <= `sup`'s (note: a RAW
        // distinct-variable channel would have failed this exact pair —
        // see the module-doc counterexample — which is why it isn't one
        // of the five channels).
        let sub = vec![(true, app(vec![s("p"), v(0)]))];
        let sup = vec![(true, app(vec![s("p"), s("a")]))];
        let fv_sub = fv_from_terms(&sub);
        let fv_sup = fv_from_terms(&sup);
        assert!(fv_sub.le(&fv_sup), "{:?} vs {:?}", fv_sub, fv_sup);
        assert!(super::super::clause_subsumes(&sub, &sup));
    }

    #[test]
    fn multi_literal_subset_pair_is_pointwise_le() {
        let sub = vec![
            (false, app(vec![s("q"), v(0)])),
            (true,  app(vec![s("p"), v(0)])),
        ];
        let sup = vec![
            (false, app(vec![s("q"), s("a")])),
            (true,  app(vec![s("p"), s("a")])),
            (true,  app(vec![s("r"), s("b")])),
        ];
        let fv_sub = fv_from_terms(&sub);
        let fv_sup = fv_from_terms(&sup);
        assert!(fv_sub.le(&fv_sup));
        assert!(super::super::clause_subsumes(&sub, &sup));
    }

    #[test]
    fn variable_expanding_match_is_pointwise_le() {
        // sub = (p ?0)  vs  sup = (p (f ?1 ?2)) — the OTHER
        // counterexample direction from the module doc (sub has fewer
        // distinct variables than sup here); confirms the five real
        // channels don't smuggle back a variable-count assumption.
        let sub = vec![(true, app(vec![s("p"), v(0)]))];
        let sup = vec![(true, app(vec![s("p"), app(vec![s("f"), v(1), v(2)])]))];
        let fv_sub = fv_from_terms(&sub);
        let fv_sup = fv_from_terms(&sup);
        assert!(fv_sub.le(&fv_sup), "{:?} vs {:?}", fv_sub, fv_sup);
        assert!(super::super::clause_subsumes(&sub, &sup));
    }

    #[test]
    fn rejected_pair_fails_the_pointwise_check_and_would_fail_clause_subsumes() {
        // sub has MORE literals than sup ⇒ #lits channel alone rejects
        // it, and (independently) `clause_subsumes` agrees: len check
        // fails outright (`sub.len() > sup.len()`).
        let sub = vec![
            (true, app(vec![s("p"), v(0)])),
            (true, app(vec![s("q"), v(0)])),
        ];
        let sup = vec![(true, app(vec![s("p"), s("a")]))];
        let fv_sub = fv_from_terms(&sub);
        let fv_sup = fv_from_terms(&sup);
        assert!(!fv_sub.le(&fv_sup), "expected rejection: {:?} vs {:?}", fv_sub, fv_sup);
        // Twin-check: the exact routine must also say no.
        assert!(!super::super::clause_subsumes(&sub, &sup));
    }

    #[test]
    fn polarity_mismatch_pair_is_rejected_by_channel_and_by_clause_subsumes() {
        // Same shape, opposite polarity: #pos/#neg channels disagree, so
        // the prefilter must reject even though #lits/size/weight match.
        let sub = vec![(true, app(vec![s("p"), v(0)]))];
        let sup = vec![(false, app(vec![s("p"), s("a")]))];
        let fv_sub = fv_from_terms(&sub);
        let fv_sup = fv_from_terms(&sup);
        assert!(!fv_sub.le(&fv_sup));
        assert!(!super::super::clause_subsumes(&sub, &sup));
    }

    #[test]
    fn heavier_subsumer_is_rejected_by_weight_channel() {
        // sub is a HEAVIER ground unit than sup's corresponding atom
        // shape ⇒ KBO-weight (and size) channels reject (sub can't be a
        // generalization of something lighter than it).
        let sub = vec![(true, app(vec![s("p"), app(vec![s("f"), s("a")])]))];
        let sup = vec![(true, app(vec![s("p"), s("b")]))];
        let fv_sub = fv_from_terms(&sub);
        let fv_sup = fv_from_terms(&sup);
        assert!(!fv_sub.le(&fv_sup));
        assert!(!super::super::clause_subsumes(&sub, &sup));
    }
}
