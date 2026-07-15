// crates/core/src/saturate/model/seminaive.rs
//
// Phase 6 — indexed semi-naive Datalog(¬) evaluation.
//
// Replaces the naive fixpoint (re-derive everything every round, O(facts) full
// scans per body literal) with the standard production-grade strategy:
//
//   * SEMI-NAIVE — a new fact in round k must use a fact that was new in round
//     k-1 (the "delta"), so each recursive rule is fired with one body literal
//     ranging over the delta and the rest over the full relation; each fact is
//     derived ~once, not once-per-remaining-round (Bancilhon-Ramakrishnan
//     1986; Abiteboul-Hull-Vianu).
//   * INDEXED joins — a body literal with a bound position is resolved by a
//     hash lookup `(relation, position, value) → tuples` instead of scanning
//     the whole relation (the same join-indexing the prover already uses in
//     `discharge_horn_joins`' seat index and `syntactic::residue_index`).
//
// Task #32 (the seminaive package) adds two kernel-level mechanisms:
//
//   * EGDs — equality-generating dependencies ([`super::extract::Egd`]).  A
//     union-find over `SymbolId` with a JUSTIFICATION FOREST ([`EqClasses`]):
//     when two stored tuples violate an FD, their value symbols are unioned,
//     the union edge is labeled with the EGD's axiom sid + both witness
//     tuples, and the store is RE-CANONICALIZED — the absorbed rep's rows are
//     re-inserted in canonical form and pushed into the current delta, which
//     re-drives affected rules (sound; terminates because the class count
//     strictly decreases and the tuple universe only shrinks).  A union of
//     two RIGID (numeric-literal) symbols aborts with
//     [`ModelError::Inconsistent`] carrying the citation chain.
//   * BUILT-IN transitive closure — relations in `Program::builtin_transitive`
//     have no transitivity schema rule; a body literal over one with a bound
//     side resolves by on-demand BFS over the stored base edges (memoized per
//     (relation, seed), invalidated when the relation grows), so the dense
//     closure is never materialized.  A both-free literal falls back to base
//     edges only (documented under-enumeration; such relations are excluded
//     from certification — see `certify`).
//
// Computes the SAME perfect model as the naive evaluator, just far faster.
// Stratified: semi-naive WITHIN a stratum (positive recursion only there);
// negation is a membership filter against the fully-computed lower strata.
// EGD merges in a program that carries ANY negated body literal abort with
// `Unstratifiable`: a merge can retroactively change an earlier absence
// check, and refusing to answer is the sound response (the monotone /
// magic-scoped programs the answer paths evaluate are negation-free, so
// they are unaffected).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use crate::clock::Instant;

use smallvec::SmallVec;

use crate::types::{SentenceId, SymbolId};

use super::super::hash64::Set64;
use super::{
    ground_atom, unify, Atom, DTerm, Derivation, Literal, Model, ModelError, Pred, Program,
    Provenance, Rule, Tuple, BUILTIN_RULE, EQ_CANON_RULE,
};

/// The ground tuples a rule firing's positive body literals matched — the
/// emitted head's first-parent provenance (see [`Derivation`]).
type Parents = SmallVec<[(Pred, Tuple); 4]>;

// ---------------------------------------------------------------------------
// Equality classes with a justification forest (proof-producing union-find).
// ---------------------------------------------------------------------------

/// The justification label on one ORIGINAL union edge `a = b`: the EGD whose
/// firing asserted it and the two witness tuples that matched its key.
#[derive(Clone, Debug)]
pub(crate) struct EqEdge {
    pub egd_sid:   Option<SentenceId>,
    pub witness_a: (Pred, Tuple),
    pub witness_b: (Pred, Tuple),
}

/// What one union attempt did.
pub(super) enum UnionOutcome {
    /// Already equal — nothing recorded.
    Noop,
    /// Merged; `absorbed` is the representative that lost rep-hood (its rows
    /// must be re-canonicalized).
    Merged { absorbed: SymbolId },
    /// The union would equate two distinct RIGID symbols `a` / `b` (numeric
    /// literals) — the proof edge IS recorded (so the conflict chain can be
    /// explained) but the classes are NOT merged; the caller must abort.
    Rigid { a: SymbolId, b: SymbolId },
}

/// Union-find over `SymbolId` with a justification forest.
///
/// Two parent structures (the standard proof-producing union-find split):
/// `parent` is the ordinary union-by-rank forest with path compression —
/// it answers `find` — while `proof` is the UNCOMPRESSED proof forest whose
/// edges are exactly the original union edges; to explain `a = b` we walk
/// the proof-forest path between them and read each edge's label from
/// `edges`.
#[derive(Clone, Debug, Default)]
pub(crate) struct EqClasses {
    /// Compressed union-find parents (absent key ⇒ self-parented).
    parent: HashMap<SymbolId, SymbolId>,
    rank:   HashMap<SymbolId, u32>,
    /// The proof forest: NEVER path-compressed; edges are original unions.
    proof:  HashMap<SymbolId, SymbolId>,
    /// Edge labels, keyed by the normalized (min, max) endpoint pair.
    pub(crate) edges: HashMap<(SymbolId, SymbolId), EqEdge>,
    /// Per-class numeric (rigid) member, keyed by current rep — the rigid-
    /// conflict detector.
    numeric: HashMap<SymbolId, SymbolId>,
    /// Number of successful merges (0 ⇒ everything is its own class and all
    /// canonicalization is the identity — the fast path).
    merges: usize,
}

impl EqClasses {
    /// Whether any merge happened (canonicalization is non-trivial).
    #[inline]
    pub(crate) fn merged(&self) -> bool {
        self.merges > 0
    }

