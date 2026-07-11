// crates/core/src/saturate/model/cluster.rs
//
// Phase 4 — automatic definitional-cluster partitioning.
//
// Evaluating one giant program over a whole ontology fails: a single negation
// cycle anywhere makes the *monolith* `Unstratifiable` (the Merge.kif probe:
// 224 rules → unstratifiable as a whole, yet the taxonomy predicates form a
// clean stratifiable program on their own).  This module discovers those
// evaluable sub-programs automatically — replacing the hand-picked allowlist —
// and isolates the unstratifiable parts so they bail cleanly instead of
// poisoning the good clusters.
//
// Method (standard dependency-graph decomposition):
//   1. predicate dependency graph: edge head → body-pred, marked negative for
//      negated body literals;
//   2. condense into SCCs (mutual recursion); an SCC with a negative edge
//      *inside* it is unstratifiable → "bad";
//   3. taint = bad preds ∪ everything that can reach a bad pred (a predicate
//      depending on an unmodelable one can't be soundly modeled either);
//   4. the modelable preds partition (by the rules connecting them) into
//      clusters, each a self-contained, stratifiable sub-program.
//
// SOUNDNESS: the cluster boundary is the *exact* dependency closure — never a
// relevance heuristic — so each cluster is definitionally self-contained.
// Relevance (SInE) only *selects which* clusters to evaluate per query
// (`relevant_clusters`); it can narrow work but never decide truth.

use std::collections::{HashMap, HashSet};

use super::{Pred, Program};

use crate::prover::saturate::parked;

/// A definitional cluster: a set of predicates and the self-contained
/// sub-program (rules + EDB facts) that defines them.
#[derive(Debug, Clone)]
pub(crate) struct Cluster {
    pub preds:   HashSet<Pred>,
    #[allow(dead_code)] // parked
    pub program: Program,
}

/// Partition a program into stratifiable definitional clusters, dropping the
/// predicates tangled in (or depending on) a negation cycle.
pub(crate) fn partition(prog: &Program) -> Vec<Cluster> {
    // 1. all predicates.
    let mut preds: HashSet<Pred> = HashSet::new();
    for r in &prog.rules {
        preds.insert(r.head.pred);
        for l in &r.body {
            preds.insert(l.atom.pred);
        }
    }
    for p in prog.edb.keys() {
        preds.insert(*p);
    }

    // 2. dependency edges head → body, and the negated ones.
    let mut adj: HashMap<Pred, Vec<Pred>> = preds.iter().map(|p| (*p, Vec::new())).collect();
    let mut neg_edges: Vec<(Pred, Pred)> = Vec::new();
    for r in &prog.rules {
        for l in &r.body {
            adj.get_mut(&r.head.pred).unwrap().push(l.atom.pred);
            if l.negated {
                neg_edges.push((r.head.pred, l.atom.pred));
            }
        }
    }

    // 3. SCCs, then bad SCCs (a negated edge with both endpoints in one SCC).
    let scc_of = scc_index(&preds, &adj);
    let mut bad_scc: HashSet<usize> = HashSet::new();
    for (h, b) in &neg_edges {
        if scc_of[h] == scc_of[b] {
            bad_scc.insert(scc_of[h]);
        }
    }
    let bad: HashSet<Pred> = preds.iter().copied().filter(|p| bad_scc.contains(&scc_of[p])).collect();

    // 4. taint: reverse-reachability from bad preds along head → body edges
    //    (anything that depends on a bad pred is itself unmodelable).
    let mut rev: HashMap<Pred, Vec<Pred>> = preds.iter().map(|p| (*p, Vec::new())).collect();
    for (h, bs) in &adj {
        for b in bs {
            rev.get_mut(b).unwrap().push(*h);
        }
    }
    let mut tainted = bad.clone();
    let mut stack: Vec<Pred> = bad.iter().copied().collect();
    while let Some(p) = stack.pop() {
        for &up in &rev[&p] {
            if tainted.insert(up) {
                stack.push(up);
            }
        }
    }

    // 5. union-find over modelable preds, connected by the rules among them.
    let modelable: Vec<Pred> = preds.iter().copied().filter(|p| !tainted.contains(p)).collect();
    let mut uf = UnionFind::new(&modelable);
    for r in &prog.rules {
        if tainted.contains(&r.head.pred) {
            continue;
        }
        for l in &r.body {
            if !tainted.contains(&l.atom.pred) {
                uf.union(r.head.pred, l.atom.pred);
            }
        }
    }

    // 6. build a sub-program per component.
    let mut by_root: HashMap<Pred, HashSet<Pred>> = HashMap::new();
    for &p in &modelable {
        by_root.entry(uf.find(p)).or_default().insert(p);
    }
    let mut clusters = Vec::new();
    for (_, cpreds) in by_root {
        let mut program = Program {
            egds: prog.egds.iter().filter(|e| cpreds.contains(&e.rel)).cloned().collect(),
            builtin_transitive: prog
                .builtin_transitive
                .iter()
                .filter(|(r, _)| cpreds.contains(r))
                .map(|(r, s)| (*r, *s))
                .collect(),
            rigid: prog.rigid.clone(),
            instance_pred: prog.instance_pred,
            ..Program::default()
        };
        for (p, facts) in &prog.edb {
            if cpreds.contains(p) {
                program.edb.insert(*p, facts.clone());
            }
        }
        for ((p, t), sid) in &prog.edb_sids {
            if cpreds.contains(p) {
                program.edb_sids.insert((*p, t.clone()), *sid);
            }
        }
        for r in &prog.rules {
            if cpreds.contains(&r.head.pred) {
                program.rules.push(r.clone());
            }
        }
        clusters.push(Cluster { preds: cpreds, program });
    }
    clusters.sort_by_key(|c| std::cmp::Reverse(c.preds.len()));
    clusters
}

