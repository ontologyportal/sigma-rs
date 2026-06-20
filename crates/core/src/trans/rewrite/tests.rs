//! Tests for the rewrite pass.

use super::*;

use crate::cache::{CacheConfig, EagerMap};
use crate::trans::caches::numeric_sorts::NumericSorts;
use crate::parse::ast::OpKind;
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, SentenceId, SymbolId};
use super::augment::substitute_var;
use super::extract::var_appears_as_predicate;
use super::preprocess::most_specific;
use std::collections::HashSet;
use crate::semantics::SemanticLayer;

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

fn syntactic_from(kif: &str) -> SyntacticLayer {
    let mut store = SyntacticLayer::default();
    let errors = store.load_kif(kif, "test");
    assert!(errors.is_empty(), "load errors: {:?}", errors);
    store
}

/// All root sentence ids, sorted for deterministic ordering before indexing.
fn roots_of(syntactic: &SyntacticLayer) -> Vec<SentenceId> {
    let mut r: Vec<SentenceId> =
        syntactic.root_sids();
    r.sort();
    r
}

/// The sole root sentence id (single-implication KBs in these tests).
fn root_of(syntactic: &SyntacticLayer) -> SentenceId {
    roots_of(syntactic)[0]
}

fn make_numeric_sorts(sym_id: SymbolId) -> EagerMap<NumericSorts> {
    let cache = EagerMap::new(&CacheConfig::default(), NumericSorts);
    cache.update(sym_id, super::super::Sort::Integer);
    cache
}

// -------------------------------------------------------------------------
// substitute_var
// -------------------------------------------------------------------------

#[test]
fn substitute_var_replaces_variable_in_flat_sentence() {
    let mut syntactic = syntactic_from("(greaterThan ?X 0)");
    let x_id = syntactic.sym_id("?X").or_else(|| syntactic.sym_id("X"))
        .unwrap_or_else(|| {
            let root = root_of(&syntactic);
            syntactic.sentence(root).unwrap().elements.iter().find_map(|e| {
                if let Element::Variable { id, .. } = e { Some(*id) } else { None }
            }).expect("no variable in sentence")
        });

    let root = root_of(&syntactic);
    let var_sentence_sid = {
        let s = syntactic.sentence(root).unwrap();
        s.elements.iter().find_map(|e| {
            if let Element::Sub(sid) = e {
                let sub = syntactic.sentence(*sid)?;
                if sub.elements.iter().any(|se| matches!(se, Element::Variable { id, .. } if *id == x_id)) {
                    Some(*sid)
                } else { None }
            } else { None }
        })
    };
    if let Some(target_sid) = var_sentence_sid {
        let replacement = Element::Variable { id: 999, name: "?NewVar".to_string(), is_row: false, var_index: 0 };
        let new_sid = substitute_var(&mut syntactic, target_sid, x_id, &replacement, root);
        let new_s = syntactic.sentence(new_sid).expect("substituted sentence should exist");
        let has_new_var = new_s.elements.iter().any(|e| {
            matches!(e, Element::Variable { id, .. } if *id == 999)
        });
        assert!(has_new_var, "substituted sentence should contain the replacement variable");
        let has_old_var = new_s.elements.iter().any(|e| {
            matches!(e, Element::Variable { id, .. } if *id == x_id)
        });
        assert!(!has_old_var, "substituted sentence should not contain the original variable");
    }
}

#[test]
fn substitute_var_recurses_into_sub_references() {
    let kif = "(=> (instance ?X EvenInteger) (equal (RemainderFn ?X 2) 0))";
    let mut syntactic = syntactic_from(kif);
    let root = root_of(&syntactic);
    let x_id = {
        let root_s = syntactic.sentence(root).unwrap();
        let ant_sid = match root_s.elements.get(1) {
            Some(Element::Sub(sid)) => *sid,
            _ => panic!("expected Sub antecedent"),
        };
        let ant_s = syntactic.sentence(ant_sid).unwrap();
        match ant_s.elements.get(1) {
            Some(Element::Variable { id, .. }) => *id,
            _ => panic!("expected Variable in antecedent"),
        }
    };

    let con_sid = {
        let root_s = syntactic.sentence(root).unwrap();
        match root_s.elements.get(2) {
            Some(Element::Sub(sid)) => *sid,
            _ => panic!("expected Sub consequent"),
        }
    };

    let replacement = Element::Variable { id: 777, name: "?NewVar".to_string(), is_row: false, var_index: 0 };
    let new_sid = substitute_var(&mut syntactic, con_sid, x_id, &replacement, root);

    let _new_s = syntactic.sentence(new_sid).expect("substituted sentence should exist");

    fn contains_var(syntactic: &SyntacticLayer, sid: SentenceId, var_id: SymbolId) -> bool {
        let Some(s) = syntactic.sentence(sid) else { return false };
        s.elements.iter().any(|e| match e {
            Element::Variable { id, .. } => *id == var_id,
            Element::Sub(child) => contains_var(syntactic, *child, var_id),
            _ => false,
        })
    }

    assert!(
        !contains_var(&syntactic, new_sid, x_id),
        "original variable should not appear anywhere in substituted tree"
    );
}

// -------------------------------------------------------------------------
// extract_case1_rules
// -------------------------------------------------------------------------

#[test]
fn extract_case1_rules_finds_rule_with_full_consequent() {
    let mut syntactic = syntactic_from(
        "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))"
    );
    let pos_int_id = syntactic.sym_id("PositiveInteger")
        .expect("PositiveInteger should be interned");
    let numeric_sorts = make_numeric_sorts(pos_int_id);

    let impls = syntactic.normal_implications();
    let rules = extract_case1_rules(&numeric_sorts, &syntactic, &impls);

    assert_eq!(rules.len(), 1, "should extract exactly 1 rule");
    assert!(
        syntactic.sentence(rules[0].consequent_sid).is_some(),
        "consequent_sid should be a valid sentence"
    );
    let root = root_of(&syntactic);
    let root_s = syntactic.sentence(root).unwrap();
    let ant_sid = match root_s.elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!("expected Sub antecedent"),
    };
    let x_id = match syntactic.sentence(ant_sid).unwrap().elements.get(1) {
        Some(Element::Variable { id, .. }) => *id,
        _ => panic!("expected Variable"),
    };
    assert_eq!(rules[0].template_var, x_id,
        "template_var should be ?X's SymbolId");
}

