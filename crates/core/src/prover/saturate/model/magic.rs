// crates/core/src/saturate/model/magic.rs
//
// Phase 5 slice 4b — magic-set rewrite (sideways information passing).
//
// Predicate-cone scoping (slice 4) shrinks the *rules*, but a single relation
// in a dense ontology (OpenCyc `genls`) still has a huge *fact* extension, and
// computing its whole transitive closure is what grinds.  Magic sets fix that
// by scoping on the conjecture's CONSTANTS: for a query `genls(SpecificA, ?Y)`
// they restrict derivation to the genls-facts reachable *from `SpecificA`*,
// not the whole relation.
//
// This is the textbook Generalized Magic Sets transformation for POSITIVE
// Datalog (the monotone model is negation-free, so the simple form is sound),
// with a left-to-right SIPS (every preceding body literal passes its bindings)
// and prefix-inlined magic rules (no separate supplementary predicates —
// correct, slightly less sharing).  The output is an ordinary `Program` fed to
// the same kernel; the magic predicates guard derivation to demanded tuples.

use std::collections::HashSet;

use crate::types::SymbolId;

use super::{Atom, DTerm, Literal, Pred, Program, Rule};

const MAGIC_SALT: u64 = 0x9E37_79B9_7F4A_7C15;

/// A deterministic synthetic predicate id for `magic_p^mask`.  Collision with a
/// real relation id is astronomically unlikely and harmless (magic preds are
/// internal to the rewritten program and never emitted).
fn magic_pred(p: Pred, mask: u64) -> Pred {
    p.wrapping_mul(0xff51_afd7_ed55_8ccd)
        ^ mask.wrapping_mul(0xc4ce_b9fe_1a85_ec53)
        ^ MAGIC_SALT
}

/// Bitmask of the argument positions bound given the currently-bound variables
/// (a constant is always bound).
fn adornment(args: &[DTerm], bound: &HashSet<u32>) -> u64 {
    let mut mask = 0u64;
    for (i, a) in args.iter().enumerate().take(64) {
        let is_bound = match a {
            DTerm::Const(_) => true,
            DTerm::Var(v) => bound.contains(v),
        };
        if is_bound {
            mask |= 1 << i;
        }
    }
    mask
}

/// The args at the bound positions — the key a magic predicate carries.
fn bound_args(args: &[DTerm], mask: u64) -> Vec<DTerm> {
    args.iter()
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .map(|(_, a)| a.clone())
        .collect()
}