/// The predicates a query over `seed` actually needs: `seed` plus everything
/// transitively reachable through head → body edges (a relation's definition
/// pulls in the relations its rule bodies mention).  This is the demand
/// transformation — the dependency-exact analogue of SInE relevance — that
/// lets us materialize only the conjecture-relevant slice of a large program.
pub(crate) fn dependency_cone(prog: &Program, seed: &HashSet<Pred>) -> HashSet<Pred> {
    // Index rules by head predicate once (a big program has thousands).
    let mut by_head: HashMap<Pred, Vec<usize>> = HashMap::new();
    for (i, r) in prog.rules.iter().enumerate() {
        by_head.entry(r.head.pred).or_default().push(i);
    }
    let mut cone = seed.clone();
    let mut stack: Vec<Pred> = seed.iter().copied().collect();
    while let Some(p) = stack.pop() {
        for &ri in by_head.get(&p).into_iter().flatten() {
            for l in &prog.rules[ri].body {
                if cone.insert(l.atom.pred) {
                    stack.push(l.atom.pred);
                }
            }
        }
    }
    cone
}

/// Restrict a program to a predicate set: keep rules whose head is in `preds`
/// (their bodies are guaranteed in-set when `preds` is a dependency cone) and
/// EDB facts for in-set predicates.  EGDs / builtin-closure markings follow
/// their relation into the scope; the rigid set and instance-pred anchor are
/// copied wholesale (an out-of-scope `instance` relation simply leaves
/// guarded EGDs unable to verify — they under-fire, soundly).
pub(crate) fn scope_program(prog: &Program, preds: &HashSet<Pred>) -> Program {
    let mut p = Program {
        egds: prog.egds.iter().filter(|e| preds.contains(&e.rel)).cloned().collect(),
        builtin_transitive: prog
            .builtin_transitive
            .iter()
            .filter(|(r, _)| preds.contains(r))
            .map(|(r, s)| (*r, *s))
            .collect(),
        rigid: prog.rigid.clone(),
        instance_pred: prog.instance_pred,
        ..Program::default()
    };
    for (pred, facts) in &prog.edb {
        if preds.contains(pred) {
            p.edb.insert(*pred, facts.clone());
        }
    }
    for ((pred, t), sid) in &prog.edb_sids {
        if preds.contains(pred) {
            p.edb_sids.insert((*pred, t.clone()), *sid);
        }
    }
    for r in &prog.rules {
        if preds.contains(&r.head.pred) {
            p.rules.push(r.clone());
        }
    }
    p
}

