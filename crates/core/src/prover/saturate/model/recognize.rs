// crates/core/src/saturate/model/recognize.rs
//
// Prototype: recognize taxonomy ROLES by Horn-clause signature.
//
// A Datalog rule `H :- B1, …, Bn` IS a definite Horn clause
// (`H ∨ ¬B1 ∨ … ∨ ¬Bn`).  After extraction (which already clausifies the
// implication roots), the surface dialect is gone — what remains is the
// clause's *shape*: the relation identities and the variable-sharing graph
// over argument positions.  Recognizing roles on THAT signature is invariant
// to how the axiom was written (biconditional / split / De Morgan / quantifier
// placement) AND to the relation NAMES — `transitive(genls)` and
// `transitive(subclass)` match the same pattern.
//
// This is the clause-level counterpart to the sentence-shape matchers in
// `semantics/roles.rs` (`match_bridge`, `match_disjoint_*`, …): one uniform
// signature matcher instead of a per-shape zoo.  It recovers the FIRST-ORDER
// role axioms (an explicit transitivity rule, a subrelation, the
// instance/subclass bridge).  It does NOT recover REIFIED encodings — SUMO's
// `(instance R TransitiveRelation)` + the predicate-variable meta-axiom is
// second-order and never becomes a first-order Horn rule; that needs
// schema-instantiation (`collect_role_decls`), which is orthogonal to clause
// form.

use super::{DTerm, Pred, Rule};

use crate::prover::saturate::parked;

