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
const CASE_ROLE_SCHEMA: &str = "(=> (and (?ROLE ?EVENT ?OBJ) (instance ?ROLE CaseRole) \
              (instance ?EVENT Process) (instance ?OBJ Object)) \
         (playsRoleInEvent ?OBJ ?ROLE ?EVENT))";
const DISJOINT_RELATION_SCHEMA: &str =
    "(=> (and (instance ?REL1 Predicate) (instance ?REL2 Predicate) \
              (disjointRelation ?REL1 ?REL2) (?REL1 ?ROW1 ?ROW2)) \
         (not (?REL2 ?ROW1 ?ROW2)))";
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
fn detects_all_four_signature_schemas_and_rejects_unbound_higher_order_vars() {
    let trans = make_trans(&joined(&[
        DOMAIN_SCHEMA,
        DOMAIN_SUBCLASS_SCHEMA,
        RANGE_SCHEMA,
        RANGE_SUBCLASS_SCHEMA,
        // The applied function variable is not the guarded one.
        "(=> (and (range ?F List) (equal (AssignmentFn ?G ?X) ?V)) (instance ?V List))",
        // No higher-order variable at all.
        "(=> (and (range ?F ?C) (equal ?X ?V)) (instance ?V ?C))",
        // A predicate variable only inside a quoted formula: no guard binds it.
        "(=> (and (holdsDuring ?T (?REL ?A ?B)) (instance ?A Physical)) (time ?A ?T))",
        // An existentially bound variable in head position is never bound.
        "(=> (instance ?C Car) (hasPurpose ?C (exists (?A ?X) (?A ?X ?C))))",
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
    assert_eq!(
        found.iter().filter(|s| s.function_vars.len() == 1).count(),
        2
    );
    for schema in &found {
        assert!(
            !schema.call_arity.is_empty() && schema.call_arity.values().all(|&a| a == 2),
            "{}",
            rendered(&trans, schema.schema_sid)
        );
    }
}

#[test]
fn body_atoms_and_fact_guards_no_longer_block_detection() {
    let trans = make_trans(&joined(&[
        // `instance` over an ordinary variable is a body atom.
        "(=> (and (domain ?REL ?N ?C) (instance ?OTHER OtherClass) \
                  (instance ?REL Predicate) (?REL ?X)) \
             (instance (ListOrderFn (ListFn ?X) ?N) ?C))",
        // A signature atom with a constant argument is a fact guard.
        "(=> (and (instance ?REL Predicate) (domain ?REL 1 ?C) (?REL ?X)) (instance ?X ?C))",
        CASE_ROLE_SCHEMA,
        DISJOINT_RELATION_SCHEMA,
    ]));
    let found = schemas(&trans);
    assert_eq!(found.len(), 4, "{}", found.len());
    let fact_guarded = found
        .iter()
        .filter(|s| s.guards.iter().any(|g| matches!(g, PvGuard::Fact { .. })))
        .count();
    assert_eq!(fact_guarded, 2);
    let case_role = found
        .iter()
        .find(|s| rendered(&trans, s.schema_sid).contains("playsRoleInEvent"))
        .expect("CaseRole schema detected");
    assert_eq!(
        case_role.bound_vars.len(),
        1,
        "only the role variable is guard-bound"
    );
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
    assert_eq!(found[0].call_arity.get(&found[0].pred_vars[0]), Some(&2));
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

#[test]
fn group_a_instance_guard_keeps_body_atoms_and_substitutes_arguments() {
    let trans = make_trans(&joined(&[
        "(instance agent CaseRole)",
        "(instance agent BinaryPredicate)",
        "(instance located BinaryPredicate)",
        CASE_ROLE_SCHEMA,
        "(agent Walking1 Bob)",
        "(located Bob Home)",
    ]));
    let rules = instantiated(&trans);
    assert_eq!(
        rules,
        vec![
            "(=> (and (agent ?EVENT ?OBJ) (instance ?EVENT Process) (instance ?OBJ Object)) \
             (playsRoleInEvent ?OBJ agent ?EVENT))"
                .to_string()
        ],
        "only `agent` is a CaseRole; the discharged guard is dropped"
    );
}

#[test]
fn group_b_fact_guard_binds_from_ground_facts() {
    let trans = make_trans(&joined(&[
        "(instance referee BinaryPredicate)",
        "(instance plays BinaryPredicate)",
        "(instance watches BinaryPredicate)",
        "(subclass BinaryPredicate Predicate)",
        "(disjointRelation referee plays)",
        DISJOINT_RELATION_SCHEMA,
        "(referee Ann Match1)",
        "(plays Bob Match1)",
        "(watches Cy Match1)",
    ]));
    let rules = instantiated(&trans);
    assert_eq!(
        rules,
        vec!["(=> (referee ?ROW1 ?ROW2) (not (plays ?ROW1 ?ROW2)))".to_string()],
        "only the asserted disjoint pair instantiates"
    );
}

#[test]
fn group_b_fact_guard_binds_ordinary_variables_and_matches_constants() {
    let trans = make_trans(&joined(&[
        "(instance patient CaseRole)",
        "(instance patient BinaryPredicate)",
        "(requiredRole Chewing patient Food)",
        "(=> (and (requiredRole ?P ?C ?E) (instance ?PI ?P)) \
             (exists (?EI) (and (instance ?EI ?E) (?C ?PI ?EI))))",
        "(instance economyType BinaryPredicate)",
        "(instance color BinaryPredicate)",
        "(subrelation economyType attribute)",
        "(=> (and (instance ?A1 Attribute) (subrelation ?PRED attribute)) \
             (or (not (contraryAttribute ?A1)) (not (?PRED ?E ?A1))))",
        "(patient Chew1 Apple)",
        "(economyType Freedonia Market)",
        "(color Apple Red)",
    ]));
    let rules = instantiated(&trans);
    assert!(
        rules.contains(
            &"(=> (instance ?PI Chewing) (exists (?EI) (and (instance ?EI Food) (patient ?PI ?EI))))"
                .to_string()
        ),
        "{rules:?}"
    );
    assert!(
        rules.contains(
            &"(=> (instance ?A1 Attribute) (or (not (contraryAttribute ?A1)) (not (economyType ?E ?A1))))"
                .to_string()
        ),
        "{rules:?}"
    );
    assert!(
        !rules.iter().any(|r| r.contains("(color ")),
        "a relation without the constant-matching fact was bound: {rules:?}"
    );
}

#[test]
fn group_b_fact_guard_skips_relations_outside_the_problem() {
    let trans = make_trans(&joined(&[
        "(instance referee BinaryPredicate)",
        "(instance plays BinaryPredicate)",
        "(subclass BinaryPredicate Predicate)",
        "(disjointRelation referee plays)",
        DISJOINT_RELATION_SCHEMA,
    ]));
    let syn = &trans.semantic.syntactic;
    let schema_only: Vec<SentenceId> = syn
        .root_sids()
        .into_iter()
        .filter(|&sid| !rendered(&trans, sid).contains("referee"))
        .collect();
    let rules = trans.instantiate_predvars(&schema_only, &schema_only, Scope::Base);
    assert!(
        rules.is_empty(),
        "a fact over relations absent from the problem instantiated: {:?}",
        rules
            .iter()
            .map(|&sid| rendered(&trans, sid))
            .collect::<Vec<_>>()
    );
}

#[test]
fn group_c_direct_function_application_is_a_higher_order_variable() {
    let trans = make_trans(&joined(&[
        "(instance KiloFn UnitOfMeasureMultiplier)",
        "(=> (and (instance ?FUNCTION UnitOfMeasureMultiplier) \
                  (instance ?UNIT CompositeUnitOfMeasure)) \
             (instance (?FUNCTION ?UNIT) CompositeUnitOfMeasure))",
    ]));
    assert_eq!(
        instantiated(&trans),
        vec!["(=> (instance ?UNIT CompositeUnitOfMeasure) \
             (instance (KiloFn ?UNIT) CompositeUnitOfMeasure))"
            .to_string()]
    );
}

#[test]
fn group_c_guard_in_a_universally_quantified_consequent_is_lifted() {
    let trans = make_trans(&joined(&[
        "(instance AdditionFn CommutativeFunction)",
        "(domain AdditionFn 1 Quantity)",
        "(domain AdditionFn 2 Quantity)",
        "(=> (instance ?FUNCTION CommutativeFunction) \
             (forall (?INST1 ?INST2 ?CLASS) \
               (=> (and (domain ?FUNCTION 1 ?CLASS) (instance ?INST1 ?CLASS) (instance ?INST2 ?CLASS)) \
                   (equal (AssignmentFn ?FUNCTION ?INST1 ?INST2) \
                          (AssignmentFn ?FUNCTION ?INST2 ?INST1)))))",
    ]));
    assert_eq!(
        instantiated(&trans),
        vec![
            "(=> (and (instance ?INST1 Quantity) (instance ?INST2 Quantity)) \
             (equal (AdditionFn ?INST1 ?INST2) (AdditionFn ?INST2 ?INST1)))"
                .to_string()
        ],
        "the inner `domain` guard is discharged and the rule flattened"
    );
}

#[test]
fn lifting_keeps_shadowed_variables_distinct() {
    let trans = make_trans(&joined(&[
        "(instance AdditionFn CommutativeFunction)",
        "(domain AdditionFn 1 Quantity)",
        "(=> (and (instance ?FUNCTION CommutativeFunction) (instance ?X Quantity)) \
             (forall (?X ?CLASS) \
               (=> (and (domain ?FUNCTION 1 ?CLASS) (instance ?X ?CLASS)) \
                   (equal (AssignmentFn ?FUNCTION ?X ?X) ?X))))",
    ]));
    let roots = trans.semantic.syntactic.root_sids();
    let rules = trans.instantiate_predvars(&roots, &roots, Scope::Base);
    assert_eq!(rules.len(), 1, "{rules:?}");
    let mut ids: HashMap<SymbolId, u32> = HashMap::new();
    trans.semantic.syntactic.collect_vars(rules[0], &mut ids);
    assert_eq!(
        ids.len(),
        2,
        "the outer and the shadowing `?X` must stay two variables: {}",
        rendered(&trans, rules[0])
    );
}
