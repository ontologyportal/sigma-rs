//! Phase 2 — `ValidateOp` round-trips against tiny inline KIF.

use sumo_kb::KnowledgeBase;
use sumo_sdk::{IngestOp, ValidateOp};

const CLEAN_KB: &str = r#"
    (subclass Animal Organism)
    (subclass Organism PhysicalObject)
"#;

fn build_kb() -> KnowledgeBase {
    let mut kb = KnowledgeBase::new();
    IngestOp::new(&mut kb)
        .add_source("<base>", CLEAN_KB)
        .run()
        .unwrap();
    kb
}

#[test]
fn validate_all_returns_clean_report_on_well_formed_kb() {
    let mut kb = build_kb();
    let report = ValidateOp::all(&mut kb).run().unwrap();
    assert!(report.is_clean());
    assert!(report.parse_errors.is_empty());
    assert!(report.semantic_errors.is_empty());
    assert!(report.session.is_none());
}

#[test]
fn validate_all_surfaces_semantic_warnings_separate_from_errors() {
    // Load a KB whose `(instance customRel Relation)` triggers
    // `MissingArity` — a warning by default classification.  The
    // SDK's report must surface it under `semantic_warnings`, NOT
    // `semantic_errors`, and is_clean() must remain true.
    let warning_only_kb = r#"
        (subclass Relation Entity)
        (subclass Animal Entity)
        (instance customRel Relation)
    "#;
    let mut kb = sumo_kb::KnowledgeBase::new();
    sumo_sdk::IngestOp::new(&mut kb)
        .add_source("<base>", warning_only_kb)
        .run()
        .unwrap();

    let report = ValidateOp::all(&mut kb).run().unwrap();
    assert!(report.is_clean(), "warnings don't unset cleanliness");
    assert!(report.semantic_errors.is_empty(),
        "fixture has no hard errors in default mode");
    assert!(!report.semantic_warnings.is_empty(),
        "MissingArity should surface as a warning");
    // total_findings counts both sides.
    assert_eq!(report.total_findings(), report.semantic_warnings.len());
}

#[test]
fn validate_parse_only_skips_semantic_pass() {
    let mut kb = build_kb();
    let report = ValidateOp::all(&mut kb).parse_only(true).run().unwrap();
    assert!(report.is_clean());
    // parse_only path inspects no sentences (KB sentences are
    // already-parsed by definition).
    assert_eq!(report.inspected, 0);
}

#[test]
fn validate_formula_against_kb_records_session() {
    let mut kb = build_kb();
    let formula = "(subclass Mammal Animal)";
    let report = ValidateOp::formula(&mut kb, "<inline>", formula)
        .skip_kb_check(true)
        .run()
        .unwrap();
    assert!(report.is_clean());
    assert_eq!(report.session.as_deref(), Some("<inline>"));
    assert_eq!(report.inspected, 1);
}

#[test]
fn validate_formula_with_garbage_input_records_parse_errors() {
    let mut kb = build_kb();

    // Unbalanced paren — parser should reject.
    let bad = "(subclass Mammal Animal";
    let report = ValidateOp::formula(&mut kb, "<bad>", bad)
        .skip_kb_check(true)
        .run()
        .unwrap();
    assert!(!report.parse_errors.is_empty());
    assert!(!report.is_clean());
}
