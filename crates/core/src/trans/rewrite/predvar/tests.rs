use super::reduce::{reduce_root, ReduceSymbols};
use super::*;
use crate::semantics::types::Scope;
use crate::semantics::SemanticLayer;
use crate::syntactic::{sentence_to_plain_kif, SyntacticLayer};
use crate::trans::TranslationLayer;

const DOMAIN_SCHEMA: &str = "(=> (and (domain ?REL ?N ?C) (instance ?REL Predicate) (?REL ?X ?Y)) \
     (instance (ListOrderFn (ListFn ?X ?Y) ?N) ?C))";
const DOMAIN_SUBCLASS_SCHEMA: &str =
    "(=> (and (domainSubclass ?REL ?N ?C) (instance ?REL Predicate) (?REL ?X ?Y)) \
     (subclass (ListOrderFn (ListFn ?X ?Y) ?N) ?C))";
const RANGE_SCHEMA: &str =
    "(=> (and (range ?F ?C) (equal (AssignmentFn ?F ?X ?Y) ?V)) (instance ?V ?C))";
const RANGE_SUBCLASS_SCHEMA: &str =
    "(=> (and (rangeSubclass ?F ?C) (equal (AssignmentFn ?F ?X ?Y) ?V)) (subclass ?V ?C))";
const TRANSITIVITY_SCHEMA: &str = "(=> (instance ?REL TransitiveRelation) \
     (forall (?I1 ?I2 ?I3) (=> (and (?REL ?I1 ?I2) (?REL ?I2 ?I3)) (?REL ?I1 ?I3))))";

fn make_trans(kif: &str) -> TranslationLayer {
    let mut syntactic = SyntacticLayer::default();
    let errors = syntactic.load_kif(kif, "signature-schema-test");
    assert!(errors.is_empty(), "load errors: {errors:?}");
    TranslationLayer::new(SemanticLayer::new(syntactic))
}

fn schemas(trans: &TranslationLayer) -> Vec<PredVarSchema> {
    detect_predvar_schemas(
        &trans.semantic,
        &trans.semantic.syntactic.normal_implications(),
    )
}

fn rendered(trans: &TranslationLayer, sid: SentenceId) -> String {
    sentence_to_plain_kif(sid, &trans.semantic.syntactic)
}

fn instantiated(trans: &TranslationLayer) -> Vec<String> {
    let roots = trans.semantic.syntactic.root_sids();
    trans
        .instantiate_predvars(&roots, &roots, Scope::Base)
        .into_iter()
        .map(|sid| rendered(trans, sid))
        .collect()
}

fn joined(kif: &[&str]) -> String {
    kif.join("\n")
}

