// crates/core/src/prover/saturate/prover/make.rs
//
// Clause construction: `make` simplifies, canonicalizes, and registers
// a raw literal list (arithmetic + ground-equality normalization,
// forward demodulation, oracle theory propagation, depth/size caps,
// unit subsumption, schema absorption, forward subsumption, tier
// weighting) -- plus its immediate private helpers (the demodulator
// index, the ground-equality decision procedure, FD-equality /
// list-theory drain, and the equality union-find registration).

use smallvec::SmallVec;

use crate::parse::OpKind;
use crate::types::{Element, Literal, SentenceId, Symbol, SymbolId};

use super::super::canon::{blank_key, canonical_clause};
use super::super::clause::{AtomId, PClause, Term};
use super::super::hash64::Set64;
use super::super::kbo::KboCmp;
use super::super::oracle::Witness;
use super::super::unify::{apply, match_one_way, shift_slots, slot_atom, Subst};
use super::{
    arith_norm, classify_seats, eq_key, eq_sides,
    is_equality_atom, lit_kif, max_slot, replace, stepdbg, term_binary_ids,
    term_depth, term_ground_equality_sides, term_head_key, term_kif, term_size,
    term_skolem_apps, witnesses_kif, ClauseRec, NativeProver, BACKGROUND, CONJECTURE,
    MATCH_TARGET_OFF, SUPPORT,
};

/// Verdict of the ground-equality decision procedure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EqDecision {
    /// Provably equal (closure / identical / equal numeric values).
    Entailed,
    /// Provably UNEQUAL (distinct numeric literal values only).
    Refuted,
    /// Neither provable — ordinary search decides.
    Unknown,
}

impl<'a> NativeProver<'a> {
    /// Rewrite every ground constant in `t` to its equality-class
    /// representative, IN PLACE — touched nodes are replaced, untouched
    /// subtrees are never rebuilt, and the no-equalities case (most
    /// runs, most literals) is a single branch.
    fn normalize_eq(&self, t: &mut Term) {
        if !self.oracle.has_equalities() {
            return;
        }
        self.normalize_eq_rec(t);
    }

