// crates/core/src/prover/saturate/terms.rs
//
// Prover-side ground-term facts memo — tier 2 of the two-tier ground-term
// identity design (tier 1 is `syntactic::term_facts`, riding the sentence
// store's hash-cons).
//
// KEYSPACE ([VERIFY] resolved): `TermKey` IS the shared content-hash
// keyspace.  `AtomTable::intern_atom` assigns ids with the exact store
// hash (`Sentence::hash` → `content_hash`), and `AtomTable::term_of`
// lifts `Element::Sub(sid)` boundaries carrying those sids — so a
// KB-origin ground subtree, a prover-interned atom, and a derived ground
// `Term` hashed here all land on ONE u64 key.  Derived-term keys are
// computed with the store's own byte scheme through the shared
// [`ElementHasher`] (`syntactic::sentence::hash`), no interning needed:
// `ground_key_facts(t).0 == atoms.intern_atom(t)` for every ground
// compound `t` (property-tested below).
//
// V1 BOUNDARY: open (non-ground) terms are never keyed or memoized —
// every `Option` return below is `None` exactly when a variable occurs.
//
// DESIGN DELTA (documented): the work order sketched `kbo_weight:
// SmallVec<[(u64 seed, u64 w); 1]>` keyed by `(TermKey, prec_seed)`.
// Verified against kbo.rs: `prec_seed` permutes PRECEDENCE only — the
// weight table is seed-independent (`with_prec_seed` keeps the default
// uniform weights; `set_weight` is test-only), stated by the field docs
// ("the weight memo is precedence-independent").  A per-seed weight list
// would be dead structure, so `kbo_weight` is a single `u64` computed
// with the layer KBO's weight table; the debug twins in `demod_oriented`
// / `equality_oriented` / the FVI channel assert it equals the active
// (possibly prec-seeded) KBO's memoized weight on every fast-path use.

use dashmap::DashMap;

use crate::syntactic::caches::term_facts::{bloom_bit_op, bloom_bit_symbol};
use crate::syntactic::sentence::ElementHasher;
use crate::syntactic::SyntacticLayer;
use crate::types::Element;

use super::clause::{AtomId, AtomTable, Term};
use super::hash64::BuildContentHasher;
use super::kbo::KboOrdering;

/// A ground term's content-hash identity — the same 64-bit keyspace as
/// `SentenceId` / `AtomId` (see module docs).
pub(crate) type TermKey = u64;

/// One recorded demodulation normal-form outcome (the Part-4 memo,
/// owned per prover run — validity is generation-scoped).  Keyed by the
/// ORIGINAL subtree's `TermKey`; `gen` must equal the demod index's
/// current generation to be usable (older entries are lazily discarded:
/// a newly registered rule can enable further rewrites, so an old NF is
/// no longer known-normal — retirement never bumps, see
/// `DemodIndex::generation`).
#[derive(Debug, Clone)]
pub(crate) struct NfEntry {
    /// `DemodIndex::generation()` at record time.
    pub(crate) gen: u32,
    /// The demodulator clause id of EVERY rewrite applied on the way to
    /// the normal form, in application order — replayed into the proof
    /// DAG (`parents`) and the demod-cap accounting on a splice.  Empty
    /// ⇔ the term was already normal.
    pub(crate) used: smallvec::SmallVec<[u32; 4]>,
    /// The owned normal form, for splicing (one clone per hit).
    /// `None` ⇔ unchanged (`used` empty).
    pub(crate) term: Option<Term>,
}

/// Structural + weight facts of one GROUND term, memoized by content key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PTermFacts {
    /// Leaf count (saturating) — `AtomInfo.size` semantics.
    pub(crate) size: u16,
    /// Term depth (saturating): leaf = 0, flat compound = 1.
    pub(crate) depth: u8,
    /// OR of `1 << (symbol_key % 64)` over every symbol and operator in
    /// the tree — same bit derivation as tier 1 and as `DemodIndex`'s
    /// registered head-bit mask, so an empty intersection is a proof
    /// that no node in the subtree carries any demodulator's head key.
    pub(crate) sym_bloom: u64,
    /// KBO weight (Σ leaf weights; ground ⇒ no variable contribution).
    /// Seed-independent — see the module-docs delta note.
    pub(crate) kbo_weight: u64,
}

