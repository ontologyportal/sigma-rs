//! Integration tests for the classified-findings flow added by the
//! Option-B warning-print extraction.
//!
//! Two invariants under test:
//!
//! 1. `kb.validate_*_findings(...)` returns BOTH warnings and hard
//!    errors, classified per `SemanticError::is_warn`.
//! 2. `SemanticError::handle()` no longer side-effects (no console
//!    output, no log calls) — the only path to surface a finding is
//!    through the collector / findings methods.
//!
//! The "no log calls" half is hard to test directly without a custom
//! logger; we lean on the explicit Findings classification instead
//! and trust that `cargo test --workspace` would surface any
//! regression in the existing logging-coupled tests.

use std::sync::{Mutex, MutexGuard, OnceLock};

use sigmakee_rs_core::KnowledgeBase;
use sigmakee_rs_core::{clear_promoted_errors, promote_to_error, set_all_errors};

/// Tests in this file mutate process-global classification flags
/// (`ALL_ERRORS`, `PROMOTED_TO_ERROR`).  `cargo test` runs tests
/// from one binary in parallel by default; serialise them via a
/// shared mutex so flag writes from one test can't be observed by
/// another mid-execution.
fn global_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        // Tests panicking with the lock held would poison it;
        // recover anyway so other tests still run.
        .unwrap_or_else(|p| p.into_inner())
}

/// A KIF fixture that triggers multiple semantic checks:
/// - `MissingArity` on `customRel` (declared a relation, no arity ancestry)
/// - `ArityMismatch` on `(instance Foo Bar Baz)` (3 args to a binary relation)
/// - `HeadNotRelation` on `(NotARelation X Y)`
///
/// In default mode (no `-Wall` / `-W <code>`) every finding above
/// classifies as a warning per `current_level()` — sigmakee-rs-core's default
/// is "warn unless explicitly promoted".  These tests therefore
/// either flip the global promotion flags to get hard errors OR
/// assert the all-warnings shape explicitly.
const KB_WITH_FINDINGS: &str = r#"
    (subclass Relation Entity)
    (subclass BinaryRelation Relation)
    (instance instance BinaryRelation)
    (domain instance 1 Entity)
    (domain instance 2 Class)
    (subclass Animal Entity)

    (instance customRel Relation)
    (instance Foo Bar Baz)
    (NotARelation X Y)
"#;

/// Tests in this file mutate process-global flags (via `-Wall` etc).
/// `cargo test` runs them in parallel by default — to keep tests
/// independent we use the same single-threaded harness convention
/// the rest of the workspace uses (`mod {set_all_errors(true);
/// /* test body */ set_all_errors(false);}`).  Each test reset the
/// flag at the end, even on panic, via a drop guard.
struct AllErrorsGuard(bool);
impl AllErrorsGuard {
    fn enable() -> Self { set_all_errors(true); Self(true) }
}
impl Drop for AllErrorsGuard {
    fn drop(&mut self) { if self.0 { set_all_errors(false); } }
}

/// Restores the promotion table to empty on drop — every test that
/// touches it must hold one of these so a leaked promotion can't
/// poison the next test.
struct PromotionGuard;
impl PromotionGuard {
    fn fresh() -> Self { clear_promoted_errors(); Self }
}
impl Drop for PromotionGuard {
    fn drop(&mut self) { clear_promoted_errors(); }
}

#[test]
fn validate_all_findings_partitions_when_some_promoted_to_error() {
    let _lock = global_lock();
    let _pg = PromotionGuard::fresh();
    let _guard = AllErrorsGuard::enable();

    let mut kb = KnowledgeBase::new();
    let r = kb.load_kif(KB_WITH_FINDINGS, "fixture", None);
    assert!(r.ok, "fixture must parse");

    let findings = kb.validate_all_findings();
    // With -Wall every finding is a hard error.
    assert!(!findings.is_clean(), "with -Wall, fixture should have hard errors");
    assert!(!findings.errors.is_empty(), "expected at least one hard error");
    assert!(findings.warnings.is_empty(),
        "with -Wall, all findings classify as errors so warnings should be empty");

    // Every error must self-classify as non-warn.
    assert!(findings.errors.iter().all(|(_, e)| !e.is_warn()));
}

#[test]
fn validate_all_findings_default_mode_classifies_everything_as_warning() {
    let _lock = global_lock();
    let _g = PromotionGuard::fresh();
    // Without any promotion, sigmakee-rs-core's default classification is
    // Warn for every variant.  Findings.errors must therefore be
    // empty and is_clean() must return true — even though we've
    // produced legitimate parse-passes-but-semantically-suspect KIF.
    let mut kb = KnowledgeBase::new();
    let _ = kb.load_kif(KB_WITH_FINDINGS, "fixture", None);

    let findings = kb.validate_all_findings();
    assert!(findings.is_clean(), "default mode treats every finding as a warning");
    assert!(findings.errors.is_empty());
    assert!(!findings.warnings.is_empty(),
        "but warnings should be present so consumers can render them");
    assert!(findings.warnings.iter().all(|(_, e)| e.is_warn()));
}

