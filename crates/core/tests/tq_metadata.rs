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
         (note sample) (time 12) (include \"Merge.kif\") (answer yes)
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
    assert_eq!(tc.includes, vec!["Merge.kif"]);
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

#[test]
fn includes_preserve_paths_without_becoming_logical_inputs() {
    let tc = parse_test_content(
        "(include tests/CASC/axiom-sets/full.sources)\n\
         (include \"a missing file.kif\")\n\
         (include Merge.kif)\n\
         (query (geographicSubregion Egypt Africa))",
        "GEG.kif.tq",
    )
    .unwrap();
    assert_eq!(
        tc.includes,
        vec![
            "tests/CASC/axiom-sets/full.sources",
            "a missing file.kif",
            "Merge.kif"
        ]
    );
    assert!(tc.axioms.is_empty());
    assert_eq!(tc.input_formulas, 1);
    assert_eq!(tc.unaccounted_inputs, 0);
    assert!(is_tq_directive("include"));
    assert!(!is_tq_directive("file"));
}

#[test]
fn nested_include_remains_logical() {
    let tc = parse_test_content(
        "(=> (include ?X) (instance ?X Entity)) (query (p a))",
        "nested.kif.tq",
    )
    .unwrap();
    assert!(tc.includes.is_empty());
    assert_eq!(tc.axiom_kif(), "(=> (include ?X) (instance ?X Entity))");
    assert_eq!(tc.input_formulas, 2);
    assert_eq!(tc.unaccounted_inputs, 0);
}

#[cfg(feature = "external-prover")]
#[test]
fn test_includes_never_reach_tptp_or_load_dependencies() {
    use sigmakee_rs_core::{ExternalOpts, KnowledgeBase, SourceFile, TptpOptions};

    let mut kb = KnowledgeBase::new();
    let loaded = kb.load(
        SourceFile::kif("base.kif".into(), "(subclass Dog Animal)".into()),
        "base.kif",
    );
    assert!(loaded.ok, "{:?}", loaded.diagnostics);
    kb.make_session_axiomatic("base.kif").expect("promote");
    let tc = parse_test_content(
        "(include tests/CASC/axiom-sets/full.sources)\n\
         (include \"not-present.kif\")\n\
         (instance Fido Dog)\n\
         (query (instance Fido Animal))",
        "includes.kif.tq",
    )
    .expect("parses");
    let tptp = kb
        .tc_to_tptp(tc, &TptpOptions::default(), &ExternalOpts::default())
        .expect("translates without reading includes");
    assert!(!tptp.contains("s__include"), "{tptp}");
    assert!(!tptp.contains("full_sources"), "{tptp}");
    assert!(!tptp.contains("not_present"), "{tptp}");
    assert!(tptp.contains("s__Fido"), "{tptp}");
    assert!(tptp.contains(", conjecture,"), "{tptp}");
}

#[test]
fn include_without_a_path_is_rejected_by_kif_syntax() {
    assert!(parse_test_content("(include)", "empty.kif.tq").is_err());
}
