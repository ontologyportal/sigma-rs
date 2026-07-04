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
        // EGDs / builtin closures / rigid symbols ride along unchanged: the
        // magic rewrite narrows DEMAND, not the constraint semantics.
        egds:     prog.egds.clone(),
        builtin_transitive: prog.builtin_transitive.clone(),
        rigid:    prog.rigid.clone(),
        instance_pred: prog.instance_pred,
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

    let mtrace = std::env::var_os("SIGMA_MAGIC_TRACE").is_some();
    while let Some((p, pmask)) = work.pop() {
        if !idb.contains(&p) || !processed.insert((p, pmask)) {
            continue;
        }
        if mtrace {
            eprintln!("MAGIC {:#x} = magic({p:#x}, {pmask:#b})", magic_pred(p, pmask));
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

            // BOUND-FIRST SIPS: order the body greedily so each placed
            // literal has as many bound seats as possible given the head's
            // bound positions + everything placed before it (reordering a
            // conjunction is semantics-free).  The old left-to-right SIPS
            // adorned a literal with an ALL-FREE mask whenever a rule merely
            // LISTED an unrelated literal first — e.g. Merge.kif's bridge
            // `(=> (and (subclass ?X ?Y) (instance ?Z ?X)) (instance ?Z ?Y))`
            // demanded `subclass` (and, through sibling rules, `instance`)
            // wholesale, materializing the full dense closure the built-in
            // transitive relations exist to avoid.  A negated literal is
            // only placed once ALL its variables are bound (safety); an
            // unsafe leftover keeps source order for the validator to catch.
            let order: Vec<usize> = {
                let mut placed: Vec<usize> = Vec::with_capacity(rule.body.len());
                let mut remaining: Vec<usize> = (0..rule.body.len()).collect();
                let mut b = bound.clone();
                while !remaining.is_empty() {
                    // Score = (all-seats-bound?, #bound seats, NOT a
                    // right-only-bound builtin, smaller extension, source
                    // order).  The builtin penalty matters: a builtin
                    // transitive literal bound only on the RIGHT enumerates
                    // the seed's DESCENDANT cone (huge for a top class),
                    // while its sibling literal usually binds the shared
                    // variable from a far smaller set first — e.g. the
                    // bridge `instance(Z,y) :- subclass(X,y), instance(Z,X)`
                    // under a ground goal must probe `instance(subj, X)`
                    // BEFORE `subclass(X, anc)`, or the demand explodes to
                    // subj × descendants(anc).
                    let score = |li: usize| -> Option<(bool, usize, bool, std::cmp::Reverse<usize>)> {
                        let lit = &rule.body[li];
                        let seat_bound: Vec<bool> = lit
                            .atom
                            .args
                            .iter()
                            .map(|a| match a {
                                DTerm::Const(_) => true,
                                DTerm::Var(v) => b.contains(v),
                            })
                            .collect();
                        let n = seat_bound.iter().filter(|x| **x).count();
                        if lit.negated && n < lit.atom.args.len() {
                            return None; // negated: only when fully bound
                        }
                        let builtin_right_only = prog.builtin_transitive.contains_key(&lit.atom.pred)
                            && seat_bound.len() == 2
                            && !seat_bound[0]
                            && seat_bound[1];
                        // Extension estimate: EDB size for extensional
                        // relations; an IDB relation's extension is unknown
                        // and potentially huge — never prefer it on size
                        // (an empty-EDB derived relation placed first would
                        // be demanded with an all-free adornment, deriving
                        // it wholesale).
                        let ext = if idb.contains(&lit.atom.pred) {
                            usize::MAX
                        } else {
                            prog.edb.get(&lit.atom.pred).map_or(usize::MAX, HashSet::len)
                        };
                        Some((n == seat_bound.len(), n, !builtin_right_only, std::cmp::Reverse(ext)))
                    };
                    let pick = remaining
                        .iter()
                        .enumerate()
                        .filter_map(|(ri, &li)| score(li).map(|sc| (sc, std::cmp::Reverse(li), ri)))
                        .max()
                        .map(|(_, _, ri)| ri)
                        .unwrap_or(0); // only unsafe negated left: source order
                    let li = remaining.remove(pick);
                    for a in &rule.body[li].atom.args {
                        if let DTerm::Var(v) = a {
                            b.insert(*v);
                        }
                    }
                    placed.push(li);
                }
                placed
            };

            // Adorned rule: head :- magic_head, body (in SIPS order)…
            let mut new_body = Vec::with_capacity(rule.body.len() + 1);
            new_body.push(Literal { atom: magic_head.clone(), negated: false });

            // Each IDB body literal gets a magic rule from the head's magic
            // plus the literals PLACED before it (the SIPS prefix).
            for (j, &li) in order.iter().enumerate() {
                let lit = &rule.body[li];
                if !lit.negated && idb.contains(&lit.atom.pred) {
                    let qmask = adornment(&lit.atom.args, &bound);
                    let mut mbody = Vec::with_capacity(j + 1);
                    mbody.push(Literal { atom: magic_head.clone(), negated: false });
                    mbody.extend(order[..j].iter().map(|&pi| rule.body[pi].clone()));
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