/// The negation-free fragment: every rule with no negated body literal, plus
/// all EDB facts.  It is monotone, so its least model is always well-defined
/// and SOUND as an under-approximation — every derived fact is entailed (it
/// may merely miss facts that need negation).  This is the robust home for
/// heavily-shared predicates (`instance`/`subclass`) that predicate-SCC
/// partitioning over-taints: a shared predicate sits in one giant recursive
/// SCC that also contains a negated literal, so SCC tainting drops it — yet its
/// *positive* definition is perfectly sound for positive queries, and lives
/// here.  Negative/complete decisions still require a stratifiable cluster from
/// `partition`; this fragment serves the (common) positive case.
pub(crate) fn positive_program(prog: &Program) -> Program {
    let mut p = Program {
        rules:    Vec::new(),
        edb:      prog.edb.clone(),
        edb_sids: prog.edb_sids.clone(),
        egds:     prog.egds.clone(),
        builtin_transitive: prog.builtin_transitive.clone(),
        rigid:    prog.rigid.clone(),
        instance_pred: prog.instance_pred,
    };
    for r in &prog.rules {
        if r.body.iter().all(|l| !l.negated) {
            p.rules.push(r.clone());
        }
    }
    p
}

parked! {
    /// Demand selection (the SInE hook): the clusters touched by a set of seed
    /// predicates — exactly what to materialize for a query whose relevant symbols
    /// are `seed`.  `seed` is meant to come from SInE's symbol selection over the
    /// conjecture (`kb::sine`), so this narrows *which* clusters to evaluate
    /// without affecting *what* each cluster decides.  Returns cluster indices.
    pub(crate) fn relevant_clusters(clusters: &[Cluster], seed: &HashSet<Pred>) -> Vec<usize> {
        clusters
            .iter()
            .enumerate()
            .filter(|(_, c)| c.preds.iter().any(|p| seed.contains(p)))
            .map(|(i, _)| i)
            .collect()
    }
}

// -- Tarjan SCC ---------------------------------------------------------------

/// Map each predicate to its strongly-connected-component index.
fn scc_index(preds: &HashSet<Pred>, adj: &HashMap<Pred, Vec<Pred>>) -> HashMap<Pred, usize> {
    let nodes: Vec<Pred> = preds.iter().copied().collect();
    let idx_of: HashMap<Pred, usize> = nodes.iter().enumerate().map(|(i, p)| (*p, i)).collect();
    let n = nodes.len();
    let adj_idx: Vec<Vec<usize>> = nodes
        .iter()
        .map(|p| adj[p].iter().map(|q| idx_of[q]).collect())
        .collect();

    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stk: Vec<usize> = Vec::new();
    let mut counter = 0usize;
    let mut comp = vec![usize::MAX; n];
    let mut ncomp = 0usize;

    for v in 0..n {
        if index[v] == usize::MAX {
            strongconnect(
                v, &adj_idx, &mut index, &mut low, &mut on_stack, &mut stk,
                &mut counter, &mut comp, &mut ncomp,
            );
        }
    }
    nodes.iter().enumerate().map(|(i, p)| (*p, comp[i])).collect()
}

#[allow(clippy::too_many_arguments)]
fn strongconnect(
    v: usize, adj: &[Vec<usize>], index: &mut [usize], low: &mut [usize],
    on_stack: &mut [bool], stk: &mut Vec<usize>, counter: &mut usize,
    comp: &mut [usize], ncomp: &mut usize,
) {
    index[v] = *counter;
    low[v] = *counter;
    *counter += 1;
    stk.push(v);
    on_stack[v] = true;
    for &w in &adj[v] {
        if index[w] == usize::MAX {
            strongconnect(w, adj, index, low, on_stack, stk, counter, comp, ncomp);
            low[v] = low[v].min(low[w]);
        } else if on_stack[w] {
            low[v] = low[v].min(index[w]);
        }
    }
    if low[v] == index[v] {
        loop {
            let w = stk.pop().unwrap();
            on_stack[w] = false;
            comp[w] = *ncomp;
            if w == v {
                break;
            }
        }
        *ncomp += 1;
    }
}

// -- union-find ---------------------------------------------------------------

struct UnionFind {
    parent: HashMap<Pred, Pred>,
}