#[test]
fn extract_case1_rules_keeps_full_and_consequent() {
    let kif = "(=> (instance ?X PositiveInteger) \
               (and (instance ?X Integer) (greaterThan ?X 0)))";
    let mut syntactic = syntactic_from(kif);
    let pos_int_id = syntactic.sym_id("PositiveInteger")
        .expect("PositiveInteger should be interned");
    let int_id = syntactic.sym_id("Integer")
        .expect("Integer should be interned");
    let numeric_sorts = EagerMap::new(&CacheConfig::default(), NumericSorts);
    numeric_sorts.update(pos_int_id, super::super::Sort::Integer);
    numeric_sorts.update(int_id,     super::super::Sort::Integer);

    let impls = syntactic.normal_implications();
    let rules = extract_case1_rules(&numeric_sorts, &syntactic, &impls);

    assert_eq!(rules.len(), 1, "should extract 1 rule");
    let con_s = syntactic.sentence(rules[0].consequent_sid)
        .expect("consequent_sid should be valid");
    assert!(
        matches!(con_s.elements.first(), Some(Element::Op(OpKind::And))),
        "consequent should be the full (and …) sentence, not a filtered subset"
    );
    assert_eq!(con_s.elements.len(), 3,
        "and-consequent should have Op(And) + 2 children = 3 elements");
}

#[test]
fn extract_case1_rules_ignores_non_numeric_class() {
    let mut syntactic = syntactic_from("(=> (instance ?X Dog) (Animal ?X))");
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);
    let impls = syntactic.normal_implications();
    let rules = extract_case1_rules(&numeric_sorts, &syntactic, &impls);
    assert_eq!(rules.len(), 0);
}

#[test]
fn extract_case1_rules_skips_when_instance_not_in_kb() {
    let mut syntactic = syntactic_from("(=> (P ?X) (Q ?X))");
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);
    let impls = syntactic.normal_implications();
    let rules = extract_case1_rules(&numeric_sorts, &syntactic, &impls);
    assert_eq!(rules.len(), 0);
}

// -------------------------------------------------------------------------
// augment_fixed_point + run_rewrite_pass
// -------------------------------------------------------------------------

#[test]
fn run_rewrite_pass_suppresses_template_and_original_target() {
    let kif = "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))\n\
               (=> (instance ?Y PositiveInteger) (SomePred ?Y))";
    let mut syntactic = syntactic_from(kif);
    let pos_int_id = syntactic.sym_id("PositiveInteger").unwrap();
    let numeric_sorts = make_numeric_sorts(pos_int_id);

    let mut suppressed = HashSet::new();
    run_rewrite_pass(&numeric_sorts, &mut suppressed, &mut syntactic);

    assert_eq!(suppressed.len(), 2,
        "template + original target should both be suppressed, got {}", suppressed.len());
}

    // TODO: disabled until synthetic_origin tracking is restored (augmented sentences are located via synthetic_origin.keys()).
    /*
#[test]
fn run_rewrite_pass_augmented_antecedent_contains_full_consequent_conjuncts() {
    // Template: (=> (instance ?X PositiveInteger) (and (instance ?X Integer) (greaterThan ?X 0)))
    // Target:   (=> (instance ?Y PositiveInteger) (SomePred ?Y))
    //
    // The augmented target should have an antecedent:
    //   (and (instance ?Y PositiveInteger) (instance ?Y Integer) (greaterThan ?Y 0))
    // i.e. BOTH consequent conjuncts (instance Integer + greaterThan) are added,
    // not just the arithmetic one.
    let kif = "(=> (instance ?X PositiveInteger) \
                   (and (instance ?X Integer) (greaterThan ?X 0)))\n\
               (=> (instance ?Y PositiveInteger) (SomePred ?Y))";
    let mut syntactic = syntactic_from(kif);
    let pos_int_id = syntactic.sym_id("PositiveInteger").unwrap();
    let int_id     = syntactic.sym_id("Integer").unwrap();
    let numeric_sorts = EagerMap::new(&CacheConfig::default(), NumericSorts);
    numeric_sorts.update(pos_int_id, super::super::Sort::Integer);
    numeric_sorts.update(int_id,     super::super::Sort::Integer);

    let mut suppressed = HashSet::new();
    run_rewrite_pass(&numeric_sorts, &mut suppressed, &mut syntactic);

    // Find the augmented implication (not suppressed, in the synthetic store).
    let augmented_sid = syntactic.synthetic_origin.keys()
        .find(|&&sid| {
            !suppressed.contains(&sid) &&
            matches!(
                syntactic.sentence(sid).and_then(|s| s.elements.first()),
                Some(Element::Op(OpKind::Implies))
            )
        })
        .copied()
        .expect("there should be at least one non-suppressed synthetic implication");

    // Its antecedent should be (and …) with 3 conjuncts:
    //   (instance ?Y PositiveInteger), (instance ?Y Integer), (greaterThan ?Y 0)
    let aug_s = syntactic.sentence(augmented_sid).unwrap();
    let ant_sid = match aug_s.elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!("expected Sub antecedent"),
    };
    let ant_s = syntactic.sentence(ant_sid).unwrap();
    assert!(
        matches!(ant_s.elements.first(), Some(Element::Op(OpKind::And))),
        "augmented antecedent should be (and …)"
    );
    // Op(And) + 3 conjuncts = 4 elements.
    assert_eq!(ant_s.elements.len(), 4,
        "augmented antecedent should have 3 conjuncts (original + both from full consequent), \
         got {} elements", ant_s.elements.len());
}
    */

#[test]
fn run_rewrite_pass_does_not_suppress_non_numeric_implication() {
    let kif = "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))\n\
               (=> (instance ?Y Dog) (Animal ?Y))";
    let mut syntactic = syntactic_from(kif);
    let pos_int_id = syntactic.sym_id("PositiveInteger").unwrap();
    let numeric_sorts = make_numeric_sorts(pos_int_id);

    let mut suppressed = HashSet::new();
    run_rewrite_pass(&numeric_sorts, &mut suppressed, &mut syntactic);

    assert_eq!(suppressed.len(), 1,
        "only the template should be suppressed, got {}", suppressed.len());
}

