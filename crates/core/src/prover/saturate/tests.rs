use std::sync::Arc;

use crate::kb::KnowledgeBase;
use crate::parse::kif::dis::AstKif;
use crate::prover::{ProverStatus, TerminationReason};
use crate::semantics::caches::test_support::kif_layer;
use crate::SineParams;
use crate::types::{Element, SentenceId};

use super::ProverLayer;
use super::clause::{PClause, PLit};
use super::prover::NativeOpts;

/// ProverLayer over a one-or-more-formula KIF string, plus the root
/// sids in the file (tag `base`, document order not guaranteed —
/// single-root fixtures preferred).
fn layer_with(kif: &str) -> (ProverLayer, Vec<SentenceId>) {
    let layer = ProverLayer::new(kif_layer(kif));
    let roots = layer.semantic.syntactic.file_root_sids("base");
    (layer, roots)
}

/// Clauses of the single root in `kif`.
fn clauses_of(kif: &str) -> (ProverLayer, Arc<Vec<PClause>>) {
    let (layer, roots) = layer_with(kif);
    assert_eq!(roots.len(), 1, "fixture must hold exactly one root");
    let cls = layer.clauses_for(roots[0]);
    (layer, cls)
}

/// Head-symbol name of a literal's atom (skolem heads included).
fn head_of(layer: &ProverLayer, lit: &PLit) -> String {
    let s = layer.atoms.resolve(lit.atom, &layer.semantic.syntactic)
        .expect("atom resolvable");
    match s.elements.first() {
        Some(Element::Symbol(sym)) => sym.name().to_string(),
        Some(Element::Op(op))      => format!("{:?}", op),
        other                      => format!("{:?}", other),
    }
}

// -- Phase 5 slice 1: the model registry builds, caches, and the monotone
//    fragment computes the taxonomy closure (no allowlist, no query wiring).
#[test]
fn model_registry_builds_and_caches() {
    use crate::types::Symbol;
    // `(instance subclass TransitiveRelation)` licenses subclass
    // transitivity by DERIVATION (no conventional seed).
    let kif = "(subclass RoadVehicle LandVehicle)\n\
                (subclass LandVehicle Vehicle)\n\
                (instance Bus1 RoadVehicle)\n\
                (instance subclass TransitiveRelation)\n\
                (=> (and (instance ?Z ?X) (subclass ?X ?Y)) (instance ?Z ?Y))";
    let (layer, _) = layer_with(kif);

    let mp = layer.model_program();
    assert!(!mp.program.rules.is_empty(), "extracted some rules");
    assert!(!mp.clusters.is_empty(), "discovered clusters");

    // Cached: a second request returns the same Arc.
    let mp2 = layer.model_program();
    assert!(Arc::ptr_eq(&mp, &mp2), "registry is cached for the KB's life");

    // The sound positive model (monotone + KB-derived transitivity).
    let m = mp.positive_model().expect("positive model evaluates");
    let tuple = |a: &str, b: &str| vec![Symbol::hash_name(a), Symbol::hash_name(b)];
    let has = |p: &str, a: &str, b: &str|
        m.get(&Symbol::hash_name(p)).is_some_and(|s| s.contains(&tuple(a, b)));
    assert!(has("instance", "Bus1", "Vehicle"), "instance closure climbs subclass");
    assert!(has("subclass", "RoadVehicle", "Vehicle"), "subclass transitive (DERIVED from declaration)");

    // The taxonomy predicates are clustered together.
    let sub = Symbol::hash_name("subclass");
    assert!(mp.clusters.iter().any(|c| c.preds.contains(&sub)), "subclass clustered");
}

// -- frozen background: mask (contract) + delta-load (extend) -------------

#[test]
fn retain_background_masks_excluded_roots() {
    use crate::semantics::types::Scope;
    use super::prover::NativeProver;

    let (layer, roots) = layer_with("(p A)\n(q B)");
    assert_eq!(roots.len(), 2);
    let mut prover = NativeProver::new(&layer, Scope::Base, Default::default());
    for r in &roots {
        prover.add_background_root(*r);
    }
    let snap = prover.freeze();
    assert_eq!(snap.loaded_roots.len(), 2);

    // Atom ids of the two facts (root sentences ARE the unit atoms).
    let p_atom = layer.clauses_for(roots[0])[0].lits[0].atom;
    let q_atom = layer.clauses_for(roots[1])[0].lits[0].atom;

    // Narrow rehydration: keep only the first root.
    let keep: std::collections::HashSet<_> = [roots[0]].into_iter().collect();
    let mut narrow = NativeProver::from_snapshot(
        &layer, Scope::Base, Default::default(), &snap);
    narrow.retain_background(&keep);
    assert!(narrow.test_ground_unit(true, p_atom), "kept root stays probeable");
    assert!(!narrow.test_ground_unit(true, q_atom), "masked root must vanish");

    // Extension: delta-load the second root back on top.
    narrow.add_background_root(roots[1]);
    assert!(narrow.test_ground_unit(true, q_atom), "delta-loaded root probeable");
    let refrozen = narrow.freeze();
    assert_eq!(refrozen.loaded_roots.len(), 2, "coverage = union after extend");
}

// -- conjecture-distance factor (Liu & Xu via leaf signatures) ------------

#[test]
fn conjecture_distance_orders_by_shared_leaves() {
    use crate::semantics::types::Scope;
    use super::prover::{NativeProver, BACKGROUND};

    // Goal mentions Int1/Int2; one equality shares those constants,
    // the other shares only the BeginFn function symbol.
    let (layer, roots) = layer_with(
        "(overlapsTemporally Int1 Int2)\n\
            (equal (BeginFn Int1) (BeginFn Int2))\n\
            (equal (BeginFn Far1) (BeginFn Far2))",
    );
    assert_eq!(roots.len(), 3);
    let conj = roots.iter()
        .find(|r| head_of(&layer, &layer.clauses_for(**r)[0].lits[0]) == "overlapsTemporally")
        .copied().expect("conjecture root");
    let mut opts = super::prover::NativeOpts::default();
    opts.strategy.goal_dist = true; // opt-in knob (default off)
    let mut prover = NativeProver::new(&layer, Scope::Base, opts);
    prover.set_goal(&layer.clauses_for(conj));

    let mut factors: Vec<u64> = roots.iter()
        .filter(|r| **r != conj)
        .map(|r| prover.goal_distance_factor(&layer.clauses_for(*r)[0].lits, BACKGROUND))
        .collect();
    factors.sort_unstable();
    // Near equality (shares Int1/Int2): low factor.  Far equality
    // (shares only BeginFn): maximal factor 1 + GOAL_DIST_W.
    assert!(factors[0] < factors[1], "shared leaves must rank closer: {factors:?}");
    assert_eq!(factors[1], 3, "no-overlap clause sinks to 1 + GOAL_DIST_W");
    // The goal itself is distance-free.
    assert_eq!(
        prover.goal_distance_factor(&layer.clauses_for(conj)[0].lits, BACKGROUND),
        1
    );
}

// -- equality factoring (superposition phase 5) --------------------------

// From `(ff ?x)≈aa ∨ (ff ?x)≈bb` the rule unifies the two maximal
// LHS (σ = ∅) and derives the merge residue — a NEW negative equality
// `aa ≉ bb` (and its symmetric variants).  Direct inference-level
// check that the rule fires and produces the Bachmair–Ganzinger shape.
#[test]
fn equality_factoring_merges_positive_equalities() {
    use crate::semantics::types::Scope;
    use super::prover::{NativeProver, NativeOpts, BACKGROUND};

    let (layer, roots) = layer_with("(or (equal (ff ?x) aa) (equal (ff ?x) bb))");
    let pc = layer.clauses_for(roots[0])[0].clone();
    let mut opts = NativeOpts::default();
    opts.strategy.superposition = true; // turns on maximality marking
    opts.strategy.eq_factoring = true;
    let mut prover = NativeProver::new(&layer, Scope::Base, opts);
    let terms = prover.pclause_terms(&pc).expect("lift clause");
    assert_eq!(terms.len(), 2, "merging clause has two positive equalities");
    let id = prover
        .make(terms, vec![], "axiom", BACKGROUND, Some(roots[0]), false)
        .expect("clause interned");

    let factored: Vec<u32> = prover.equality_factors(id).into_iter().flatten().collect();
    assert!(!factored.is_empty(), "equality factoring produced no clause");

    // At least one factored clause must carry a NEGATIVE equality whose
    // sides are exactly the two small sides aa/bb — the merge residue
    // that was absent from the (all-positive) input.
    let has_residue = factored.iter().any(|&cid| {
        prover.dbg_lits(cid).iter().any(|(pos, kif)| {
            !*pos && kif.contains("aa") && kif.contains("bb")
        })
    });
    assert!(has_residue,
        "a factored clause must contain the residue disequality aa≉bb; got {:?}",
        factored.iter().map(|&c| prover.dbg_lits(c)).collect::<Vec<_>>());
}

// -- clausifier ----------------------------------------------------------

#[test]
fn implication_gives_one_two_literal_clause() {
    let (layer, cls) = clauses_of("(=> (instance ?X Dog) (attribute ?X Loyal))");
    assert_eq!(cls.len(), 1);
    let c = &cls[0];
    assert_eq!(c.lits.len(), 2);
    assert_eq!(c.nvars, 1, "one shared variable");
    // Canonical order puts the negative literal first.
    assert!(!c.lits[0].pos && c.lits[1].pos);
    assert_eq!(head_of(&layer, &c.lits[0]), "instance");
    assert_eq!(head_of(&layer, &c.lits[1]), "attribute");
}

#[test]
fn iff_gives_two_clauses() {
    // The KIF parser macro-expands `<=>` into the two implications at
    // parse time, so the biconditional arrives as two roots; together
    // they clausify to the two complementary clauses.
    let (layer, roots) = layer_with("(<=> (p ?X) (q ?X))");
    assert_eq!(roots.len(), 2, "parser splits <=> into two => roots");
    let cls: Vec<_> = roots.iter()
        .flat_map(|r| layer.clauses_for(*r).iter().cloned().collect::<Vec<_>>())
        .collect();
    assert_eq!(cls.len(), 2);
    assert!(cls.iter().all(|c| c.lits.len() == 2));
    assert_ne!(cls[0].key, cls[1].key);
}