/// Rewrite `prog` so bottom-up evaluation derives only the tuples demanded by
/// the goal `goal_rel(goal_args)` (constants in `goal_args` are the bound seed).
/// EDB facts are kept; IDB derivation is guarded by magic predicates.
pub(crate) fn magic_rewrite(prog: &Program, goal_rel: Pred, goal_args: &[DTerm]) -> Program {
    let idb: HashSet<Pred> = prog.rules.iter().map(|r| r.head.pred).collect();

    // The goal adornment: bound = constant positions.
    let goal_mask = adornment(goal_args, &HashSet::new());

    let mut out = Program {
        rules:    Vec::new(),
        edb:      prog.edb.clone(),
        edb_sids: prog.edb_sids.clone(),
    };

    // Seed: the demanded bound tuple from the conjecture's constants.
    let seed: Vec<SymbolId> = goal_args
        .iter()
        .enumerate()
        .filter(|(i, _)| goal_mask & (1 << i) != 0)
        .filter_map(|(_, a)| match a {
            DTerm::Const(c) => Some(*c),
            DTerm::Var(_) => None,
        })
        .collect();
    out.fact(magic_pred(goal_rel, goal_mask), seed);

    let mut processed: HashSet<(Pred, u64)> = HashSet::new();
    let mut work: Vec<(Pred, u64)> = vec![(goal_rel, goal_mask)];

    while let Some((p, pmask)) = work.pop() {
        if !idb.contains(&p) || !processed.insert((p, pmask)) {
            continue;
        }
        for rule in prog.rules.iter().filter(|r| r.head.pred == p) {
            // Bound variables from the head's bound positions.
            let mut bound: HashSet<u32> = HashSet::new();
            for (i, a) in rule.head.args.iter().enumerate() {
                if pmask & (1 << i) != 0 {
                    if let DTerm::Var(v) = a {
                        bound.insert(*v);
                    }
                }
            }
            let magic_head = Atom { pred: magic_pred(p, pmask), args: bound_args(&rule.head.args, pmask) };

            // Adorned rule: head :- magic_head, body…
            let mut new_body = Vec::with_capacity(rule.body.len() + 1);
            new_body.push(Literal { atom: magic_head.clone(), negated: false });

            // Left-to-right SIPS: each IDB body literal gets a magic rule from
            // the head's magic plus the preceding body literals (prefix).
            for (j, lit) in rule.body.iter().enumerate() {
                if !lit.negated && idb.contains(&lit.atom.pred) {
                    let qmask = adornment(&lit.atom.args, &bound);
                    let mut mbody = Vec::with_capacity(j + 1);
                    mbody.push(Literal { atom: magic_head.clone(), negated: false });
                    mbody.extend(rule.body[..j].iter().cloned());
                    // Magic rules are demand bookkeeping, not entailment
                    // steps — they carry no citation of their own (their
                    // matched prefix facts still cite through `parents`).
                    out.rules.push(Rule {
                        head: Atom { pred: magic_pred(lit.atom.pred, qmask), args: bound_args(&lit.atom.args, qmask) },
                        body: mbody,
                        sid:  None,
                    });
                    work.push((lit.atom.pred, qmask));
                }
                // All preceding positive literals pass their variables onward.
                for a in &lit.atom.args {
                    if let DTerm::Var(v) = a {
                        bound.insert(*v);
                    }
                }
                new_body.push(lit.clone());
            }
            // The adorned rule derives the same heads as the original — it
            // keeps the original's citation.
            out.rules.push(Rule { head: rule.head.clone(), body: new_body, sid: rule.sid });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Symbol;

    fn s(n: &str) -> Pred { Symbol::hash_name(n) }
    fn atom(p: &str, a: Vec<DTerm>) -> Atom { Atom { pred: s(p), args: a } }
    fn v(i: u32) -> DTerm { DTerm::Var(i) }
    fn c(n: &str) -> DTerm { DTerm::Const(s(n)) }
    fn pos(a: Atom) -> Literal { Literal { atom: a, negated: false } }

    // Transitive closure restricted to the queried source: the rewritten
    // program answers `genls(b0, ?)` correctly but does not derive closure for
    // an unrelated source.
    #[test]
    fn magic_restricts_transitive_closure_to_query_source() {
        let mut prog = Program::default();
        // two disjoint chains: b0<b1<b2  and  z0<z1
        prog.fact(s("genls"), vec![s("b0"), s("b1")]);
        prog.fact(s("genls"), vec![s("b1"), s("b2")]);
        prog.fact(s("genls"), vec![s("z0"), s("z1")]);
        // genls(X,Z) :- genls(X,Y), genls(Y,Z)
        prog.rule(atom("genls", vec![v(0), v(2)]),
                  vec![pos(atom("genls", vec![v(0), v(1)])), pos(atom("genls", vec![v(1), v(2)]))]);

        // Query genls(b0, ?Y): demand source b0.
        let rw = magic_rewrite(&prog, s("genls"), &[c("b0"), v(99)]);
        let m = rw.evaluate().expect("stratifiable");
        let g = m.get(&s("genls")).cloned().unwrap_or_default();
        // b0's closure derived (b0→b2 via transitivity).
        assert!(g.contains(&vec![s("b0"), s("b2")]), "transitive answer for the queried source");
        // the unrelated chain's TRANSITIVE closure is not demanded (z0→z1 is an
        // EDB fact, but z0→… needs no extension here; assert no spurious z work
        // beyond EDB by checking the magic predicate only holds b-side sources).
        let magic = m.get(&magic_pred(s("genls"), 0b01)).cloned().unwrap_or_default();
        assert!(magic.contains(&vec![s("b0")]));
        assert!(!magic.contains(&vec![s("z0")]), "unrelated source not demanded");
    }
}