    fn normalize_eq_rec(&self, t: &mut Term) {
        match t {
            Term::Sym(_) | Term::Lit(Literal::Number(_)) => {
                let Some(key) = eq_key(t) else { return };
                let rep = self.oracle.eq_rep(key);
                if rep == key {
                    return;
                }
                if let Some(r) = self.eq_terms.get(&rep) {
                    *t = r.clone();
                } else if let Some(sym) = self.syn().sym_name(rep) {
                    *t = Term::Sym(sym);
                }
            }
            Term::App(elems) => {
                for e in elems.iter_mut() {
                    self.normalize_eq_rec(e);
                }
                if self.has_compound_eqs
                    && t.is_ground()
                    && term_size(t) <= self.opts.strategy.max_term_size
                {
                    let key = self.layer.atoms.intern_atom(t);
                    let rep = self.oracle.eq_rep(key);
                    if rep != key {
                        if let Some(r) = self.eq_terms.get(&rep) {
                            *t = r.clone();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Forward demodulation: rewrite `t` to KBO normal form using the
    /// indexed oriented unit equations ([`Self::demods`], populated at
    /// activation).  For a demodulator `l → r` (with `l >_kbo r`, decided
    /// once at registration and stable under substitution) we rewrite a
    /// subterm matching `l` under σ to `rσ` — so every rewrite is
    /// strictly downhill in a well-founded order (it terminates) and
    /// sound (equals for equals).  Unlike `paramodulants` (which UNIFIES
    /// and keeps the parent), this MATCHES one-way (binds the rule's
    /// variables only) and the rewritten clause replaces the original —
    /// a simplification.  Per subterm position, candidates come from one
    /// head-shape hash probe instead of a scan of every active equation
    /// (the scan was the measured TPTP regression that kept `demod`
    /// off — see `strategy.rs`).  Demodulator clause ids are pushed to
    /// `used` for the proof DAG.
    fn demodulate(&mut self, t: &mut Term, used: &mut Vec<u32>) -> u64 {
        if !self.opts.strategy.demod || self.demods.is_empty() {
            return 0;
        }
        // Cap total rewrites per term — a guard, not the terminator
        // (KBO already guarantees termination); bounds pathological
        // fan-out on huge clauses.  Parameterized (default 64).
        let demod_cap = self.opts.strategy.demod_cap.max(1);
        let mut rewrites = 0u64;
        'fixpoint: loop {
            if rewrites >= demod_cap {
                break;
            }
            // Shift the demodulator's slots clear of the target's, so
            // one-way matching never confuses a rule variable with a
            // target variable (mirrors `paramodulants`' offset trick).
            let off = max_slot(t).map_or(0, |m| m + 1);
            let hit = self.find_demod_redex(t, off);
            // Soundness cross-check (debug builds / tests only, zero cost
            // in release): the prefilter must never change WHAT
            // `demodulate` finds, only how cheaply it finds it.  Re-walk
            // with the prefilter bypassed and require byte-identical
            // results (same redex, or both `None`).
            #[cfg(any(test, debug_assertions))]
            {
                let reference = self.find_demod_redex_unfiltered(t, off);
                debug_assert_eq!(
                    hit, reference,
                    "SYMBOL-SIGNATURE prefilter changed demodulate's result \
                     (prefiltered {:?} vs unfiltered {:?}) for term {:?}",
                    hit, reference, t,
                );
            }
            if let Some((path, rr, clause)) = hit {
                *t = replace(t, &path, &rr);
                used.push(clause);
                rewrites += 1;
                // The term changed; restart the scan from the top (a
                // rewrite can expose new redexes / new `off`).
                continue 'fixpoint;
            }
            break;
        }
        rewrites
    }

    /// One pass over `t`'s non-variable subterm positions (heads
    /// skipped, same traversal `positions` performs) looking for the
    /// first demodulation redex, returning its path, replacement, and
    /// owning clause.  Fused with the SYMBOL-SIGNATURE prefilter: each
    /// visited node's head key is checked against the index's bucket
    /// set (`DemodIndex::possibly_matches`, O(1)) BEFORE the subterm is
    /// cloned or a match probe is built — a subterm whose head shape has
    /// no indexed demodulator can never produce a match, so the clone
    /// (`sub`), the `shift_slots`/`Subst` allocation, and the match walk
    /// are all skipped outright for it.
    ///
    /// Per-NODE, not per-subtree: a parent's head key says nothing about
    /// its children's (a rewrite site can sit arbitrarily deep under an
    /// unrelated head), so a negative prefilter on a node still recurses
    /// into its children — it only skips THAT node's own probe.  `Term`
    /// carries no per-term symbol-set fingerprint to cache (checked:
    /// the `gf64`/schema fingerprints in `schema.rs` key ATOM shapes for
    /// the schema/open-unit indexes, not a generic per-subterm symbol
    /// multiset), so subtree-level pruning is not soundly available
    /// here — see the module docs on `DemodIndex::possibly_matches`.
    ///
    /// Counts every visited node into `self.stats.demod_scans_skipped_
    /// by_prefilter` (prefilter said no) or `self.stats.demod_scans_
    /// performed` (passed the prefilter, handed to the candidate loop),
    /// so `demodulate`'s behavior is externally observable without
    /// changing what it returns.
    fn find_demod_redex(&mut self, atom: &Term, off: u64) -> Option<(Vec<usize>, Term, u32)> {
        fn walk(
            this: &mut NativeProver<'_>,
            t: &Term,
            path: &mut Vec<usize>,
            off: u64,
        ) -> Option<(Vec<usize>, Term, u32)> {
            if let Term::App(elems) = t {
                for (i, e) in elems.iter().enumerate().skip(1) {
                    path.push(i);
                    if let Some(hit) = walk(this, e, path, off) {
                        return Some(hit);
                    }
                    path.pop();
                }
            }
            if path.is_empty() || matches!(t, Term::Var(_)) {
                return None;
            }
            if !this.demods.possibly_matches(t) {
                this.stats.demod_scans_skipped_by_prefilter += 1;
                return None;
            }
            this.stats.demod_scans_performed += 1;
            // Only now — having passed the O(1) shape check — clone the
            // subterm and build the match probe.
            let sub = t.clone();
            let cands = this.demods.candidates(&sub)?;
            for d in cands {
                let l2 = shift_slots(&d.l, off);
                let mut s: Subst = vec![None; (off + u64::from(d.nslots)) as usize + 1];
                if match_one_way(&l2, &sub, &mut s) {
                    // r's variables ⊆ l's (KBO variable condition), so
                    // the match bound everything r mentions.
                    let rr = apply(&shift_slots(&d.r, off), &s);
                    return Some((path.clone(), rr, d.clause));
                }
            }
            None
        }
        let mut path = Vec::new();
        walk(self, atom, &mut path, off)
    }

    /// Reference (unprefiltered) twin of [`Self::find_demod_redex`]:
    /// identical traversal and match logic, but every visited node is
    /// unconditionally handed to `self.demods.candidates` — no
    /// `possibly_matches` gate, no stats bump.  Exists ONLY for the
    /// `debug_assert_eq!` cross-check in `demodulate` (debug/test builds)
    /// that proves the prefilter is a pure performance change: compiled
    /// out of release builds, so it costs nothing in the timed gates.
    #[cfg(any(test, debug_assertions))]
    fn find_demod_redex_unfiltered(&self, atom: &Term, off: u64) -> Option<(Vec<usize>, Term, u32)> {
        fn walk(
            this: &NativeProver<'_>,
            t: &Term,
            path: &mut Vec<usize>,
            off: u64,
        ) -> Option<(Vec<usize>, Term, u32)> {
            if let Term::App(elems) = t {
                for (i, e) in elems.iter().enumerate().skip(1) {
                    path.push(i);
                    if let Some(hit) = walk(this, e, path, off) {
                        return Some(hit);
                    }
                    path.pop();
                }
            }
            if path.is_empty() || matches!(t, Term::Var(_)) {
                return None;
            }
            let sub = t.clone();
            let cands = this.demods.candidates(&sub)?;
            for d in cands {
                let l2 = shift_slots(&d.l, off);
                let mut s: Subst = vec![None; (off + u64::from(d.nslots)) as usize + 1];
                if match_one_way(&l2, &sub, &mut s) {
                    let rr = apply(&shift_slots(&d.r, off), &s);
                    return Some((path.clone(), rr, d.clause));
                }
            }
            None
        }
        let mut path = Vec::new();
        walk(self, atom, &mut path, off)
    }

    /// Register clause `id` as a forward demodulator if it is a positive
    /// unit equality with a KBO-strictly-oriented side.  Called at
    /// ACTIVATION (every path: background load, support load, the
    /// given-clause loop, background completion) — the same moment the
    /// unit stores register it — and from the hydrate/mask rebuilds.
    /// At most one direction can be strictly greater, so at most one
    /// entry per equation.
    pub(super) fn index_demodulator(&mut self, id: u32) {
        let (pos, atom) = {
            let c = &self.clauses[id as usize];
            if c.lits.len() != 1 {
                return;
            }
            (c.lits[0].pos, c.lits[0].atom)
        };
        if !pos {
            return;
        }
        let Some(t) = slot_atom(&self.layer.atoms, self.syn(), atom, 0) else { return };
        let Some((a, b)) = eq_sides(&t) else { return };
        if a == b {
            return;
        }
        for (l, r) in [(&a, &b), (&b, &a)] {
            // A bare-variable left side rewrites everything — never a
            // sound demodulator (and never KBO-greater); skip.
            if matches!(l, Term::Var(_)) {
                continue;
            }
            if self.demod_oriented(l, r) {
                self.demods.add(id, l.clone(), r.clone());
                return;
            }
        }
    }

    /// Rebuild the demodulator index from the activated arena — the
    /// hydrate-path peer of `rebuild_superposition_index` (orientation
    /// depends on THIS run's KBO, so a frozen index cannot be trusted
    /// across strategies).
    pub(super) fn rebuild_demod_index(&mut self) {
        self.demods.clear();
        let n = self.clauses.len() as u32;
        for id in 0..n {
            if self.clauses[id as usize].activated {
                self.index_demodulator(id);
            }
        }
    }

    /// Whether `(equal l r)` is a sound left-to-right demodulator: `l`
    /// strictly greater than `r` in the layer's KBO.  Stable under
    /// substitution, so the single check licenses rewriting every
    /// matched instance.  Both sides intern (content-addressed, cheap)
    /// and the comparison is memoized.
    fn demod_oriented(&self, l: &Term, r: &Term) -> bool {
        let la = self.layer.atoms.intern_atom(l);
        let ra = self.layer.atoms.intern_atom(r);
        matches!(
            self.kbo().compare(la, ra, &self.layer.atoms, self.syn()),
            KboCmp::Greater
        )
    }

    /// Synthesize the concrete subrelation rule `(=> (R ?x ?y) (S ?x ?y))`
    /// for every `(subrelation R S)` ground fact in `clauses`, adding it
    /// as an activated BACKGROUND clause.  This is the first-order
    /// instantiation of SUMO's second-order subrelation schema for the
    /// relations actually present: it lets resolution chain subrelation
    /// inheritance directly (binding open `(part ?N ?A)` literals from
    /// `(component ?N ?A)` facts) instead of branching through the
    /// predicate-variable seat-0 bucket, which otherwise explodes.
    pub(crate) fn synthesize_subrelation_rules(&mut self, clauses: &[PClause]) {
        let subrel = self.oracle.roles().subrelation;
        let mut pairs: Vec<(Symbol, Symbol)> = Vec::new();
        for c in clauses {
            if c.lits.len() != 1 || !c.lits[0].pos {
                continue;
            }
            let Some(sent) = self.layer.atoms.resolve(c.lits[0].atom, self.syn()) else { continue };
            if sent.elements.len() != 3 {
                continue;
            }
            match (sent.elements.first(), &sent.elements[1], &sent.elements[2]) {
                (Some(Element::Symbol(h)), Element::Symbol(r), Element::Symbol(s))
                    if h.id() == subrel && r.id() != s.id() =>
                {
                    pairs.push((r.0.clone(), s.0.clone()));
                }
                _ => {}
            }
        }
        for (r, s) in pairs {
            if std::env::var_os("SIGMA_ORACLE_TRACE").is_some() {
                eprintln!("SUBREL-SCHEMA {} -> {}", r.name(), s.name());
            }
            let body = Term::App(vec![Term::Sym(r), Term::Var(0), Term::Var(1)]);
            let head = Term::App(vec![Term::Sym(s), Term::Var(0), Term::Var(1)]);
            if let Some(id) =
                self.make(vec![(false, body), (true, head)], vec![], "subrel_schema", BACKGROUND, None, false)
            {
                let key = self.clauses[id as usize].key;
                if self.clauses[id as usize].lits.len() <= self.opts.max_lits
                    && self.seen.insert(key)
                {
                    self.activate(id);
                }
            }
        }
    }

    /// Queue theory units for every NEW ground `(ListFn …)` subterm of
    /// `t`: `(inList mᵢ L)` and `(equal mᵢ (ListOrderFn L i))` (1-based,
    /// SUMO's ListOrderFn convention).
    fn collect_ground_lists(&mut self, t: &Term) {
        let Term::App(elems) = t else { return };
        for el in elems {
            self.collect_ground_lists(el);
        }
        if !matches!(elems.first(), Some(Term::Sym(h)) if &*h.name() == "ListFn") {
            return;
        }
        if elems.len() < 2 || elems.len() > 16 || !t.is_ground() {
            return;
        }
        // LEAF members only: a ground list of compounds (nested lists,
        // function terms) is the signature of SUMO's generative list
        // machinery — synthesizing its extension feeds the loop that
        // grows new lists forever.
        if elems.iter().skip(1).any(|m| matches!(m, Term::App(_))) {
            return;
        }
        // Global cap: list theory is for the handful of lists a
        // problem mentions, not a list-universe enumeration.
        if self.lists_done.len() >= 64 {
            return;
        }
        let key = self.layer.atoms.intern_atom(t);
        if !self.lists_done.insert(key) {
            return;
        }
        let in_list = crate::types::Symbol::from("inList");
        let order_fn = crate::types::Symbol::from("ListOrderFn");
        for (i, m) in elems.iter().enumerate().skip(1) {
            self.pending_list_units.push(Term::App(vec![
                Term::Sym(in_list.clone()), m.clone(), t.clone(),
            ]));
            self.pending_list_units.push(Term::App(vec![
                Term::Op(OpKind::Equal),
                m.clone(),
                Term::App(vec![
                    Term::Sym(order_fn.clone()),
                    t.clone(),
                    Term::Lit(Literal::Number(i.to_string())),
                ]),
            ]));
        }
    }

    /// Surface FD-derived equalities as activated `(equal a b)` unit
    /// clauses, with the deriving clauses + uniqueness axiom as proof
    /// parents — so they resolve and paramodulate like any equality
    /// and the transcript shows where they came from.
    pub(crate) fn drain_fd_equalities(&mut self) {
        // Ground-list theory units → activated background facts.
        let list_units = std::mem::take(&mut self.pending_list_units);
        for term in list_units {
            let made = self.make(
                vec![(true, term)], Vec::new(), "list_theory", BACKGROUND, None, true);
            let Some(id) = made else { continue };
            let key = self.clauses[id as usize].key;
            if self.seen.insert(key) {
                self.activate(id);
                self.push(Some(id));
            }
        }
        // Exhaustiveness-derived positive facts → activated units.
        for (rel, x, y, just) in self.oracle.take_pending_facts() {
            let term_of = |key: u64| -> Option<Term> {
                self.eq_terms.get(&key).cloned()
                    .or_else(|| self.syn().sym_name(key).map(Term::Sym))
            };
            let (Some(tr), Some(tx), Some(ty)) = (term_of(rel), term_of(x), term_of(y))
            else { continue };
            let term = Term::App(vec![tr, tx, ty]);
            let made = self.make(
                vec![(true, term)], just.clause_parents.clone(),
                "exhaustive", SUPPORT, None, true);
            let Some(id) = made else { continue };
            self.clauses[id as usize].fact_parents.extend(just.fact_sids.iter().copied());
            if let Some(ax) = just.axiom {
                self.clauses[id as usize].fact_parents.push(ax);
            }
            let key = self.clauses[id as usize].key;
            if self.seen.insert(key) {
                self.activate(id);
                self.push(Some(id));
            }
        }
        for (a, b, just) in self.oracle.take_pending_eq() {
            let term_of = |key: u64| -> Option<Term> {
                self.eq_terms.get(&key).cloned()
                    .or_else(|| self.syn().sym_name(key).map(Term::Sym))
            };
            let (Some(ta), Some(tb)) = (term_of(a), term_of(b)) else { continue };
            let term = Term::App(vec![Term::Op(OpKind::Equal), ta, tb]);
            let made = self.make(
                vec![(true, term)], just.clause_parents.clone(),
                "fd_congruence", SUPPORT, None, true);
            let Some(id) = made else { continue };
            self.clauses[id as usize].fact_parents.extend(just.fact_sids.iter().copied());
            if let Some(ax) = just.axiom {
                self.clauses[id as usize].fact_parents.push(ax);
            }
            let key = self.clauses[id as usize].key;
            if self.seen.insert(key) {
                self.activate(id);
                self.push(Some(id));
            }
        }
    }

    /// Congruence-closure pre-pass: union every ground `(equal a b)`
    /// unit in `clauses` into the oracle's equality closure, so the
    /// later `make` of every clause normalizes against the complete
    /// closure.  Must run before any non-equality clause is added.
    pub(crate) fn register_equalities(&mut self, clauses: &[PClause]) {
        for c in clauses {
            if c.lits.len() != 1 || !c.lits[0].pos {
                continue;
            }
            if let Some((ta, tb, ka, kb)) = self.ground_equality(c.lits[0].atom) {
                self.register_equality(ta, tb, ka, kb);
            }
        }
    }

    /// Equality-class key of any GROUND term: leaf keys from `eq_key`,
    /// compounds by content hash (interning into the prover-local
    /// AtomTable — the same id `Element::Sub` carries, so store-side
    /// and prover-side spellings of one subterm share a class).
    fn term_eq_key(&self, t: &Term) -> Option<u64> {
        if let Some(k) = eq_key(t) {
            return Some(k);
        }
        match t {
            Term::App(_) if t.is_ground() => Some(self.layer.atoms.intern_atom(t)),
            _ => None,
        }
    }

    /// Union one ground equality, remembering renderable terms for both
    /// keys and preferring a NUMERIC literal as the class root (so
    /// `normalize_eq` rewrites symbols TO numbers, keeping arithmetic
    /// comparisons decidable downstream).
    fn register_equality(&mut self, ta: Term, tb: Term, ka: u64, kb: u64) {
        if matches!(ta, Term::App(_)) || matches!(tb, Term::App(_)) {
            self.has_compound_eqs = true;
        }
        self.eq_terms.entry(ka).or_insert(ta);
        self.eq_terms.entry(kb).or_insert(tb);
        // Root preference: number > symbol > compound.  Rewriting
        // TOWARD numbers keeps comparisons decidable; rewriting toward
        // symbols keeps terms from GROWING (a compound root would make
        // normalize_eq inflate every occurrence of the symbol into the
        // compound — and a compound containing a ground list regrows
        // new lists forever).
        fn rank(t: &Term) -> u8 {
            match t {
                Term::Lit(Literal::Number(_)) => 0,
                Term::Sym(_) | Term::Lit(_) | Term::Op(_) => 1,
                Term::Var(_) | Term::App(_) => 2,
            }
        }
        let (ra, rb) = (rank(&self.eq_terms[&ka]), rank(&self.eq_terms[&kb]));
        let (root, child) = match ra.cmp(&rb) {
            std::cmp::Ordering::Less => (ka, kb),
            std::cmp::Ordering::Greater => (kb, ka),
            std::cmp::Ordering::Equal => {
                self.oracle.add_equality(ka, kb);
                return;
            }
        };
        self.oracle.add_equality_rooted(root, child);
    }

    /// Whether `t` is a symbol-headed relation atom the oracle can prove
    /// ill-sorted (an argument disjoint from its declared domain).
    fn atom_ill_sorted(&self, t: &Term) -> bool {
        let Term::App(elems) = t else { return false };
        let Some(Term::Sym(rel)) = elems.first() else { return false };
        let args: Vec<Option<SymbolId>> = elems
            .iter()
            .skip(1)
            .map(|e| match e {
                Term::Sym(s) => Some(s.id()),
                _ => None,
            })
            .collect();
        self.oracle.ill_sorted(rel.id(), &args)
    }

    /// For a ground `(equal l r)` atom term, whether the oracle entails
    /// it — symbol equality through reflexivity / equality-class /
    /// subclass antisymmetry (with witnesses), compound equality
    /// structurally (content-addressed).  `None` if `t` is not a ground
    /// equality atom.
    fn ground_equality_holds(&self, t: &Term) -> Option<(EqDecision, Vec<Witness>, Vec<u32>)> {
        let Term::App(elems) = t else { return None };
        if elems.len() != 3 || !matches!(elems[0], Term::Op(OpKind::Equal)) {
            return None;
        }
        let (l, r) = (&elems[1], &elems[2]);
        if !l.is_ground() || !r.is_ground() {
            return None;
        }
        // Numeric literals decide OUTRIGHT, both ways: equal canonical
        // values are entailed, different values are REFUTED (literal
        // semantics — there is no model where 1 = 2).  Symbols never
        // get the refuted arm (no unique-names assumption for them).
        if let (Term::Lit(Literal::Number(a)), Term::Lit(Literal::Number(b))) = (l, r) {
            if let (Some(x), Some(y)) =
                (crate::numeric::parse_num(a), crate::numeric::parse_num(b))
            {
                let d = if x == y { EqDecision::Entailed } else { EqDecision::Refuted };
                return Some((d, Vec::new(), Vec::new()));
            }
        }
        match (self.term_eq_key(l), self.term_eq_key(r)) {
            (Some(ka), Some(kb)) => {
                let mut why = Vec::new();
                if self.oracle.equal_holds(ka, kb, Some(&mut why)) {
                    // Deriving clauses from the equality proof forest
                    // (FD-congruence merges) become proof-DAG parents.
                    let clause_parents = self.oracle.eq_explain(ka, kb).1;
                    Some((EqDecision::Entailed, why, clause_parents))
                } else {
                    Some((EqDecision::Unknown, Vec::new(), Vec::new()))
                }
            }
            _ => {
                let d = if l == r { EqDecision::Entailed } else { EqDecision::Unknown };
                Some((d, Vec::new(), Vec::new()))
            }
        }
    }

    /// Decide a ground arithmetic comparison literal
    /// (`(greaterThan -100.0 0.0)` → `Some(false)`); `None` when the
    /// term isn't a comparison over two numeric literals.
    fn ground_compare(t: &Term) -> Option<bool> {
        let Term::App(elems) = t else { return None };
        if elems.len() != 3 { return None; }
        let (Term::Sym(p), Term::Lit(Literal::Number(a)), Term::Lit(Literal::Number(b))) =
            (&elems[0], &elems[1], &elems[2]) else { return None };
        let (x, y) = (crate::numeric::parse_num(a)?, crate::numeric::parse_num(b)?);
        crate::numeric::eval_compare(&p.name(), x, y)
    }

    /// `(a, b)` iff `atom` is a ground `(equal a b)` over two symbols.
    pub(super) fn ground_equality(&self, atom: AtomId) -> Option<(Term, Term, u64, u64)> {
        let s = self.layer.atoms.resolve(atom, self.syn())?;
        if s.elements.len() != 3 {
            return None;
        }
        if !matches!(s.elements.first(), Some(Element::Op(OpKind::Equal))) {
            return None;
        }
        let lift = |el: &Element| -> Option<(Term, u64)> {
            match el {
                Element::Symbol(sym) => {
                    let t = Term::Sym(sym.0.clone());
                    let k = eq_key(&t)?;
                    Some((t, k))
                }
                Element::Literal(l @ Literal::Number(_)) => {
                    let t = Term::Lit(l.clone());
                    let k = eq_key(&t)?;
                    Some((t, k))
                }
                // A ground sub-sentence: its sid IS its content hash —
                // the compound's equality key.  Lift to a Term for the
                // registry (renderable representative).
                Element::Sub(sid) => {
                    let t = self.layer.atoms.term_of(*sid, self.syn())?;
                    t.is_ground().then_some((t, *sid))
                }
                _ => None,
            }
        };
        let (ta, ka) = lift(&s.elements[1])?;
        let (tb, kb) = lift(&s.elements[2])?;
        if ka == kb {
            return None;
        }
        Some((ta, tb, ka, kb))
    }

    /// Build a clause from raw slot-form literals: arithmetic
    /// normalization, oracle discharge, depth cap, unit
    /// subsumption/simplification, learned-unit feedback, canonical
    /// dedup, tier weighting.  `None` = redundant/discarded.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn make(
        &mut self,
        lits:    Vec<(bool, Term)>,
        parents: Vec<u32>,
        rule:    &'static str,
        tier:    u8,
        source:  Option<SentenceId>,
        derived: bool,
    ) -> Option<u32> {
        // Interactive single-step: show the proposed derivation (the "match")
        // — rule, parent clauses, and the literals about to be built — and
        // block before we normalize/register it.  Only INFERENCES pause
        // (`derived`); the initial axiom/conjecture load (derived = false) is
        // not a match and would otherwise flood the prompt before the loop.
        if derived && stepdbg::enabled() {
            let body = format!(
                "rule = {rule}   tier = {tier}   derived = {derived}\n  \
                 conclusion: {}\n  from parents:\n{}",
                self.dbg_lits_kif(&lits),
                if parents.is_empty() {
                    "    (none — input/oracle unit)".to_string()
                } else {
                    parents
                        .iter()
                        .map(|p| format!("    [{p}] {}", self.dbg_clause_kif(*p)))
                        .collect::<Vec<_>>()
                        .join("\n")
                },
            );
            stepdbg::pause("MAKE", &body);
        }

        let mut fact_parents: Vec<SentenceId> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let mut parents = parents;

        // Arithmetic normalization, then ground-equality normalization:
        // every ground constant collapses to its equality-class
        // representative, so `equal` constants become one symbol.
        // Both are in-place: the (overwhelmingly common) untouched
        // literal costs a walk, not a rebuild.
        let mut lits = lits;
        let mut demod_used: Vec<u32> = Vec::new();
        // Duplicate-hit probe (Part 2; SIGMA_STATS instrumentation only):
        // eligible when demod is on and there is at least one indexed
        // demodulator to rewrite with — mirrors `demodulate`'s own early-out
        // so "attempts" means "demodulate actually scanned this literal",
        // not "demod is compiled in".
        //
        // CONJECTURE-tier literals are only demodulated under the
        // superposition regime: there, active clauses get rewritten by
        // newly activated equations through superposition too, so goal
        // and fact meet in normal form (standard, complete).  Without
        // superposition (the KIF/SUMO set-of-support path), rewriting
        // only the goal could orphan it from an already-active fact in
        // the original form — and the paraconsistent conjecture guard
        // (see the disjointness discharge below) wants the asked literal
        // kept as asked.
        let demod_eligible = self.opts.strategy.demod
            && !self.demods.is_empty()
            && (tier != CONJECTURE || self.opts.strategy.superposition);
        for (_, t) in lits.iter_mut() {
            arith_norm(t);
            self.normalize_eq(t);
            // Forward demodulation: rewrite to KBO normal form with the
            // indexed oriented unit equations (a simplification — the
            // normalized literal replaces the original).
            if demod_eligible {
                self.stats.demod_rewrite_attempts += 1;
                let n = self.demodulate(t, &mut demod_used);
                self.stats.demod_rewrites += n;
            }
        }
        let was_demodulated = !demod_used.is_empty();
        if demod_eligible && was_demodulated {
            self.stats.demod_rewrites_applied += 1;
        }
        if was_demodulated {
            demod_used.sort_unstable();
            demod_used.dedup();
            if self.want_notes() {
                notes.push(format!("demodulated by {} unit equation(s)", demod_used.len()));
            }
            parents.extend(demod_used);
        }

        // Equality-presence signal for strict saturation verdicts: once
        // equality is in play, a saturation without a complete equality
        // calculus cannot honestly claim "no".  Sticky bit, only paid
        // for on the strict path.
        if self.opts.strategy.strict_saturation && !self.stats.saw_equality {
            self.stats.saw_equality = lits.iter().any(|(_, t)| is_equality_atom(t));
        }

        // Symmetric-argument orientation: a GROUND argument pair of a
        // symmetric relation sorts into one canonical order (the same
        // blank-key order `orient_equality` uses), so `(R b a)` and
        // `(R a b)` collapse to one literal — the metaschema's mirrored
        // resolvents die at dedup instead of multiplying.  Ground pairs
        // only: orienting OPEN literals is unstable under substitution
        // (the oriented pattern's instances may orient the other way);
        // open-literal completeness is covered by the symmetric
        // retrieval retry in `resolve` instead.  Top-level atoms only —
        // never inside argument subterms, where embedded formulas
        // (PropositionalAttitude contexts) are referentially opaque.
        if self.opts.strategy.schema {
            let mut oriented: SmallVec<[SymbolId; 2]> = SmallVec::new();
            for (_, t) in lits.iter_mut() {
                let Term::App(elems) = t else { continue };
                if elems.len() != 3 || elems[1] == elems[2] {
                    continue;
                }
                let Term::Sym(h) = &elems[0] else { continue };
                if !elems[1].is_ground() || !elems[2].is_ground() {
                    continue;
                }
                let rel = h.id();
                if !self.oracle.is_symmetric(rel) {
                    continue;
                }
                if blank_key(&elems[1]) > blank_key(&elems[2]) {
                    elems.swap(1, 2);
                    self.stats.sym_oriented += 1;
                    if !oriented.contains(&rel) {
                        oriented.push(rel);
                    }
                }
            }
            for rel in oriented {
                if let Some(sid) = self.oracle.symmetric_source(rel) {
                    fact_parents.push(sid);
                }
                if self.want_notes() {
                    let name = self
                        .syn()
                        .sym_name(rel)
                        .map(|s| s.name().to_string())
                        .unwrap_or_else(|| format!("{rel:#x}"));
                    notes.push(format!("oriented symmetric {name} arguments"));
                }
            }
        }

        // Sorted-relation filter: a ground relation atom whose argument
        // is provably disjoint from the position's declared domain is
        // ill-sorted (false in SUMO's typed reading).  An ill-sorted
        // positive literal drops (false ∨ C ≡ C); an ill-sorted negative
        // literal is vacuously true → the clause is a tautology; dropping
        // all positives leaves a vacuous clause.  All three → discard.
        // INPUT clauses (axioms / hypotheses / the conjecture) are
        // exempt: asserted facts are ground truth, and SUMO itself
        // violates its own domain declarations (Merge asserts
        // `component` over nuclei while declaring component's domains
        // CorpuscularObject ⊥ Substance).  Silently deleting an input
        // changes the problem; the filter exists to stop DERIVED
        // ill-sorted fabrications.
        if derived && self.oracle.has_disjointness() {
            let mut filtered = Vec::with_capacity(lits.len());
            let mut dropped_positive = false;
            for (pos, t) in &lits {
                if self.atom_ill_sorted(t) {
                    if !*pos {
                        return None; // (not ill-sorted) ≡ tautology
                    }
                    dropped_positive = true;
                    continue;
                }
                filtered.push((*pos, t.clone()));
            }
            if dropped_positive && filtered.is_empty() {
                return None; // vacuous: only ill-sorted positives
            }
            lits = filtered;
        }

        // Theory propagation: discharge ground binary literals against
        // the oracle.  An entailed-FALSE negative literal is deleted
        // (unit resolution with a virtual entailed unit); a clause with
        // an entailed-TRUE positive literal is redundant (oracle-
        // subsumed) — except unit facts, which stay as index content.
        let mut kept: Vec<(bool, Term)> = Vec::with_capacity(lits.len());
        for (pos, t) in &lits {
            // Ground arithmetic comparisons decide outright: a FALSE
            // literal drops (unit resolution against arithmetic), a
            // TRUE literal satisfies the clause (redundant).
            if let Some(truth) = Self::ground_compare(t) {
                if truth != *pos {
                    self.stats.oracle_discharges += 1;
                    if self.want_notes() {
                        notes.push(format!(
                            "{} -- arithmetic", lit_kif(*pos, t, self.syn())));
                    }
                    continue;
                }
                self.stats.oracle_subsumed += 1;
                return None;
            }
            if let Some((decision, why, eq_clauses)) = self.ground_equality_holds(t) {
                match decision {
                    EqDecision::Entailed => {
                        if !*pos {
                            self.stats.oracle_discharges += 1;
                            if self.want_notes() {
                                notes.push(format!(
                                    "(not {}) -- oracle: {}",
                                    term_kif(t, self.syn()),
                                    if why.is_empty() { "x = x".to_string() }
                                    else { witnesses_kif(&why, self.syn()) }));
                            }
                            for w in &why {
                                if let Some(sid) = w.sid { fact_parents.push(sid); }
                            }
                            parents.extend(eq_clauses);
                            continue;
                        }
                        if lits.len() > 1 {
                            self.stats.oracle_subsumed += 1;
                            return None;
                        }
                    }
                    EqDecision::Refuted => {
                        // Mirror image: a FALSE positive equality drops
                        // (1 ≠ 2 by literal semantics); a satisfied
                        // negative one makes the clause redundant.
                        if *pos {
                            self.stats.oracle_discharges += 1;
                            if self.want_notes() {
                                notes.push(format!(
                                    "{} -- numeric disequality",
                                    term_kif(t, self.syn())));
                            }
                            continue;
                        }
                        if lits.len() > 1 {
                            self.stats.oracle_subsumed += 1;
                            return None;
                        }
                    }
                    EqDecision::Unknown => {}
                }
                kept.push((*pos, t.clone()));
                continue;
            }
            if let Some((rel, x, y)) = term_binary_ids(t) {
                // Disjointness refutation: `(instance x C)` is provably
                // FALSE when a known class of x is provably disjoint
                // from C (partition / disjoint declarations).  A FALSE
                // positive drops; a satisfied negative literal makes
                // the clause redundant.
                let mut why_r: Vec<Witness> = Vec::new();
                if self.oracle.refutes_instance(rel, x, y, Some(&mut why_r)) {
                    if *pos {
                        self.stats.oracle_discharges += 1;
                        if self.want_notes() {
                            notes.push(format!(
                                "{} -- oracle refutes: {}",
                                term_kif(t, self.syn()),
                                witnesses_kif(&why_r, self.syn())));
                        }
                        for w in &why_r {
                            if let Some(sid) = w.sid { fact_parents.push(sid); }
                        }
                        continue;
                    }
                    // A satisfied NEGATIVE literal makes the clause
                    // redundant — but never discard a CONJECTURE
                    // clause this way: in an inconsistent KB the same
                    // atom can be both refuted (disjointness) and
                    // derivable (rules), and the goal literal must
                    // stay for the search to prove positively (the
                    // paraconsistent reading every other prover gives
                    // these tests).
                    if tier != CONJECTURE {
                        self.stats.oracle_subsumed += 1;
                        return None;
                    }
                }
                // Memoized witness-free check first; the witness walk
                // (uncached) runs only for entailed atoms.
                if self.oracle.holds(rel, x, y, None) {
                    let mut why: Vec<Witness> = Vec::new();
                    let _ = self.oracle.holds(rel, x, y, Some(&mut why));
                    if !*pos {
                        self.stats.oracle_discharges += 1;
                        if self.want_notes() {
                            notes.push(format!(
                                "(not {}) -- oracle: {}",
                                term_kif(t, self.syn()),
                                witnesses_kif(&why, self.syn())));
                        }
                        for w in &why {
                            // Stored facts cite their sid; learned units
                            // cite the deriving clause so the unit's own
                            // derivation chain stays in the proof DAG.
                            if let Some(sid) = w.sid {
                                fact_parents.push(sid);
                            } else if let Some(cid) =
                                self.oracle.learned_src(w.rel, w.x, w.y)
                            {
                                parents.push(cid);
                            }
                        }
                        continue;
                    }
                    if lits.len() > 1 {
                        self.stats.oracle_subsumed += 1;
                        return None;
                    }
                }
            }
            kept.push((*pos, t.clone()));
        }
        let lits = kept;

        // Ground-list theory: the first sighting of each ground
        // `(ListFn …)` term synthesizes its extension — membership and
        // positional facts — as pending units (drained outside make).
        // SUMO's list axioms quantify over these; saturation cannot
        // enumerate a ground list's members without them.
        for (_, t) in &lits {
            self.collect_ground_lists(t);
        }

        // Depth AND size caps for derived clauses.  Depth alone is not
        // enough: substitution duplicates subterms, and SUMO's
        // recursive list machinery can grow term WIDTH without bound
        // (a 52 GB / 7-hour intern_atom death spiral at full-config
        // scale found this the hard way).
        if derived && lits.iter().any(|(_, t)| {
            term_depth(t) > self.opts.strategy.max_depth
                || term_size(t) > self.opts.strategy.max_term_size
        }) {
            self.stats.discarded_deep += 1;
            return None;
        }

        // Unit subsumption / simplification against the active units.
        let mut kept: Vec<(bool, Term)> = Vec::with_capacity(lits.len());
        for (pos, t) in &lits {
            if t.is_ground() {
                let atom = self.layer.atoms.intern_atom(t);
                if self.units.ground_unit(*pos, atom).is_some() {
                    self.stats.unit_subsumed += 1;
                    return None; // subsumed by an active unit
                }
                if let Some(cid) = self.units.ground_unit(!*pos, atom) {
                    self.stats.unit_simplified += 1;
                    if self.want_notes() {
                        notes.push(format!(
                            "{} -- refuted by unit clause {}",
                            lit_kif(*pos, t, self.syn()), cid));
                    }
                    parents.push(cid);
                    continue;
                }
            }
            // Open units, reached through the mask/residue index — THE
            // KEY EQUATION routes the target to exactly the patterns
            // whose ground seats agree with its coins (the flat
            // same-head scan went superlinear on deep searches).
            if let Some((h, ar)) = term_head_key(t) {
                // The shifted match target is built LAZILY: most
                // literals have zero open-unit candidates, and the
                // shift is a full tree clone.
                let mut target: Option<Term> = None;
                let (n_elems, seats) = classify_seats(t);
                let mut dropped = false;
                'pol: for same_pol in [*pos, !*pos] {
                    for u in self.units.open_candidates(same_pol, h, ar, n_elems, &seats) {
                        self.stats.open_match_attempts += 1;
                        let tgt = target.get_or_insert_with(|| shift_slots(t, MATCH_TARGET_OFF));
                        let n = u.nvars as usize + 1;
                        let mut s = self.take_scratch(n);
                        let hit = match_one_way(&u.pattern, tgt, &mut s);
                        self.put_scratch(s, n);
                        if hit {
                            self.stats.open_match_hits += 1;
                            if same_pol == *pos {
                                self.stats.unit_subsumed += 1;
                                return None;
                            }
                            self.stats.unit_simplified += 1;
                            if self.want_notes() {
                                notes.push(format!(
                                    "{} -- refuted by unit clause {}",
                                    lit_kif(*pos, t, self.syn()), u.clause));
                            }
                            parents.push(u.clause);
                            dropped = true;
                            break 'pol;
                        }
                    }
                }
                if dropped { continue; }
            }
            kept.push((*pos, t.clone()));
        }
        let lits = kept;

        // Any resulting ground positive unit extends the oracle: a binary
        // relation edge feeds the closure; a ground `(equal a b)` feeds
        // the equality union-find (helps later derivations — already-made
        // clauses are normalized by the input pre-pass).  The edge is
        // registered AFTER the clause is pushed (below) so the learned
        // entry can carry this clause's id as its proof-DAG source.
        let unit_edge = if lits.len() == 1 && lits[0].0 {
            if let Some((rel, x, y)) = term_binary_ids(&lits[0].1) {
                // Remember the constants: FD-derived equalities over
                // prover-local skolems must be re-buildable as terms
                // (the store's symbol cache has never seen them).
                if let Term::App(elems) = &lits[0].1 {
                    for el in elems {
                        if let Some(k) = eq_key(el) {
                            self.eq_terms.entry(k).or_insert_with(|| el.clone());
                        }
                    }
                }
                Some((rel, x, y))
            } else {
                if let Some((l, r)) = term_ground_equality_sides(&lits[0].1) {
                    if let (Some(ka), Some(kb)) = (self.term_eq_key(l), self.term_eq_key(r)) {
                        if ka != kb {
                            let (l, r) = (l.clone(), r.clone());
                            self.register_equality(l, r, ka, kb);
                        }
                    }
                }
                None
            }
        } else {
            None
        };
        // Negative ground units feed the oracle's exclusion store
        // (exhaustiveness case-elimination).
        let neg_unit_edge = if lits.len() == 1 && !lits[0].0 {
            term_binary_ids(&lits[0].1)
        } else {
            None
        };

        let clause = canonical_clause(lits, &self.layer.atoms);

        // Duplicate-hit probe (Part 2, continued): of the clauses demod
        // actually rewrote, how many collapse onto an already-known clause's
        // key right here — i.e. would dedup away via the same
        // `self.seen`/`ClauseKey` path `push()` uses later.  Read-only probe:
        // `push()` still does the real (insert-and-check) dedup itself, so
        // this changes no behavior, only counts.
        if demod_eligible && was_demodulated && self.seen.contains(&clause.key) {
            self.stats.demod_dup_hits += 1;
        }

        // Tautology check on the canonical literals.
        let pos_atoms: Set64<AtomId> =
            clause.lits.iter().filter(|l| l.pos).map(|l| l.atom).collect();
        if clause.lits.iter().any(|l| !l.pos && pos_atoms.contains(&l.atom)) {
            return None;
        }

        // Schema channel: theory-rule shapes register with their
        // oracle registries, and the fully-replaced ones (symmetry
        // rule / symmetry metaschema) are absorbed outright.  This
        // catches DERIVED instances too — the metaschema resolving
        // against `(instance R SymmetricRelation)` births exactly the
        // per-R symmetry rule, which dies here at birth.  Never for
        // CONJECTURE-tier clauses: a negated existential conjecture
        // (`exists x y. R(x,y) ∧ ¬R(y,x)`) IS the symmetry shape, and
        // absorbing it would erase the goal (and break vacuity
        // detection, which requires a negated_conjecture proof step).
        if self.opts.strategy.schema
            && tier != CONJECTURE
            && clause.nvars >= 1
            && clause.lits.len() <= 4
        {
            if let Some(hit) =
                self.layer.schema.probe(&clause.lits, &self.layer.atoms, self.syn())
            {
                self.stats.schema_hits += 1;
                if self.apply_schema_hit(&hit, source) {
                    self.stats.schema_absorbed += 1;
                    return None;
                }
            }
        }

        // Slot-form terms, lifted once (canonical vars → dense slots).
        let terms: Vec<(bool, Term)> = clause
            .lits
            .iter()
            .filter_map(|l| {
                slot_atom(&self.layer.atoms, self.syn(), l.atom, 0).map(|t| (l.pos, t))
            })
            .collect();
        debug_assert_eq!(terms.len(), clause.lits.len());

        // Forward subsumption: an active clause already covers this one
        // ⇒ it is redundant, drop it (the flooding floor).  The new
        // clause is not yet in the arena, so no self-subsumption.
        if let Some(_by) = self.forward_subsumed(&clause.lits, &terms) {
            self.stats.subsumed += 1;
            return None;
        }

        let size: u64 = terms.iter().map(|(_, t)| term_size(t) as u64).sum();
        // Generative-existential throttle: every skolem-function
        // application multiplies the clause's weight, so self-feeding
        // chains (sk1(sk0(x))…) sink in the queue instead of flooding
        // it.  One skolem (an ordinary existential witness) costs a
        // factor of 2 — mild; nested chains grow superlinearly.
        let skolems: u64 = terms.iter().map(|(_, t)| term_skolem_apps(t)).sum();
        // Parameterized clause-weight function (the selection genome).
        // Defaults (cw_lits=1, cw_size=1, cw_vars=2, cw_skolem=1) reproduce
        // the historical `(#lits + size + 2·#vars)·(1 + skolems)`.
        let st = &self.opts.strategy;
        let base = (st.cw_lits * clause.lits.len() as u64
            + st.cw_size * size
            + st.cw_vars * u64::from(clause.nvars))
            .max(1)
            * (1 + st.cw_skolem * skolems);
        // Conjecture-distance factor: structurally goal-near clauses
        // keep their base weight; goal-far ones sink (×1..×1+W).
        let dist = self.goal_distance_factor(&clause.lits, tier);
        // KBO-maximal literals (ordered-inference eligibility).  Only
        // computed when an ordered rule needs it; otherwise all-maximal
        // (no restriction) so the unordered default pays nothing.
        let max_mask = self.maximal_literals(&clause.lits);
        let id = self.clauses.len() as u32;
        self.clauses.push(ClauseRec {
            id,
            lits: clause.lits,
            terms,
            nvars: clause.nvars,
            key: clause.key,
            parents,
            fact_parents,
            source,
            rule,
            tier,
            weight: base * self.opts.strategy.tier_weight[tier as usize] * dist,
            activated: false,
            max_mask,
            notes,
        });
        if let Some((rel, x, y)) = unit_edge {
            self.oracle.add_unit(rel, x, y, Some(id));
        }
        if let Some((rel, x, y)) = neg_unit_edge {
            self.oracle.add_neg_unit(rel, x, y, Some(id));
        }
        Some(id)
    }
}