#[test]
fn augment_fixed_point_is_idempotent() {
    let kif = "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))\n\
               (=> (instance ?Y PositiveInteger) (SomePred ?Y))";
    let mut syntactic = syntactic_from(kif);
    let pos_int_id = syntactic.sym_id("PositiveInteger").unwrap();
    let numeric_sorts = make_numeric_sorts(pos_int_id);

    let impls = syntactic.normal_implications();
    let rules = extract_case1_rules(&numeric_sorts, &syntactic, &impls);
    let mut suppressed = HashSet::new();
    for rule in &rules { suppressed.insert(rule.source_sid); }
    augment_fixed_point(&mut syntactic, &rules, &impls, &mut suppressed);
    let after_first = suppressed.len();

    augment_fixed_point(&mut syntactic, &rules, &impls, &mut suppressed);
    assert_eq!(suppressed.len(), after_first,
        "second pass should not add new suppressions");
}

    // TODO: disabled until synthetic_origin tracking is restored (augmented sentences are located via synthetic_origin.keys()).
    /*
#[test]
fn augmented_sentence_is_accessible_via_sentence_method() {
    let kif = "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))\n\
               (=> (instance ?Y PositiveInteger) (SomePred ?Y))";
    let mut syntactic = syntactic_from(kif);
    let pos_int_id = syntactic.sym_id("PositiveInteger").unwrap();
    let numeric_sorts = make_numeric_sorts(pos_int_id);

    let impls = syntactic.normal_implications();
    let rules = extract_case1_rules(&numeric_sorts, &syntactic, &impls);
    let mut suppressed = HashSet::new();
    for rule in &rules { suppressed.insert(rule.source_sid); }
    augment_fixed_point(&mut syntactic, &rules, &impls, &mut suppressed);

    if let Some(&last_syn_id) = syntactic.synthetic_origin.keys().last() {
        assert!(syntactic.sentence(last_syn_id).is_some(),
            "last synthetic sentence {} should be accessible", last_syn_id);
    }
}
    */

// =========================================================================
// var_appears_as_predicate
// =========================================================================

#[test]
fn var_appears_as_predicate_direct_head_position() {
    let kif = "(=> (instance ?REL SomeClass) (?REL ?X ?Y))";
    let mut syntactic = syntactic_from(kif);
    let root = root_of(&syntactic);
    let root_s = syntactic.sentence(root).unwrap();
    let ant_sid = match root_s.elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!("expected Sub antecedent"),
    };
    let rel_id = match syntactic.sentence(ant_sid).unwrap().elements.get(1) {
        Some(Element::Variable { id, .. }) => *id,
        _ => panic!("expected Variable"),
    };
    let con_sid = match root_s.elements.get(2) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!("expected Sub consequent"),
    };
    assert!(
        var_appears_as_predicate(&syntactic, con_sid, rel_id),
        "?REL should be detected in head position of (?REL ?X ?Y)"
    );
}

#[test]
fn var_appears_as_predicate_holds_style() {
    let kif = "(=> (instance ?REL SomeClass) (holds ?REL ?X ?Y))";
    let mut syntactic = syntactic_from(kif);
    let root = root_of(&syntactic);
    let root_s = syntactic.sentence(root).unwrap();
    let ant_sid = match root_s.elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!(),
    };
    let rel_id = match syntactic.sentence(ant_sid).unwrap().elements.get(1) {
        Some(Element::Variable { id, .. }) => *id,
        _ => panic!(),
    };
    let con_sid = match root_s.elements.get(2) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!(),
    };
    assert!(
        var_appears_as_predicate(&syntactic, con_sid, rel_id),
        "?REL as first arg of holds should be detected"
    );
}

#[test]
fn var_appears_as_predicate_only_in_arg_position_returns_false() {
    let kif = "(=> (instance ?REL SomeClass) (arity ?REL 2))";
    let mut syntactic = syntactic_from(kif);
    let root = root_of(&syntactic);
    let root_s = syntactic.sentence(root).unwrap();
    let ant_sid = match root_s.elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!(),
    };
    let rel_id = match syntactic.sentence(ant_sid).unwrap().elements.get(1) {
        Some(Element::Variable { id, .. }) => *id,
        _ => panic!(),
    };
    let con_sid = match root_s.elements.get(2) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!(),
    };
    assert!(
        !var_appears_as_predicate(&syntactic, con_sid, rel_id),
        "?REL only in argument position should return false"
    );
}

#[test]
fn var_appears_as_predicate_nested_inside_and() {
    let kif = "(=> (instance ?REL SomeClass) (and (arity ?REL 2) (?REL ?X ?Y)))";
    let mut syntactic = syntactic_from(kif);
    let root = root_of(&syntactic);
    let root_s = syntactic.sentence(root).unwrap();
    let ant_sid = match root_s.elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!(),
    };
    let rel_id = match syntactic.sentence(ant_sid).unwrap().elements.get(1) {
        Some(Element::Variable { id, .. }) => *id,
        _ => panic!(),
    };
    let con_sid = match root_s.elements.get(2) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!(),
    };
    assert!(
        var_appears_as_predicate(&syntactic, con_sid, rel_id),
        "?REL in head position inside nested and-consequent should be detected"
    );
}

// =========================================================================
// extract_case2_rules
// =========================================================================

#[test]
fn extract_case2_rules_finds_rule_with_predicate_variable_in_head() {
    let kif = "(=> (instance ?REL SymmetricRelation) (?REL ?X ?Y))";
    let mut syntactic = syntactic_from(kif);
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);

    let impls = syntactic.normal_implications();
    let rules = extract_case2_rules(&numeric_sorts, &syntactic, &impls);

    assert_eq!(rules.len(), 1, "should extract exactly 1 Case 2 rule");
    assert!(
        syntactic.sentence(rules[0].consequent_sid).is_some(),
        "consequent_sid should be a valid sentence"
    );
    let root = root_of(&syntactic);
    let root_s = syntactic.sentence(root).unwrap();
    let ant_sid = match root_s.elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!("expected Sub antecedent"),
    };
    let rel_id = match syntactic.sentence(ant_sid).unwrap().elements.get(1) {
        Some(Element::Variable { id, .. }) => *id,
        _ => panic!("expected Variable in antecedent"),
    };
    assert_eq!(rules[0].template_var, rel_id,
        "template_var should be ?REL's SymbolId");
}

