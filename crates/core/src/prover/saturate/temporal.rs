// crates/core/src/saturate/temporal.rs
//
// Temporal reasoning for the oracle — a point-constraint network (the
// STP / point-algebra fragment) over time-interval endpoints.
//
// SUMO axiomatizes its interval relations directly in terms of the
// endpoint functions `BeginFn`/`EndFn` and the strict point order
// `before` (see docs/temporal-reasoning-plan.md, audited against
// Merge.kif).  So the reasoning reduces to: mint a `Begin`/`End` point
// per `TimeInterval`, translate each interval-relation fact into `<` /
// `≤` / `=` constraints between those points, close under transitivity,
// and answer a temporal-relation goal by checking entailment (or, for a
// negated goal, refutation via inconsistency).
//
// This is the engine only (Phase 1).  Building it from KB facts and
// wiring discharge into `SemanticOracle::holds` is Phase 2.

use std::collections::HashMap;
use std::collections::VecDeque;

use crate::types::{SentenceId, SymbolId};

/// A node in the network: a time point identity.  An interval `I`
/// contributes `Begin(I)` and `End(I)`; a bare `TimePoint` `P` is a
/// degenerate interval with `Begin(P) = End(P) = Point(P)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum PointKey {
    Point(SymbolId),
    Begin(SymbolId),
    End(SymbolId),
}

/// An order query between two points.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Order {
    /// strictly before (`a < b`)
    Lt,
    /// before-or-equal (`a ≤ b`)
    Le,
    /// equal (`a = b`)
    Eq,
}

/// A point-constraint network: nodes are time points, edges are
/// `<` / `≤` constraints, `=` is a union-find.  Closure is a transitive
/// reachability over the edges (a path is strict iff it crosses ≥1
/// strict edge); inconsistency is a strict self-loop (`a < a`).
#[derive(Debug, Default)]
pub(crate) struct TemporalNet {
    index:  HashMap<PointKey, usize>,
    /// union-find parent (for `=`).
    parent: Vec<usize>,
    /// raw constraints `(a, b, strict)` meaning `a < b` (strict) or `a ≤ b`.
    edges:  Vec<(usize, usize, bool)>,
    /// Proof-provenance adjacency, keyed by [`PointKey`] (NOT collapsed
    /// node ids): every constraint as a directed edge `from →(strict, sid)
    /// to`.  Equality contributes both directions.  Used only by
    /// [`Self::path_witness`] to recover the chain of KB facts behind an
    /// entailment — independent of the boolean decision machinery.
    wadj:   HashMap<PointKey, Vec<(PointKey, bool, Option<SentenceId>)>>,
    /// closure (computed by `close`): `le[a][b]` ⇔ `a ≤ b`, `lt[a][b]` ⇔ `a < b`.
    le:     Vec<Vec<bool>>,
    lt:     Vec<Vec<bool>>,
    consistent: bool,
    closed: bool,
}

impl TemporalNet {
    pub(crate) fn new() -> Self {
        Self { consistent: true, ..Default::default() }
    }

    /// Intern a point key to a node id.
    fn node(&mut self, key: PointKey) -> usize {
        if let Some(&n) = self.index.get(&key) {
            return n;
        }
        let n = self.parent.len();
        self.parent.push(n);
        self.index.insert(key, n);
        self.closed = false;
        n
    }