    /// The class representative of `s` — read-only walk (no compression, so
    /// `&self` suffices; chains stay short because the mutating find used by
    /// the kernel compresses).
    pub(crate) fn find(&self, s: SymbolId) -> SymbolId {
        if self.merges == 0 {
            return s;
        }
        let mut cur = s;
        while let Some(&p) = self.parent.get(&cur) {
            if p == cur {
                break;
            }
            cur = p;
        }
        cur
    }

    /// The class representative, with path compression (kernel hot path).
    fn find_mut(&mut self, s: SymbolId) -> SymbolId {
        let root = self.find(s);
        let mut cur = s;
        while cur != root {
            let next = self.parent.get(&cur).copied().unwrap_or(cur);
            self.parent.insert(cur, root);
            if next == cur {
                break;
            }
            cur = next;
        }
        root
    }

    /// Canonicalize a tuple through `find` (identity clone when no merges).
    pub(crate) fn canon_tuple(&self, t: &Tuple) -> Tuple {
        if self.merges == 0 {
            return t.clone();
        }
        t.iter().map(|&v| self.find(v)).collect()
    }

    /// The rigid member of `rep`'s class, if any: the recorded numeric
    /// member, or `rep` itself when it is rigid.
    fn numeric_member(&self, rep: SymbolId, rigid: &Set64<SymbolId>) -> Option<SymbolId> {
        self.numeric.get(&rep).copied().or_else(|| rigid.contains(&rep).then_some(rep))
    }

    /// Record the ORIGINAL union edge `x = y` in the proof forest: reverse
    /// `x`'s path to its proof root, then hang `x` on `y` (the standard
    /// path-reversal insertion that keeps the forest a forest).
    fn add_proof_edge(&mut self, x: SymbolId, y: SymbolId, edge: EqEdge) {
        let mut path = vec![x];
        let mut cur = x;
        while let Some(&p) = self.proof.get(&cur) {
            path.push(p);
            cur = p;
        }
        for w in path.windows(2) {
            self.proof.insert(w[1], w[0]);
        }
        self.proof.insert(x, y);
        self.edges.entry(norm(x, y)).or_insert(edge);
    }

    /// Union the classes of `x` and `y`, justified by `edge`.  Records the
    /// proof edge for every non-noop attempt (including a rigid conflict, so
    /// the conflict chain is explainable), merges by rank, and tracks each
    /// class's rigid member.
    pub(super) fn union(
        &mut self,
        x:     SymbolId,
        y:     SymbolId,
        edge:  EqEdge,
        rigid: &Set64<SymbolId>,
    ) -> UnionOutcome {
        let rx = self.find_mut(x);
        let ry = self.find_mut(y);
        if rx == ry {
            return UnionOutcome::Noop;
        }
        let nx = self.numeric_member(rx, rigid);
        let ny = self.numeric_member(ry, rigid);
        self.add_proof_edge(x, y, edge);
        if let (Some(a), Some(b)) = (nx, ny) {
            if a != b {
                return UnionOutcome::Rigid { a, b };
            }
        }
        let (kx, ky) = (
            self.rank.get(&rx).copied().unwrap_or(0),
            self.rank.get(&ry).copied().unwrap_or(0),
        );
        let (winner, loser) = if kx >= ky { (rx, ry) } else { (ry, rx) };
        self.parent.insert(loser, winner);
        if kx == ky {
            self.rank.insert(winner, kx + 1);
        }
        if let Some(n) = nx.or(ny) {
            self.numeric.insert(winner, n);
        }
        self.merges += 1;
        UnionOutcome::Merged { absorbed: loser }
    }

    /// The proof-forest path between `a` and `b` as normalized edge keys
    /// (look labels up in [`edges`](Self::edges)).  Empty when they are the
    /// same symbol or not connected.
    pub(crate) fn explain(&self, a: SymbolId, b: SymbolId) -> Vec<(SymbolId, SymbolId)> {
        if a == b {
            return Vec::new();
        }
        // Ancestor chain of `a` in the proof forest.
        let mut a_chain: Vec<SymbolId> = vec![a];
        let mut a_set: HashSet<SymbolId> = HashSet::new();
        a_set.insert(a);
        let mut cur = a;
        while let Some(&p) = self.proof.get(&cur) {
            a_chain.push(p);
            a_set.insert(p);
            cur = p;
        }
        // Walk from `b` to the first common ancestor.
        let mut b_chain: Vec<SymbolId> = vec![b];
        let mut cur = b;
        while !a_set.contains(&cur) {
            match self.proof.get(&cur) {
                Some(&p) => {
                    b_chain.push(p);
                    cur = p;
                }
                None => return Vec::new(), // different proof trees: unconnected
            }
        }
        let lca = cur;
        let mut out: Vec<(SymbolId, SymbolId)> = Vec::new();
        for w in a_chain.iter().take_while(|&&n| n != lca).zip(a_chain.iter().skip(1)) {
            out.push(norm(*w.0, *w.1));
        }
        for w in b_chain.iter().take_while(|&&n| n != lca).zip(b_chain.iter().skip(1)) {
            out.push(norm(*w.0, *w.1));
        }
        out
    }
}

#[inline]
fn norm(a: SymbolId, b: SymbolId) -> (SymbolId, SymbolId) {
    if a <= b { (a, b) } else { (b, a) }
}

// ---------------------------------------------------------------------------
// The indexed relation store.
// ---------------------------------------------------------------------------

/// One relation: its tuples, a membership set, and a per-position value index.
#[derive(Default)]
struct Rel {
    rows:   Vec<Tuple>,
    set:    HashSet<Tuple>,
    /// `by_pos[i][value]` = indices into `rows` whose position `i` is `value`.
    by_pos: Vec<HashMap<SymbolId, Vec<u32>>>,
}