#[test]
fn findings_total_is_warnings_plus_errors() {
    let _lock = global_lock();
    let _g = PromotionGuard::fresh();
    let mut kb = KnowledgeBase::new();
    let _ = kb.load_kif(KB_WITH_FINDINGS, "fixture", None);
    let f = kb.validate_all_findings();
    assert_eq!(f.total(), f.errors.len() + f.warnings.len());
    assert!(f.total() >= 2);
}

#[test]
fn promote_to_error_targets_a_specific_code() {
    let _lock = global_lock();
    let _g = PromotionGuard::fresh();
    let mut kb = KnowledgeBase::new();
    let _ = kb.load_kif(KB_WITH_FINDINGS, "fixture", None);

    // Promote just E005 (ArityMismatch).  Other findings stay
    // warnings.  The pretty-printable code is the one returned by
    // SemanticError::code().
    promote_to_error("E005");

    let findings = kb.validate_all_findings();
    // At least one ArityMismatch should be classified as a hard error.
    assert!(findings.errors.iter().any(|(_, e)| e.code() == "E005"),
        "E005 should have been promoted to errors by promote_to_error(\"E005\")");
    // Other-code findings should remain in warnings.
    assert!(findings.warnings.iter().any(|(_, e)| e.code() != "E005"),
        "non-promoted findings should remain warnings");
}

#[test]
fn validate_sentence_findings_returns_per_sentence_classification() {
    let _lock = global_lock();
    let _g = PromotionGuard::fresh();
    let mut kb = KnowledgeBase::new();
    let _ = kb.load_kif(KB_WITH_FINDINGS, "fixture", None);

    // Each root sentence's findings list should match what
    // validate_all_findings collected for that same sid.
    let global = kb.validate_all_findings();
    for &sid in kb.file_roots("fixture") {
        let local = kb.validate_sentence_findings(sid);
        let global_for_sid: usize = global.errors.iter().filter(|(s,_)| *s == sid).count()
                                   + global.warnings.iter().filter(|(s,_)| *s == sid).count();
        assert_eq!(local.total(), global_for_sid,
            "per-sentence finding count must match the per-sid slice of validate_all_findings");
    }
}

#[test]
fn validate_session_findings_scopes_correctly() {
    let _lock = global_lock();
    let _g = PromotionGuard::fresh();
    let mut kb = KnowledgeBase::new();
    // Load the fixture into session "loaded" — sentences are
    // session-scoped (NOT axiomatic).  validate_session_findings
    // should return findings only for those sentences.
    let _ = kb.load_kif(KB_WITH_FINDINGS, "fixture", Some("loaded"));

    let session_findings = kb.validate_session_findings("loaded");
    assert!(session_findings.total() >= 2);

    // Empty session returns empty findings.
    let empty = kb.validate_session_findings("does-not-exist");
    assert!(empty.is_clean());
    assert_eq!(empty.total(), 0);
}

#[test]
fn handle_no_longer_logs_warnings() {
    let _lock = global_lock();
    let _g = PromotionGuard::fresh();
    // Indirect test: `validate_sentence` (NOT the `_findings`
    // variant) does NOT install a collector, so a pure-warning
    // finding should be silently consumed via `Ok(())` — proving
    // handle() neither logs nor returns Err for warnings.
    //
    // Pre-Option-B, this test would have produced stderr output
    // from `log::warn!(...)`; post-Option-B it is silent.
    // (We can't assert "no logs"; we can only assert the return
    // shape — first-error-or-Ok.)
    let warning_only = r#"
        (subclass Relation Entity)
        (subclass Animal Entity)
        ; MissingArity is a warning — handle() must NOT short-circuit.
        (instance customRel Relation)
    "#;
    let mut kb = KnowledgeBase::new();
    let _ = kb.load_kif(warning_only, "warns", None);

    for &sid in kb.file_roots("warns") {
        // validate_sentence returns Result<(), SemanticError>.
        // For warning-only sentences this must be Ok.
        assert!(kb.validate_sentence(sid).is_ok(),
            "warning-only sentence {sid} should validate as Ok");
    }

    // The findings entry point still surfaces them.
    let f = kb.validate_all_findings();
    assert!(f.is_clean(), "warning-only fixture should have no hard errors");
    assert!(!f.warnings.is_empty(), "but should have at least one warning");
}