impl UnionFind {
    fn new(preds: &[Pred]) -> Self {
        Self { parent: preds.iter().map(|p| (*p, *p)).collect() }
    }
    fn find(&mut self, p: Pred) -> Pred {
        let mut root = p;
        while self.parent[&root] != root {
            root = self.parent[&root];
        }
        // path-compress
        let mut cur = p;
        while cur != root {
            let next = self.parent[&cur];
            self.parent.insert(cur, root);
            cur = next;
        }
        root
    }
    fn union(&mut self, a: Pred, b: Pred) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent.insert(ra, rb);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{Atom, DTerm, Literal};
    use crate::types::Symbol;

    fn s(n: &str) -> Pred { Symbol::hash_name(n) }
    fn atom(p: &str, a: &[u32]) -> Atom {
        Atom { pred: s(p), args: a.iter().map(|i| DTerm::Var(*i)).collect() }
    }
    fn pos(a: Atom) -> Literal { Literal { atom: a, negated: false } }
    fn neg(a: Atom) -> Literal { Literal { atom: a, negated: true } }

    // Two independent clean clusters + a negation cycle.  Partition must find
    // the two clean clusters and DROP the cycle's predicates (and dependents).
    #[test]
    fn partition_isolates_negation_cycle_keeps_clean_clusters() {
        let mut p = Program::default();
        // cluster A: taxonomy-like — subclass transitive + instance bridge.
        p.fact(s("subclass"), vec![s("A"), s("B")]);
        p.fact(s("instance"), vec![s("x"), s("A")]);
        p.rule(atom("subclass", &[0, 2]), vec![pos(atom("subclass", &[0, 1])), pos(atom("subclass", &[1, 2]))]);
        p.rule(atom("instance", &[2, 1]), vec![pos(atom("instance", &[2, 0])), pos(atom("subclass", &[0, 1]))]);
        // cluster B: a separate definite rule.
        p.fact(s("edge"), vec![s("u"), s("v")]);
        p.rule(atom("path", &[0, 1]), vec![pos(atom("edge", &[0, 1]))]);
        // bad: negation cycle  p2(X):-dom(X),not q2(X) ; q2(X):-dom(X),not p2(X)
        p.fact(s("dom"), vec![s("k")]);
        p.rule(atom("p2", &[0]), vec![pos(atom("dom", &[0])), neg(atom("q2", &[0]))]);
        p.rule(atom("q2", &[0]), vec![pos(atom("dom", &[0])), neg(atom("p2", &[0]))]);

        let clusters = partition(&p);
        // every returned cluster must evaluate (be stratifiable).
        for c in &clusters {
            assert!(c.program.evaluate().is_ok(), "cluster should be stratifiable");
        }
        // p2/q2 are dropped; the clean predicates survive.
        let all: HashSet<Pred> = clusters.iter().flat_map(|c| c.preds.iter().copied()).collect();
        assert!(!all.contains(&s("p2")) && !all.contains(&s("q2")), "cycle preds excluded");
        assert!(all.contains(&s("subclass")) && all.contains(&s("instance")));
        assert!(all.contains(&s("path")));
        // taxonomy preds land together; path is its own cluster.
        let tax = clusters.iter().find(|c| c.preds.contains(&s("subclass"))).unwrap();
        assert!(tax.preds.contains(&s("instance")), "instance bridges into the subclass cluster");
        assert!(!tax.preds.contains(&s("path")), "unrelated predicate is a separate cluster");
    }

    // The SInE-demand hook selects only clusters touched by the seed.
    #[test]
    fn relevant_clusters_selects_by_seed() {
        let mut p = Program::default();
        p.fact(s("subclass"), vec![s("A"), s("B")]);
        p.rule(atom("subclass", &[0, 2]), vec![pos(atom("subclass", &[0, 1])), pos(atom("subclass", &[1, 2]))]);
        p.fact(s("edge"), vec![s("u"), s("v")]);
        p.rule(atom("path", &[0, 1]), vec![pos(atom("edge", &[0, 1]))]);

        let clusters = partition(&p);
        let seed: HashSet<Pred> = [s("subclass")].into_iter().collect();
        let sel = relevant_clusters(&clusters, &seed);
        assert_eq!(sel.len(), 1);
        assert!(clusters[sel[0]].preds.contains(&s("subclass")));
        assert!(!clusters[sel[0]].preds.contains(&s("path")));
    }
}