impl Rel {
    fn insert(&mut self, t: Tuple) -> bool {
        if !self.set.insert(t.clone()) {
            return false;
        }
        let idx = self.rows.len() as u32;
        if self.by_pos.len() < t.len() {
            self.by_pos.resize_with(t.len(), HashMap::new);
        }
        for (i, &v) in t.iter().enumerate() {
            self.by_pos[i].entry(v).or_default().push(idx);
        }
        self.rows.push(t);
        true
    }
}

/// The indexed relation store.
#[derive(Default)]
struct Store {
    rels: HashMap<Pred, Rel>,
}

impl Store {
    fn insert(&mut self, p: Pred, t: Tuple) -> bool {
        self.rels.entry(p).or_default().insert(t)
    }

    fn contains(&self, p: Pred, t: &Tuple) -> bool {
        self.rels.get(&p).is_some_and(|r| r.set.contains(t))
    }

    /// Candidate tuples for `atom` under the current binding: the index bucket
    /// of the most selective bound position, or every row if nothing is bound.
    fn candidates(&self, atom: &Atom, binding: &HashMap<u32, SymbolId>) -> Vec<Tuple> {
        let Some(rel) = self.rels.get(&atom.pred) else { return Vec::new() };
        let mut best: Option<(usize, SymbolId, usize)> = None; // (pos, val, bucket size)
        for (i, a) in atom.args.iter().enumerate() {
            let val = match a {
                DTerm::Const(c) => Some(*c),
                DTerm::Var(v) => binding.get(v).copied(),
            };
            if let Some(v) = val {
                let sz = rel.by_pos.get(i).and_then(|m| m.get(&v)).map_or(0, Vec::len);
                if best.is_none_or(|(_, _, bs)| sz < bs) {
                    best = Some((i, v, sz));
                }
            }
        }
        match best {
            Some((i, v, _)) => rel
                .by_pos
                .get(i)
                .and_then(|m| m.get(&v))
                .map(|idxs| idxs.iter().map(|&j| rel.rows[j as usize].clone()).collect())
                .unwrap_or_default(),
            None => rel.rows.clone(),
        }
    }

    fn into_model(self) -> Model {
        self.rels.into_iter().map(|(p, r)| (p, r.set)).collect()
    }
}

// ---------------------------------------------------------------------------
// Built-in transitive-closure reachability (on-demand, memoized).
// ---------------------------------------------------------------------------

/// One direction's adjacency + per-seed BFS memo for a builtin relation.
/// `version` is the relation's row count when the adjacency was built — a
/// grown relation invalidates both (derived base edges must be visible).
#[derive(Default)]
struct ReachIndex {
    version: usize,
    adj:     HashMap<SymbolId, Vec<SymbolId>>,
    memo:    HashMap<SymbolId, Reached>,
}

/// One BFS result: the reached nodes (seed excluded) and the BFS-tree parent
/// of each — enough to reconstruct the path for provenance.
#[derive(Clone, Default)]
struct Reached {
    nodes:  Vec<SymbolId>,
    parent: HashMap<SymbolId, SymbolId>,
}

// ---------------------------------------------------------------------------
// The evaluation kernel.
// ---------------------------------------------------------------------------

/// Per-evaluation state: the store, the provenance under construction
/// (including the equality classes), the EGD index, and the builtin-closure
/// caches.  `fire`/`join_rec` take `&Kernel` (BFS memos live behind
/// `RefCell`/`Cell`); insertion takes `&mut Kernel`.
struct Kernel<'p> {
    prog:        &'p Program,
    store:       Store,
    prov:        Provenance,
    egd_idx:     HashMap<Pred, Vec<usize>>,
    total:       usize,
    max_tuples:  usize,
    /// Any rule carries a negated body literal — an EGD merge then aborts
    /// (`Unstratifiable`): a merge can retroactively change an earlier
    /// absence check, and refusing is the sound response.
    has_negation: bool,
    /// An EGD merge happened since the last `take_merged` — the semi-naive
    /// loop runs a full catch-up pass (re-canonicalized rows of other
    /// strata's relations are not delta-driven).
    merged:      bool,
    /// A BUILTIN-transitive relation gained base edges since the last
    /// `take_builtin_grew` — a new edge extends the CLOSURE by pairs no
    /// delta tuple represents, so the semi-naive loop runs a full catch-up
    /// pass instead of materializing the closure-pair expansion (which
    /// would recreate the very blowup built-ins exist to kill).
    builtin_grew: bool,
    /// Auxiliary budget: builtin-BFS nodes visited + expanded closure driver
    /// tuples.  Charged against `max_tuples` alongside stored rows.
    aux:         Cell<usize>,
    reach:       RefCell<HashMap<(Pred, bool), ReachIndex>>,
    /// Wall-clock deadline for the WHOLE evaluation (not just between-rule
    /// checks) — see `join_rec`'s tick counter below.  `None` ⇒ unbounded.
    deadline:    Option<Instant>,
    /// Ticks since `Instant::now()` was last consulted inside `join_rec`'s
    /// candidate-tuple loop.  A single `fire` call for ONE rule can recurse
    /// through a huge join (e.g. a dense transitive `sub`/`subclass` body) —
    /// the old code only checked `deadline` once per rule, BEFORE `fire`
    /// started, so a single long join could run arbitrarily far past the
    /// deadline before the next check.  Checking `Instant::now()` on every
    /// candidate tuple would itself be a real cost on a hot join, so the
    /// clock is sampled only every `DEADLINE_CHECK_TICKS` expansions — cheap
    /// (a `Cell` increment) on every tuple, an actual syscall-ish read only
    /// every Nth.
    join_ticks:  Cell<u64>,
    /// Set once `join_rec` samples the clock past `deadline` — `join_rec`
    /// checks this at recursion entry to unwind immediately (every stack
    /// frame stops iterating its candidates) rather than only stopping the
    /// one frame that happened to sample the clock.
    deadline_hit: Cell<bool>,
}