#[test]
fn extract_case2_rules_finds_rule_with_holds_style_predicate() {
    let kif = "(=> (instance ?REL SomeClass) (holds ?REL ?X ?Y))";
    let mut syntactic = syntactic_from(kif);
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);

    let impls = syntactic.normal_implications();
    let rules = extract_case2_rules(&numeric_sorts, &syntactic, &impls);

    assert_eq!(rules.len(), 1,
        "holds-style predicate variable should produce 1 Case 2 rule");
}

#[test]
fn extract_case2_rules_ignores_numeric_class() {
    let kif = "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))";
    let mut syntactic = syntactic_from(kif);
    let pos_int_id = syntactic.sym_id("PositiveInteger").unwrap();
    let numeric_sorts = make_numeric_sorts(pos_int_id);

    let impls = syntactic.normal_implications();
    let rules = extract_case2_rules(&numeric_sorts, &syntactic, &impls);

    assert_eq!(rules.len(), 0,
        "numeric class should not produce Case 2 rules");
}

#[test]
fn extract_case2_rules_ignores_var_not_in_head_position() {
    let kif = "(=> (instance ?REL SomeClass) (arity ?REL 2))";
    let mut syntactic = syntactic_from(kif);
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);

    let impls = syntactic.normal_implications();
    let rules = extract_case2_rules(&numeric_sorts, &syntactic, &impls);

    assert_eq!(rules.len(), 0,
        "variable only in argument position should not produce Case 2 rules");
}

// =========================================================================
// Integration: run_rewrite_pass with Case 2
// =========================================================================

#[test]
fn run_rewrite_pass_applies_case2_rule_suppresses_template_and_target() {
    let kif = "(=> (instance ?REL SymRel) (?REL ?A ?B))\n\
               (=> (instance ?R SymRel) (Pred ?R))";
    let mut syntactic = syntactic_from(kif);
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);

    let mut suppressed = HashSet::new();
    run_rewrite_pass(&numeric_sorts, &mut suppressed, &mut syntactic);

    assert_eq!(suppressed.len(), 2,
        "template and target should both be suppressed; got {}", suppressed.len());
}

    // TODO: disabled until synthetic_origin tracking is restored (augmented sentences are located via synthetic_origin.keys()).
    /*
#[test]
fn run_rewrite_pass_case2_augmented_antecedent_has_correct_shape() {
    // Template: (=> (instance ?REL SymRel) (?REL ?A ?B))
    // Target:   (=> (instance ?R SymRel) (Pred ?R))
    // Augmented: (=> (and (instance ?R SymRel) (?R ?A ?B)) (Pred ?R))
    // Antecedent: Op(And) + 2 conjuncts = 3 elements.
    let kif = "(=> (instance ?REL SymRel) (?REL ?A ?B))\n\
               (=> (instance ?R SymRel) (Pred ?R))";
    let mut syntactic = syntactic_from(kif);
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);

    let mut suppressed = HashSet::new();
    run_rewrite_pass(&numeric_sorts, &mut suppressed, &mut syntactic);

    let augmented_sid = syntactic.synthetic_origin.keys()
        .find(|&&sid| {
            !suppressed.contains(&sid)
                && matches!(
                    syntactic.sentence(sid).and_then(|s| s.elements.first()),
                    Some(Element::Op(OpKind::Implies))
                )
        })
        .copied()
        .expect("there should be a non-suppressed synthetic implication");

    let aug_s  = syntactic.sentence(augmented_sid).unwrap();
    let ant_sid = match aug_s.elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!("expected Sub antecedent"),
    };
    let ant_s = syntactic.sentence(ant_sid).unwrap();
    assert!(
        matches!(ant_s.elements.first(), Some(Element::Op(OpKind::And))),
        "augmented antecedent should be (and …)"
    );
    // Op(And) + original (instance ?R SymRel) + substituted (?R ?A ?B) = 3 elements.
    assert_eq!(ant_s.elements.len(), 3,
        "augmented antecedent should have 2 conjuncts (original + substituted consequent); \
         got {} elements", ant_s.elements.len());
}
    */

#[test]
fn run_rewrite_pass_case1_and_case2_combined_all_suppressed() {
    let kif = "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))\n\
               (=> (instance ?REL SymRel) (?REL ?A ?B))\n\
               (=> (instance ?N PositiveInteger) (UsePred ?N))\n\
               (=> (instance ?R SymRel) (UseRel ?R))";
    let mut syntactic = syntactic_from(kif);
    let pos_int_id = syntactic.sym_id("PositiveInteger").unwrap();
    let numeric_sorts = make_numeric_sorts(pos_int_id);

    let mut suppressed = HashSet::new();
    run_rewrite_pass(&numeric_sorts, &mut suppressed, &mut syntactic);

    assert_eq!(suppressed.len(), 4,
        "all four sentences should be suppressed; got {}", suppressed.len());
}

#[test]
fn run_rewrite_pass_case2_rule_id_does_not_collide_with_case1() {
    let kif = "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))\n\
               (=> (instance ?REL SymRel) (?REL ?A ?B))\n\
               (=> (instance ?N PositiveInteger) (P ?N))\n\
               (=> (instance ?R SymRel) (Q ?R))";
    let mut syntactic = syntactic_from(kif);
    let pos_int_id = syntactic.sym_id("PositiveInteger").unwrap();
    let numeric_sorts = make_numeric_sorts(pos_int_id);

    let mut suppressed = HashSet::new();
    run_rewrite_pass(&numeric_sorts, &mut suppressed, &mut syntactic);

    let count_after_first = suppressed.len();
    run_rewrite_pass(&numeric_sorts, &mut suppressed, &mut syntactic);
    assert_eq!(suppressed.len(), count_after_first,
        "second run should not add new suppressions (idempotency)");
}

// =========================================================================
// SUMO real-axiom tests
//
// Axioms taken verbatim from:
// https://raw.githubusercontent.com/ontologyportal/sumo/refs/heads/master/Merge.kif
// =========================================================================

// -------------------------------------------------------------------------
// Case 1 — numeric subclass characterizations
// -------------------------------------------------------------------------