/// Layer-owned memo: `TermKey -> PTermFacts`, content-addressed and
/// therefore permanent (facts can never go stale, only unreferenced) —
/// the same contract as `AtomInfos` beside it on `ProverLayer`.
#[derive(Debug, Default)]
pub(crate) struct TermFactsTable {
    map: DashMap<TermKey, PTermFacts, BuildContentHasher>,
}

impl TermFactsTable {
    /// Number of memoized ground terms (tests / diagnostics).
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    /// Facts of a ground term, WITHOUT computing its key when the root
    /// is a leaf (leaves have O(1) facts and no compound key).  `None`
    /// when `t` contains a variable.  Compound sub-nodes memoize under
    /// their own keys on the way up.
    pub(crate) fn ground_facts(&self, t: &Term, kbo: &KboOrdering) -> Option<PTermFacts> {
        match t {
            Term::App(_) => self.ground_key_facts(t, kbo).map(|(_, f)| f),
            Term::Var(_) => None,
            leaf => Some(leaf_facts(leaf, kbo)),
        }
    }

    /// The content key + facts of a ground COMPOUND term, computed in
    /// one bottom-up walk (children keys inline; every compound
    /// sub-node memoized on the way up).  `None` when `t` is not an
    /// `App` or contains a variable.  The key equals what
    /// `AtomTable::intern_atom(t)` would assign — no interning happens.
    pub(crate) fn ground_key_facts(&self, t: &Term, kbo: &KboOrdering) -> Option<(TermKey, PTermFacts)> {
        let Term::App(elems) = t else { return None };
        let mut h = ElementHasher::new(elems.len());
        let mut size: u16 = 0;
        let mut depth: u8 = 0;
        let mut sym_bloom = 0u64;
        let mut kbo_weight = 0u64;
        for e in elems {
            match e {
                Term::Var(_) => return None, // open: never keyed or memoized
                Term::App(_) => {
                    let (k, f) = self.ground_key_facts(e, kbo)?;
                    h.sub(k);
                    size = size.saturating_add(f.size);
                    depth = depth.max(f.depth);
                    sym_bloom |= f.sym_bloom;
                    kbo_weight = kbo_weight.saturating_add(f.kbo_weight);
                }
                leaf => {
                    match leaf {
                        Term::Sym(s) => h.symbol(s.id()),
                        Term::Lit(l) => h.literal(l),
                        Term::Op(op) => h.op(op),
                        _ => unreachable!("Var/App handled above"),
                    }
                    let f = leaf_facts(leaf, kbo);
                    size = size.saturating_add(f.size);
                    sym_bloom |= f.sym_bloom;
                    kbo_weight = kbo_weight.saturating_add(f.kbo_weight);
                }
            }
        }
        let key = h.finish();
        let facts = PTermFacts { size, depth: depth.saturating_add(1), sym_bloom, kbo_weight };
        // Read-first: ground subterms recur constantly, so most calls here
        // find the key already memoized — `.entry()` alone would take the
        // shard's write lock even on a hit. See `AtomTable::intern_atom`'s
        // matching comment; the race with a concurrent first-insert is
        // benign (facts are a pure function of `key`'s content).
        if !self.map.contains_key(&key) {
            self.map.entry(key).or_insert(facts);
        }
        Some((key, facts))
    }