/// How many candidate-tuple expansions `join_rec` processes between
/// `Instant::now()` samples — frequent enough that a dense join can't run far
/// past the deadline (a few thousand hash-map probes is sub-millisecond),
/// infrequent enough that the clock read itself is not the hot-path cost.
const DEADLINE_CHECK_TICKS: u64 = 1000;

impl<'p> Kernel<'p> {
    fn new(prog: &'p Program, max_tuples: usize, deadline: Option<Instant>) -> Self {
        let mut egd_idx: HashMap<Pred, Vec<usize>> = HashMap::new();
        for (i, e) in prog.egds.iter().enumerate() {
            egd_idx.entry(e.rel).or_default().push(i);
        }
        let prov = Provenance {
            rule_sids: prog.rules.iter().map(|r| r.sid).collect(),
            edb_sids:  prog.edb_sids.clone(),
            derived:   HashMap::new(),
            eq:        EqClasses::default(),
            builtin_sids: prog
                .builtin_transitive
                .iter()
                .filter_map(|(p, s)| (*s).map(|sid| (*p, sid)))
                .collect(),
            budget_used: 0,
        };
        let has_negation = prog.rules.iter().any(|r| r.body.iter().any(|l| l.negated));
        Kernel {
            prog,
            store: Store::default(),
            prov,
            egd_idx,
            total: 0,
            max_tuples,
            has_negation,
            merged: false,
            builtin_grew: false,
            aux: Cell::new(0),
            reach: RefCell::new(HashMap::new()),
            deadline,
            join_ticks: Cell::new(0),
            deadline_hit: Cell::new(false),
        }
    }

    #[inline]
    fn is_builtin(&self, p: Pred) -> bool {
        self.prog.builtin_transitive.contains_key(&p)
    }

    fn take_merged(&mut self) -> bool {
        std::mem::replace(&mut self.merged, false)
    }

    fn take_builtin_grew(&mut self) -> bool {
        std::mem::replace(&mut self.builtin_grew, false)
    }

    fn check_budget(&self) -> Result<(), ModelError> {
        if self.total + self.aux.get() > self.max_tuples || self.deadline_hit.get() {
            Err(ModelError::Overflow)
        } else {
            Ok(())
        }
    }

    /// `true` once a wall-clock deadline was sampled (or previously found)
    /// past its limit.  Called on every candidate-tuple expansion inside
    /// `join_rec`'s hot loop, so the actual `Instant::now()` read is
    /// throttled to once every `DEADLINE_CHECK_TICKS` calls — cheap enough
    /// that a dense single-rule join (the case the per-rule-only check
    /// missed) is caught within a few thousand expansions instead of running
    /// to completion regardless of how long that takes.
    #[inline]
    fn join_over_deadline(&self) -> bool {
        if self.deadline_hit.get() {
            return true;
        }
        let Some(dl) = self.deadline else { return false };
        let t = self.join_ticks.get() + 1;
        self.join_ticks.set(t);
        if t % DEADLINE_CHECK_TICKS != 0 {
            return false;
        }
        if Instant::now() >= dl {
            self.deadline_hit.set(true);
            true
        } else {
            false
        }
    }

    /// A copy of `a` with constants canonicalized through the equality
    /// classes (rule constants can go stale after a merge).
    fn canon_atom(&self, a: &Atom) -> Atom {
        Atom {
            pred: a.pred,
            args: a
                .args
                .iter()
                .map(|arg| match arg {
                    DTerm::Const(c) => DTerm::Const(self.prov.eq.find(*c)),
                    v => v.clone(),
                })
                .collect(),
        }
    }

    // -- builtin transitive closure ----------------------------------------

    /// BFS reachability from `seed` over the builtin relation's CURRENT
    /// stored edges (`rev` ⇒ reverse edges), memoized per (relation, seed)
    /// and invalidated when the relation grows.  Freshly visited nodes are
    /// charged to the auxiliary budget.
    fn reach_from(&self, pred: Pred, seed: SymbolId, rev: bool) -> Reached {
        let mut cache = self.reach.borrow_mut();
        let e = cache.entry((pred, rev)).or_default();
        let version = self.store.rels.get(&pred).map_or(0, |r| r.rows.len());
        if e.version != version {
            e.version = version;
            e.adj.clear();
            e.memo.clear();
            if let Some(rel) = self.store.rels.get(&pred) {
                for row in &rel.rows {
                    if row.len() == 2 {
                        let (a, b) = if rev { (row[1], row[0]) } else { (row[0], row[1]) };
                        e.adj.entry(a).or_default().push(b);
                    }
                }
            }
        }
        if let Some(r) = e.memo.get(&seed) {
            return r.clone();
        }
        let mut nodes: Vec<SymbolId> = Vec::new();
        let mut parent: HashMap<SymbolId, SymbolId> = HashMap::new();
        let mut seen: HashSet<SymbolId> = HashSet::new();
        seen.insert(seed);
        let mut q: VecDeque<SymbolId> = VecDeque::new();
        q.push_back(seed);
        while let Some(u) = q.pop_front() {
            for &v in e.adj.get(&u).map(Vec::as_slice).unwrap_or(&[]) {
                if seen.insert(v) {
                    parent.insert(v, u);
                    nodes.push(v);
                    q.push_back(v);
                }
            }
        }
        self.aux.set(self.aux.get() + nodes.len() + 1);
        let r = Reached { nodes, parent };
        e.memo.insert(seed, r.clone());
        r
    }

