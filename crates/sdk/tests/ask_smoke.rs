//! Phase 3 — `AskOp` smoke tests.
//!
//! Tests that need a real Vampire binary check `which vampire` at
//! run time and skip cleanly if it's missing.  Tests of error paths
//! and builder ergonomics run unconditionally.

#![cfg(feature = "ask")]

use std::path::PathBuf;
use std::process::Command;

use sigmakee_rs_core::KnowledgeBase;
use sigmakee_rs_sdk::{AskOp, IngestOp, ProverBackend, ProverStatus, SdkError};

const TINY_KB: &str = r#"
    (subclass Animal Organism)
    (subclass Organism PhysicalObject)
    (instance Fido Animal)
"#;

fn build_kb() -> KnowledgeBase {
    let mut kb = KnowledgeBase::new();
    IngestOp::new(&mut kb)
        .add_source("<base>", TINY_KB)
        .run()
        .unwrap();
    kb
}

/// Check that an external `vampire` binary is on `$PATH`.  Returns
/// `Some(path)` if found, `None` otherwise.  Used to gate tests that
/// would otherwise return `SdkError::VampireNotFound` everywhere
/// without one installed.
fn vampire_on_path() -> Option<PathBuf> {
    Command::new("which").arg("vampire").output().ok().and_then(|o| {
        if !o.status.success() { return None }
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(PathBuf::from(s)) }
    })
}

// ---------------------------------------------------------------------------
// Error paths — no prover required
// ---------------------------------------------------------------------------

#[test]
fn missing_vampire_returns_vampire_not_found() {
    let mut kb = build_kb();
    let result = AskOp::new(&mut kb, "(instance Fido Animal)")
        .vampire_path("/tmp/sigmakee-rs-sdk-no-such-vampire-xxxxx")
        .timeout_secs(5)
        .run();
    assert!(matches!(result, Err(SdkError::VampireNotFound(_))),
        "expected VampireNotFound, got {:?}", result.err());
}

#[test]
fn malformed_tell_aborts_with_kb_error() {
    let mut kb = build_kb();
    let result = AskOp::new(&mut kb, "(instance Fido Animal)")
        .tell("(subclass Mammal")  // unbalanced paren
        .timeout_secs(5)
        .run();
    assert!(matches!(result, Err(SdkError::Kb(_))));
}

// ---------------------------------------------------------------------------
// Happy path — requires vampire
// ---------------------------------------------------------------------------

#[test]
fn proves_trivial_conjecture_against_kb() {
    let Some(vampire) = vampire_on_path() else {
        eprintln!("skipping: vampire not on PATH");
        return;
    };
    let mut kb = build_kb();
    let report = AskOp::new(&mut kb, "(instance Fido Animal)")
        .vampire_path(vampire)
        .timeout_secs(15)
        .run()
        .unwrap();
    // Tautology of the form "X is in the KB" should prove.
    assert_eq!(report.status, ProverStatus::Proved,
        "expected Proved, got {:?} (raw: {})", report.status, report.raw_output);
    assert!(report.is_proved());
    assert!(report.is_decided());
}

#[test]
fn tell_assertions_visible_to_query() {
    let Some(vampire) = vampire_on_path() else {
        eprintln!("skipping: vampire not on PATH");
        return;
    };
    let mut kb = build_kb();
    // The base KB knows about Fido but not about Rex.  We tell that
    // Rex is an Animal in the session, then ask whether Rex is one.
    let report = AskOp::new(&mut kb, "(instance Rex Animal)")
        .tell("(instance Rex Animal)")
        .session("ask-test-tell")
        .vampire_path(vampire)
        .timeout_secs(15)
        .run()
        .unwrap();
    assert_eq!(report.status, ProverStatus::Proved);
}

#[test]
fn progress_callback_fires_around_ask() {
    use std::sync::{Arc, Mutex};

    let Some(vampire) = vampire_on_path() else {
        eprintln!("skipping: vampire not on PATH");
        return;
    };

    let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let collected = log.clone();

    struct Sink(Arc<Mutex<Vec<&'static str>>>);
    impl sigmakee_rs_sdk::ProgressSink for Sink {
        fn emit(&self, e: &sigmakee_rs_sdk::ProgressEvent) {
            let label = match *e {
                sigmakee_rs_sdk::ProgressEvent::AskStarted  { .. } => "ask-started",
                sigmakee_rs_sdk::ProgressEvent::AskFinished { .. } => "ask-finished",
                _ => "other",
            };
            self.0.lock().unwrap().push(label);
        }
    }

    let mut kb = build_kb();
    let _ = AskOp::new(&mut kb, "(instance Fido Animal)")
        .vampire_path(vampire)
        .timeout_secs(15)
        .progress(Box::new(Sink(collected)))
        .run()
        .unwrap();

    let evs = log.lock().unwrap().clone();
    assert_eq!(evs.first().copied(), Some("ask-started"));
    assert_eq!(evs.last().copied(),  Some("ask-finished"));
}

// ---------------------------------------------------------------------------
// Builder defaults sanity
// ---------------------------------------------------------------------------

#[test]
fn default_backend_is_subprocess() {
    assert_eq!(ProverBackend::default(), ProverBackend::Subprocess);
}