#[test]
fn ground_exists_skolemizes_to_constant() {
    let (layer, cls) = clauses_of("(exists (?Y) (parent Fido ?Y))");
    assert_eq!(cls.len(), 1);
    let c = &cls[0];
    assert_eq!((c.lits.len(), c.nvars), (1, 0), "ground unit after skolemization");
    // Second argument is a skolem constant: a BARE symbol, like any
    // other constant — wrapping it as a 1-element App would exempt
    // skolem facts from every symbol-shaped fast path (learned
    // units, FD congruence, the decode phone book).
    let atom = layer.atoms.resolve(c.lits[0].atom, &layer.semantic.syntactic).unwrap();
    let Some(Element::Symbol(sym)) = atom.elements.get(2) else {
        panic!("arg 2 should be a bare skolem constant, got {:?}", atom.elements.get(2));
    };
    assert!(sym.name().starts_with("sk_"), "skolem name {} has sk_ prefix", sym.name());
}

#[test]
fn exists_under_universal_skolemizes_over_scope() {
    let (layer, cls) =
        clauses_of("(=> (instance ?X Human) (exists (?P) (parent ?X ?P)))");
    assert_eq!(cls.len(), 1);
    let c = &cls[0];
    assert_eq!(c.nvars, 1);
    let pos = c.lits.iter().find(|l| l.pos).expect("positive parent literal");
    let atom = layer.atoms.resolve(pos.atom, &layer.semantic.syntactic).unwrap();
    let Some(Element::Sub(sk)) = atom.elements.get(2) else {
        panic!("arg 2 should be a skolem subterm");
    };
    let sk_sent = layer.atoms.resolve(*sk, &layer.semantic.syntactic).unwrap();
    assert_eq!(sk_sent.elements.len(), 2, "skolem function of the one universal");
    assert!(matches!(sk_sent.elements.get(1), Some(Element::Variable { .. })));
}

#[test]
fn tautology_dropped() {
    let (_, cls) = clauses_of("(or (p Fido) (not (p Fido)))");
    assert!(cls.is_empty(), "P ∨ ¬P clausifies to nothing");
}