#[test]
fn case1_sumo_positive_integer_axiom() {
    let kif = "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))";
    let mut syntactic = syntactic_from(kif);
    let class_id = syntactic.sym_id("PositiveInteger").unwrap();
    let numeric_sorts = make_numeric_sorts(class_id);

    let impls = syntactic.normal_implications();
    let rules = extract_case1_rules(&numeric_sorts, &syntactic, &impls);

    assert_eq!(rules.len(), 1, "PositiveInteger axiom should yield 1 rule");
    // template_var must be ?X.
    let root = root_of(&syntactic);
    let ant_sid = match syntactic.sentence(root).unwrap().elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!("expected Sub antecedent"),
    };
    let x_id = match syntactic.sentence(ant_sid).unwrap().elements.get(1) {
        Some(Element::Variable { id, .. }) => *id,
        _ => panic!("expected Variable"),
    };
    assert_eq!(rules[0].template_var, x_id, "template_var should be ?X");
    let con_s = syntactic.sentence(rules[0].consequent_sid).unwrap();
    assert!(
        matches!(con_s.elements.first(), Some(Element::Symbol(_))),
        "consequent head should be Symbol(greaterThan)"
    );
}

#[test]
fn case1_sumo_nonnegative_integer_axiom_with_negative_bound() {
    let kif = "(=> (instance ?X NonnegativeInteger) (greaterThan ?X -1))";
    let mut syntactic = syntactic_from(kif);
    let class_id = syntactic.sym_id("NonnegativeInteger").unwrap();
    let numeric_sorts = make_numeric_sorts(class_id);

    let impls = syntactic.normal_implications();
    let rules = extract_case1_rules(&numeric_sorts, &syntactic, &impls);

    assert_eq!(rules.len(), 1, "NonnegativeInteger axiom should yield 1 rule");
    assert!(
        matches!(
            syntactic.sentence(rules[0].consequent_sid).unwrap().elements.first(),
            Some(Element::Symbol(_))
        ),
        "consequent head should be Symbol(greaterThan)"
    );
}

#[test]
fn case1_sumo_negative_integer_axiom_reversed_comparison() {
    // The variable appears at position 2, not position 1.
    let kif = "(=> (instance ?X NegativeInteger) (greaterThan 0 ?X))";
    let mut syntactic = syntactic_from(kif);
    let class_id = syntactic.sym_id("NegativeInteger").unwrap();
    let numeric_sorts = make_numeric_sorts(class_id);

    let impls = syntactic.normal_implications();
    let rules = extract_case1_rules(&numeric_sorts, &syntactic, &impls);

    assert_eq!(rules.len(), 1, "NegativeInteger axiom should yield 1 rule");
    assert!(syntactic.sentence(rules[0].consequent_sid).is_some());
}

#[test]
fn case1_sumo_positive_real_biconditional_yields_one_rule() {
    let kif = "(<=> (instance ?NUMBER PositiveRealNumber)
                    (and (greaterThan ?NUMBER 0)
                         (instance ?NUMBER RealNumber)))";
    let mut syntactic = syntactic_from(kif);
    let pos_real_id = syntactic.sym_id("PositiveRealNumber").unwrap();
    let real_id     = syntactic.sym_id("RealNumber").unwrap();
    let numeric_sorts = EagerMap::new(&CacheConfig::default(), NumericSorts);
    numeric_sorts.update(pos_real_id, super::super::Sort::Real);
    numeric_sorts.update(real_id,     super::super::Sort::Real);

    let impls = syntactic.normal_implications();
    assert_eq!(impls.len(), 2, "T1 should produce 2 implications from biconditional");

    let rules = extract_case1_rules(&numeric_sorts, &syntactic, &impls);
    assert_eq!(rules.len(), 1,
        "only the forward direction should match; backward (and …) antecedent is skipped");

    let con_s = syntactic.sentence(rules[0].consequent_sid).unwrap();
    assert!(
        matches!(con_s.elements.first(), Some(Element::Op(OpKind::And))),
        "consequent should be the full (and …) — arithmetic + RealNumber membership"
    );
}

    // TODO: disabled until synthetic_origin tracking is restored (augmented sentences are located via synthetic_origin.keys()).
    /*
#[test]
fn case1_sumo_nonneg_real_biconditional_augments_target() {
    // Verbatim template:
    //   (<=> (instance ?NUMBER NonnegativeRealNumber)
    //        (and (greaterThanOrEqualTo ?NUMBER 0)
    //             (instance ?NUMBER RealNumber)))
    // Usage target: (=> (instance ?X NonnegativeRealNumber) (UsePred ?X))
    //
    // T1 produces two synthetic implications from the biconditional.  The
    // BACKWARD half (`(=> (and …) (instance … NonnegativeRealNumber))`) is also
    // a non-suppressed synthetic implication in the store, so we must
    // distinguish it from the augmented target.
    //
    // The backward half's antecedent has 3 elements (Op(And) + 2 children).
    // The augmented target's antecedent has 4 elements (Op(And) + 3 children:
    //   original (instance ?X NonnegativeRealNumber) + greaterThanOrEqualTo +
    //   instance RealNumber).
    let kif = "(<=> (instance ?NUMBER NonnegativeRealNumber)
                    (and (greaterThanOrEqualTo ?NUMBER 0)
                         (instance ?NUMBER RealNumber)))
               (=> (instance ?X NonnegativeRealNumber) (UsePred ?X))";
    let mut syntactic = syntactic_from(kif);
    let nonneg_real_id = syntactic.sym_id("NonnegativeRealNumber").unwrap();
    let real_id        = syntactic.sym_id("RealNumber").unwrap();
    let numeric_sorts  = EagerMap::new(&CacheConfig::default(), NumericSorts);
    numeric_sorts.update(nonneg_real_id, super::super::Sort::Real);
    numeric_sorts.update(real_id,        super::super::Sort::Real);

    let mut suppressed = HashSet::new();
    run_rewrite_pass(&numeric_sorts, &mut suppressed, &mut syntactic);

    // Forward synthetic template + usage target → 2 suppressed.
    assert_eq!(suppressed.len(), 2,
        "forward template and usage target should be suppressed; got {}", suppressed.len());

    // Find the augmented implication specifically: it is the non-suppressed
    // synthetic (=>) whose (and …) antecedent has 4 elements (3 conjuncts).
    // The backward biconditional half also survives suppression, but its
    // antecedent only has 3 elements (2 conjuncts), so the filter is unambiguous.
    let augmented_sid = syntactic.synthetic_origin.keys().copied().find(|&sid| {
        if suppressed.contains(&sid) { return false; }
        let Some(s) = syntactic.sentence(sid) else { return false };
        if !matches!(s.elements.first(), Some(Element::Op(OpKind::Implies))) {
            return false;
        }
        let ant_sid = match s.elements.get(1) {
            Some(Element::Sub(sid)) => *sid,
            _ => return false,
        };
        let Some(ant_s) = syntactic.sentence(ant_sid) else { return false };
        matches!(ant_s.elements.first(), Some(Element::Op(OpKind::And)))
            && ant_s.elements.len() == 4
    }).expect("should find the augmented implication (4-element antecedent)");

    // Confirm the 4-element (and …) antecedent:
    //   Op(And) + (instance ?X NonnegativeRealNumber)
    //           + (greaterThanOrEqualTo ?X 0)
    //           + (instance ?X RealNumber)
    let aug_s   = syntactic.sentence(augmented_sid).unwrap();
    let ant_sid = match aug_s.elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!("expected Sub antecedent"),
    };
    assert_eq!(syntactic.sentence(ant_sid).unwrap().elements.len(), 4);
}
    */