    /// Facts of an INTERNED ground atom/subterm by id — the by-key
    /// probe path (FVI weight channel, tier-1-origin subtrees).  Probes
    /// first, so recurring ids never re-walk; a miss resolves through
    /// the atom table (store fall-back) and recurses by child sid, each
    /// child probing its own entry.  `None` when the content is open or
    /// the id is unresolvable.
    pub(crate) fn facts_for_atom(
        &self,
        id:    AtomId,
        atoms: &AtomTable,
        syn:   &SyntacticLayer,
        kbo:   &KboOrdering,
    ) -> Option<PTermFacts> {
        if let Some(hit) = self.map.get(&id) {
            return Some(*hit.value());
        }
        let sent = atoms.resolve(id, syn)?;
        let mut size: u16 = 0;
        let mut depth: u8 = 0;
        let mut sym_bloom = 0u64;
        let mut kbo_weight = 0u64;
        for el in sent.elements.iter() {
            match el {
                Element::Variable { .. } => return None, // open: v1 boundary
                Element::Sub(sub) => {
                    let f = self.facts_for_atom(*sub, atoms, syn, kbo)?;
                    size = size.saturating_add(f.size);
                    depth = depth.max(f.depth);
                    sym_bloom |= f.sym_bloom;
                    kbo_weight = kbo_weight.saturating_add(f.kbo_weight);
                }
                Element::Symbol(s) => {
                    size = size.saturating_add(1);
                    sym_bloom |= bloom_bit_symbol(s.id());
                    kbo_weight = kbo_weight.saturating_add(kbo.element_leaf_weight(el));
                }
                Element::Literal(_) => {
                    size = size.saturating_add(1);
                    kbo_weight = kbo_weight.saturating_add(kbo.element_leaf_weight(el));
                }
                Element::Op(op) => {
                    size = size.saturating_add(1);
                    sym_bloom |= bloom_bit_op(op);
                    kbo_weight = kbo_weight.saturating_add(kbo.element_leaf_weight(el));
                }
            }
        }
        let facts = PTermFacts { size, depth: depth.saturating_add(1), sym_bloom, kbo_weight };
        // Tier-1 cross-check (debug builds only): a store-resident sid's
        // structural facts must agree with `syntactic::term_facts`.
        #[cfg(any(test, debug_assertions))]
        if syn.has_sentence(id) {
            let t1 = syn.term_facts(id);
            debug_assert!(
                t1.ground
                    && t1.size == facts.size
                    && t1.depth == facts.depth
                    && t1.sym_bloom == facts.sym_bloom,
                "tier-1/tier-2 term-facts divergence for sid {id:#x}: {t1:?} vs {facts:?}",
            );
        }
        self.map.entry(id).or_insert(facts);
        Some(facts)
    }
}