#[test]
fn alpha_equivalent_roots_share_clause_keys() {
    // Same formula under different variable names: distinct roots
    // (variable ids are scope-qualified) but identical canonical clauses.
    let (layer, roots) = layer_with("
        (=> (p ?A ?B) (q ?B ?A))
        (=> (p ?Y ?Z) (q ?Z ?Y))
    ");
    assert_eq!(roots.len(), 2, "alpha-variants are distinct roots");
    let a = layer.clauses_for(roots[0]);
    let b = layer.clauses_for(roots[1]);
    assert_eq!(a.len(), 1);
    assert_eq!(
        a.iter().map(|c| c.key).collect::<Vec<_>>(),
        b.iter().map(|c| c.key).collect::<Vec<_>>(),
        "alpha-equivalent roots must collapse to the same ClauseKey"
    );
    // And the canonical atoms are literally shared (content-addressed).
    assert_eq!(a[0].lits, b[0].lits);
}

#[test]
fn equality_sides_orient_canonically() {
    let (layer, roots) = layer_with("
        (equal (AbsFn ?X) ?Y)
        (equal ?B (AbsFn ?A))
    ");
    assert_eq!(roots.len(), 2);
    let a = layer.clauses_for(roots[0]);
    let b = layer.clauses_for(roots[1]);
    assert_eq!(a[0].key, b[0].key, "flipped equality sides hash identically");
}

#[test]
fn reclausification_is_idempotent() {
    let (layer, roots) = layer_with("(=> (instance ?X Human) (exists (?P) (parent ?X ?P)))");
    let first = layer.clauses_for(roots[0]);
    layer.clause_store.clear();
    let second = layer.clauses_for(roots[0]);
    assert_eq!(*first, *second,
        "evict + regenerate must reproduce identical clauses (incl. skolem names)");
}

#[test]
fn conjecture_negation_flips_quantification() {
    // Negated conjecture: ¬∃-shaped query becomes a universal clause set.
    let (layer, roots) = layer_with("(exists (?X) (instance ?X Dog))");
    let sent = layer.semantic.syntactic.sentence(roots[0]).unwrap();
    let cls = super::clausify::clausify_sentence(
        &layer.semantic.syntactic, &layer.atoms, &sent, roots[0], true);
    assert_eq!(cls.len(), 1);
    let c = &cls[0];
    assert_eq!(c.lits.len(), 1);
    assert!(!c.lits[0].pos, "negated conjecture literal");
    assert_eq!(c.nvars, 1, "∃ flips to ∀ under negation — variable stays open");
}

// -- residue index / unify / units (phase 3) -------------------------------

/// All (pos, AtomId) literals of every clause of every root in `kif`.
fn all_lits(layer: &ProverLayer) -> Vec<(bool, super::clause::AtomId)> {
    let roots = layer.semantic.syntactic.file_root_sids("base");
    let mut out = Vec::new();
    for r in roots {
        for c in layer.clauses_for(r).iter() {
            for l in &c.lits {
                out.push((l.pos, l.atom));
            }
        }
    }
    out
}

#[test]
fn key_equation_holds_for_matching_atoms() {
    // Pattern (p ?X b) matches fact (p a b): the fact's residue under
    // the pattern's mask must equal the pattern's own fingerprint.
    let (layer, _) = layer_with("
        (=> (p ?X b) (q ?X))
        (p a b)
        (p a c)
    ");
    let lits = all_lits(&layer);
    let syn = &layer.semantic.syntactic;
    // Pick out (p V0 b) / (p a b) / (p a c) by groundness + 3rd seat.
    let (mut pat, mut fb, mut fc) = (None, None, None);
    for (_, a) in &lits {
        let s = layer.atoms.resolve(*a, syn).unwrap();
        let Some(crate::types::Element::Symbol(h)) = s.elements.first() else { continue };
        if &*h.name() != "p" { continue }
        let info = layer.atom_info(*a);
        let third = match s.elements.get(2) {
            Some(crate::types::Element::Symbol(sym)) => sym.name().to_string(),
            _ => continue,
        };
        match (info.is_ground(), third.as_str()) {
            (false, "b") => pat = Some(*a),
            (true,  "b") => fb = Some(*a),
            (true,  "c") => fc = Some(*a),
            _ => {}
        }
    }
    let (pat, fb, fc) = (pat.unwrap(), fb.unwrap(), fc.unwrap());
    let p_info  = layer.atom_info(pat);
    let fb_info = layer.atom_info(fb);
    let fc_info = layer.atom_info(fc);
    assert_eq!(p_info.mask, 0b010, "seat 1 open in (p V0 b)");
    // THE KEY EQUATION: matching atom agrees under the pattern's mask…
    assert_eq!(fb_info.residue_under(p_info.mask), p_info.base_residue);
    // …and the non-matching atom (different seat-2 coin) does not.
    assert_ne!(fc_info.residue_under(p_info.mask), p_info.base_residue);
}

#[test]
fn probe_returns_exactly_the_unifiable_set_after_verify() {
    use super::index::{EntryRef, LiteralIndex};
    use super::unify::{slot_atom, unify, Subst};

    let (layer, _) = layer_with("
        (p a b)
        (p a c)
        (p ?X b)
        (q a b)
        (=> (r ?X) (p ?X ?Y))
    ");
    let lits = all_lits(&layer);
    let syn = &layer.semantic.syntactic;
    let src = |a| layer.atom_info(a);

    let mut idx = LiteralIndex::default();
    for (i, (pos, atom)) in lits.iter().enumerate() {
        idx.add(EntryRef { clause: i as u32, lit: 0 }, *pos, *atom, &src);
    }

    // Query with the open pattern (p a ?Z): unifiable with (p a b),
    // (p a c), (p V0 b), (p V0 V1) — not (q a b), not (r V0).
    let (q_pos, q_atom) = *lits.iter()
        .find(|(pos, a)| {
            *pos && {
                let info = layer.atom_info(*a);
                let s = layer.atoms.resolve(*a, syn).unwrap();
                matches!(s.elements.first(),
                    Some(crate::types::Element::Symbol(h)) if &*h.name() == "p")
                    && !info.is_ground()
                    && info.mask == 0b010
            }
        })
        .expect("(p V0 b) present");
    let _ = q_pos;
    // Use (p V0 b) itself as the query; brute-force ground truth.
    let q_info = layer.atom_info(q_atom);
    let candidates = idx.probe(true, &q_info, &src);

    let q_term = slot_atom(&layer.atoms, syn, q_atom, 0).unwrap();
    let mut truth = Vec::new();
    for (i, (pos, atom)) in lits.iter().enumerate() {
        if !*pos { continue }
        let t = slot_atom(&layer.atoms, syn, *atom, 8).unwrap();
        let mut s: Subst = vec![None; 16];
        if unify(&q_term, &t, &mut s) {
            truth.push(i as u32);
        }
    }
    let mut got: Vec<u32> = candidates.iter().map(|e| e.clause).collect();
    got.sort_unstable(); got.dedup();
    let mut truth_sorted = truth.clone();
    truth_sorted.sort_unstable();
    // Probe is a superset of the unifiable set…
    for t in &truth_sorted {
        assert!(got.contains(t), "unifiable entry {} missing from probe", t);
    }
    // …and after the unify verify it is exactly the unifiable set.
    let verified: Vec<u32> = got.iter().copied()
        .filter(|i| {
            let (pos, atom) = lits[*i as usize];
            assert!(pos);
            let t = slot_atom(&layer.atoms, syn, atom, 8).unwrap();
            let mut s: Subst = vec![None; 16];
            unify(&q_term, &t, &mut s)
        })
        .collect();
    assert_eq!(verified, truth_sorted, "verify filters probe to ground truth");
}

#[test]
fn view_derivation_equals_recompute() {
    use super::index::{EntryRef, LiteralIndex};

    // Open query against ground facts forces a union view (Mq ⊋ Mp=∅).
    let (layer, _) = layer_with("
        (p a b)
        (p c b)
        (p a d)
        (=> (s ?W) (p ?W ?V))
    ");
    let lits = all_lits(&layer);
    let src = |a| layer.atom_info(a);

    let mut idx = LiteralIndex::default();
    for (i, (pos, atom)) in lits.iter().enumerate() {
        idx.add(EntryRef { clause: i as u32, lit: 0 }, *pos, *atom, &src);
    }
    // (p V0 V1) — the open rule head; its probe walks the ground
    // facts' group through a derived view at U = {1,2}.
    let q_atom = lits.iter()
        .find(|(pos, a)| *pos && layer.atom_info(*a).mask == 0b110)
        .map(|(_, a)| *a)
        .expect("(p V0 V1) present");
    let q_info = layer.atom_info(q_atom);

    let via_view: Vec<u32> = {
        let mut v: Vec<u32> = idx.probe(true, &q_info, &src)
            .iter().map(|e| e.clause).collect();
        v.sort_unstable(); v.dedup(); v
    };
    assert!(idx.view_derivations() > 0, "an actual union view was derived");

    // Recompute ground truth directly: same residue under U for the
    // stored atom ⇔ candidate.
    let u = q_info.mask; // Mp = 0 for ground facts, so U = Mq
    let rq = q_info.residue_under(u);
    let mut direct: Vec<u32> = lits.iter().enumerate()
        .filter(|(_, (pos, a))| {
            *pos && {
                let info = layer.atom_info(*a);
                info.arity == q_info.arity
                    && info.mask == 0
                    && info.residue_under(u) == rq
            }
        })
        .map(|(i, _)| i as u32)
        .collect();
    // Plus same-mask entries (Mp == Mq: the identity view).
    for (i, (pos, a)) in lits.iter().enumerate() {
        let info = layer.atom_info(*a);
        if *pos && info.arity == q_info.arity && info.mask == q_info.mask
            && info.residue_under(u) == rq
            && !direct.contains(&(i as u32))
        {
            direct.push(i as u32);
        }
    }
    direct.sort_unstable();
    assert_eq!(via_view, direct, "derived view == residue recompute");
}

#[test]
fn index_add_after_view_keeps_view_fresh() {
    use super::index::{EntryRef, LiteralIndex};

    let (layer, _) = layer_with("
        (p a b)
        (=> (s ?W) (p ?W ?V))
        (p c d)
    ");
    let lits = all_lits(&layer);
    let src = |a| layer.atom_info(a);
    let q_atom = lits.iter()
        .find(|(pos, a)| *pos && layer.atom_info(*a).mask == 0b110)
        .map(|(_, a)| *a).unwrap();
    let q_info = layer.atom_info(q_atom);

    // Index only (p a b) first; probe (derives the view); then add
    // (p c d) and probe again — the late fact must surface.
    let ground: Vec<(usize, super::clause::AtomId)> = lits.iter().enumerate()
        .filter(|(_, (pos, a))| *pos && layer.atom_info(*a).is_ground())
        .map(|(i, (_, a))| (i, *a)).collect();
    assert_eq!(ground.len(), 2);

    let mut idx = LiteralIndex::default();
    idx.add(EntryRef { clause: ground[0].0 as u32, lit: 0 }, true, ground[0].1, &src);
    let first = idx.probe(true, &q_info, &src).len();
    assert_eq!(first, 1);
    idx.add(EntryRef { clause: ground[1].0 as u32, lit: 0 }, true, ground[1].1, &src);
    let second = idx.probe(true, &q_info, &src).len();
    assert_eq!(second, 2, "fact added after view derivation still probes");
}

#[test]
fn unify_basic_and_occurs_check() {
    use super::clause::Term;
    use super::unify::{apply, unify, Subst};
    use crate::types::Symbol;

    let sym = |n: &str| Term::Sym(Symbol::from(n));
    // unify (p ?0 (f ?1)) with (p a (f b))
    let pat = Term::App(vec![sym("p"), Term::Var(0),
        Term::App(vec![sym("f"), Term::Var(1)])]);
    let tgt = Term::App(vec![sym("p"), sym("a"),
        Term::App(vec![sym("f"), sym("b")])]);
    let mut s: Subst = vec![None; 2];
    assert!(unify(&pat, &tgt, &mut s));
    assert_eq!(apply(&Term::Var(0), &s), sym("a"));
    assert_eq!(apply(&Term::Var(1), &s), sym("b"));

    // Occurs check: ?0 with (f ?0) must fail and roll back.
    let mut s2: Subst = vec![None; 1];
    let circular = Term::App(vec![sym("f"), Term::Var(0)]);
    assert!(!unify(&Term::Var(0), &circular, &mut s2));
    assert!(s2[0].is_none(), "failed unify leaves no bindings");
}

#[test]
fn match_is_one_way() {
    use super::clause::Term;
    use super::unify::{match_one_way, Subst};
    use crate::types::Symbol;

    let sym = |n: &str| Term::Sym(Symbol::from(n));
    let pat = Term::App(vec![sym("p"), Term::Var(0)]);
    let tgt_ground = Term::App(vec![sym("p"), sym("a")]);
    let tgt_open   = Term::App(vec![sym("p"), Term::Var(9)]);

    let mut s: Subst = vec![None; 16];
    assert!(match_one_way(&pat, &tgt_ground, &mut s), "pattern var binds");
    let mut s2: Subst = vec![None; 16];
    assert!(match_one_way(&pat, &tgt_open, &mut s2),
        "pattern var binds a target var as an opaque term");
    // But a ground pattern never binds target variables:
    let mut s3: Subst = vec![None; 16];
    assert!(!match_one_way(&tgt_ground, &tgt_open, &mut s3),
        "target variables are constants to the matcher");
}

// THE KEY EQUATION at subterm grain: probing the TermIndex with a
// pattern returns a superset of the unifiable subterm positions —
// here exactly the f-headed subterms, excluding the g-headed one.
#[test]
fn term_index_probes_unifiable_subterm_positions() {
    use super::clause::Term;
    use super::index::{TermIndex, TermPos};
    use crate::types::Symbol;
    use smallvec::smallvec;

    let (layer, _) = layer_with("(instance a Thing)");
    let s = |n: &str| Term::Sym(Symbol::from(n));
    let fa = layer.atoms.intern_atom(&Term::App(vec![s("f"), s("a")]));
    let fb = layer.atoms.intern_atom(&Term::App(vec![s("f"), s("b")]));
    let ga = layer.atoms.intern_atom(&Term::App(vec![s("g"), s("a")]));

    let mut ti = TermIndex::default();
    for (k, atom) in [fa, fb, ga].into_iter().enumerate() {
        let info = layer.atom_info(atom);
        ti.add(TermPos { clause: k as u32, lit: 0, path: smallvec![] }, atom, &info);
    }

    // Probe with (f ?X).
    let pat = layer.atoms.intern_atom(&Term::App(vec![s("f"), Term::Var(0)]));
    let pinfo = layer.atom_info(pat);
    let l = &layer;
    let hits = ti.probe(&pinfo, &(|a| l.atom_info(a)));
    let clauses: std::collections::HashSet<u32> =
        hits.iter().map(|p| p.clause).collect();
    assert!(clauses.contains(&0) && clauses.contains(&1),
        "both f-subterms must surface: {clauses:?}");
    assert!(!clauses.contains(&2), "(g a) must not match (f ?X): {clauses:?}");
}

// Index removal: a retired clause never surfaces — including through
// an already-derived union view (the cache-coherence risk).
#[test]
fn index_removal_tombstones_through_views() {
    use super::clause::Term;
    use super::index::{EntryRef, LiteralIndex};
    use crate::types::Symbol;

    let (layer, _) = layer_with("(instance a Thing)");
    let s = |n: &str| Term::Sym(Symbol::from(n));
    let pa = layer.atoms.intern_atom(&Term::App(vec![s("p"), s("a")]));
    let l = &layer;
    let src = |a| l.atom_info(a);

    let mut idx = LiteralIndex::default();
    idx.add(EntryRef { clause: 0, lit: 0 }, true, pa, &src);
    idx.add(EntryRef { clause: 1, lit: 0 }, true, pa, &src);

    // Probe with (p ?X): a DIFFERENT mask, so a union view is derived.
    let q = layer.atoms.intern_atom(&Term::App(vec![s("p"), Term::Var(0)]));
    let qinfo = layer.atom_info(q);
    let before = idx.probe(true, &qinfo, &src);
    assert_eq!(before.len(), 2, "both clauses before retirement");
    assert!(idx.view_derivations() > 0, "a union view was derived");

    // Retire clause 0; the same probe hits the CACHED view and must
    // still drop it.
    idx.retire(0);
    let after = idx.probe(true, &qinfo, &src);
    assert_eq!(after.len(), 1, "retired clause filtered from the view");
    assert_eq!(after[0].clause, 1);
}

#[test]
fn unit_stores_subsume_and_refute() {
    use super::units::{UnitHit, UnitStores};

    let (layer, _) = layer_with("
        (holdsDuring t1 x)
        (p a)
        (p ?X)
        (not (q a))
        (q ?Y)
    ");
    let syn = &layer.semantic.syntactic;
    let lits = all_lits(&layer);
    let mut units = UnitStores::default();
    // Activate every unit literal under ids = their position.
    for (i, (pos, atom)) in lits.iter().enumerate() {
        units.add_unit(i as u32, *pos, *atom, 4, &layer.atom_infos, &layer.atoms, syn);
    }

    // Ground subsumption: (p a) again → Subsumes.
    let (pa_pos, pa) = *lits.iter().find(|(pos, a)| {
        *pos && {
            let s = layer.atoms.resolve(*a, syn).unwrap();
            matches!(s.elements.first(),
                Some(crate::types::Element::Symbol(h)) if &*h.name() == "p")
                && layer.atom_info(*a).is_ground()
        }
    }).unwrap();
    assert!(matches!(
        units.check(pa_pos, pa, 0, &layer.atom_infos, &layer.atoms, syn),
        Some(UnitHit::Subsumes(_))));

    // Ground refutation: positive (q a) vs the active ¬(q a)…
    // — but (q ?Y) also subsumes it; either hit is a valid stop, the
    // ground probe just runs first.  Check the negative side instead:
    // ¬(p a) is refuted by the active (p a).
    assert!(matches!(
        units.check(false, pa, 0, &layer.atom_infos, &layer.atoms, syn),
        Some(UnitHit::Refutes(_))));

    // Open subsumption: ground (q a)… the (q ?Y) unit matches it.
    let (_, qa) = *lits.iter().find(|(pos, a)| {
        !*pos && {
            let s = layer.atoms.resolve(*a, syn).unwrap();
            matches!(s.elements.first(),
                Some(crate::types::Element::Symbol(h)) if &*h.name() == "q")
        }
    }).unwrap();
    // As a *positive* literal, (q a) hits ¬(q a) ground-refute or
    // (q ?Y) open-subsume; ground wins (probe order).
    assert!(units.check(true, qa, 0, &layer.atom_infos, &layer.atoms, syn).is_some());
}

#[test]
fn equality_units_register_both_orientations() {
    use super::units::UnitStores;

    let (layer, _) = layer_with("(equal c d)");
    let syn = &layer.semantic.syntactic;
    let lits = all_lits(&layer);
    assert_eq!(lits.len(), 1);
    let mut units = UnitStores::default();
    units.add_unit(7, lits[0].0, lits[0].1, 4, &layer.atom_infos, &layer.atoms, syn);
    assert_eq!(units.equals.len(), 2, "l→r and r→l both present");
    assert_eq!(units.equals[0].1, units.equals[1].2);
    assert_eq!(units.equals[0].2, units.equals[1].1);
}

// -- theory oracle (phase 4) ------------------------------------------------

#[test]
fn oracle_instance_chain_with_witnesses() {
    use super::oracle::SemanticOracle;
    use crate::semantics::types::Scope;

    let (layer, _) = layer_with("
        (subclass Dog Animal)
        (subclass Animal Entity)
        (instance Fido Dog)
    ");
    let syn = &layer.semantic.syntactic;
    let ora = SemanticOracle::new(&layer.semantic, Scope::Base);
    let id = |n: &str| syn.sym_id(n).unwrap();

    let mut why = Vec::new();
    assert!(ora.holds(id("instance"), id("Fido"), id("Entity"), Some(&mut why)));
    // (instance Fido Dog), (subclass Dog Animal), (subclass Animal Entity)
    assert_eq!(why.len(), 3);
    assert_eq!((why[0].rel, why[0].x, why[0].y),
        (id("instance"), id("Fido"), id("Dog")));
    assert_eq!((why[1].rel, why[1].x, why[1].y),
        (id("subclass"), id("Dog"), id("Animal")));
    assert_eq!((why[2].rel, why[2].x, why[2].y),
        (id("subclass"), id("Animal"), id("Entity")));
    // Every hop cites its stored fact — provenance to file:line.
    for w in &why {
        let sid = w.sid.expect("stored witness has a sid");
        assert!(syn.sentence(sid).is_some(), "witness sid resolves in the store");
    }
    assert!(!ora.holds(id("instance"), id("Dog"), id("Entity"), None),
        "Dog is a subclass, not an instance, of Entity");
}

#[test]
fn oracle_subclass_and_reflexivity() {
    use super::oracle::SemanticOracle;
    use crate::semantics::types::Scope;

    let (layer, _) = layer_with("
        (subclass Dog Animal)
        (equal c c)
    ");
    let syn = &layer.semantic.syntactic;
    let ora = SemanticOracle::new(&layer.semantic, Scope::Base);
    let id = |n: &str| syn.sym_id(n).unwrap();

    assert!(ora.holds(id("subclass"), id("Dog"), id("Animal"), None));
    assert!(ora.holds(id("subclass"), id("Dog"), id("Dog"), None), "subclass reflexive");
    assert!(!ora.holds(id("subclass"), id("Animal"), id("Dog"), None), "not symmetric");

    // Equality reflexivity at the atom level, compounds included.
    let lits = all_lits(&layer);
    let (_, eq_atom) = lits.iter()
        .find(|(_, a)| {
            let s = layer.atoms.resolve(*a, syn).unwrap();
            matches!(s.elements.first(),
                Some(crate::types::Element::Op(crate::parse::OpKind::Equal)))
        })
        .expect("(equal c c) clausified");
    assert_eq!(
        SemanticOracle::equal_reflexive(&layer.atoms, syn, *eq_atom),
        Some(true));
}

#[test]
fn oracle_mined_rule_edge_discharges_with_rule_witness() {
    use super::oracle::SemanticOracle;
    use crate::semantics::types::Scope;

    let (layer, roots) = layer_with("
        (=> (gt ?X ?Y) (ge ?X ?Y))
        (gt five three)
    ");
    let syn = &layer.semantic.syntactic;
    let ora = SemanticOracle::new(&layer.semantic, Scope::Base);
    let id = |n: &str| syn.sym_id(n).unwrap();

    let mut why = Vec::new();
    assert!(ora.holds(id("ge"), id("five"), id("three"), Some(&mut why)),
        "(ge five three) inherited through the mined rule-edge");
    assert_eq!(why.len(), 2);
    assert_eq!((why[0].rel, why[0].x, why[0].y), (id("gt"), id("five"), id("three")));
    // "subrelation" is never interned by this fixture — the witness
    // carries the name-hash id (mined edges are virtual subrelations).
    assert_eq!((why[1].rel, why[1].x, why[1].y),
        (crate::types::Symbol::hash_name("subrelation"), id("gt"), id("ge")));
    // The subrelation hop cites the RULE's sid (it is mined, not declared).
    let rule_sid = roots.iter().copied().find(|r| {
        syn.sentence(*r).is_some_and(|s| s.is_operator())
    }).unwrap();
    assert_eq!(why[1].sid, Some(rule_sid), "mined hop cites the rule's source");
}

#[test]
fn oracle_transitive_reachability_with_chain_witnesses() {
    use super::oracle::SemanticOracle;
    use crate::semantics::types::Scope;

    let (layer, _) = layer_with("
        (instance located TransitiveRelation)
        (located a b)
        (located b c)
        (located c d)
    ");
    let syn = &layer.semantic.syntactic;
    let ora = SemanticOracle::new(&layer.semantic, Scope::Base);
    let id = |n: &str| syn.sym_id(n).unwrap();

    let mut why = Vec::new();
    assert!(ora.holds(id("located"), id("a"), id("d"), Some(&mut why)));
    // Chain a→b→c→d plus the transitivity license.
    assert_eq!(why.len(), 4);
    assert_eq!((why[0].x, why[0].y), (id("a"), id("b")));
    assert_eq!((why[1].x, why[1].y), (id("b"), id("c")));
    assert_eq!((why[2].x, why[2].y), (id("c"), id("d")));
    assert_eq!((why[3].rel, why[3].x, why[3].y),
        (id("instance"), id("located"), id("TransitiveRelation")));
    // Chain hops cite stored facts.
    assert!(why[..3].iter().all(|w| w.sid.is_some()));
    // Non-transitive relations do not chain.
    assert!(!ora.holds(id("located"), id("d"), id("a"), None), "no reverse");
}

#[test]
fn oracle_learned_units_extend_the_closure() {
    use super::oracle::SemanticOracle;
    use crate::semantics::types::Scope;

    let (layer, _) = layer_with("
        (instance located TransitiveRelation)
        (located a b)
    ");
    let syn = &layer.semantic.syntactic;
    let mut ora = SemanticOracle::new(&layer.semantic, Scope::Base);
    let id = |n: &str| syn.sym_id(n).unwrap();
    // A symbol the KB never interned — learned units are id-level,
    // so the oracle handles it regardless (SymbolId = name hash).
    let zz9 = crate::types::Symbol::hash_name("zz9");

    assert!(!ora.holds(id("located"), id("a"), zz9, None));
    ora.add_unit(id("located"), id("b"), zz9, None);
    let mut why = Vec::new();
    assert!(ora.holds(id("located"), id("a"), zz9, Some(&mut why)),
        "learned edge b→zz9 chains with the stored a→b");
    assert!(why.iter().any(|w| w.sid.is_none()),
        "the learned hop has no store sid");
    assert!(why.iter().any(|w| w.sid.is_some()),
        "the stored hop still cites its fact");
}

#[test]
fn oracle_respects_session_scope() {
    use super::oracle::SemanticOracle;
    use crate::semantics::types::Scope;
    use crate::syntactic::caches::session::session_id;

    let mut kb = KnowledgeBase::new_native();
    let r = kb.reload_kif(
        "(instance located TransitiveRelation)\n(located a b)",
        &std::path::PathBuf::from("base.kif"), "load");
    assert!(r.ok);
    kb.make_session_axiomatic("load").expect("promote");
    // Session-scoped hypothesis.
    assert!(kb.tell("(located b c)", "hypo").ok);

    let syn = kb.store_for_testing();
    let id = |n: &str| syn.sym_id(n).unwrap();
    let (located, a, c) = (id("located"), id("a"), id("c"));

    let sem = kb.semantic();
    let base_oracle = SemanticOracle::new(sem, Scope::Base);
    assert!(!base_oracle.holds(located, a, c, None),
        "Base never sees the session's transient edge");

    let sess_oracle = SemanticOracle::new(sem, Scope::Session(session_id("hypo")));
    let mut why = Vec::new();
    assert!(sess_oracle.holds(located, a, c, Some(&mut why)),
        "the session sees base ∪ its own overlay");
    assert_eq!(why.len(), 3, "a→b hop, b→c hop, transitivity license");
}

#[test]
fn clause_store_evicts_on_retraction() {
    let mut kb = KnowledgeBase::new_native();
    let r = kb.reload_kif("(=> (p ?X) (q ?X))",
        &std::path::PathBuf::from("evict.kif"), "s1");
    assert!(r.ok);
    let roots = kb.store_for_testing().file_root_sids("evict.kif");
    assert_eq!(roots.len(), 1);
    let root = roots[0];

    let cls = kb.prover().clauses_for(root);
    assert_eq!(cls.len(), 1);
    assert!(kb.prover().clause_store.peek(&root).is_some(), "cached after first ask");

    // Retract the file (truncate re-ingest — the `remove_file` core);
    // the RootRemoved cascade must evict the entry.
    let _ = kb.ingest_source(
        crate::types::SourceFile::truncate(std::path::PathBuf::from("evict.kif")),
        "evict.kif", true);
    assert!(kb.prover().clause_store.peek(&root).is_none(),
        "retraction evicts the root's clauses");
}

// The full lower stack works under the prover top layer: ingest,
// promotion, semantic queries, and SInE selection — everything the
// native prover builds on — with no TranslationLayer in the stack.
#[test]
fn native_stack_smoke() {
    let mut kb = KnowledgeBase::new_native();
    let kif = "(subclass Dog Animal)\n\
                (instance Fido Dog)\n\
                (=> (instance ?X Dog) (attribute ?X Loyal))";
    let r = kb.reload_kif(kif, &std::path::PathBuf::from("smoke.kif"), "s1");
    assert!(r.ok, "ingest: {:?}", r.diagnostics);

    // The generic promotion core — the TranslationLayer-specific
    // `make_session_axiomatic` (TPTP consistency check) doesn't exist
    // on the prover stack; the native equivalent lands in the
    // given-clause phase.
    kb.make_session_axiomatic("s1").expect("promote");

    // Semantic layer answers through the generic accessors.
    let syn = kb.store_for_testing();
    let dog = syn.sym_id("Dog").expect("Dog interned");
    let animal = syn.sym_id("Animal").expect("Animal interned");
    let fido = syn.sym_id("Fido").expect("Fido interned");
    assert!(kb.semantic().has_ancestor(dog, animal), "taxonomy edge live");
    assert!(kb.semantic().reaches_via_instance(fido, dog), "instance live");

    // SInE selection over the promoted axioms.
    assert_eq!(kb.sine_axiom_count(), 3);
    let selected = kb
        .sine_select_for_query("(attribute Fido Loyal)", SineParams::default())
        .expect("sine select");
    assert!(!selected.is_empty(), "SInE selects relevant axioms");
}
    // Phase-3 probe: run the automatic Horn extractor on a real ontology file
    // (`SIGMA_EXTRACT_FILE=/path/to/Merge.kif cargo test … extract_from_large_file
    //  -- --nocapture`), reporting what the extractor recovers and whether the
    // resulting Datalog(¬) program is stratifiable.  Skips cheaply when unset.
    // Set `SIGMA_EXTRACT_EVAL=1` to also evaluate the program to its model
    // (a preview of the Phase-5 scale question — may be slow on large KBs).
    #[test]
    fn extract_from_large_file() {
        let Some(path) = std::env::var_os("SIGMA_EXTRACT_FILE") else {
            eprintln!("SIGMA_EXTRACT_FILE unset — skipping large-file extraction probe");
            return;
        };
        let text = std::fs::read_to_string(&path).expect("read SIGMA_EXTRACT_FILE");
        let mut kb = KnowledgeBase::new_native();
        let r = kb.reload_kif(&text, &std::path::PathBuf::from(&path), "load");
        eprintln!("ingest: ok={} warnings={}", r.ok, r.warnings().count());
        let _ = kb.make_session_axiomatic("load");

        let syn = &kb.layer.semantic.syntactic;
        let roots = syn.root_sids().len();
        let prog = crate::saturate::model::extract::extract_horn_program(syn);

        // Clause-signature role recognition (prototype): recover roles from the
        // extracted Horn-clause shapes, name-independently.  Cross-check the
        // bridge against the bespoke sentence-shape recognizer.
        {
            use crate::saturate::model::recognize;
            let rp = recognize::recognize(&prog.rules);
            let name = |p: &crate::SymbolId| syn.sym_name(*p).map(|s| s.name().to_string())
                .unwrap_or_else(|| format!("{p:x}"));
            eprintln!(
                "clause-sig recognition: {} transitive, {} symmetric, {} subrelation, {} bridges",
                rp.transitive.len(), rp.symmetric.len(), rp.subrelation.len(), rp.bridges.len(),
            );
            for (i, c) in rp.bridges.iter().take(4) {
                eprintln!("  bridge: instance-like={} subclass-like={}", name(i), name(c));
            }
            for r in rp.transitive.iter().take(6) {
                eprintln!("  transitive-rule: {}", name(r));
            }
            // Bespoke recognizer's roles, for comparison.
            let roles = crate::semantics::roles::TaxonomyRoles::recognize(syn, syn.root_sids());
            let bridge_recovers = rp.bridges.iter()
                .any(|(i, c)| *i == roles.instance && *c == roles.subclass);
            eprintln!("  bespoke roles: instance={} subclass={}; clause-sig bridge recovers them: {bridge_recovers}",
                name(&roles.instance), name(&roles.subclass));
        }
        let n_facts: usize = prog.edb.values().map(|s| s.len()).sum();
        let head_preds: std::collections::HashSet<_> =
            prog.rules.iter().map(|r| r.head.pred).collect();
        eprintln!(
            "roots={roots} → extracted {} rules ({} distinct heads), \
             {} facts over {} relations",
            prog.rules.len(), head_preds.len(), n_facts, prog.edb.len(),
        );
        for name in ["instance", "subclass", "subrelation", "disjoint"] {
            let id = crate::types::Symbol::hash_name(name);
            let rules = prog.rules.iter().filter(|r| r.head.pred == id).count();
            let facts = prog.edb.get(&id).map_or(0, |s| s.len());
            eprintln!("  {name}: {rules} rules, {facts} EDB facts");
        }
        match prog.stratify() {
            Ok(strata) => eprintln!("stratifiable: {} strata", strata.len()),
            Err(e) => eprintln!("NOT stratifiable (whole-KB monolith): {e:?}"),
        }

        // Per-cluster: restrict to the taxonomy definitional fragment
        // (instance/subclass/subrelation) — a rule is kept only if its head AND
        // every body predicate lie in the cluster.  This is the manual stand-in
        // for Phase-4 cluster partitioning; it should stratify + evaluate to the
        // taxonomy closure even though the monolith does not.
        let allow: std::collections::HashSet<crate::SymbolId> =
            ["instance", "subclass", "subrelation"]
                .iter().map(|n| crate::types::Symbol::hash_name(n)).collect();

        // Phase 3.5: the generalized schema expander, with the transitive set
        // DERIVED from the model (no hard-coded seed).  Base = EDB + extracted
        // Horn rules + subrelation schema rules.  Then fixpoint: evaluate,
        // derive `transitive(R) ⟸ (R,TransitiveRelation) ∈ instance-closure`,
        // instantiate transitivity for the fresh ones, repeat until stable.
        use crate::saturate::model::extract;
        let roles = crate::semantics::roles::TaxonomyRoles::default();
        let decls = extract::collect_role_decls(syn, &roles);
        eprintln!(
            "role decls: {} subrelation, {} direct-transitive, {} symmetric",
            decls.subrelation.len(), decls.transitive.len(), decls.symmetric.len(),
        );

        let mut tax = crate::saturate::model::Program::default();
        for (p, facts) in &prog.edb {
            if allow.contains(p) {
                for t in facts { tax.fact(*p, t.clone()); }
            }
        }
        let subrel_only = extract::RoleDecls {
            subrelation: decls.subrelation.clone(), transitive: vec![], symmetric: vec![],
        };
        for r in prog.rules.iter().chain(extract::schema_rules(&subrel_only, &[]).iter()) {
            let in_cluster = allow.contains(&r.head.pred)
                && r.body.iter().all(|l| allow.contains(&l.atom.pred));
            if in_cluster { tax.rules.push(r.clone()); }
        }

        let mut known: std::collections::HashSet<crate::SymbolId> = std::collections::HashSet::new();
        let mut model = tax.evaluate().expect("taxonomy cluster stratifiable");
        for pass in 0.. {
            let trans = extract::transitive_members(&model, &roles);
            let fresh: Vec<_> = trans.into_iter().filter(|r| known.insert(*r)).collect();
            if fresh.is_empty() {
                eprintln!("transitive-derivation fixpoint converged after {pass} passes");
                break;
            }
            for r in extract::schema_rules(&extract::RoleDecls::default(), &fresh) {
                if allow.contains(&r.head.pred)
                    && r.body.iter().all(|l| allow.contains(&l.atom.pred)) {
                    tax.rules.push(r);
                }
            }
            model = tax.evaluate().expect("stratifiable");
        }
        let subclass_id = crate::types::Symbol::hash_name("subclass");
        eprintln!(
            "DERIVED transitive relations: {} (subclass derived-transitive: {})",
            known.len(), known.contains(&subclass_id),
        );
        for name in ["instance", "subclass", "subrelation"] {
            let id = crate::types::Symbol::hash_name(name);
            eprintln!("  cluster {name}: {} tuples (closure)", model.get(&id).map_or(0, |s| s.len()));
        }

        // Phase 4: automatic cluster partitioning (no allowlist).  Discover the
        // stratifiable definitional clusters of the FULL extracted program and
        // isolate the unstratifiable parts.
        use crate::saturate::model::cluster;
        let clusters = cluster::partition(&prog);
        let total_preds: std::collections::HashSet<crate::SymbolId> = prog.rules.iter()
            .flat_map(|r| std::iter::once(r.head.pred).chain(r.body.iter().map(|l| l.atom.pred)))
            .chain(prog.edb.keys().copied()).collect();
        let modelable: std::collections::HashSet<_> =
            clusters.iter().flat_map(|c| c.preds.iter().copied()).collect();
        eprintln!(
            "partition: {} clusters; {} of {} preds modelable ({} dropped to negation cycles)",
            clusters.len(), modelable.len(), total_preds.len(),
            total_preds.len() - modelable.len(),
        );
        let stratifiable = clusters.iter().filter(|c| c.program.evaluate().is_ok()).count();
        eprintln!("  every cluster stratifiable: {} ({stratifiable}/{})", stratifiable == clusters.len(), clusters.len());
        let sub = crate::types::Symbol::hash_name("subclass");
        if let Some((i, c)) = clusters.iter().enumerate().find(|(_, c)| c.preds.contains(&sub)) {
            let inst = c.preds.contains(&crate::types::Symbol::hash_name("instance"));
            eprintln!("  discovered taxonomy cluster #{i}: {} preds, {} rules (contains instance: {inst})",
                c.preds.len(), c.program.rules.len());
        }
        // SInE-as-demand hook: a conjecture mentioning `instance` selects only
        // the cluster(s) it touches.  `seed` is exactly SInE's symbol output.
        let seed: std::collections::HashSet<crate::SymbolId> =
            [crate::types::Symbol::hash_name("instance")].into_iter().collect();
        let rel = cluster::relevant_clusters(&clusters, &seed);
        eprintln!("  SInE-demand: seed={{instance}} -> selects {} of {} clusters", rel.len(), clusters.len());

        // Shared predicates (instance/subclass) get over-tainted by predicate-
        // SCC partitioning (one giant SCC + a negation makes the whole SCC
        // bad).  Their POSITIVE definition is still sound — recover it via the
        // monotone (negation-free) fragment, with the derived transitivity
        // schema rules folded in.  This reproduces the taxonomy closure with no
        // allowlist and no manual cluster.
        let mut mono = cluster::positive_program(&prog);
        let known_vec: Vec<crate::SymbolId> = known.iter().copied().collect();
        for r in extract::schema_rules(&decls, &known_vec) {
            mono.rules.push(r);
        }
        match mono.evaluate() {
            Ok(m) => {
                let total: usize = m.values().map(|s| s.len()).sum();
                eprintln!("monotone fragment (sound positive model): {} rules, {total} tuples", mono.rules.len());
                for name in ["instance", "subclass", "subrelation"] {
                    let id = crate::types::Symbol::hash_name(name);
                    eprintln!("  {name}: {} tuples (closure, no allowlist)", m.get(&id).map_or(0, |s| s.len()));
                }
            }
            Err(e) => eprintln!("monotone fragment evaluate failed: {e:?}"),
        }

        if std::env::var_os("SIGMA_EXTRACT_EVAL").is_some() {
            match prog.evaluate() {
                Ok(m) => {
                    let total: usize = m.values().map(|s| s.len()).sum();
                    eprintln!("evaluated: {} relations, {total} total tuples", m.len());
                    for name in ["instance", "subclass", "subrelation"] {
                        let id = crate::types::Symbol::hash_name(name);
                        eprintln!("  {name}: {} tuples in model", m.get(&id).map_or(0, |s| s.len()));
                    }
                }
                Err(e) => eprintln!("evaluate failed: {e:?}"),
            }
        }
    }

    fn kb_from(kif: &str) -> KnowledgeBase<ProverLayer> {
        let mut kb = KnowledgeBase::new_native();
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("base.kif"), "load");
        assert!(r.ok, "fixture ingest failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("load").expect("promote");
        kb
    }

    fn fast() -> NativeOpts {
        NativeOpts {
            max_steps: 2000, max_lits: 8, time_limit_secs: 10,
            forward_close: true, profile: false,
            // Tests assert on transcripts.
            want_proof: true,
            ..Default::default()
        }
    }

    // The TQG5 pattern in miniature: an equality chain bridges a gap in
    // the subclass hierarchy.  `Org` is a `Lizard ⊂ C1`, and only the
    // chain `C1 = C2 = C3 = C4` connects `C1` to `C4 ⊂ Animal`.  Without
    // ground-equality congruence closure the four C-classes stay
    // distinct and the goal is unreachable; with it they collapse to one
    // representative and the subclass chain resolves.
    #[test]
    fn equality_chain_bridges_subclass_gap() {
        let mut kb = KnowledgeBase::new_native();
        for ax in [
            "(=> (and (instance ?X ?C) (subclass ?C ?D)) (instance ?X ?D))",
            "(instance Org Lizard)",
            "(subclass Lizard C1)",
            "(equal C1 C2)", "(equal C2 C3)", "(equal C3 C4)",
            "(subclass C4 Animal)",
        ] {
            assert!(kb.tell(ax, "h").ok, "tell {ax}");
        }
        let res = kb.ask_query("(instance Org Animal)", Some("h"),
            SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // TQG14 shape: a uniqueness axiom stated over `part`, facts given
    // via the subrelation `component`.  Proving it needs the synthesized
    // `(=> (component ?x ?y) (part ?x ?y))` rule to bind the uniqueness
    // axiom's open part-literals, then the derived equality contradicts
    // the negated-uniqueness query's skolem.
    #[test]
    fn uniqueness_via_subrelation_bridge() {
        let mut kb = KnowledgeBase::new_native();
        for ax in [
            "(subrelation component part)",
            "(=> (instance ?A Atom) \
                 (forall (?N1 ?N2) \
                   (=> (and (part ?N1 ?A) (part ?N2 ?A) \
                            (instance ?N1 Nucleus) (instance ?N2 Nucleus)) \
                       (equal ?N1 ?N2))))",
            "(instance MyAtom Atom)",
            "(instance N1 Nucleus)",
            "(component N1 MyAtom)",
        ] {
            assert!(kb.tell(ax, "h").ok);
        }
        let res = kb.ask_query(
            "(not (exists (?N) (and (instance ?N Nucleus) (component ?N MyAtom) \
                                    (not (equal ?N N1)))))",
            Some("h"), SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // Sorted-relation filter: a ground relation atom whose argument is
    // provably disjoint from the position's domain is ill-typed and is
    // dropped, so nothing downstream can use it (the CellPartFn
    // The sort filter applies to DERIVED clauses only.  An ASSERTED
    // ill-typed fact is ground truth — SUMO itself violates its own
    // domain declarations (Merge asserts `component` over nuclei
    // against component's CorpuscularObject ⊥ Substance typing, which
    // is exactly TQG14's shape) — so deleting it would silently change
    // the problem.  Backward refutation through an asserted ill-typed
    // fact is therefore legitimate; what the filter still prunes is
    // FORWARD fabrication (fc conclusions, derived positive units)
    // polluting the unit stores and oracle.
    #[test]
    fn sorted_filter_exempts_asserted_facts() {
        let mut kb = KnowledgeBase::new_native();
        for ax in [
            "(domain likes 1 Person)",
            "(disjoint Person Rock)",
            "(instance Pebble Rock)",
            "(=> (likes ?X ?Y) (happy ?Y))",
            "(likes Pebble Joy)", // ill-typed per the domain — but asserted
        ] {
            assert!(kb.tell(ax, "h").ok);
        }
        let res = kb.ask_query("(happy Joy)", Some("h"),
            SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved,
            "asserted facts are exempt from the sort filter; raw: {}", res.raw_output);
    }

    // Control: the same shape with a well-typed actor proves normally —
    // the filter rejects only provable type violations.
    #[test]
    fn sorted_filter_keeps_welltyped_atom() {
        let mut kb = KnowledgeBase::new_native();
        for ax in [
            "(domain likes 1 Person)",
            "(disjoint Person Rock)",
            "(instance Alice Person)",
            "(=> (likes ?X ?Y) (happy ?Y))",
            "(likes Alice Joy)",
        ] {
            assert!(kb.tell(ax, "h").ok);
        }
        let res = kb.ask_query("(happy Joy)", Some("h"),
            SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // TQG30 shape: two classes that subclass each other are equal
    // (subclass antisymmetry, discharged by the oracle directly).
    #[test]
    fn mutual_subclass_proves_equality() {
        let mut kb = KnowledgeBase::new_native();
        for ax in ["(subclass Foo Bar)", "(subclass Bar Foo)"] {
            assert!(kb.tell(ax, "h").ok);
        }
        let res = kb.ask_query("(equal Foo Bar)", Some("h"),
            SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
        // A one-directional subclass must NOT prove equality.
        let mut kb2 = KnowledgeBase::new_native();
        assert!(kb2.tell("(subclass Sub Super)", "h").ok);
        let res2 = kb2.ask_query("(equal Sub Super)", Some("h"),
            SineParams::default(), fast());
        assert_ne!(res2.status, ProverStatus::Proved, "raw: {}", res2.raw_output);
    }

    // TQG9 shape: an existential whose witness is pinned by a variable
    // equality `(equal ?E Human)`.  Equality resolution binds ?E↦Human,
    // and the residual subclass literals discharge against the oracle.
    #[test]
    fn variable_equality_resolves_existential() {
        let mut kb = KnowledgeBase::new_native();
        for ax in ["(subclass Human Animal)", "(subclass Human CognitiveAgent)"] {
            assert!(kb.tell(ax, "h").ok);
        }
        let res = kb.ask_query(
            "(exists (?E) (and (subclass ?E Animal) (subclass ?E CognitiveAgent) \
                               (equal ?E Human)))",
            Some("h"), SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // TQG22 shape: variable equality + a transitive relation.  ?X↦Ancestor
    // via equality resolution, then (ancestor Man Ancestor) discharges
    // through the oracle's transitive reachability.
    #[test]
    fn variable_equality_with_transitive_relation() {
        let mut kb = KnowledgeBase::new_native();
        for ax in [
            "(instance ancestor TransitiveRelation)",
            "(ancestor Man Mid)", "(ancestor Mid Ancestor)",
        ] {
            assert!(kb.tell(ax, "h").ok);
        }
        let res = kb.ask_query(
            "(exists (?X) (and (ancestor Man ?X) (equal ?X Ancestor)))",
            Some("h"), SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // Congruence closure must not invent equalities: with the chain
    // broken (no `(equal C2 C3)`), the goal stays unreachable.
    #[test]
    fn broken_equality_chain_does_not_prove() {
        let mut kb = KnowledgeBase::new_native();
        for ax in [
            "(=> (and (instance ?X ?C) (subclass ?C ?D)) (instance ?X ?D))",
            "(instance Org Lizard)",
            "(subclass Lizard C1)",
            "(equal C1 C2)", "(equal C3 C4)", // gap: C2 ≠ C3
            "(subclass C4 Animal)",
        ] {
            assert!(kb.tell(ax, "h").ok);
        }
        let res = kb.ask_query("(instance Org Animal)", Some("h"),
            SineParams::default(), fast());
        assert_ne!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    #[test]
    fn horn_chain_is_proved_with_proof() {
        let mut kb = kb_from("
            (instance Corleone Mafioso)
            (=> (instance ?X Mafioso) (criminal ?X))
            (=> (criminal ?X) (suspect ?X))
        ");
        let res = kb.ask_query("(suspect Corleone)", None, SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
        assert!(!res.proof_kif.is_empty(), "a refutation carries a proof");
        assert!(res.proof_kif.iter().any(|s| s.rule == "negated_conjecture"));
        // Input steps cite their stored roots (file:line provenance).
        let cited: Vec<_> = res.proof_kif.iter()
            .filter(|s| s.source_sid.is_some()).collect();
        assert!(!cited.is_empty(), "axiom steps cite source sids");
        for s in &cited {
            assert!(kb.store_for_testing().sentence(s.source_sid.unwrap()).is_some(),
                "cited sid resolves in the store");
        }
        // The final step is the empty clause.
        let last = res.proof_kif.last().unwrap();
        assert!(matches!(&last.formula,
            crate::AstNode::Symbol { name, .. } if name == "FALSE"));
    }

    #[test]
    fn non_theorem_saturates_to_disproved() {
        let mut kb = kb_from("
            (instance Rex Dog)
            (=> (instance ?X Cat) (meows ?X))
        ");
        let res = kb.ask_query("(meows Rex)", None, SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Disproved, "raw: {}", res.raw_output);
        assert_eq!(res.termination, Some(TerminationReason::Saturation));
        assert!(res.proof_kif.is_empty());
    }

    #[test]
    fn oracle_discharge_appears_in_proof_with_fact_sids() {
        // The rule needs (instance ?X Dog); Fido is a Puppy, a subclass
        // of Dog — only the oracle's taxonomy closure bridges the gap.
        let mut kb = kb_from("
            (subclass Puppy Dog)
            (instance Fido Puppy)
            (=> (instance ?X Dog) (barks ?X))
        ");
        let res = kb.ask_query("(barks Fido)", None, SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
        // The witnessing taxonomy facts surface as cited axiom steps.
        let cited: std::collections::HashSet<SentenceId> = res.proof_kif.iter()
            .filter_map(|s| s.source_sid).collect();
        let syn = kb.store_for_testing();
        let instance_fact = syn.file_root_sids("base.kif").into_iter()
            .find(|sid| {
                let s = syn.sentence(*sid).unwrap();
                s.head_symbol_name().is_some_and(|h| &*h.name() == "instance")
            }).unwrap();
        let subclass_fact = syn.file_root_sids("base.kif").into_iter()
            .find(|sid| {
                let s = syn.sentence(*sid).unwrap();
                s.head_symbol_name().is_some_and(|h| &*h.name() == "subclass")
            }).unwrap();
        assert!(cited.contains(&instance_fact),
            "(instance Fido Puppy) cited as an oracle witness");
        assert!(cited.contains(&subclass_fact),
            "(subclass Puppy Dog) cited as an oracle witness");
    }

    #[test]
    fn consistency_check_native() {
        let mut kb = kb_from("
            (instance Rex Dog)
            (=> (instance ?X Dog) (barks ?X))
        ");
        let res = kb.check_satisfiable(fast());
        assert_eq!(res.status, ProverStatus::Consistent, "raw: {}", res.raw_output);

        let mut kb2 = kb_from("
            (barks Rex)
            (not (barks Rex))
        ");
        let res2 = kb2.check_satisfiable(fast());
        assert_eq!(res2.status, ProverStatus::Inconsistent, "raw: {}", res2.raw_output);
    }

    #[test]
    fn session_hypotheses_drive_the_support_set() {
        let mut kb = kb_from("(=> (wet ?X) (slippery ?X))");
        assert!(kb.tell("(wet Floor)", "hypo").ok);
        let res = kb.ask_query(
            "(slippery Floor)", Some("hypo"), SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
        // Without the session, the same ask saturates.
        let res2 = kb.ask_query("(slippery Floor)", None, SineParams::default(), fast());
        assert_ne!(res2.status, ProverStatus::Proved);
    }

    #[test]
    fn parse_error_maps_to_input_error() {
        let mut kb = kb_from("(instance Rex Dog)");
        let res = kb.ask_query("(broken (", None, SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::InputError);
        // And the failed parse left no residue: a follow-up ask works.
        let res2 = kb.ask_query("(instance Rex Dog)", None, SineParams::default(), fast());
        assert_ne!(res2.status, ProverStatus::InputError);
    }

    // The ingest pipeline splits a top-level `(and A B)` query into
    // separate roots.  Negating each root independently asserts ¬A ∧ ¬B
    // — the negation of the DISJUNCTION — so refuting one provable
    // conjunct would "prove" the whole conjunction even when the other
    // conjunct is false.  The negation must wrap the rebuilt
    // conjunction (TQG7's shape, caught via a planted false conjunct).
    #[test]
    fn conjunction_with_unprovable_conjunct_is_not_proved() {
        let mut kb = kb_from("(instance Rex Dog)");
        let res = kb.ask_query(
            "(and (instance Rex Dog) (instance Rex Cat))",
            None, SineParams::default(), fast());
        assert_ne!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
        // Both conjuncts provable → proves.
        let res2 = kb.ask_query(
            "(and (instance Rex Dog) (instance Rex Dog))",
            None, SineParams::default(), fast());
        assert_eq!(res2.status, ProverStatus::Proved, "raw: {}", res2.raw_output);
    }

    // TQG14 in miniature: a guarded uniqueness axiom (part keyed on
    // the whole, both sides guarded by Nucleus) + facts arriving via
    // the subrelation component.  Proving "no OTHER nucleus" requires
    // deriving skolem = N1 — FD congruence supplies it without
    // saturation having to schedule the uniqueness clause.
    #[test]
    fn fd_congruence_proves_guarded_uniqueness() {
        let mut kb = kb_from("
            (subrelation component part)
            (=> (and (instance ?A At)
                     (part ?N1 ?A)
                     (part ?N2 ?A)
                     (instance ?N1 Nuc)
                     (instance ?N2 Nuc))
                (equal ?N1 ?N2))");
        for f in ["(instance A1 At)", "(instance N1 Nuc)", "(component N1 A1)"] {
            assert!(kb.tell(f, "h").ok, "tell {f}");
        }
        let res = kb.ask_query(
            "(not (exists (?N) (and (instance ?N Nuc) (component ?N A1) (not (equal ?N N1)))))",
            Some("h"), SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
        // The transcript must cite the uniqueness axiom's derivation,
        // not a bare oracle FALSE.
        let flat: Vec<String> =
            res.proof_kif.iter().map(|s| s.formula.flat()).collect();
        assert!(flat.iter().any(|f| f.contains("equal")),
            "equality step missing from proof: {flat:?}");
    }

    // TQG25 in miniature: a partition declares exhaustiveness; with
    // all members but one excluded, the survivor is entailed — by
    // oracle case-elimination, not by saturating SUMO's ListFn-based
    // exhaustiveness axiom (which floods).
    #[test]
    fn exhaustiveness_case_elimination() {
        let mut kb = kb_from("(partition Org A B C)");
        for f in [
            "(instance X Org)",
            "(not (instance X A))",
            "(not (instance X B))",
        ] {
            assert!(kb.tell(f, "h").ok, "tell {f}");
        }
        let res = kb.ask_query("(instance X C)", Some("h"),
            SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // TQG36 in miniature: equality between COMPOUND terms enters the
    // union-find by content hash, so two facts equating different
    // arguments' images under one function connect transitively.
    #[test]
    fn compound_term_equality_closes_transitively() {
        let mut kb = kb_from("(instance FooFn Function)");
        for f in ["(equal (FooFn A) (FooFn C))", "(equal (FooFn B) (FooFn C))"] {
            assert!(kb.tell(f, "h").ok, "tell {f}");
        }
        let res = kb.ask_query("(equal (FooFn A) (FooFn B))", Some("h"),
            SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // TQG28 in miniature: an n-ary contraryAttribute fact bridges to
    // binary pairs through inList over a ground ListFn — the list's
    // extension is synthesized as theory units ((inList m L), (equal m
    // (ListOrderFn L i))) the first time the ground list appears.
    #[test]
    fn ground_list_theory_derives_membership() {
        let mut kb = kb_from("
            (=> (and (contraryAttribute @ROW)
                     (inList ?A1 (ListFn @ROW))
                     (inList ?A2 (ListFn @ROW)))
                (contraryAttribute ?A1 ?A2))
            (=> (and (contraryAttribute ?A1 ?A2) (attribute ?O ?A1))
                (not (attribute ?O ?A2)))");
        for f in [
            "(contraryAttribute Rocky Icy Watery Gaseous)",
            "(attribute Obj Watery)",
        ] {
            assert!(kb.tell(f, "h").ok, "tell {f}");
        }
        let res = kb.ask_query("(not (attribute Obj Gaseous))", Some("h"),
            SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // Input contradictions discovered during an UNRELATED ask are
    // harvested as citable transcripts (and the ask itself is not
    // poisoned by them — paraconsistent set of support).
    #[test]
    fn input_contradictions_are_harvested_with_transcripts() {
        let mut kb = kb_from("(=> (p ?X) (q ?X))");
        for f in ["(p A)", "(not (q A))", "(r B)"] {
            assert!(kb.tell(f, "h").ok, "tell {f}");
        }
        // (r B) is asserted, so this proves — DESPITE p/¬q contradicting.
        let res = kb.ask_query("(r B)", Some("h"), SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
        assert!(!res.contradiction_proofs.is_empty(),
            "the p/q contradiction must be harvested; raw: {}", res.raw_output);
        let flat: Vec<String> = res.contradiction_proofs[0]
            .iter().map(|s| s.formula.flat()).collect();
        assert!(flat.iter().any(|f| f == "FALSE"),
            "transcript must end in FALSE: {flat:?}");
        assert!(res.raw_output.contains("input contradiction"),
            "stats line must warn: {}", res.raw_output);
    }

    // TQG7 in miniature: the second conjunct is only reachable through
    // a forward-closure derivation chain (sibling shares parent; parent
    // + Female = mother).  The discharge happens against a LEARNED
    // oracle unit — the learned entry must carry its deriving clause as
    // a proof-DAG parent so the chain appears in the transcript instead
    // of a bare `[oracle] FALSE`.
    #[test]
    fn learned_unit_discharge_surfaces_its_derivation() {
        let mut kb = kb_from("
            (=> (mother ?A ?B) (parent ?A ?B))
            (=> (mother ?C ?M) (attribute ?M Female))
            (=> (and (sibling ?X ?Y) (parent ?X ?P)) (parent ?Y ?P))
            (=> (and (parent ?C ?P) (attribute ?P Female)) (mother ?C ?P))");
        assert!(kb.tell("(mother Bill Jane)", "h").ok);
        assert!(kb.tell("(sibling Bill Bob)", "h").ok);
        let res = kb.ask_query(
            "(and (mother Bill Jane) (mother Bob Jane))",
            Some("h"), SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
        // The transcript must include the derived intermediate steps,
        // not just the conjecture and FALSE.
        let flat: Vec<String> =
            res.proof_kif.iter().map(|s| s.formula.flat()).collect();
        assert!(flat.iter().any(|f| f.contains("(parent Bob Jane)")),
            "derivation chain missing from proof: {flat:?}");
        assert!(flat.iter().any(|f| f.contains("(mother Bob Jane)")),
            "derived conjunct missing from proof: {flat:?}");
    }

    // -- schema channel ----------------------------------------------------------

    // DECLARED symmetry: the fact is stored one way round, the query
    // asks the other.  No metaschema axiom is loaded — orientation plus
    // the oracle's reversed-edge check must close the gap alone.
    #[test]
    fn declared_symmetry_proves_reversed_query() {
        let mut kb = kb_from("(instance friendOf SymmetricRelation)");
        assert!(kb.tell("(friendOf Bob Alice)", "h").ok);
        let res = kb.ask_query("(friendOf Alice Bob)", Some("h"),
            SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // RULE-STATED symmetry (no SymmetricRelation declaration anywhere):
    // the schema channel mines `(=> (R x y) (R y x))`, absorbs the rule
    // clause, and orientation carries the proof.
    #[test]
    fn rule_stated_symmetry_mined_and_proved() {
        let mut kb = kb_from("(=> (friendOf ?X ?Y) (friendOf ?Y ?X))");
        assert!(kb.tell("(friendOf Bob Alice)", "h").ok);
        let res = kb.ask_query("(friendOf Alice Bob)", Some("h"),
            SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // The full SUMO configuration in miniature: metaschema + declaration
    // + fact.  The metaschema is absorbed at load; the declaration route
    // must still prove the reversed query (this is exactly the TQG36/8
    // flood source — the metaschema may not be needed for ANY of it).
    #[test]
    fn symmetry_metaschema_absorbed_without_loss() {
        let mut kb = kb_from("
            (=> (and (instance ?REL SymmetricRelation) (?REL ?I1 ?I2)) (?REL ?I2 ?I1))
            (instance friendOf SymmetricRelation)");
        assert!(kb.tell("(friendOf Bob Alice)", "h").ok);
        let res = kb.ask_query("(friendOf Alice Bob)", Some("h"),
            SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // Open-literal completeness across orientation: the friendOf fact
    // is DERIVED (so it is oriented before indexing), and the rule's
    // body literal mentions the arguments the other way round.  The
    // symmetric dual retrieval + swap-retry unification (or the
    // oracle's reversed-edge check, whichever fires first) must connect
    // them — this is the case naive orientation alone gets wrong.
    #[test]
    fn symmetric_open_literal_resolves_across_orientation() {
        let mut kb = kb_from("
            (instance friendOf SymmetricRelation)
            (=> (instance ?X Greeter) (friendOf ?X Alice))
            (=> (friendOf Bob ?P) (happyAbout ?P))");
        assert!(kb.tell("(instance Bob Greeter)", "h").ok);
        let res = kb.ask_query("(happyAbout Alice)", Some("h"),
            SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // Rule-stated transitivity registers with the oracle's reachability
    // (the clause is kept — absorption would lose open-goal
    // enumeration); a two-hop chain must still prove.
    #[test]
    fn rule_stated_transitivity_proves_chain() {
        let mut kb = kb_from("(=> (and (taller ?X ?Y) (taller ?Y ?Z)) (taller ?X ?Z))");
        assert!(kb.tell("(taller A B)", "h").ok);
        assert!(kb.tell("(taller B C)", "h").ok);
        let res = kb.ask_query("(taller A C)", Some("h"),
            SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // Without want_proof the verdict is identical (vacuity is decided
    // on the raw derivation DAG, not on rendered steps) but no
    // transcript is rendered.
    #[test]
    fn want_proof_false_drops_transcript_but_not_status() {
        let mut kb = kb_from("(=> (instance ?X Dog) (attribute ?X Loyal))");
        assert!(kb.tell("(instance Rex Dog)", "h").ok);
        let opts = NativeOpts { want_proof: false, ..fast() };
        let res = kb.ask_query("(attribute Rex Loyal)", Some("h"),
            SineParams::default(), opts);
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
        assert!(res.proof_kif.is_empty(), "transcript must be skipped");
    }

    // Forward demodulation by a NON-ground unit equation — the case the
    // oracle's ground congruence closure does NOT cover, so demodulation
    // is genuinely the mechanism.  `(equal (sideKick ?X) ?X)` is a
    // background rule; KBO orients it `sideKick(X) → X` (heavier, still
    // contains X), so the support fact `(admires Lois (sideKick Clark))`
    // rewrites to `(admires Lois Clark)` — the conjecture.
    #[test]
    fn forward_demodulation_rewrites_function_term() {
        let mut kb = kb_from("(equal (sideKick ?X) ?X)");
        assert!(kb.tell("(admires Lois (sideKick Clark))", "h").ok);
        // demod is OFF by default (measured net-negative on TPTP
        // pre-indexing); enable it explicitly to exercise the mechanism.
        let mut opts = fast();
        opts.strategy = crate::saturate::strategy::Strategy::base();
        opts.strategy.demod = true;
        opts.want_proof = true;
        let res = kb.ask_query("(admires Lois Clark)", Some("h"),
            SineParams::default(), opts);
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
        assert!(!res.raw_output.contains("0 demodulated"),
            "demodulation must fire: {}", res.raw_output);
    }

    // The knob genuinely gates the mechanism: with demod off, no rewrite
    // is performed (the goal may still prove by paramodulation).
    #[test]
    fn demod_knob_gates_the_rewrite() {
        let mut kb = kb_from("(equal (sideKick ?X) ?X)");
        assert!(kb.tell("(admires Lois (sideKick Clark))", "h").ok);
        let mut opts = fast();
        opts.strategy = crate::saturate::strategy::Strategy::base();
        opts.strategy.demod = false;
        let res = kb.ask_query("(admires Lois Clark)", Some("h"),
            SineParams::default(), opts);
        assert!(res.raw_output.contains("0 demodulated"),
            "demod off must perform no rewrites: {}", res.raw_output);
    }

    // Ordered resolution (superposition prerequisite) restricts binary
    // resolution to KBO-maximal literals — a complete refinement, so a
    // provable goal stays provable.
    #[test]
    fn ordered_resolution_preserves_a_proof() {
        let mut kb = kb_from(
            "(=> (and (instance ?X ?C) (subclass ?C ?D)) (instance ?X ?D))");
        assert!(kb.tell("(instance Org Lizard)", "h").ok);
        assert!(kb.tell("(subclass Lizard Reptile)", "h").ok);
        assert!(kb.tell("(subclass Reptile Animal)", "h").ok);
        let mut opts = fast();
        opts.strategy = crate::saturate::strategy::Strategy::base();
        opts.strategy.ordered_resolution = true;
        let res = kb.ask_query("(instance Org Animal)", Some("h"),
            SineParams::default(), opts);
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // The frozen-background cache: an identical repeat ask hydrates
    // the snapshot instead of rebuilding (same verdict, no new cache
    // entry); any KB/session change reshapes the key and never hits a
    // stale base.
    #[test]
    fn background_snapshot_reuses_and_invalidates() {
        let mut kb = kb_from("(=> (instance ?X Dog) (attribute ?X Loyal))");
        assert!(kb.tell("(instance Rex Dog)", "h").ok);

        let r1 = kb.ask_query("(attribute Rex Loyal)", Some("h"),
            SineParams::default(), fast());
        assert_eq!(r1.status, ProverStatus::Proved, "raw: {}", r1.raw_output);
        let after_first = kb.layer.bg_snapshots.len();
        assert!(after_first >= 1, "miss path must freeze a snapshot");

        // Identical repeat: hits, proves identically, adds nothing.
        let r2 = kb.ask_query("(attribute Rex Loyal)", Some("h"),
            SineParams::default(), fast());
        assert_eq!(r2.status, ProverStatus::Proved, "warm path: {}", r2.raw_output);
        assert_eq!(kb.layer.bg_snapshots.len(), after_first,
            "identical repeat must hit, not re-freeze");

        // Session mutation reshapes the key; the new ask must reflect
        // the new fact (a stale hit would not know Fido).
        assert!(kb.tell("(instance Fido Dog)", "h").ok);
        let r3 = kb.ask_query("(attribute Fido Loyal)", Some("h"),
            SineParams::default(), fast());
        assert_eq!(r3.status, ProverStatus::Proved, "post-tell: {}", r3.raw_output);
        assert!(kb.layer.bg_snapshots.len() > after_first,
            "changed session/conjecture must be a fresh freeze");
    }

    // A negated EXISTENTIAL conjecture clausifies to exactly the
    // symmetry-rule shape (`∃x y. R(x,y) ∧ ¬R(y,x)` negates to
    // `¬R(x,y) ∨ R(y,x)`).  The CONJECTURE-tier guard must keep it out
    // of the schema channel — absorbing it would erase the goal.  Here
    // nothing entails the existential, so the honest answer is
    // anything but Proved; the run must also not panic or hang.
    #[test]
    fn negated_existential_conjecture_is_never_absorbed() {
        let mut kb = kb_from("(instance likes BinaryPredicate)");
        assert!(kb.tell("(likes Bob Alice)", "h").ok);
        assert!(kb.tell("(likes Alice Bob)", "h").ok);
        let res = kb.ask_query(
            "(exists (?X ?Y) (and (likes ?X ?Y) (not (likes ?Y ?X))))",
            Some("h"), SineParams::default(), fast());
        assert_ne!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    // -- strategy / portfolio seam ----------------------------------------

    // The strategy rides NativeOpts into the loop: with the schema
    // channel off, the same symmetry rule is never mined (the raw
    // stats line says so) — and the goal still proves by ordinary
    // resolution against the un-absorbed rule.
    #[test]
    fn strategy_schema_knob_reaches_the_loop() {
        let kif = "(=> (touches ?X ?Y) (touches ?Y ?X))";
        let q = "(touches B A)";

        let mut kb_on = kb_from(kif);
        assert!(kb_on.tell("(touches A B)", "h").ok);
        let on = kb_on.ask_query(q, Some("h"), SineParams::default(), fast());
        assert_eq!(on.status, ProverStatus::Proved, "schema on: {}", on.raw_output);
        assert!(!on.raw_output.contains("mined 0 sym"),
            "schema on must mine the symmetry rule: {}", on.raw_output);

        let mut kb_off = kb_from(kif);
        assert!(kb_off.tell("(touches A B)", "h").ok);
        let mut opts = fast();
        opts.strategy = crate::saturate::strategy::Strategy::base();
        opts.strategy.schema = false;
        let off = kb_off.ask_query(q, Some("h"), SineParams::default(), opts);
        assert_eq!(off.status, ProverStatus::Proved, "schema off: {}", off.raw_output);
        assert!(off.raw_output.contains("mined 0 sym"),
            "schema off must mine nothing: {}", off.raw_output);
    }

    // The portfolio seam end to end: `ask_native` is `&self`, so
    // differently-configured lanes run CONCURRENTLY against one shared
    // KB (scoped threads — requires KnowledgeBase<ProverLayer>: Sync).
    #[test]
    fn portfolio_lanes_share_one_kb_across_threads() {
        use crate::saturate::strategy::Strategy;

        let mut kb = kb_from("(=> (instance ?X Dog) (attribute ?X Loyal))");
        assert!(kb.tell("(instance Rex Dog)", "h").ok);
        let kb = kb; // freeze: lanes borrow immutably

        let lanes = Strategy::default_portfolio();
        let results: Vec<(String, ProverStatus)> = std::thread::scope(|s| {
            let handles: Vec<_> = lanes.into_iter().map(|strat| {
                let kb = &kb;
                s.spawn(move || {
                    let name = strat.name.clone();
                    let opts = NativeOpts { strategy: strat, ..fast() };
                    let r = kb.ask_query("(attribute Rex Loyal)", Some("h"),
                        SineParams::default(), opts);
                    (name, r.status)
                })
            }).collect();
            handles.into_iter().map(|h| h.join().expect("lane panicked")).collect()
        });
        for (name, status) in results {
            assert_eq!(status, ProverStatus::Proved, "lane {name} failed");
        }
    }

    // The cooperative cancel flag: a pre-raised flag stops the run at
    // the first loop check (Timeout verdict), the portfolio runner's
    // kill-the-losers mechanism.
    #[test]
    fn cancel_flag_stops_the_run() {
        use std::sync::{Arc, atomic::AtomicBool};

        let mut kb = kb_from("(=> (instance ?X Dog) (attribute ?X Loyal))");
        assert!(kb.tell("(instance Rex Dog)", "h").ok);
        let cancel = Arc::new(AtomicBool::new(true));
        let opts = NativeOpts {
            cancel: Some(cancel),
            // Autoscale would retry the cancelled run; single shot.
            ..fast()
        };
        let res = kb.ask_query("(attribute Rex Loyal)", Some("h"),
            SineParams { autoscale: false, ..SineParams::default() }, opts);
        assert_eq!(res.status, ProverStatus::Timeout,
            "pre-raised cancel must stop the loop: {}", res.raw_output);
    }