    /// Closure membership `x →+ y` over a builtin relation.
    fn reaches(&self, pred: Pred, x: SymbolId, y: SymbolId) -> bool {
        self.reach_from(pred, x, false).parent.contains_key(&y)
    }

    /// The BFS path `x →+ y` as base-edge parent tuples (for the
    /// [`BUILTIN_RULE`] derivation).  Empty when unreachable (stale memo
    /// race — the derivation then cites only the declaring sid).
    fn builtin_path(&self, pred: Pred, x: SymbolId, y: SymbolId) -> Parents {
        let r = self.reach_from(pred, x, false);
        let mut edges: Vec<(Pred, Tuple)> = Vec::new();
        let mut cur = y;
        while cur != x {
            let Some(&p) = r.parent.get(&cur) else { return Parents::new() };
            edges.push((pred, vec![p, cur]));
            cur = p;
        }
        edges.reverse();
        edges.into_iter().collect()
    }

    /// Candidate tuples for a body literal — the builtin-closure dispatch
    /// wrapped around [`Store::candidates`]:
    ///
    ///   * bound-left  → forward-BFS closure pairs `(x, z)`;
    ///   * bound-right → reverse-BFS closure pairs `(x, y)`;
    ///   * both bound  → a reachability check;
    ///   * both free   → the BASE edges only (the un-closed relation): a
    ///     both-free builtin literal under-enumerates the closure, which is
    ///     why rules reaching a builtin relation are excluded from the
    ///     certified/complete sets (see `certify`).
    fn candidates(&self, atom: &Atom, binding: &HashMap<u32, SymbolId>) -> Vec<Tuple> {
        if atom.args.len() == 2 && self.is_builtin(atom.pred) {
            let bound = |a: &DTerm| match a {
                DTerm::Const(c) => Some(*c),
                DTerm::Var(v) => binding.get(v).copied(),
            };
            match (bound(&atom.args[0]), bound(&atom.args[1])) {
                (Some(x), Some(y)) => {
                    return if self.reaches(atom.pred, x, y) {
                        vec![vec![x, y]]
                    } else {
                        Vec::new()
                    };
                }
                (Some(x), None) => {
                    return self
                        .reach_from(atom.pred, x, false)
                        .nodes
                        .iter()
                        .map(|&z| vec![x, z])
                        .collect();
                }
                (None, Some(y)) => {
                    return self
                        .reach_from(atom.pred, y, true)
                        .nodes
                        .iter()
                        .map(|&x| vec![x, y])
                        .collect();
                }
                (None, None) => {} // base edges only — fall through
            }
        }
        self.store.candidates(atom, binding)
    }

    /// Ground membership for a (possibly negated) literal — closure-aware
    /// for builtin relations.
    fn contains_closure(&self, pred: Pred, t: &Tuple) -> bool {
        if self.store.contains(pred, t) {
            return true;
        }
        t.len() == 2 && self.is_builtin(pred) && self.reaches(pred, t[0], t[1])
    }

    // -- insertion + EGD firing ---------------------------------------------

    /// Insert one row (canonicalized), record its derivation, then fire any
    /// EGDs on its relation — a merge cascades through re-canonicalization,
    /// which may insert further rows.  Every row that is NEW to its
    /// relation's set is appended to `news` (the caller feeds them into the
    /// current delta — the semi-naive integration).
    fn insert_row(
        &mut self,
        pred:  Pred,
        tuple: Tuple,
        deriv: Option<(u32, Parents)>,
        news:  &mut Vec<(Pred, Tuple)>,
    ) -> Result<(), ModelError> {
        self.insert_row_inner(pred, tuple, deriv, news, true)
    }

    /// [`insert_row`](Self::insert_row) with the EGD probe optional: the EDB
    /// load defers probing until every fact is stored, so guard checks (and
    /// therefore which merges fire) do not depend on `HashMap` iteration
    /// order — the probe pass then sees the complete EDB deterministically.
    fn insert_row_inner(
        &mut self,
        pred:  Pred,
        tuple: Tuple,
        deriv: Option<(u32, Parents)>,
        news:  &mut Vec<(Pred, Tuple)>,
        probe: bool,
    ) -> Result<(), ModelError> {
        let mut tuple = tuple;
        // The pre-canonicalization original — kept only when it differs, to
        // bridge an EDB row's citation back to its `edb_sids` key.
        let mut orig: Option<Tuple> = None;
        if self.prov.eq.merged() {
            let c = self.prov.eq.canon_tuple(&tuple);
            if c != tuple {
                orig = Some(std::mem::replace(&mut tuple, c));
            }
        }
        if !self.store.insert(pred, tuple.clone()) {
            return Ok(());
        }
        self.total += 1;
        if self.total > self.max_tuples {
            return Err(ModelError::Overflow);
        }
        if self.is_builtin(pred) {
            self.builtin_grew = true;
        }
        match deriv {
            Some((ri, parents)) => {
                // Builtin-closure parents materialize their own BUILTIN
                // derivation (the BFS path) so `cite` can resolve them.
                for (bp, bt) in parents.iter() {
                    if bt.len() == 2
                        && self.is_builtin(*bp)
                        && !self.store.contains(*bp, bt)
                        && !self.prov.derived.contains_key(&(*bp, bt.clone()))
                    {
                        let path = self.builtin_path(*bp, bt[0], bt[1]);
                        self.prov
                            .derived
                            .insert((*bp, bt.clone()), Derivation { rule: BUILTIN_RULE, parents: path });
                    }
                }
                self.prov
                    .derived
                    .entry((pred, tuple.clone()))
                    .or_insert(Derivation { rule: ri, parents });
            }
            None => {
                // An EDB fact whose stored form was canonicalized: bridge the
                // canonical row back to the original (whose sid lives in
                // `edb_sids`) so the citation resolves.
                if let Some(orig) = orig {
                    self.prov.derived.entry((pred, tuple.clone())).or_insert(Derivation {
                        rule:    EQ_CANON_RULE,
                        parents: std::iter::once((pred, orig)).collect(),
                    });
                }
            }
        }
        news.push((pred, tuple.clone()));
        if probe {
            self.egd_probe(pred, &tuple, news)
        } else {
            Ok(())
        }
    }