/// O(1) facts of a ground LEAF term (symbols, literals, operators).
fn leaf_facts(t: &Term, kbo: &KboOrdering) -> PTermFacts {
    let sym_bloom = match t {
        Term::Sym(s) => bloom_bit_symbol(s.id()),
        Term::Op(op) => bloom_bit_op(op),
        _ => 0, // literals never key a demodulator bucket
    };
    PTermFacts { size: 1, depth: 0, sym_bloom, kbo_weight: kbo.term_leaf_weight(t) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Literal, Symbol};

    fn sym(name: &str) -> Term {
        Term::Sym(Symbol::from(name))
    }
    fn num(v: &str) -> Term {
        Term::Lit(Literal::Number(v.to_string()))
    }
    fn app(v: Vec<Term>) -> Term {
        Term::App(v)
    }

    /// Brute-force twins over raw terms (no memo, no hashing).
    fn brute_size(t: &Term) -> u16 {
        match t {
            Term::App(e) => e.iter().map(brute_size).fold(0u16, u16::saturating_add),
            _ => 1,
        }
    }
    fn brute_depth(t: &Term) -> u8 {
        match t {
            Term::App(e) => 1 + e.iter().map(brute_depth).max().unwrap_or(0),
            _ => 0,
        }
    }
    fn brute_bloom(t: &Term) -> u64 {
        match t {
            Term::App(e) => e.iter().map(brute_bloom).fold(0, |a, b| a | b),
            Term::Sym(s) => crate::syntactic::caches::term_facts::bloom_bit_symbol(s.id()),
            Term::Op(op) => crate::syntactic::caches::term_facts::bloom_bit_op(op),
            _ => 0,
        }
    }

    /// A small fixture set: flat / nested / literal-carrying / op-headed
    /// ground terms (the shapes the demod walk actually meets).
    fn fixtures() -> Vec<Term> {
        vec![
            app(vec![sym("f"), sym("a")]),
            app(vec![sym("f"), sym("a"), sym("b")]),
            app(vec![sym("g"), app(vec![sym("f"), sym("a")]), sym("c")]),
            app(vec![sym("h"), app(vec![sym("g"), app(vec![sym("f"), sym("a")]), sym("c")])]),
            app(vec![sym("MeasureFn"), num("3"), sym("Meter")]),
            app(vec![
                Term::Op(crate::parse::OpKind::Equal),
                app(vec![sym("f"), sym("a")]),
                sym("b"),
            ]),
        ]
    }

    // The PTermFacts walk must agree with a direct recompute on every
    // fixture — and the computed key must equal the id `intern_atom`
    // assigns the identical term (the shared-keyspace property).
    #[test]
    fn ptermfacts_walk_matches_direct_recompute_and_intern_key() {
        let table = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let atoms = AtomTable::default();
        let syn = SyntacticLayer::default();
        for t in fixtures() {
            let (key, f) = table.ground_key_facts(&t, &kbo).expect("fixtures are ground");
            assert_eq!(f.size, brute_size(&t), "size for {t:?}");
            assert_eq!(f.depth, brute_depth(&t), "depth for {t:?}");
            assert_eq!(f.sym_bloom, brute_bloom(&t), "bloom for {t:?}");
            // Shared keyspace: the walk's key IS the intern id.
            let id = atoms.intern_atom(&t);
            assert_eq!(key, id, "content key must equal intern_atom id for {t:?}");
            // Weight agrees with the KBO memo on the interned atom.
            assert_eq!(
                f.kbo_weight,
                kbo.info(id, &atoms, &syn).weight,
                "kbo weight for {t:?}",
            );
            // The by-id path returns the identical facts (memo or walk).
            let by_id = table.facts_for_atom(id, &atoms, &syn, &kbo).expect("ground");
            assert_eq!(by_id, f, "by-id facts for {t:?}");
        }
    }

    #[test]
    fn open_terms_are_never_keyed_or_memoized() {
        let table = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let open = app(vec![sym("f"), Term::Var(0)]);
        assert!(table.ground_key_facts(&open, &kbo).is_none());
        assert!(table.ground_facts(&Term::Var(3), &kbo).is_none());
        // Nested variable anywhere poisons the whole tree.
        let nested = app(vec![sym("g"), app(vec![sym("f"), Term::Var(1)]), sym("c")]);
        assert!(table.ground_key_facts(&nested, &kbo).is_none());
        assert_eq!(table.len(), 0, "no entry may be recorded for open probes");
    }

    #[test]
    fn compound_subnodes_memoize_on_the_way_up() {
        let table = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let inner = app(vec![sym("f"), sym("a")]);
        let outer = app(vec![sym("g"), inner.clone(), sym("c")]);
        let _ = table.ground_key_facts(&outer, &kbo);
        // Both the root and the inner compound must be present.
        assert_eq!(table.len(), 2);
        let atoms = AtomTable::default();
        let inner_id = atoms.intern_atom(&inner);
        assert!(table.map.get(&inner_id).is_some(), "inner sub-node memoized under its own key");
    }

    // The op bloom key must stay byte-for-byte equal to the demodulator
    // index's op bucket key (`units::op_tag`) — the cross-checked seam
    // the whole-subtree prune relies on.
    #[test]
    fn op_bloom_key_matches_units_op_tag() {
        use crate::parse::OpKind::*;
        for op in [And, Or, Not, Implies, Iff, Equal, ForAll, Exists] {
            assert_eq!(
                crate::syntactic::caches::term_facts::op_bloom_key(&op),
                u64::from(super::super::units::op_tag(&op)),
                "op {op:?}",
            );
        }
    }
}
