/// Integration tests: duplicate detection in tell() / load_kif().
use sumo_kb::{KnowledgeBase, TellWarning};

fn kb() -> KnowledgeBase {
    KnowledgeBase::new()
}

fn dup_assertion_warnings(r: &sumo_kb::TellResult) -> Vec<&TellWarning> {
    r.warnings.iter().filter(|w| matches!(w, TellWarning::DuplicateAssertion { .. })).collect()
}

fn dup_axiom_warnings(r: &sumo_kb::TellResult) -> Vec<&TellWarning> {
    r.warnings.iter().filter(|w| matches!(w, TellWarning::DuplicateAxiom { .. })).collect()
}

#[test]
fn tell_same_formula_twice_warns_duplicate_assertion() {
    let mut kb = kb();

    let r1 = kb.tell("s1", "(instance Dog Animal)");
    assert!(r1.ok, "first tell should succeed");
    assert!(dup_assertion_warnings(&r1).is_empty(), "no dup warnings on first tell");

    let r2 = kb.tell("s1", "(instance Dog Animal)");
    assert!(r2.ok, "second tell ok=true (not a hard error)");
    let dups = dup_assertion_warnings(&r2);
    assert_eq!(dups.len(), 1, "exactly one DuplicateAssertion warning, warnings={:?}", r2.warnings);
    assert!(
        matches!(dups[0], TellWarning::DuplicateAssertion { existing_session, .. }
            if existing_session == "s1"),
        "expected DuplicateAssertion from session s1, got {:?}", dups[0]
    );
}

#[test]
fn tell_same_formula_in_different_sessions_warns_duplicate() {
    let mut kb = kb();

    let r1 = kb.tell("s1", "(instance Dog Animal)");
    assert!(r1.ok);

    let r2 = kb.tell("s2", "(instance Dog Animal)");
    assert!(r2.ok, "second session tell ok=true");
    assert_eq!(r2.warnings.len(), 1, "one duplicate warning");
    // s2 sees s1's copy as a DuplicateAssertion
    assert!(
        matches!(&r2.warnings[0], TellWarning::DuplicateAssertion { .. }),
        "expected DuplicateAssertion, got {:?}", r2.warnings[0]
    );
}

#[test]
fn alpha_equivalent_formulas_are_deduplicated() {
    let mut kb = kb();

    // These two are alpha-equivalent (variables renamed positionally)
    let r1 = kb.tell("s1", "(forall (?x) (instance ?x Animal))");
    assert!(r1.ok);

    let r2 = kb.tell("s1", "(forall (?y) (instance ?y Animal))");
    assert!(r2.ok, "alpha-equivalent formula should be accepted gracefully");
    // Should emit a duplicate warning (same canonical form after var normalisation)
    assert!(
        r2.warnings.iter().any(|w| matches!(w, TellWarning::DuplicateAssertion { .. })),
        "expected DuplicateAssertion for alpha-equivalent formula"
    );
}

#[test]
fn distinct_formulas_are_not_deduplicated() {
    let mut kb = kb();
    let r1 = kb.tell("s1", "(instance Dog Animal)");
    let r2 = kb.tell("s1", "(instance Cat Animal)");
    assert!(r1.ok && r2.ok);
    assert!(dup_assertion_warnings(&r1).is_empty());
    assert!(dup_assertion_warnings(&r2).is_empty(), "distinct formulas should not dup-warn");
    assert!(dup_axiom_warnings(&r2).is_empty(), "distinct formulas should not dup-warn");
}

#[test]
fn flush_session_removes_assertions_and_dedup_table() {
    let mut kb = kb();
    let r1 = kb.tell("s1", "(instance Dog Animal)");
    assert!(r1.ok);

    kb.flush_session("s1");

    // After flush, the same formula should be accepted again without a duplicate warning
    let r2 = kb.tell("s1", "(instance Dog Animal)");
    assert!(r2.ok);
    assert!(
        dup_assertion_warnings(&r2).is_empty() && dup_axiom_warnings(&r2).is_empty(),
        "after flush, re-tell should not emit duplicate warning: {:?}", r2.warnings
    );
}

#[test]
fn load_kif_deduplicates_within_text() {
    let mut kb = kb();
    // Two copies of the same sentence in one KIF string
    let kif = "(instance Dog Animal)\n(instance Dog Animal)\n";
    let r = kb.load_kif(kif, "test.kif", Some("s1"));
    assert!(r.ok);
    // One duplicate warning for the second occurrence
    assert_eq!(r.warnings.len(), 1, "expected 1 duplicate warning, got {:?}", r.warnings);
}

#[test]
fn parse_error_makes_tell_not_ok() {
    let mut kb = kb();
    let r = kb.tell("s1", "(unclosed");
    assert!(!r.ok, "parse error should set ok=false");
    assert!(!r.errors.is_empty(), "errors vec should be non-empty");
}