    /// All instance-typing guards hold for `val` w.r.t. the store's CURRENT
    /// `instance` facts — an under-approximation of the instance closure, so
    /// a guarded EGD can only under-fire (sound).
    fn guards_hold(&self, guards: &[SymbolId], val: SymbolId) -> bool {
        if guards.is_empty() {
            return true;
        }
        let Some(inst) = self.prog.instance_pred else { return false };
        guards
            .iter()
            .all(|c| self.store.contains(inst, &vec![self.prov.eq.find(val), self.prov.eq.find(*c)]))
    }

    /// Fire the EGDs of `pred` against a freshly inserted tuple: probe the
    /// key index for a row with the same key and a different value rep; on a
    /// hit, union the values (recording the justification edge with both
    /// witnesses) and re-canonicalize.
    fn egd_probe(
        &mut self,
        pred: Pred,
        t:    &Tuple,
        news: &mut Vec<(Pred, Tuple)>,
    ) -> Result<(), ModelError> {
        if t.len() != 2 {
            return Ok(());
        }
        let Some(ids) = self.egd_idx.get(&pred) else { return Ok(()) };
        let ids = ids.clone();
        for ei in ids {
            let (kp, vp, sid) = {
                let egd = &self.prog.egds[ei];
                (egd.key_pos as usize, egd.val_pos as usize, egd.sid)
            };
            if kp >= t.len() || vp >= t.len() || kp == vp {
                continue;
            }
            {
                let egd = &self.prog.egds[ei];
                if !self.guards_hold(&egd.key_guards, t[kp])
                    || !self.guards_hold(&egd.val_guards, t[vp])
                {
                    continue;
                }
            }
            let partners: Vec<Tuple> = self
                .store
                .rels
                .get(&pred)
                .and_then(|r| r.by_pos.get(kp).and_then(|m| m.get(&t[kp])))
                .map(|idxs| {
                    let rel = &self.store.rels[&pred];
                    idxs.iter().map(|&i| rel.rows[i as usize].clone()).collect()
                })
                .unwrap_or_default();
            for r in partners {
                if r == *t || r.len() != 2 {
                    continue;
                }
                let (va, vb) = (t[vp], r[vp]);
                if self.prov.eq.find(va) == self.prov.eq.find(vb) {
                    continue;
                }
                let val_guards_ok = {
                    let egd = &self.prog.egds[ei];
                    self.guards_hold(&egd.val_guards, vb)
                };
                if !val_guards_ok {
                    continue;
                }
                self.apply_union(
                    va,
                    vb,
                    EqEdge {
                        egd_sid:   sid,
                        witness_a: (pred, t.clone()),
                        witness_b: (pred, r.clone()),
                    },
                    news,
                )?;
            }
        }
        Ok(())
    }

    /// One asserted equality: union the classes, then re-canonicalize the
    /// absorbed representative's rows.  Aborts (`Unstratifiable`) when the
    /// program carries negation (a merge could retroactively change an
    /// absence check), and (`Inconsistent`) on a rigid conflict — carrying
    /// the citation chain of the equality path between the two numeric
    /// literals.
    fn apply_union(
        &mut self,
        x:    SymbolId,
        y:    SymbolId,
        edge: EqEdge,
        news: &mut Vec<(Pred, Tuple)>,
    ) -> Result<(), ModelError> {
        if self.has_negation {
            return Err(ModelError::Unstratifiable);
        }
        match self.prov.eq.union(x, y, edge, &self.prog.rigid) {
            UnionOutcome::Noop => Ok(()),
            UnionOutcome::Rigid { a, b } => {
                let chain = self.prov.explain_eq(a, b);
                Err(ModelError::Inconsistent(chain))
            }
            UnionOutcome::Merged { absorbed } => {
                self.merged = true;
                self.recanon(absorbed, news)
            }
        }
    }

    /// Re-canonicalize after a merge: every row (in any relation, at any
    /// position) carrying the absorbed representative is re-inserted in
    /// canonical form; rows NEW to their relation's set flow into `news`
    /// (the current delta) and may cascade further EGD firings.
    fn recanon(
        &mut self,
        absorbed: SymbolId,
        news:     &mut Vec<(Pred, Tuple)>,
    ) -> Result<(), ModelError> {
        let mut work: Vec<(Pred, Tuple)> = Vec::new();
        for (p, rel) in &self.store.rels {
            for posmap in &rel.by_pos {
                if let Some(idxs) = posmap.get(&absorbed) {
                    for &i in idxs {
                        work.push((*p, rel.rows[i as usize].clone()));
                    }
                }
            }
        }
        work.sort_unstable();
        work.dedup();
        for (p, row) in work {
            let canon = self.prov.eq.canon_tuple(&row);
            if canon != row {
                self.insert_row(
                    p,
                    canon,
                    Some((EQ_CANON_RULE, std::iter::once((p, row)).collect())),
                    news,
                )?;
            }
        }
        Ok(())
    }

    // -- rule firing ----------------------------------------------------------