#[test]
fn detects_all_four_signature_schemas_and_rejects_bad_shapes() {
    let trans = make_trans(&joined(&[
        DOMAIN_SCHEMA,
        DOMAIN_SUBCLASS_SCHEMA,
        RANGE_SCHEMA,
        RANGE_SUBCLASS_SCHEMA,
        // A signature atom with a constant argument is a body atom, not a guard.
        "(=> (and (instance ?REL Predicate) (domain ?REL 1 ?C) (?REL ?X)) (instance ?X ?C))",
        // The applied function variable is not the guarded one.
        "(=> (and (range ?F List) (equal (AssignmentFn ?G ?X) ?V)) (instance ?V List))",
        // A taxonomy guard over a variable the signature guard does not bind.
        "(=> (and (domain ?REL ?N ?C) (instance ?OTHER OtherClass) \
                  (instance ?REL Predicate) (?REL ?X)) \
             (instance (ListOrderFn (ListFn ?X) ?N) ?C))",
        // An `equal` atom that is not an assignment equality.
        "(=> (and (range ?F ?C) (equal ?X ?V)) (instance ?V ?C))",
    ]));
    let found = schemas(&trans);
    assert_eq!(
        found.len(),
        4,
        "detected: {}",
        found
            .iter()
            .map(|schema| rendered(&trans, schema.schema_sid))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(found.iter().filter(|s| s.pred_vars.len() == 1).count(), 2);
    assert_eq!(found.iter().filter(|s| s.pred_vars.is_empty()).count(), 2);
    for schema in &found {
        assert_eq!(
            schema.body_arity,
            Some(2),
            "{}",
            rendered(&trans, schema.schema_sid)
        );
    }
}

#[test]
fn quantified_variables_do_not_count_as_predicate_variables() {
    let trans = make_trans(TRANSITIVITY_SCHEMA);
    let found = schemas(&trans);
    assert_eq!(
        found.len(),
        1,
        "a forall-bodied property schema must still be detected"
    );
    assert_eq!(found[0].pred_vars.len(), 1);
    assert_eq!(found[0].bound_vars, found[0].pred_vars);
    assert_eq!(found[0].body_arity, Some(2));
}

#[test]
fn taxonomy_and_signature_schemas_instantiate_side_by_side() {
    let trans = make_trans(&joined(&[
        "(instance part BinaryPredicate)",
        "(instance part TransitiveRelation)",
        "(subclass BinaryPredicate Predicate)",
        "(domain part 1 Object)",
        "(domain part 2 Object)",
        TRANSITIVITY_SCHEMA,
        DOMAIN_SCHEMA,
        "(part A B)",
    ]));
    let rules = instantiated(&trans);
    assert!(
        rules
            .iter()
            .any(|r| r.contains("(part ?I1 ?I2)") && r.contains("(part ?I1 ?I3)")),
        "transitivity rule missing: {rules:?}"
    );
    assert!(
        rules
            .iter()
            .any(|r| r.contains("(part ?X ?Y)") && r.contains("(instance ?X Object)")),
        "domain rule missing: {rules:?}"
    );
    assert!(
        rules
            .iter()
            .any(|r| r.contains("(part ?X ?Y)") && r.contains("(instance ?Y Object)")),
        "domain rule for position 2 missing: {rules:?}"
    );
    assert!(
        rules
            .iter()
            .all(|r| !r.contains("ListOrderFn") && !r.contains("(domain ")),
        "unreduced term or undischarged guard leaked: {rules:?}"
    );
}

#[test]
fn signature_bindings_instantiate_domain_and_range_rules() {
    let trans = make_trans(&joined(&[
        "(instance part Predicate)",
        "(domain part 1 Object)",
        "(domain part 2 Object)",
        "(instance ListFn Function)",
        "(range ListFn List)",
        DOMAIN_SCHEMA,
        RANGE_SCHEMA,
        DOMAIN_SUBCLASS_SCHEMA,
        RANGE_SUBCLASS_SCHEMA,
        "(part A B)",
        "(instance child Predicate)",
        "(subrelation child part)",
        "(child A B)",
        "(instance ternary Predicate)",
        "(instance ternary TernaryRelation)",
        "(subclass TernaryRelation Relation)",
        "(domain ternary 1 Object)",
        "(ternary A B C)",
        "(instance subpart Predicate)",
        "(domainSubclass subpart 1 Class)",
        "(instance SubListFn Function)",
        "(rangeSubclass SubListFn SubClass)",
        "(subpart A B)",
        "(equal (ListFn A B) C)",
        "(equal (SubListFn A B) D)",
    ]));
    let rules = instantiated(&trans);
    let has = |a: &str, b: &str| rules.iter().any(|r| r.contains(a) && r.contains(b));
    assert!(has("(part ?X ?Y)", "(instance ?X Object)"), "{rules:?}");
    assert!(has("(part ?X ?Y)", "(instance ?Y Object)"), "{rules:?}");
    assert!(
        has("(child ?X ?Y)", "(instance ?X Object)"),
        "inherited signature missing: {rules:?}"
    );
    assert!(
        !rules.iter().any(|r| r.contains("(ternary ?X ?Y ?Z)")),
        "wrong-arity schema variant was instantiated: {rules:?}"
    );
    assert!(
        has("(equal (ListFn ?X ?Y) ?V)", "(instance ?V List)"),
        "{rules:?}"
    );
    assert!(has("(subpart ?X ?Y)", "(subclass ?X Class)"), "{rules:?}");
    assert!(
        has("(equal (SubListFn ?X ?Y) ?V)", "(subclass ?V SubClass)"),
        "{rules:?}"
    );
}

#[test]
fn unreducible_list_index_drops_the_rule() {
    // The row variant applies `?REL` to one argument but the list term
    // asks for position 2, so reduction cannot resolve it.
    let trans = make_trans(&joined(&[
        "(instance unary Predicate)",
        "(instance unary UnaryPredicate)",
        "(domain unary 2 Object)",
        "(=> (and (domain ?REL ?N ?C) (instance ?REL Predicate) (?REL ?X)) \
             (instance (ListOrderFn (ListFn ?X) ?N) ?C))",
        "(unary A)",
    ]));
    let rules = instantiated(&trans);
    assert!(
        rules.is_empty(),
        "rule with an unresolved list term emitted: {rules:?}"
    );
}

#[test]
fn reducer_rewrites_definitions_and_leaves_unchanged_roots_alone() {
    let mut syntactic = SyntacticLayer::default();
    let errors = syntactic.load_kif(
        &joined(&[
            "(equal (ListOrderFn (ListFn a b c) 2) ?V)",
            "(equal (ListOrderFn (ListFn a b) 4) ?V)",
            "(equal (AssignmentFn ListFn a b) ?V)",
            "(equal (AssignmentFn ?F a b) ?V)",
        ]),
        "term-reducer-test",
    );
    assert!(errors.is_empty(), "load errors: {errors:?}");
    let syms = ReduceSymbols::from_consts();
    let roots = syntactic.root_sids();
    let mut unchanged = 0;
    let reduced: Vec<String> = roots
        .iter()
        .map(|&sid| {
            let out = reduce_root(&syntactic, sid, sid, &syms);
            if out == sid {
                unchanged += 1;
            }
            sentence_to_plain_kif(out, &syntactic)
        })
        .collect();
    assert!(
        reduced.iter().any(|s| s.contains("(equal b ?V)")),
        "{reduced:?}"
    );
    assert!(
        reduced
            .iter()
            .any(|s| s.contains("(ListOrderFn (ListFn a b) 4)")),
        "{reduced:?}"
    );
    assert!(
        reduced
            .iter()
            .any(|s| s.contains("(equal (ListFn a b) ?V)")),
        "{reduced:?}"
    );
    assert!(
        reduced.iter().any(|s| s.contains("(AssignmentFn ?F a b)")),
        "an unbound function variable must not be applied: {reduced:?}"
    );
    assert_eq!(
        unchanged, 2,
        "the two irreducible roots must come back as themselves"
    );
}