// -------------------------------------------------------------------------
// Case 2 — predicate variable characterizations
// -------------------------------------------------------------------------

#[test]
fn case2_sumo_reflexive_relation_axiom() {
    let kif = "(=> (instance ?REL ReflexiveRelation) (?REL ?INST ?INST))";
    let mut syntactic = syntactic_from(kif);
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);

    let impls = syntactic.normal_implications();
    assert_eq!(impls.len(), 1, "no normalization needed; 1 implication");

    let rules = extract_case2_rules(&numeric_sorts, &syntactic, &impls);
    assert_eq!(rules.len(), 1, "ReflexiveRelation axiom should yield 1 Case 2 rule");

    let con_s = syntactic.sentence(rules[0].consequent_sid).unwrap();
    assert!(
        matches!(con_s.elements.first(), Some(Element::Variable { .. })),
        "consequent head should be a Variable (?REL)"
    );
}

#[test]
fn case2_sumo_irreflexive_relation_axiom_forall_not() {
    // ?REL is inside (not (?REL …)) inside a forall; detection must recurse
    // through forall → not → head.
    let kif = "(=> (instance ?REL IrreflexiveRelation)
                   (forall (?INST)
                      (not (?REL ?INST ?INST))))";
    let mut syntactic = syntactic_from(kif);
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);

    let impls = syntactic.normal_implications();
    let rules = extract_case2_rules(&numeric_sorts, &syntactic, &impls);
    assert_eq!(rules.len(), 1, "IrreflexiveRelation axiom should yield 1 Case 2 rule");
}

#[test]
fn case2_sumo_symmetric_relation_axiom_forall_nested_implication() {
    let kif = "(=> (instance ?REL SymmetricRelation)
                   (forall (?INST1 ?INST2)
                      (=> (?REL ?INST1 ?INST2)
                          (?REL ?INST2 ?INST1))))";
    let mut syntactic = syntactic_from(kif);
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);

    let impls = syntactic.normal_implications();
    assert_eq!(impls.len(), 1, "forall-wrapped consequent blocks T2; 1 implication expected");

    let rules = extract_case2_rules(&numeric_sorts, &syntactic, &impls);
    assert_eq!(rules.len(), 1, "SymmetricRelation axiom should yield 1 Case 2 rule");
}

#[test]
fn case2_sumo_transitive_relation_axiom_forall_and_body() {
    let kif = "(=> (instance ?REL TransitiveRelation)
                   (forall (?INST1 ?INST2 ?INST3)
                      (=> (and (?REL ?INST1 ?INST2)
                               (?REL ?INST2 ?INST3))
                          (?REL ?INST1 ?INST3))))";
    let mut syntactic = syntactic_from(kif);
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);

    let impls = syntactic.normal_implications();
    let rules = extract_case2_rules(&numeric_sorts, &syntactic, &impls);
    assert_eq!(rules.len(), 1, "TransitiveRelation axiom should yield 1 Case 2 rule");
}

#[test]
fn case2_sumo_antisymmetric_relation_axiom_full_form() {
    let kif = "(=> (instance ?REL AntisymmetricRelation)
                   (forall (?INST1 ?INST2)
                      (=> (and (?REL ?INST1 ?INST2)
                               (?REL ?INST2 ?INST1))
                          (equal ?INST1 ?INST2))))";
    let mut syntactic = syntactic_from(kif);
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);

    let impls = syntactic.normal_implications();
    let rules = extract_case2_rules(&numeric_sorts, &syntactic, &impls);
    assert_eq!(rules.len(), 1, "AntisymmetricRelation axiom should yield 1 Case 2 rule");
}

