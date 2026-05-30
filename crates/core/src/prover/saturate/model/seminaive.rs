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
// Computes the SAME perfect model as the naive evaluator, just far faster.
// Stratified: semi-naive WITHIN a stratum (positive recursion only there);
// negation is a membership filter against the fully-computed lower strata.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::types::SymbolId;

use super::{ground_atom, unify, Atom, DTerm, Literal, Model, ModelError, Pred, Program, Tuple};

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

    fn len(&self) -> usize {
        self.rels.values().map(|r| r.rows.len()).sum()
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

/// Evaluate one rule body, emitting head tuples.  `driver` forces one body
/// literal to range over a delta tuple slice (semi-naive); `None` ranges all
/// literals over the full store (the round-0 / exhaustive pass).
fn fire(body: &[Literal], head: &Atom, driver: Option<(usize, &[Tuple])>, store: &Store, out: &mut Vec<Tuple>) {
    let driver_idx = driver.map(|(d, _)| d);
    // Order: driver first (small), then the other positive literals (so each
    // is reached with bound positions for the index), then negated filters.
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
    join_rec(body, &order, 0, driver, store, &mut binding, head, out);
}

#[allow(clippy::too_many_arguments)]
fn join_rec(
    body:    &[Literal],
    order:   &[usize],
    oi:      usize,
    driver:  Option<(usize, &[Tuple])>,
    store:   &Store,
    binding: &mut HashMap<u32, SymbolId>,
    head:    &Atom,
    out:     &mut Vec<Tuple>,
) {
    if oi == order.len() {
        if let Some(t) = ground_atom(head, binding) {
            out.push(t);
        }
        return;
    }
    let li = order[oi];
    let lit = &body[li];
    if lit.negated {
        if let Some(t) = ground_atom(&lit.atom, binding) {
            if !store.contains(lit.atom.pred, &t) {
                join_rec(body, order, oi + 1, driver, store, binding, head, out);
            }
        }
        return;
    }
    let cands: Vec<Tuple> = match driver {
        Some((d, tuples)) if d == li => tuples.to_vec(),
        _ => store.candidates(&lit.atom, binding),
    };
    for tup in &cands {
        if let Some(undo) = unify(&lit.atom.args, tup, binding) {
            join_rec(body, order, oi + 1, driver, store, binding, head, out);
            for v in undo {
                binding.remove(&v);
            }
        }
    }
}

/// Run the program to its perfect model, stratum by stratum, semi-naively.
/// `strata` is the precomputed stratification; bails with `Overflow` past
/// `max_tuples`.
pub(super) fn run(
    prog:      &Program,
    strata:    &[Vec<Pred>],
    max_tuples: usize,
    deadline:  Option<Instant>,
) -> Result<Model, ModelError> {
    let mut store = Store::default();
    for (p, facts) in &prog.edb {
        for t in facts {
            store.insert(*p, t.clone());
        }
    }
    let mut total = store.len();
    // Check the wall-clock deadline cheaply (once per derived batch, not per
    // tuple); `Overflow` doubles as the bail signal.
    let over_deadline = |d: Option<Instant>| d.is_some_and(|dl| Instant::now() > dl);

    for stratum in strata {
        let in_stratum: HashSet<Pred> = stratum.iter().copied().collect();
        let srules: Vec<&super::Rule> =
            prog.rules.iter().filter(|r| in_stratum.contains(&r.head.pred)).collect();
        if srules.is_empty() {
            continue;
        }

        // Round 0: full evaluation over the store (EDB + lower strata + this
        // stratum's EDB), seeding the delta.
        let mut delta: HashMap<Pred, Vec<Tuple>> = HashMap::new();
        for r in &srules {
            if over_deadline(deadline) {
                return Err(ModelError::Overflow);
            }
            let mut out = Vec::new();
            fire(&r.body, &r.head, None, &store, &mut out);
            for t in out {
                if store.insert(r.head.pred, t.clone()) {
                    delta.entry(r.head.pred).or_default().push(t);
                    total += 1;
                    if total > max_tuples {
                        return Err(ModelError::Overflow);
                    }
                }
            }
        }

        // Semi-naive rounds: fire each rule with each recursive (in-stratum)
        // body literal driven by its delta.
        while delta.values().any(|v| !v.is_empty()) {
            let mut next: HashMap<Pred, Vec<Tuple>> = HashMap::new();
            for r in &srules {
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
                    let drv = drv.clone();
                    let mut out = Vec::new();
                    fire(&r.body, &r.head, Some((i, &drv)), &store, &mut out);
                    for t in out {
                        if store.insert(r.head.pred, t.clone()) {
                            next.entry(r.head.pred).or_default().push(t);
                            total += 1;
                            if total > max_tuples {
                                return Err(ModelError::Overflow);
                            }
                        }
                    }
                }
            }
            delta = next;
        }
    }

    // Ensure every head predicate is present (empty if nothing derived).
    let mut model = store.into_model();
    for r in &prog.rules {
        model.entry(r.head.pred).or_default();
    }
    Ok(model)
}