    /// Evaluate one rule body, emitting head tuples together with the ground
    /// tuples the positive body literals matched (the head's first-parent
    /// provenance trail).  `driver` forces one body literal to range over a
    /// delta tuple slice (semi-naive); `None` ranges all literals over the
    /// full store (the round-0 / exhaustive / merge-catch-up pass).
    fn fire(
        &self,
        body:   &[Literal],
        head:   &Atom,
        driver: Option<(usize, &[Tuple])>,
        out:    &mut Vec<(Tuple, Parents)>,
    ) {
        // After a merge, rule constants may be stale — canonicalize copies.
        let canon_store: (Vec<Literal>, Atom);
        let (body, head) = if self.prov.eq.merged() {
            canon_store = (
                body.iter()
                    .map(|l| Literal { atom: self.canon_atom(&l.atom), negated: l.negated })
                    .collect(),
                self.canon_atom(head),
            );
            (canon_store.0.as_slice(), &canon_store.1)
        } else {
            (body, head)
        };
        let driver_idx = driver.map(|(d, _)| d);
        // Order: driver first (small), then the other positive literals (so
        // each is reached with bound positions for the index), then negated
        // filters.
        let mut order: Vec<usize> = Vec::with_capacity(body.len());
        if let Some(d) = driver_idx {
            order.push(d);
        }
        for (i, l) in body.iter().enumerate() {
            if Some(i) != driver_idx && !l.negated {
                order.push(i);
            }
        }
        for (i, l) in body.iter().enumerate() {
            if l.negated {
                order.push(i);
            }
        }
        let mut binding: HashMap<u32, SymbolId> = HashMap::new();
        let mut trail: Vec<(Pred, Tuple)> = Vec::with_capacity(body.len());
        join_rec(self, body, &order, 0, driver, &mut binding, &mut trail, head, out);
    }
}

#[allow(clippy::too_many_arguments)]
fn join_rec(
    k:       &Kernel,
    body:    &[Literal],
    order:   &[usize],
    oi:      usize,
    driver:  Option<(usize, &[Tuple])>,
    binding: &mut HashMap<u32, SymbolId>,
    trail:   &mut Vec<(Pred, Tuple)>,
    head:    &Atom,
    out:     &mut Vec<(Tuple, Parents)>,
) {
    // Finer-grained deadline check (task: "checked every N join-branch
    // expansions, N~1000, via a cheap counter — do NOT call Instant::now()
    // per branch").  `join_over_deadline` throttles the actual clock read
    // internally; this call itself is just a `Cell` increment + compare on
    // the common path, so it is safe to call on every recursive entry
    // (equivalently, every join-branch expansion) rather than only once per
    // rule the way the old per-rule check did.
    if k.join_over_deadline() {
        return;
    }
    if oi == order.len() {
        if let Some(t) = ground_atom(head, binding) {
            // Skip tuples the store already holds: the insert would discard
            // them anyway (the store is not mutated while a `fire` is in
            // flight), and re-derivations vastly outnumber first derivations
            // on dense programs — this keeps the provenance trail snapshot
            // off the re-derivation flood.
            if !k.store.contains(head.pred, &t) {
                out.push((t, trail.iter().cloned().collect()));
            }
        }
        return;
    }
    let li = order[oi];
    let lit = &body[li];
    if lit.negated {
        // Negated literals contribute no parents (they cite absence).
        // Closure-aware for builtin relations: absence means absence from
        // the CLOSURE, not just the base edges.
        if let Some(t) = ground_atom(&lit.atom, binding) {
            if !k.contains_closure(lit.atom.pred, &t) {
                join_rec(k, body, order, oi + 1, driver, binding, trail, head, out);
            }
        }
        return;
    }
    let cands: Vec<Tuple> = match driver {
        Some((d, tuples)) if d == li => tuples.to_vec(),
        _ => k.candidates(&lit.atom, binding),
    };
    for tup in &cands {
        if let Some(undo) = unify(&lit.atom.args, tup, binding) {
            trail.push((lit.atom.pred, tup.clone()));
            join_rec(k, body, order, oi + 1, driver, binding, trail, head, out);
            trail.pop();
            for v in undo {
                binding.remove(&v);
            }
        }
    }
}