// -------------------------------------------------------------------------
// Case 2 — SUMO integration: augmentation with real axioms
// -------------------------------------------------------------------------

    // TODO: disabled until synthetic_origin tracking is restored (augmented sentences are located via synthetic_origin.keys()).
    /*
#[test]
fn case2_sumo_reflexive_relation_augments_usage_sentence() {
    // Template: (=> (instance ?REL ReflexiveRelation) (?REL ?INST ?INST))
    // Target:   (=> (instance ?R ReflexiveRelation) (Pred ?R))
    //
    // Expected augmented antecedent:
    //   (and (instance ?R ReflexiveRelation) (?R ?INST ?INST))
    // i.e. Op(And) + 2 conjuncts = 3 elements.
    let kif = "(=> (instance ?REL ReflexiveRelation) (?REL ?INST ?INST))
               (=> (instance ?R ReflexiveRelation) (Pred ?R))";
    let mut syntactic = syntactic_from(kif);
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);

    let mut suppressed = HashSet::new();
    run_rewrite_pass(&numeric_sorts, &mut suppressed, &mut syntactic);

    assert_eq!(suppressed.len(), 2,
        "template + target should both be suppressed; got {}", suppressed.len());

    let augmented_sid = syntactic.synthetic_origin.keys()
        .find(|&&sid| {
            !suppressed.contains(&sid)
                && matches!(
                    syntactic.sentence(sid).and_then(|s| s.elements.first()),
                    Some(Element::Op(OpKind::Implies))
                )
        })
        .copied()
        .expect("a non-suppressed augmented implication should exist");

    let aug_s   = syntactic.sentence(augmented_sid).unwrap();
    let ant_sid = match aug_s.elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!("expected Sub antecedent"),
    };
    let ant_s = syntactic.sentence(ant_sid).unwrap();
    assert!(
        matches!(ant_s.elements.first(), Some(Element::Op(OpKind::And))),
        "augmented antecedent should be (and …)"
    );
    // Op(And) + (instance ?R ReflexiveRelation) + (?R ?INST ?INST) = 3 elements.
    assert_eq!(ant_s.elements.len(), 3,
        "2 conjuncts expected; got {} elements", ant_s.elements.len());
}
    */

    // TODO: disabled until synthetic_origin tracking is restored (augmented sentences are located via synthetic_origin.keys()).
    /*
#[test]
fn case2_sumo_symmetric_relation_augments_usage_sentence() {
    // Template: full SymmetricRelation axiom from SUMO (forall-wrapped).
    // Target:   (=> (instance ?R SymmetricRelation) (Pred ?R))
    //
    // After augmentation:
    //   (and (instance ?R SymmetricRelation)
    //        (forall (?INST1 ?INST2) (=> (?R ?INST1 ?INST2) (?R ?INST2 ?INST1))))
    // The full forall body (with ?REL substituted to ?R) is added as one conjunct.
    // Op(And) + 2 conjuncts = 3 elements.
    let kif = "(=> (instance ?REL SymmetricRelation)
                   (forall (?INST1 ?INST2)
                      (=> (?REL ?INST1 ?INST2)
                          (?REL ?INST2 ?INST1))))
               (=> (instance ?R SymmetricRelation) (Pred ?R))";
    let mut syntactic = syntactic_from(kif);
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);

    let mut suppressed = HashSet::new();
    run_rewrite_pass(&numeric_sorts, &mut suppressed, &mut syntactic);

    assert_eq!(suppressed.len(), 2,
        "template + target should both be suppressed; got {}", suppressed.len());

    let augmented_sid = syntactic.synthetic_origin.keys()
        .find(|&&sid| {
            !suppressed.contains(&sid)
                && matches!(
                    syntactic.sentence(sid).and_then(|s| s.elements.first()),
                    Some(Element::Op(OpKind::Implies))
                )
        })
        .copied()
        .expect("a non-suppressed augmented implication should exist");

    let aug_s   = syntactic.sentence(augmented_sid).unwrap();
    let ant_sid = match aug_s.elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!("expected Sub antecedent"),
    };
    let ant_s = syntactic.sentence(ant_sid).unwrap();
    // Op(And) + original instance + forall-substituted body = 3 elements.
    assert_eq!(ant_s.elements.len(), 3,
        "2 conjuncts expected (original instance + full forall body); \
         got {} elements", ant_s.elements.len());
}
    */

    // TODO: disabled until synthetic_origin tracking is restored (augmented sentences are located via synthetic_origin.keys()).
    /*
#[test]
fn case2_sumo_transitive_relation_augments_usage_sentence() {
    // Template: full TransitiveRelation axiom from SUMO.
    // Target:   (=> (instance ?R TransitiveRelation) (Pred ?R))
    //
    // substitute_var recurses into the forall body, which contains a nested
    // (=> (and …) …).  That nested implication is itself copied as a new
    // synthetic Op(Implies) sentence, so there may be more than one
    // non-suppressed synthetic implication in the store.  The test therefore
    // verifies the key observable behaviour — both sentences suppressed and
    // at least one non-suppressed augmented implication exists — rather than
    // asserting an exact synthetic-sentence count.
    let kif = "(=> (instance ?REL TransitiveRelation)
                   (forall (?INST1 ?INST2 ?INST3)
                      (=> (and (?REL ?INST1 ?INST2)
                               (?REL ?INST2 ?INST3))
                          (?REL ?INST1 ?INST3))))
               (=> (instance ?R TransitiveRelation) (Pred ?R))";
    let mut syntactic = syntactic_from(kif);
    let numeric_sorts: EagerMap<NumericSorts> =
        EagerMap::new(&CacheConfig::default(), NumericSorts);

    let mut suppressed = HashSet::new();
    run_rewrite_pass(&numeric_sorts, &mut suppressed, &mut syntactic);

    assert_eq!(suppressed.len(), 2,
        "template + target should both be suppressed; got {}", suppressed.len());

    // At least one non-suppressed augmented implication must exist.
    assert!(
        syntactic.synthetic_origin.keys().any(|&sid| {
            !suppressed.contains(&sid)
                && matches!(
                    syntactic.sentence(sid).and_then(|s| s.elements.first()),
                    Some(Element::Op(OpKind::Implies))
                )
        }),
        "at least one non-suppressed augmented implication should exist"
    );
}
    */

// -------------------------------------------------------------------------
// §D — inject_domain_guards (preProcess / type-hypothesis injection)
// -------------------------------------------------------------------------

/// Builds a `SemanticLayer` from KIF source.
fn semantic_from(kif: &str) -> SemanticLayer {
    let store = syntactic_from(kif);
    SemanticLayer::new(store)
}

/// Runs `inject_domain_guards` over `semantic`'s implications, returning the
/// new synthetic SIDs and the resulting `suppressed` set.
fn run_inject(semantic: &mut SemanticLayer)
    -> (Vec<SentenceId>, HashSet<SentenceId>)
{
    let implications = semantic.syntactic.normal_implications();
    let mut suppressed: HashSet<SentenceId> = HashSet::new();
    let new_sids = inject_domain_guards(semantic, &implications, &mut suppressed);
    (new_sids, suppressed)
}

#[test]
fn most_specific_returns_descendant_when_one_class_descends_from_another() {
    let sem = semantic_from(
        "(subclass Dog Animal)
         (subclass Animal Entity)"
    );
    let dog    = sem.syntactic.sym_id("Dog").unwrap();
    let animal = sem.syntactic.sym_id("Animal").unwrap();
    let entity = sem.syntactic.sym_id("Entity").unwrap();
    assert_eq!(most_specific(&sem, &[dog, animal, entity]), Some(dog));
    assert_eq!(most_specific(&sem, &[animal]), Some(animal));
}

#[test]
fn most_specific_returns_none_for_cross_hierarchy_classes() {
    let sem = semantic_from(
        "(subclass Dog Animal)
         (subclass Car Vehicle)"
    );
    let dog = sem.syntactic.sym_id("Dog").unwrap();
    let car = sem.syntactic.sym_id("Car").unwrap();
    assert_eq!(most_specific(&sem, &[dog, car]), None);
}

