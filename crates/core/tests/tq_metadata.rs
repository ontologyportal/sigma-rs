use sigmakee_rs_core::is_tq_directive;
use sigmakee_rs_core::parse_test_content;

#[test]
fn metadata_and_query_do_not_become_assertions() {
    let tc = parse_test_content(
        "(note Communications_3) (time 30) (category Communications)
         (query (=> (instance ?PHONE MobileCellPhone) (connected ?PHONE Internet)))
         (answer yes)",
        "Communications_3.kif.tq",
    )
    .unwrap();
    assert!(tc.axioms.is_empty());
    assert_eq!(tc.axiom_kif(), "");
    assert_eq!(tc.note, "Communications_3");
    assert_eq!(tc.timeout, 30);
    assert!(tc.note_given && tc.time_given);
    assert_eq!(tc.categories, vec!["Communications"]);
    assert_eq!(tc.expected_proof, Some(true));
    assert_eq!(tc.input_formulas, 1);
    assert_eq!(tc.unaccounted_inputs, 0);
    assert_eq!(
        tc.query_kif().unwrap(),
        "(=> (instance ?PHONE MobileCellPhone) (connected ?PHONE Internet))"
    );
    assert!(is_tq_directive("category"));
}

#[test]
fn only_top_level_metadata_is_excluded_and_real_assertions_survive() {
    let tc = parse_test_content(
        "(category Communications) (category Networking)
         (note sample) (time 12) (file \"Merge.kif\") (answer yes)
         (instance Phone MobileCellPhone)
         (=> (category ?X Communications) (instance ?X Entity))
         (ask (connected Phone Internet))",
        "sample.kif.tq",
    )
    .unwrap();
    assert_eq!(tc.axioms.len(), 2);
    assert_eq!(
        tc.axiom_kif(),
        "(instance Phone MobileCellPhone)\n(=> (category ?X Communications) (instance ?X Entity))"
    );
    assert_eq!(tc.query_kif().unwrap(), "(connected Phone Internet)");
    assert_eq!(tc.extra_files, vec!["Merge.kif"]);
    assert_eq!(tc.categories, vec!["Communications", "Networking"]);
    assert_eq!(tc.input_formulas, 3);
    assert_eq!(tc.unaccounted_inputs, 0);
}

#[test]
fn absent_note_and_time_are_defaults_not_directives() {
    let tc = parse_test_content("(query (instance Rex Animal))", "bare.kif.tq").unwrap();
    assert_eq!(tc.note, "bare.kif.tq");
    assert_eq!(tc.timeout, 30);
    assert!(!tc.note_given && !tc.time_given);
    assert!(tc.categories.is_empty());
}

#[test]
fn directive_values_parse_from_symbols_strings_and_decimals() {
    let tc = parse_test_content(
        "(note \"A quoted note\") (time 12.5) (category Astronomy \"Space Science\")
         (query (instance Rex Animal)) (answer Rex Fido)",
        "values.kif.tq",
    )
    .unwrap();
    assert_eq!(tc.note, "A quoted note");
    assert_eq!(tc.timeout, 13);
    assert_eq!(tc.categories, vec!["Astronomy", "Space Science"]);
    assert_eq!(tc.expected_proof, Some(true));
    assert_eq!(
        tc.expected_answer,
        Some(vec!["Rex".to_string(), "Fido".to_string()])
    );
}
