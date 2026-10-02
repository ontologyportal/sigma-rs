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
    assert_eq!(tc.input_formulas, 3);
    assert_eq!(tc.unaccounted_inputs, 0);
}
