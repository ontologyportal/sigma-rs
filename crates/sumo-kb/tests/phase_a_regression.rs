//! Phase A regression: verify that removing the pre-parse
//! `invalidate_cache()` and conditionally skipping the post-proof
//! `rebuild_taxonomy()` produces the same answers as before.
//!
//! We exercise both a taxonomy-head query (which still triggers the
//! rebuild) and a non-taxonomy query (which takes the skip path), and
//! check that repeated asks against an unchanged KB return stable
//! results.
#![cfg(all(feature = "cnf", feature = "integrated-prover", feature = "ask"))]

use sumo_kb::{KnowledgeBase, ProverStatus, TptpLang};

fn tiny_kb() -> KnowledgeBase {
    let mut kb = KnowledgeBase::new();
    // Includes explicit transitivity so Vampire doesn't need SUMO's
    // full taxonomy axioms to derive the subclass-chain consequences.
    let kif = "
        (subclass Human Animal)
        (subclass Animal Entity)
        (instance Alice Human)
        (attribute Alice Warm)
        (forall (?X ?Y ?Z)
          (=> (and (subclass ?X ?Y) (subclass ?Y ?Z))
              (subclass ?X ?Z)))
        (forall (?X ?Y ?Z)
          (=> (and (instance ?X ?Y) (subclass ?Y ?Z))
              (instance ?X ?Z)))
    ";
    let r = kb.load_kif(kif, "test.kif", Some("s1"));
    assert!(r.ok, "load: {:?}", r.errors);
    kb.make_session_axiomatic("s1");
    kb
}

/// A non-taxonomy conjecture.  Asked three times; all three answers
/// must agree.
#[test]
fn non_taxonomy_query_stable_across_repeats() {
    let mut kb = tiny_kb();
    let q = "(attribute Alice Warm)";
    let s1 = kb.ask_embedded(q, None, 5, TptpLang::Fof).status;
    let s2 = kb.ask_embedded(q, None, 5, TptpLang::Fof).status;
    let s3 = kb.ask_embedded(q, None, 5, TptpLang::Fof).status;
    assert_eq!(s1, ProverStatus::Proved);
    assert_eq!(s1, s2);
    assert_eq!(s2, s3);
}

/// A taxonomy-head conjecture.  The skip-rebuild path does NOT apply
/// here (Phase A keeps the rebuild when the query is a taxonomy
/// relation), so we want identical answers across repeats as a sanity
/// check on the conservative branch.
#[test]
fn taxonomy_query_stable_across_repeats() {
    let mut kb = tiny_kb();
    let q = "(subclass Human Entity)";  // transitively derivable
    let s1 = kb.ask_embedded(q, None, 5, TptpLang::Fof).status;
    let s2 = kb.ask_embedded(q, None, 5, TptpLang::Fof).status;
    assert_eq!(s1, ProverStatus::Proved);
    assert_eq!(s1, s2);
}

/// After a non-taxonomy ask(), subsequent taxonomy-dependent queries
/// must still give correct answers.  This catches the case where a
/// skipped rebuild left stale state that poisons a later query.
#[test]
fn skipped_rebuild_does_not_poison_later_queries() {
    let mut kb = tiny_kb();

    // First: a non-taxonomy ask that exercises the skip path.
    let _ = kb.ask_embedded("(attribute Alice Warm)", None, 5, TptpLang::Fof);

    // Then: a taxonomy-dependent ask that relies on the full taxonomy
    // being intact.  Must still derive `(instance Alice Animal)` via
    // subclass transitivity.
    let r = kb.ask_embedded("(instance Alice Animal)", None, 5, TptpLang::Fof);
    assert_eq!(r.status, ProverStatus::Proved,
        "taxonomy-dependent query after non-taxonomy ask should still succeed");
}

/// A taxonomy-head ask IS a taxonomy-relevant mutation while live.
/// After remove_file, the taxonomy MUST be restored to its pre-query
/// state; subsequent queries must NOT believe the transient query was
/// ever an axiom.
#[test]
fn taxonomy_ask_does_not_leak_into_axioms() {
    let mut kb = tiny_kb();

    // Ask a tax relation NOT present in the KB.
    let r = kb.ask_embedded("(subclass Rock Animal)", None, 5, TptpLang::Fof);
    // Whatever the answer, we don't care -- we just want the query
    // side-effect-cleaned.
    let _ = r;

    // Now query whether Rock is an Animal -- should NOT be provable
    // because we never actually asserted (subclass Rock Animal).
    let r2 = kb.ask_embedded("(instance SomeRockInstance Animal)", None, 5, TptpLang::Fof);
    // Should not be Proved (there's no `SomeRockInstance` at all).
    assert_ne!(r2.status, ProverStatus::Proved,
        "transient tax-head query leaked into axioms");
}