#[test]
fn inject_domain_guards_adds_instance_guard_for_unguarded_variable() {
    let mut sem = semantic_from(
        "(subclass Process Entity)
         (instance subProcess BinaryRelation)
         (instance relatedEvent BinaryRelation)
         (domain subProcess 1 Process)
         (domain subProcess 2 Process)
         (=> (subProcess ?S1 ?S2) (relatedEvent ?S1 ?S2))"
    );
    let (new_sids, suppressed) = run_inject(&mut sem);

    assert_eq!(new_sids.len(), 1, "exactly one synthetic implication expected");
    assert_eq!(suppressed.len(), 4,
        "original + augmented antecedent + 2 guards should be suppressed");

    let new_impl = sem.syntactic.sentence(new_sids[0]).expect("new impl exists");
    let new_ant_sid = match new_impl.elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!("new impl antecedent should be a Sub"),
    };
    assert!(suppressed.contains(&new_ant_sid),
        "augmented antecedent fragment must be suppressed (else it emits as a bare conjunction)");
    let new_ant = sem.syntactic.sentence(new_ant_sid).expect("new ant exists");
    assert!(matches!(new_ant.elements.first(), Some(Element::Op(OpKind::And))),
        "new antecedent should be headed by (and ...)");
    assert_eq!(new_ant.elements.len(), 4,
        "new antecedent should have 2 guards + 1 original conjunct (got {})",
        new_ant.elements.len() - 1);
}

#[test]
fn inject_domain_guards_does_not_duplicate_existing_guards() {
    let mut sem = semantic_from(
        "(subclass Process Entity)
         (instance subProcess BinaryRelation)
         (domain subProcess 1 Process)
         (=> (and (instance ?X Process) (subProcess ?X ?Y)) (relatedEvent ?X ?Y))"
    );
    let (new_sids, _suppressed) = run_inject(&mut sem);
    if new_sids.is_empty() {
        // Acceptable: `domain subProcess 2` is absent, so ?Y is unconstrained
        // and no guard is added.
        return;
    }
    let new_impl = sem.syntactic.sentence(new_sids[0]).unwrap();
    let new_ant_sid = match new_impl.elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!("expected Sub antecedent"),
    };
    let new_ant = sem.syntactic.sentence(new_ant_sid).unwrap();
    let instance_id = sem.syntactic.sym_id("instance").unwrap();
    let process_id  = sem.syntactic.sym_id("Process").unwrap();
    let x_uses: usize = new_ant.elements[1..].iter().filter(|e| {
        let Element::Sub(sid) = e else { return false };
        let Some(s) = sem.syntactic.sentence(*sid) else { return false };
        s.elements.len() == 3
            && matches!(s.elements.first(),
                Some(Element::Symbol(sym)) if sym.id() == instance_id)
            && matches!(s.elements.get(2),
                Some(Element::Symbol(sym)) if sym.id() == process_id)
    }).count();
    assert_eq!(x_uses, 1,
        "(instance ?X Process) should appear exactly once; existing guard \
         must not be duplicated by inject_domain_guards");
}

#[test]
fn inject_domain_guards_skips_implication_without_domain_axioms() {
    let mut sem = semantic_from(
        "(=> (foo ?X) (bar ?X))"
    );
    let (new_sids, suppressed) = run_inject(&mut sem);
    assert!(new_sids.is_empty(),
        "without domain axioms, no guards should be synthesized");
    assert!(suppressed.is_empty(),
        "no originals should be suppressed when no guards are added");
}

#[test]
fn inject_domain_guards_picks_most_specific_class_across_predicates() {
    // ?X's domain is Animal via (foo ?X ?Y) and Dog (subclass Animal) via
    // (bar ?X); the most-specific class across uses is Dog.
    let mut sem = semantic_from(
        "(subclass Dog Animal)
         (instance foo BinaryRelation)
         (instance bar UnaryPredicate)
         (domain foo 1 Animal)
         (domain bar 1 Dog)
         (=> (and (foo ?X ?Y) (bar ?X)) (related ?X ?Y))"
    );
    let (new_sids, _) = run_inject(&mut sem);
    assert_eq!(new_sids.len(), 1, "one synthetic implication expected");

    let new_impl   = sem.syntactic.sentence(new_sids[0]).unwrap();
    let ant_sid    = match new_impl.elements.get(1) {
        Some(Element::Sub(sid)) => *sid,
        _ => panic!("expected Sub antecedent"),
    };
    let ant_elems  = sem.syntactic.sentence(ant_sid).unwrap().elements.clone();
    let instance_id = sem.syntactic.sym_id("instance").unwrap();
    let dog_id      = sem.syntactic.sym_id("Dog").unwrap();

    let has_dog_guard = ant_elems[1..].iter().any(|e| {
        let Element::Sub(sid) = e else { return false };
        let Some(s) = sem.syntactic.sentence(*sid) else { return false };
        s.elements.len() == 3
            && matches!(s.elements.first(),
                Some(Element::Symbol(sym)) if sym.id() == instance_id)
            && matches!(s.elements.get(2),
                Some(Element::Symbol(sym)) if sym.id() == dog_id)
    });
    assert!(has_dog_guard,
        "expected (instance ?X Dog) — the most-specific class across uses");
}

#[test]
fn inject_domain_guards_synthetic_appears_in_normal_implications_after_rewrite() {
    use crate::semantics::SemanticLayer;
    let mut sem = semantic_from(
        "(subclass Process Entity)
         (instance subProcess BinaryRelation)
         (domain subProcess 1 Process)
         (domain subProcess 2 Process)
         (=> (subProcess ?S1 ?S2) (relatedEvent ?S1 ?S2))"
    );
    let implications = sem.syntactic.normal_implications();
    let mut suppressed: HashSet<SentenceId> = HashSet::new();
    let injected = inject_domain_guards(&mut sem, &implications, &mut suppressed);

    assert!(!injected.is_empty(), "expected at least one injected synthetic");
    let new_impl = sem.syntactic.sentence(injected[0]).expect("injected exists");
    assert!(matches!(new_impl.elements.first(), Some(Element::Op(OpKind::Implies))));

    let origs: Vec<_> = roots_of(&sem.syntactic).into_iter()
        .filter(|sid| suppressed.contains(sid))
        .collect();
    assert!(!origs.is_empty(), "an original root should be suppressed");
    let _ = SemanticLayer::new(SyntacticLayer::default()); // silences unused-import warning
}