    fn find(&mut self, mut a: usize) -> usize {
        while self.parent[a] != a {
            self.parent[a] = self.parent[self.parent[a]];
            a = self.parent[a];
        }
        a
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra] = rb;
            self.closed = false;
        }
    }

    // -- constraint builders -------------------------------------------------

    /// Record a directed witness edge `a →(strict, sid) b`.
    fn wedge(&mut self, a: PointKey, b: PointKey, strict: bool, sid: Option<SentenceId>) {
        self.wadj.entry(a).or_default().push((b, strict, sid));
    }

    /// `a < b`.
    pub(crate) fn add_lt(&mut self, a: PointKey, b: PointKey) {
        self.add_lt_w(a, b, None);
    }

    /// `a < b`, citing the KB fact `sid` it came from (for witnesses).
    pub(crate) fn add_lt_w(&mut self, a: PointKey, b: PointKey, sid: Option<SentenceId>) {
        self.wedge(a, b, true, sid);
        let (a, b) = (self.node(a), self.node(b));
        self.edges.push((a, b, true));
        self.closed = false;
    }

    /// `a ≤ b`.
    pub(crate) fn add_le(&mut self, a: PointKey, b: PointKey) {
        self.add_le_w(a, b, None);
    }

    /// `a ≤ b`, citing the KB fact `sid` it came from.
    pub(crate) fn add_le_w(&mut self, a: PointKey, b: PointKey, sid: Option<SentenceId>) {
        self.wedge(a, b, false, sid);
        let (a, b) = (self.node(a), self.node(b));
        self.edges.push((a, b, false));
        self.closed = false;
    }

    /// `a = b`.
    pub(crate) fn add_eq(&mut self, a: PointKey, b: PointKey) {
        self.add_eq_w(a, b, None);
    }

    /// `a = b`, citing the KB fact `sid` it came from.  Equality is a
    /// witness edge in BOTH directions (non-strict).
    pub(crate) fn add_eq_w(&mut self, a: PointKey, b: PointKey, sid: Option<SentenceId>) {
        self.wedge(a, b, false, sid);
        self.wedge(b, a, false, sid);
        let (a, b) = (self.node(a), self.node(b));
        self.union(a, b);
    }

    /// An interval's begin strictly precedes its end (`Begin(I) < End(I)`).
    /// SUMO `TimeInterval`s are proper.  Structural (no citing fact).
    pub(crate) fn add_interval(&mut self, i: SymbolId) {
        self.add_lt(PointKey::Begin(i), PointKey::End(i));
    }

    // -- closure + query -----------------------------------------------------

    /// Compute the transitive closure over `=`-collapsed nodes.  O(n³),
    /// fine for the handful of points in a temporal goal.
    pub(crate) fn close(&mut self) {
        let n = self.parent.len();
        let rep: Vec<usize> = (0..n).map(|i| self.find(i)).collect();
        let mut le = vec![vec![false; n]; n];
        let mut lt = vec![vec![false; n]; n];
        for i in 0..n {
            le[rep[i]][rep[i]] = true; // reflexive ≤
        }
        for &(a, b, strict) in &self.edges {
            let (a, b) = (rep[a], rep[b]);
            le[a][b] = true;
            if strict {
                lt[a][b] = true;
            }
        }
        // Floyd–Warshall: a path is `<` iff it crosses ≥1 strict edge.
        for k in 0..n {
            for i in 0..n {
                if !le[i][k] && !lt[i][k] {
                    continue;
                }
                for j in 0..n {
                    if le[k][j] || lt[k][j] {
                        le[i][j] = true;
                        if lt[i][k] || lt[k][j] {
                            lt[i][j] = true;
                        }
                    }
                }
            }
        }
        // Inconsistent iff some node is strictly before itself.
        self.consistent = (0..n).all(|i| !lt[i][i]);
        self.le = le;
        self.lt = lt;
        self.closed = true;
    }

    fn ensure_closed(&mut self) {
        if !self.closed {
            self.close();
        }
    }

    /// `true` iff the constraints are satisfiable (no `a < a`).
    pub(crate) fn consistent(&mut self) -> bool {
        self.ensure_closed();
        self.consistent
    }

    /// Whether the network ENTAILS `a <rel> b`.  Unknown points (never
    /// constrained) yield `false` — entailment is monotone, never a guess.
    pub(crate) fn entails(&mut self, a: PointKey, b: PointKey, rel: Order) -> bool {
        self.ensure_closed();
        let (Some(&a), Some(&b)) = (self.index.get(&a), self.index.get(&b)) else {
            return false;
        };
        let (ra, rb) = (self.find_imm(a), self.find_imm(b));
        match rel {
            Order::Eq => ra == rb,
            Order::Le => self.le[ra][rb],
            Order::Lt => self.lt[ra][rb],
        }
    }

    /// Read-only find (post-close `parent` is fully compressed enough for
    /// equality tests; no mutation so `entails` can take `&mut` only for
    /// the lazy close).
    fn find_imm(&self, mut a: usize) -> usize {
        while self.parent[a] != a {
            a = self.parent[a];
        }
        a
    }

    /// The KB facts (sids) witnessing an ENTAILED order `a <rel> b`.  Run
    /// only after [`Self::entails`] confirmed the relation holds, so a path
    /// is guaranteed to exist.  Structural interval edges (no sid) drop
    /// out; equality is witnessed in both directions.
    pub(crate) fn witness(&self, a: PointKey, b: PointKey, rel: Order) -> Vec<SentenceId> {
        let mut out = match rel {
            Order::Le => self.path_witness(a, b, false),
            Order::Lt => self.path_witness(a, b, true),
            Order::Eq => {
                let mut w = self.path_witness(a, b, false);
                w.extend(self.path_witness(b, a, false));
                w
            }
        };
        out.sort_unstable();
        out.dedup();
        out
    }

    /// BFS over the witness adjacency for a path `a → b`; when
    /// `need_strict`, the path must cross ≥1 strict edge.  Returns the
    /// `sid`s along the discovered path (`None`-sid structural edges
    /// omitted), or empty when `a == b` already satisfies the goal.
    fn path_witness(&self, a: PointKey, b: PointKey, need_strict: bool) -> Vec<SentenceId> {
        if a == b && !need_strict {
            return Vec::new();
        }
        // State = (node, has-crossed-a-strict-edge).  Parent map records
        // the edge taken to reach each state for reconstruction.
        let start = (a, false);
        let mut parent: HashMap<(PointKey, bool), (PointKey, bool, Option<SentenceId>)> =
            HashMap::new();
        let mut seen: std::collections::HashSet<(PointKey, bool)> = std::collections::HashSet::new();
        let mut q: VecDeque<(PointKey, bool)> = VecDeque::new();
        seen.insert(start);
        q.push_back(start);
        while let Some(state) = q.pop_front() {
            let (node, crossed) = state;
            if node == b && (crossed || !need_strict) {
                // Reconstruct the path back to `start`, gathering sids.
                let mut sids = Vec::new();
                let mut cur = state;
                while cur != start {
                    let (pn, pc, sid) = parent[&cur];
                    if let Some(s) = sid {
                        sids.push(s);
                    }
                    cur = (pn, pc);
                }
                return sids;
            }
            let Some(neigh) = self.wadj.get(&node) else { continue };
            for &(to, strict, sid) in neigh {
                let next = (to, crossed || strict);
                if seen.insert(next) {
                    parent.insert(next, (node, crossed, sid));
                    q.push_back(next);
                }
            }
        }
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// SUMO interval-relation dictionary + build-from-facts + query.
// ---------------------------------------------------------------------------

use crate::types::Symbol;

/// The temporal relation heads (hard-coded SUMO names; a recognition seam
/// can later re-derive them, per the plan's "hybrid" choice).
#[derive(Debug, Clone, Copy)]
pub(crate) struct TemporalRelIds {
    pub before:        SymbolId,
    pub earlier:       SymbolId,
    pub meets:         SymbolId,
    pub during:        SymbolId,
    pub starts:        SymbolId,
    pub finishes:      SymbolId,
    pub temporal_part: SymbolId,
    pub time_point:    SymbolId,
}

impl TemporalRelIds {
    pub(crate) fn standard() -> Self {
        Self {
            before:        Symbol::hash_name("before"),
            earlier:       Symbol::hash_name("earlier"),
            meets:         Symbol::hash_name("meetsTemporally"),
            during:        Symbol::hash_name("during"),
            starts:        Symbol::hash_name("starts"),
            finishes:      Symbol::hash_name("finishes"),
            temporal_part: Symbol::hash_name("temporalPart"),
            time_point:    Symbol::hash_name("TimePoint"),
        }
    }

    /// Is `rel` a temporal relation the network can reason about?
    pub(crate) fn is_temporal(&self, rel: SymbolId) -> bool {
        rel == self.before || rel == self.earlier || rel == self.meets
            || rel == self.during || rel == self.starts || rel == self.finishes
            || rel == self.temporal_part
    }

    /// The interval-relation heads that take two intervals (all but
    /// `before`, which is point×point, and `temporalPart`, whose first arg
    /// may be a point) — used to drive the build scan.
    fn interval_rels(&self) -> [(SymbolId, IntervalShape); 5] {
        use IntervalShape::*;
        [
            (self.earlier,  Earlier),
            (self.meets,    Meets),
            (self.during,   During),
            (self.starts,   Starts),
            (self.finishes, Finishes),
        ]
    }
}

#[derive(Clone, Copy)]
enum IntervalShape { Earlier, Meets, During, Starts, Finishes }

/// The `(Begin, End)` keys for a symbol: a degenerate point collapses to
/// `Point(s) = Point(s)`, an interval splits into `Begin(s)`/`End(s)`.
fn endpoints(net: &mut TemporalNet, s: SymbolId, is_point: bool) -> (PointKey, PointKey) {
    if is_point {
        (PointKey::Point(s), PointKey::Point(s))
    } else {
        net.add_interval(s);
        (PointKey::Begin(s), PointKey::End(s))
    }
}

/// Build a point network from the KB's temporal facts.  `facts(rel)`
/// yields the `(x, y, sid)` arguments + provenance of ground binary `rel`
/// atoms; `is_point(s)` classifies a symbol as a `TimePoint` (vs
/// interval).  Each constraint carries its `sid` for witness recovery.
pub(crate) fn build_net(
    ids:      &TemporalRelIds,
    mut facts: impl FnMut(SymbolId) -> Vec<(SymbolId, SymbolId, Option<SentenceId>)>,
    is_point: impl Fn(SymbolId) -> bool,
) -> TemporalNet {
    use PointKey::{Begin, End};
    let mut net = TemporalNet::new();

    for (x, y, sid) in facts(ids.before) {
        // before is point×point.
        net.add_lt_w(PointKey::Point(x), PointKey::Point(y), sid);
    }
    for (head, shape) in ids.interval_rels() {
        for (i, j, sid) in facts(head) {
            net.add_interval(i);
            net.add_interval(j);
            match shape {
                IntervalShape::Earlier  => net.add_lt_w(End(i), Begin(j), sid),
                IntervalShape::Meets    => net.add_eq_w(End(i), Begin(j), sid),
                IntervalShape::During   => { net.add_lt_w(Begin(j), Begin(i), sid); net.add_lt_w(End(i), End(j), sid); }
                IntervalShape::Starts   => { net.add_eq_w(Begin(i), Begin(j), sid); net.add_lt_w(End(i), End(j), sid); }
                IntervalShape::Finishes => { net.add_eq_w(End(i), End(j), sid);     net.add_lt_w(Begin(j), Begin(i), sid); }
            }
        }
    }
    // temporalPart(X, Y): Begin(Y) ≤ Begin(X) ∧ End(X) ≤ End(Y); X may be a point.
    for (x, y, sid) in facts(ids.temporal_part) {
        let (bx, ex) = endpoints(&mut net, x, is_point(x));
        let (by, ey) = endpoints(&mut net, y, false);
        let _ = ey; // Y is an interval; bind for symmetry
        net.add_le_w(by, bx, sid);
        net.add_le_w(ex, End(y), sid);
        let _ = ex;
    }
    net
}

/// Whether the network ENTAILS the temporal goal `(rel x y)` (positive
/// discharge).  `Unknown`/non-temporal ⇒ `false` ⇒ resolution.
pub(crate) fn query(
    net:      &mut TemporalNet,
    ids:      &TemporalRelIds,
    rel:      SymbolId,
    x:        SymbolId,
    y:        SymbolId,
    is_point: impl Fn(SymbolId) -> bool,
) -> bool {
    use PointKey::{Begin, End, Point};
    if rel == ids.before {
        net.entails(Point(x), Point(y), Order::Lt)
    } else if rel == ids.earlier {
        net.entails(End(x), Begin(y), Order::Lt)
    } else if rel == ids.meets {
        net.entails(End(x), Begin(y), Order::Eq)
    } else if rel == ids.during {
        net.entails(Begin(y), Begin(x), Order::Lt) && net.entails(End(x), End(y), Order::Lt)
    } else if rel == ids.starts {
        net.entails(Begin(x), Begin(y), Order::Eq) && net.entails(End(x), End(y), Order::Lt)
    } else if rel == ids.finishes {
        net.entails(End(x), End(y), Order::Eq) && net.entails(Begin(y), Begin(x), Order::Lt)
    } else if rel == ids.temporal_part {
        let (bx, ex) = if is_point(x) { (Point(x), Point(x)) } else { (Begin(x), End(x)) };
        net.entails(Begin(y), bx, Order::Le) && net.entails(ex, End(y), Order::Le)
    } else {
        false
    }
}

/// The KB facts witnessing an entailed temporal goal `(rel x y)` — the
/// same endpoint reduction as [`query`], unioned over the constraint
/// conjuncts.  Call only after `query` returned `true`.
pub(crate) fn query_witness(
    net:      &mut TemporalNet,
    ids:      &TemporalRelIds,
    rel:      SymbolId,
    x:        SymbolId,
    y:        SymbolId,
    is_point: impl Fn(SymbolId) -> bool,
) -> Vec<SentenceId> {
    use PointKey::{Begin, End, Point};
    net.ensure_closed();
    let mut w = if rel == ids.before {
        net.witness(Point(x), Point(y), Order::Lt)
    } else if rel == ids.earlier {
        net.witness(End(x), Begin(y), Order::Lt)
    } else if rel == ids.meets {
        net.witness(End(x), Begin(y), Order::Eq)
    } else if rel == ids.during {
        let mut w = net.witness(Begin(y), Begin(x), Order::Lt);
        w.extend(net.witness(End(x), End(y), Order::Lt));
        w
    } else if rel == ids.starts {
        let mut w = net.witness(Begin(x), Begin(y), Order::Eq);
        w.extend(net.witness(End(x), End(y), Order::Lt));
        w
    } else if rel == ids.finishes {
        let mut w = net.witness(End(x), End(y), Order::Eq);
        w.extend(net.witness(Begin(y), Begin(x), Order::Lt));
        w
    } else if rel == ids.temporal_part {
        let (bx, ex) = if is_point(x) { (Point(x), Point(x)) } else { (Begin(x), End(x)) };
        let mut w = net.witness(Begin(y), bx, Order::Le);
        w.extend(net.witness(ex, End(y), Order::Le));
        w
    } else {
        Vec::new()
    };
    w.sort_unstable();
    w.dedup();
    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Symbol;

    fn p(name: &str) -> SymbolId { Symbol::hash_name(name) }
    fn pt(name: &str) -> PointKey { PointKey::Point(Symbol::hash_name(name)) }

    #[test]
    fn point_order_transitive_and_strict() {
        let mut n = TemporalNet::new();
        n.add_lt(pt("a"), pt("b"));
        n.add_lt(pt("b"), pt("c"));
        assert!(n.entails(pt("a"), pt("c"), Order::Lt));
        assert!(n.entails(pt("a"), pt("c"), Order::Le));
        assert!(!n.entails(pt("c"), pt("a"), Order::Lt));
        assert!(n.consistent());
    }

    #[test]
    fn le_chain_is_not_strict() {
        let mut n = TemporalNet::new();
        n.add_le(pt("a"), pt("b"));
        n.add_le(pt("b"), pt("c"));
        assert!(n.entails(pt("a"), pt("c"), Order::Le));
        assert!(!n.entails(pt("a"), pt("c"), Order::Lt)); // no strict edge crossed
    }

    #[test]
    fn one_strict_edge_makes_path_strict() {
        let mut n = TemporalNet::new();
        n.add_le(pt("a"), pt("b"));
        n.add_lt(pt("b"), pt("c"));
        n.add_le(pt("c"), pt("d"));
        assert!(n.entails(pt("a"), pt("d"), Order::Lt));
    }

    #[test]
    fn equality_merges_points() {
        let mut n = TemporalNet::new();
        n.add_eq(pt("a"), pt("b"));
        n.add_lt(pt("b"), pt("c"));
        assert!(n.entails(pt("a"), pt("c"), Order::Lt)); // a=b<c
        assert!(n.entails(pt("a"), pt("b"), Order::Eq));
    }

    #[test]
    fn strict_cycle_is_inconsistent() {
        let mut n = TemporalNet::new();
        n.add_lt(pt("a"), pt("b"));
        n.add_lt(pt("b"), pt("a"));
        assert!(!n.consistent());
    }

    // ---- the target-test reductions, at the network level ----

    // TQG37: earlier(I1,I2) ∧ P1∈I1 ∧ P2∈I2 ⟹ ¬before(P2,P1)
    #[test]
    fn tqg37_earlier_orders_interior_points() {
        use PointKey::*;
        let (i1, i2) = (p("I1"), p("I2"));
        let mut n = TemporalNet::new();
        n.add_interval(i1);
        n.add_interval(i2);
        // earlier(I1,I2): End(I1) < Begin(I2)
        n.add_lt(End(i1), Begin(i2));
        // temporalPart(P1,I1): Begin(I1) ≤ P1 ≤ End(I1)
        n.add_le(Begin(i1), pt("P1"));
        n.add_le(pt("P1"), End(i1));
        // temporalPart(P2,I2): Begin(I2) ≤ P2 ≤ End(I2)
        n.add_le(Begin(i2), pt("P2"));
        n.add_le(pt("P2"), End(i2));
        // P1 ≤ End(I1) < Begin(I2) ≤ P2  ⟹  P1 < P2, so ¬(P2 < P1)
        assert!(n.entails(pt("P1"), pt("P2"), Order::Lt));
        assert!(!n.entails(pt("P2"), pt("P1"), Order::Lt));
        // refutation of before(P2,P1): adding P2 < P1 contradicts P1 < P2.
        let mut m = n;
        m.add_lt(pt("P2"), pt("P1"));
        assert!(!m.consistent());
    }

    // TQG35: temporalPart(P,I1) ∧ during(I1,I2) ⟹ temporalPart(P,I2)
    #[test]
    fn tqg35_point_in_during_interval() {
        use PointKey::*;
        let (i1, i2) = (p("I1"), p("I2"));
        let mut n = TemporalNet::new();
        n.add_interval(i1);
        n.add_interval(i2);
        // during(I1,I2): Begin(I2) < Begin(I1) ∧ End(I1) < End(I2)
        n.add_lt(Begin(i2), Begin(i1));
        n.add_lt(End(i1), End(i2));
        // temporalPart(P,I1): Begin(I1) ≤ P ≤ End(I1)
        n.add_le(Begin(i1), pt("P"));
        n.add_le(pt("P"), End(i1));
        // ⟹ temporalPart(P,I2): Begin(I2) ≤ P ∧ P ≤ End(I2)
        assert!(n.entails(Begin(i2), pt("P"), Order::Le));
        assert!(n.entails(pt("P"), End(i2), Order::Le));
    }

    // The Phase-2 build+query path: a deep meets-chain ⟹ before(first, last).
    // This is the case ordinary resolution times out on (N≥20).
    #[test]
    fn build_and_query_deep_meets_chain() {
        const N: usize = 50;
        let ids = TemporalRelIds::standard();
        let iv = |k: usize| p(&format!("I{k}"));
        let (pa, pz) = (p("Pa"), p("Pz"));
        // facts(rel): meets chain + temporalPart of the two endpoints.
        let facts = |rel: SymbolId| -> Vec<(SymbolId, SymbolId, Option<SentenceId>)> {
            if rel == ids.meets {
                (1..N).map(|k| (iv(k), iv(k + 1), None)).collect()
            } else if rel == ids.temporal_part {
                vec![(pa, iv(1), None), (pz, iv(N), None)]
            } else {
                vec![]
            }
        };
        let is_point = move |s: SymbolId| s == pa || s == pz;
        let mut net = build_net(&ids, facts, is_point);
        // before(Pa, Pz) is entailed; the reverse is not.
        assert!(query(&mut net, &ids, ids.before, pa, pz, is_point));
        assert!(!query(&mut net, &ids, ids.before, pz, pa, is_point));
        // and the interval-level earlier(I1, IN).
        assert!(query(&mut net, &ids, ids.earlier, iv(1), iv(N), is_point));
    }

    // Witness recovery: the jail shift structure — a point inside a
    // meeting-segment chain falls within the whole shift, and the proof
    // cites exactly the temporal facts on the entailing endpoint paths.
    #[test]
    fn temporal_part_witness_traces_the_shift_chain() {
        let ids = TemporalRelIds::standard();
        let (shift, s1, s2, s3, pc) =
            (p("Shift"), p("Seg1"), p("Seg2"), p("Seg3"), p("PhoneCheck"));
        // Distinct sids for each fact (SentenceId = u64).
        let (sid_starts, sid_m12, sid_m23, sid_fin, sid_tp) = (10u64, 11, 12, 13, 14);
        let facts = move |rel: SymbolId| -> Vec<(SymbolId, SymbolId, Option<SentenceId>)> {
            if rel == ids.meets {
                vec![(s1, s2, Some(sid_m12)), (s2, s3, Some(sid_m23))]
            } else if rel == ids.starts {
                vec![(s1, shift, Some(sid_starts))]
            } else if rel == ids.finishes {
                vec![(s3, shift, Some(sid_fin))]
            } else if rel == ids.temporal_part {
                vec![(pc, s2, Some(sid_tp))]
            } else {
                vec![]
            }
        };
        let is_point = move |s: SymbolId| s == pc;
        let mut net = build_net(&ids, facts, is_point);
        // PhoneCheck falls within the whole Shift.
        assert!(query(&mut net, &ids, ids.temporal_part, pc, shift, is_point));
        let w = query_witness(&mut net, &ids, ids.temporal_part, pc, shift, is_point);
        // Every shift-structure fact is on an entailing path and cited;
        // the chain is connected (both ≤ directions covered).
        for sid in [sid_tp, sid_starts, sid_m12, sid_m23, sid_fin] {
            assert!(w.contains(&sid), "missing witness sid {sid}; got {w:?}");
        }
    }

    // TQG36: starts(I1,K) ∧ starts(I2,K) ⟹ Begin(I1)=Begin(I2) (shared start)
    #[test]
    fn tqg36_shared_start_point() {
        use PointKey::*;
        let (i1, i2, k) = (p("I1"), p("I2"), p("K"));
        let mut n = TemporalNet::new();
        for iv in [i1, i2, k] { n.add_interval(iv); }
        // starts(I1,K): Begin(I1)=Begin(K), End(I1)<End(K)
        n.add_eq(Begin(i1), Begin(k));
        n.add_lt(End(i1), End(k));
        // starts(I2,K): Begin(I2)=Begin(K), End(I2)<End(K)
        n.add_eq(Begin(i2), Begin(k));
        n.add_lt(End(i2), End(k));
        // shared start ⟹ the overlap witness Begin(I1)=Begin(I2)
        assert!(n.entails(Begin(i1), Begin(i2), Order::Eq));
        // and each begin is strictly inside the other (Begin(I1) < End(I2))
        assert!(n.entails(Begin(i1), End(i2), Order::Lt));
        assert!(n.entails(Begin(i2), End(i1), Order::Lt));
    }
}
