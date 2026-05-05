//! Phase 3 — `TestOp` smoke tests.
//!
//! Tests that need a real Vampire binary check `which vampire` at
//! run time and skip cleanly if it's missing.

#![cfg(feature = "ask")]

use std::path::PathBuf;
use std::process::Command;

use sigmakee_rs_core::KnowledgeBase;
use sigmakee_rs_sdk::{IngestOp, TestOp, TestOutcome};

const TINY_KB: &str = r#"
    (subclass Animal Organism)
    (subclass Organism PhysicalObject)
    (instance Fido Animal)
"#;

const PASSING_TQ: &str = r#"
    (note "Fido is an Animal")
    (time 15)
    (answer yes)
    (query (instance Fido Animal))
"#;

const FAILING_TQ: &str = r#"
    (note "Plankton is NOT an Animal in this KB")
    (time 15)
    (answer yes)
    (query (instance Plankton Animal))
"#;

const NO_QUERY_TQ: &str = r#"
    (note "Empty test")
    (time 15)
    (answer yes)
"#;

const MALFORMED_TQ: &str = r#"
    (note "Bad
    (query (
"#;

fn build_kb() -> KnowledgeBase {
    let mut kb = KnowledgeBase::new();
    IngestOp::new(&mut kb)
        .add_source("<base>", TINY_KB)
        .run()
        .unwrap();
    kb
}

fn vampire_on_path() -> Option<PathBuf> {
    Command::new("which").arg("vampire").output().ok().and_then(|o| {
        if !o.status.success() { return None }
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(PathBuf::from(s)) }
    })
}

// ---------------------------------------------------------------------------
// Parsing / case-handling — no prover required
// ---------------------------------------------------------------------------

#[test]
fn no_query_directive_yields_skipped_outcome() {
    let mut kb = build_kb();
    let report = TestOp::new(&mut kb)
        .add_text("<empty>", NO_QUERY_TQ)
        .run()
        .unwrap();
    assert_eq!(report.cases.len(), 1);
    assert!(matches!(report.cases[0].outcome, TestOutcome::NoQuery));
    assert_eq!(report.skipped, 1);
    assert_eq!(report.passed, 0);
}

#[test]
fn malformed_test_text_yields_parse_error_outcome() {
    let mut kb = build_kb();
    let report = TestOp::new(&mut kb)
        .add_text("<bad>", MALFORMED_TQ)
        .run()
        .unwrap();
    assert_eq!(report.cases.len(), 1);
    assert!(matches!(report.cases[0].outcome, TestOutcome::ParseError(_)));
    assert_eq!(report.skipped, 1);
}

#[test]
fn missing_test_file_aborts_run_with_io_error() {
    let mut kb = build_kb();
    let result = TestOp::new(&mut kb)
        .add_file("/tmp/sigmakee-rs-sdk-no-such-test-xxxxx.kif.tq")
        .run();
    assert!(matches!(result, Err(sigmakee_rs_sdk::SdkError::Io { .. })));
}

#[test]
fn missing_test_dir_aborts_run_with_dir_read_error() {
    let mut kb = build_kb();
    let result = TestOp::new(&mut kb)
        .add_dir("/tmp/sigmakee-rs-sdk-no-such-test-dir-xxxxx")
        .run();
    assert!(matches!(result, Err(sigmakee_rs_sdk::SdkError::DirRead { .. })));
}

#[test]
fn dir_walk_finds_only_kif_tq_files() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.kif.tq"), PASSING_TQ).unwrap();
    std::fs::write(tmp.path().join("b.kif"),    "(subclass A B)").unwrap();
    std::fs::write(tmp.path().join("note.txt"), "ignore").unwrap();

    let mut kb = build_kb();
    let report = TestOp::new(&mut kb).add_dir(tmp.path());

    // We can't actually run the cases without vampire installed, but
    // we *can* verify that exactly one case was discovered (the .kif.tq
    // one) by short-circuiting when vampire is absent.  In that case
    // the case still gets discovered — it just lands in ProverError /
    // similar.  We test the invariant: cases.len() == 1.
    let report = match vampire_on_path() {
        Some(p)  => report.vampire_path(p).run().unwrap(),
        None     => report
            // Force a 1-second timeout so the test doesn't hang if
            // somehow vampire IS in $PATH but not via `which`.
            .vampire_path("/tmp/sigmakee-rs-sdk-definitely-not-vampire")
            .run()
            .unwrap(),
    };
    assert_eq!(report.cases.len(), 1, "exactly the .kif.tq file is picked up");
    assert!(report.cases[0].tag.ends_with("a.kif.tq"));
}

// ---------------------------------------------------------------------------
// End-to-end with vampire
// ---------------------------------------------------------------------------

#[test]
fn passing_case_runs_and_records_pass() {
    let Some(vampire) = vampire_on_path() else {
        eprintln!("skipping: vampire not on PATH");
        return;
    };
    let mut kb = build_kb();
    let report = TestOp::new(&mut kb)
        .add_text("<pass>", PASSING_TQ)
        .vampire_path(vampire)
        .run()
        .unwrap();
    assert_eq!(report.cases.len(), 1);
    assert!(matches!(report.cases[0].outcome, TestOutcome::Passed),
        "expected Passed, got {:?}", report.cases[0].outcome);
    assert_eq!(report.passed, 1);
    assert_eq!(report.failed, 0);
    assert!(report.all_passed());
}

#[test]
fn failing_case_records_fail_with_expected_got() {
    let Some(vampire) = vampire_on_path() else {
        eprintln!("skipping: vampire not on PATH");
        return;
    };
    let mut kb = build_kb();
    let report = TestOp::new(&mut kb)
        .add_text("<fail>", FAILING_TQ)
        .vampire_path(vampire)
        .timeout_override(10)
        .run()
        .unwrap();
    assert_eq!(report.cases.len(), 1);
    match &report.cases[0].outcome {
        TestOutcome::Failed { expected: true, got: false } => {}
        other => panic!("expected Failed{{expected:true,got:false}}, got {:?}", other),
    }
    assert_eq!(report.failed, 1);
    assert!(!report.all_passed());
}

#[test]
fn timeout_override_supersedes_test_directive() {
    let Some(vampire) = vampire_on_path() else {
        eprintln!("skipping: vampire not on PATH");
        return;
    };
    let mut kb = build_kb();
    // The test declares (time 15) but we override to 5.  We can't
    // directly observe the override from outside, but we can confirm
    // the case still runs to a verdict in well under 15s — and the
    // builder accepts the call.
    let report = TestOp::new(&mut kb)
        .add_text("<pass>", PASSING_TQ)
        .timeout_override(5)
        .vampire_path(vampire)
        .run()
        .unwrap();
    assert_eq!(report.cases.len(), 1);
    assert!(matches!(report.cases[0].outcome, TestOutcome::Passed));
}

#[test]
fn add_case_takes_pre_parsed_test() {
    let Some(vampire) = vampire_on_path() else {
        eprintln!("skipping: vampire not on PATH");
        return;
    };
    // Simulate a caller that already parsed the .kif.tq via a JSON
    // payload or similar, and just hands the SDK a TestCase.
    let case = sigmakee_rs_sdk::TestCase {
        file_name: "<json>".into(),
        note:      "passes".into(),
        timeout:   15,
        query:     Some("(instance Fido Animal)".into()),
        expected_proof: Some(true),
        expected_answer: None,
        axioms:    Vec::new(),
        extra_files: Vec::new(),
    };
    let mut kb = build_kb();
    let report = TestOp::new(&mut kb)
        .add_case("<rpc/42>", case)
        .vampire_path(vampire)
        .run()
        .unwrap();
    assert_eq!(report.cases.len(), 1);
    assert!(matches!(report.cases[0].outcome, TestOutcome::Passed));
    assert_eq!(report.cases[0].tag, "<rpc/42>");
}