parked! {
    /// The two arguments of a binary atom as a pair of DISTINCT variable ids
    /// (no constants) — the unit the signature matchers reason over.
    fn bin_vars(args: &[DTerm]) -> Option<(u32, u32)> {
        if args.len() != 2 {
            return None;
        }
        match (&args[0], &args[1]) {
            (DTerm::Var(a), DTerm::Var(b)) if a != b => Some((*a, *b)),
            _ => None,
        }
    }

    /// `R(a,c) :- R(a,b), R(b,c)` (distinct a,b,c) — `R` is transitive.
    pub(crate) fn is_transitive(r: &Rule) -> Option<Pred> {
        if r.body.len() != 2 || r.body.iter().any(|l| l.negated) {
            return None;
        }
        let rel = r.head.pred;
        if r.body[0].atom.pred != rel || r.body[1].atom.pred != rel {
            return None;
        }
        let (a, c) = bin_vars(&r.head.args)?;
        let b0 = bin_vars(&r.body[0].atom.args)?;
        let b1 = bin_vars(&r.body[1].atom.args)?;
        // Some ordering of the two body literals is (a,mid) then (mid,c).
        let chain = |x: (u32, u32), y: (u32, u32)| {
            x.0 == a && y.1 == c && x.1 == y.0 && x.1 != a && x.1 != c
        };
        if a != c && (chain(b0, b1) || chain(b1, b0)) {
            Some(rel)
        } else {
            None
        }
    }

    /// `R(a,b) :- R(b,a)` — `R` is symmetric.
    pub(crate) fn is_symmetric(r: &Rule) -> Option<Pred> {
        if r.body.len() != 1 || r.body[0].negated || r.body[0].atom.pred != r.head.pred {
            return None;
        }
        let (a, b) = bin_vars(&r.head.args)?;
        let (c, d) = bin_vars(&r.body[0].atom.args)?;
        (a == d && b == c).then_some(r.head.pred)
    }

    /// `S(a,b) :- R(a,b)` — `R` is a subrelation of `S`.  Returns `(R, S)`.
    pub(crate) fn is_subrelation(r: &Rule) -> Option<(Pred, Pred)> {
        if r.body.len() != 1 || r.body[0].negated || r.body[0].atom.pred == r.head.pred {
            return None;
        }
        let h = bin_vars(&r.head.args)?;
        let b = bin_vars(&r.body[0].atom.args)?;
        (h == b).then_some((r.body[0].atom.pred, r.head.pred))
    }

    /// `I(z,y) :- I(z,x), C(x,y)` (distinct z,x,y) — the instance/subclass bridge.
    /// Returns `(I, C)`: the instance-like and subclass-like relations, identified
    /// purely by the coupling pattern (no names).
    pub(crate) fn is_bridge(r: &Rule) -> Option<(Pred, Pred)> {
        if r.body.len() != 2 || r.body.iter().any(|l| l.negated) {
            return None;
        }
        let inst = r.head.pred;
        // One body literal repeats the head relation (the instance self-link); the
        // other is the subclass-like relation.
        let (ib, cb) = if r.body[0].atom.pred == inst && r.body[1].atom.pred != inst {
            (&r.body[0], &r.body[1])
        } else if r.body[1].atom.pred == inst && r.body[0].atom.pred != inst {
            (&r.body[1], &r.body[0])
        } else {
            return None;
        };
        let (z, y) = bin_vars(&r.head.args)?;
        let (zi, x) = bin_vars(&ib.atom.args)?;
        let (xc, yc) = bin_vars(&cb.atom.args)?;
        (zi == z && yc == y && x == xc && x != z && x != y && z != y)
            .then_some((inst, cb.atom.pred))
    }

    /// Roles recognized from a rule set by clause signature.
    #[derive(Debug, Default, Clone)]
    pub(crate) struct RolePatterns {
        pub transitive:  Vec<Pred>,
        pub symmetric:   Vec<Pred>,
        pub subrelation: Vec<(Pred, Pred)>,
        pub bridges:     Vec<(Pred, Pred)>,
    }

    /// Scan a rule set (definite Horn clauses) for the four role signatures.
    pub(crate) fn recognize(rules: &[Rule]) -> RolePatterns {
        let mut p = RolePatterns::default();
        for r in rules {
            if let Some(t) = is_transitive(r) {
                p.transitive.push(t);
            } else if let Some(s) = is_symmetric(r) {
                p.symmetric.push(s);
            } else if let Some(sr) = is_subrelation(r) {
                p.subrelation.push(sr);
            } else if let Some(br) = is_bridge(r) {
                p.bridges.push(br);
            }
        }
        p.transitive.sort_unstable();
        p.transitive.dedup();
        p.symmetric.sort_unstable();
        p.symmetric.dedup();
        p.subrelation.sort_unstable();
        p.subrelation.dedup();
        p.bridges.sort_unstable();
        p.bridges.dedup();
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{Atom, Literal};
    use crate::types::Symbol;

    fn s(n: &str) -> Pred { Symbol::hash_name(n) }
    fn atom(p: Pred, a: &[u32]) -> Atom {
        Atom { pred: p, args: a.iter().map(|i| DTerm::Var(*i)).collect() }
    }
    fn lit(p: Pred, a: &[u32]) -> Literal { Literal { atom: atom(p, a), negated: false } }
    fn rule(head: Atom, body: Vec<Literal>) -> Rule { Rule { head, body, sid: None } }

    // The signatures are NAME-INDEPENDENT: the same pattern recognizes the
    // role whatever the relation is called (`genls`, `subPlanOf`, …).
    #[test]
    fn signatures_are_name_independent() {
        for name in ["subclass", "genls", "subProcess", "wibble"] {
            let r = s(name);
            // transitive: r(0,2) :- r(0,1), r(1,2)
            let t = rule(atom(r, &[0, 2]), vec![lit(r, &[0, 1]), lit(r, &[1, 2])]);
            assert_eq!(is_transitive(&t), Some(r), "transitivity of {name}");
            // symmetric: r(0,1) :- r(1,0)
            let sym = rule(atom(r, &[0, 1]), vec![lit(r, &[1, 0])]);
            assert_eq!(is_symmetric(&sym), Some(r), "symmetry of {name}");
        }
    }

    #[test]
    fn recognizes_subrelation_and_bridge() {
        let (part, comp) = (s("part"), s("component"));
        // component(x,y) :- part(x,y)  ⇒ part ⊑ component  (wait: sub is the body)
        // subrelation pattern: S(x,y) :- R(x,y) ⇒ (R sub, S super)
        let sr = rule(atom(comp, &[0, 1]), vec![lit(part, &[0, 1])]);
        assert_eq!(is_subrelation(&sr), Some((part, comp)));

        let (inst, sub) = (s("instance"), s("subclass"));
        // instance(z,y) :- instance(z,x), subclass(x,y)
        let br = rule(atom(inst, &[0, 2]), vec![lit(inst, &[0, 1]), lit(sub, &[1, 2])]);
        assert_eq!(is_bridge(&br), Some((inst, sub)), "bridge recovers (instance, subclass)");
        // body order swapped still matches
        let br2 = rule(atom(inst, &[0, 2]), vec![lit(sub, &[1, 2]), lit(inst, &[0, 1])]);
        assert_eq!(is_bridge(&br2), Some((inst, sub)));
    }

    // A non-pattern rule matches nothing.
    #[test]
    fn rejects_non_patterns() {
        let (p, q) = (s("p"), s("q"));
        // p(0,1) :- q(0,2), q(2,1)  — not transitive (head≠body pred), not bridge
        let r = rule(atom(p, &[0, 1]), vec![lit(q, &[0, 2]), lit(q, &[2, 1])]);
        assert!(is_transitive(&r).is_none());
        assert!(is_bridge(&r).is_none());
        assert!(is_symmetric(&r).is_none());
        assert!(is_subrelation(&r).is_none());
    }
}
