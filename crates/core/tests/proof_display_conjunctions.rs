use sigmakee_rs_core::prover::proof::formula_to_kif;

#[test]
fn flattens_left_and_right_nested_conjunctions() {
    assert_eq!(
        formula_to_kif("((s__a & s__b) & (s__c & s__d))"),
        "(and a b c d)"
    );
    assert_eq!(formula_to_kif("s__a & s__b & s__c"), "(and a b c)");
    assert_eq!(formula_to_kif("s__a"), "a");
}

#[test]
fn preserves_existential_implication_in_e_proof() {
    assert_eq!(
        formula_to_kif("? [X] : (((s__p(X) & s__q(X)) & s__r(X)) => s__s(X))"),
        "(exists (?X) (=> (and (p ?X) (q ?X) (r ?X)) (s ?X)))"
    );
}

#[test]
fn preserves_other_connectives_and_quantifier_boundaries() {
    assert_eq!(
        formula_to_kif("(s__a & ~(s__b & s__c)) & (s__d | (s__e & s__f))"),
        "(and a (not (and b c)) (or d (and e f)))"
    );
    assert_eq!(
        formula_to_kif("s__a & (? [X] : (s__p(X) & s__q(X)))"),
        "(and a (exists (?X) (and (p ?X) (q ?X))))"
    );
}