/// Run the program to its perfect model, stratum by stratum, semi-naively.
/// `strata` is the precomputed stratification; bails with `Overflow` past
/// `max_tuples`.  Returns the model together with its [`Provenance`]: each
/// derived fact's FIRST derivation (deriving rule index into `prog.rules` +
/// the ground tuples its positive body literals matched), plus the program's
/// rule/EDB source sentences and the evaluation's equality classes —
/// everything `Provenance::cite` needs.
pub(super) fn run(
    prog:       &Program,
    strata:     &[Vec<Pred>],
    max_tuples: usize,
    deadline:   Option<Instant>,
) -> Result<(Model, Provenance), ModelError> {
    let mut k = Kernel::new(prog, max_tuples, deadline);
    // Between-rule deadline check (cheap: at most once per rule per round).
    // The FINER-grained check — inside a single rule's join, so one dense
    // `fire` call can't itself run arbitrarily far past `deadline` — is
    // `Kernel::join_over_deadline`, consulted from `join_rec` on every
    // recursive entry and surfaced here via `k.deadline_hit` right after
    // each `fire` call (mirrors the existing post-`fire` `check_budget()?`).
    let over_deadline = |d: Option<Instant>| d.is_some_and(|dl| Instant::now() > dl);

    let mut news: Vec<(Pred, Tuple)> = Vec::new();
    // EDB load with EGD probing DEFERRED: every fact lands first, then one
    // deterministic probe pass fires the EGDs against the complete EDB
    // (guard checks would otherwise depend on `HashMap` iteration order).
    for (p, facts) in &prog.edb {
        for t in facts {
            k.insert_row_inner(*p, t.clone(), None, &mut news, false)?;
        }
    }
    let mut egd_rels: Vec<Pred> = k.egd_idx.keys().copied().collect();
    egd_rels.sort_unstable();
    for pred in egd_rels {
        let rows: Vec<Tuple> = k
            .store
            .rels
            .get(&pred)
            .map(|r| r.rows.clone())
            .unwrap_or_default();
        for row in rows {
            let cr = k.prov.eq.canon_tuple(&row);
            k.egd_probe(pred, &cr, &mut news)?;
        }
    }
    // Pre-strata rows (and EDB-phase merge products) need no delta: the
    // round-0 pass scans the full store.
    news.clear();
    k.take_merged();
    k.take_builtin_grew();

    for stratum in strata {
        let in_stratum: HashSet<Pred> = stratum.iter().copied().collect();
        // Carry each rule's ORIGINAL index (provenance records it, and it
        // must refer to `prog.rules`, not the filtered slice).
        let srules: Vec<(u32, &Rule)> = prog
            .rules
            .iter()
            .enumerate()
            .filter(|(_, r)| in_stratum.contains(&r.head.pred))
            .map(|(i, r)| (i as u32, r))
            .collect();
        if srules.is_empty() {
            continue;
        }

        // Round 0: full evaluation over the store (EDB + lower strata + this
        // stratum's EDB), seeding the delta.
        let mut delta: HashMap<Pred, Vec<Tuple>> = HashMap::new();
        for (ri, r) in &srules {
            if over_deadline(deadline) {
                return Err(ModelError::Overflow);
            }
            let mut out: Vec<(Tuple, Parents)> = Vec::new();
            k.fire(&r.body, &r.head, None, &mut out);
            k.check_budget()?;
            for (t, parents) in out {
                k.insert_row(r.head.pred, t, Some((*ri, parents)), &mut news)?;
                for (p, nt) in news.drain(..) {
                    delta.entry(p).or_default().push(nt);
                }
            }
        }

        // Semi-naive rounds: fire each rule with each recursive (in-stratum)
        // body literal driven by its delta.  A full CATCH-UP pass (driver =
        // None) re-fires everything after a round in which (a) an EGD merged
        // — re-canonicalized rows of relations outside this stratum's delta
        // driving would otherwise be missed — or (b) a BUILTIN relation
        // gained base edges — a new edge extends the closure by pairs no
        // delta tuple represents.  Terminates: each catch-up requires a
        // strictly growing store / strictly shrinking class count.
        let trace = std::env::var_os("SIGMA_MODEL_TRACE").is_some();
        let mut round = 0usize;
        let mut catchup = k.take_merged() || k.take_builtin_grew();
        loop {
            round += 1;
            if trace {
                let dsz: usize = delta.values().map(Vec::len).sum();
                let mut sizes: Vec<(usize, Pred)> = k
                    .store
                    .rels
                    .iter()
                    .map(|(p, r)| (r.rows.len(), *p))
                    .collect();
                sizes.sort_unstable_by(|a, b| b.cmp(a));
                let top: Vec<String> = sizes
                    .iter()
                    .take(8)
                    .map(|(n, p)| format!("{p:#x}:{n}"))
                    .collect();
                eprintln!(
                    "KERNEL round {round}: total={} aux={} delta={} catchup={} top=[{}]",
                    k.total, k.aux.get(), dsz, catchup, top.join(", "),
                );
            }
            let have_delta = delta.values().any(|v| !v.is_empty());
            if !have_delta && !catchup {
                break;
            }
            let mut next: HashMap<Pred, Vec<Tuple>> = HashMap::new();
            if have_delta {
                for (ri, r) in &srules {
                    if over_deadline(deadline) {
                        return Err(ModelError::Overflow);
                    }
                    for (i, lit) in r.body.iter().enumerate() {
                        if lit.negated || !in_stratum.contains(&lit.atom.pred) {
                            continue;
                        }
                        let Some(drv) = delta.get(&lit.atom.pred) else { continue };
                        if drv.is_empty() {
                            continue;
                        }
                        // A builtin relation's delta is driven RAW (its new
                        // base edges); the closure pairs a new edge newly
                        // justifies are picked up by the catch-up pass below
                        // (`builtin_grew`) — never materialized.
                        let drv: Vec<Tuple> = drv.clone();
                        let mut out: Vec<(Tuple, Parents)> = Vec::new();
                        k.fire(&r.body, &r.head, Some((i, &drv)), &mut out);
                        k.check_budget()?;
                        for (t, parents) in out {
                            k.insert_row(r.head.pred, t, Some((*ri, parents)), &mut news)?;
                            for (p, nt) in news.drain(..) {
                                next.entry(p).or_default().push(nt);
                            }
                        }
                    }
                }
            }
            if catchup {
                for (ri, r) in &srules {
                    if over_deadline(deadline) {
                        return Err(ModelError::Overflow);
                    }
                    let mut out: Vec<(Tuple, Parents)> = Vec::new();
                    k.fire(&r.body, &r.head, None, &mut out);
                    k.check_budget()?;
                    for (t, parents) in out {
                        k.insert_row(r.head.pred, t, Some((*ri, parents)), &mut news)?;
                        for (p, nt) in news.drain(..) {
                            next.entry(p).or_default().push(nt);
                        }
                    }
                }
            }
            catchup = k.take_merged() || k.take_builtin_grew();
            delta = next;
        }
    }

    // Ensure every head predicate is present (empty if nothing derived).
    let Kernel { store, mut prov, total, aux, .. } = k;
    prov.budget_used = total + aux.get();
    let mut model = store.into_model();
    for r in &prog.rules {
        model.entry(r.head.pred).or_default();
    }
    Ok((model, prov))
}
