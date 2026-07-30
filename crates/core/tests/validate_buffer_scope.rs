use sigmakee_rs_core::{KnowledgeBase, SourceFile};
use std::path::PathBuf;

/// A live editor buffer is staged into its file's own session but NOT
/// promoted, so its declarations are session-scoped.  Base-scope validation
/// falsely flags symbols the buffer itself connects; session-scoped
/// validation must not.
#[test]
fn scope_of_buffer_added_sentences() {
    let mut kb = KnowledgeBase::default();
    let base = "(subclass Relation Entity)\n(instance instance Relation)\n(instance subclass Relation)";
    kb.stage(SourceFile::kif(PathBuf::from("base.kif"), base.into()), "base.kif");
    kb.commit("base.kif");
    kb.make_session_axiomatic("base.kif").unwrap();

    // Editor buffer adds a PROPERLY CONNECTED new declaration, committed but
    // not promoted.
    let text = "(subclass Relation Entity)\n(instance instance Relation)\n(instance subclass Relation)\n(subclass NewThing Entity)";
    kb.stage(SourceFile::kif(PathBuf::from("base.kif"), text.into()), "base.kif");
    kb.commit("base.kif");

    let base_scope = kb.validate_file("base.kif");
    assert!(base_scope.iter().any(|d| d.code == "no-entity-ancestor" && d.message.contains("NewThing")),
        "Base scope can't see the unpromoted session overlay (documents WHY the session-scoped variant exists)");

    let session_scope = kb.validate_file_in_session("base.kif", "base.kif");
    assert!(!session_scope.iter().any(|d| d.code == "no-entity-ancestor" && d.message.contains("NewThing")),
        "session-scoped validation must see the buffer's own declarations; got {session_scope:?}");
}

/// Parse-only vetting: no KB, no state, and diagnostics for broken text.
#[test]
fn kif_parse_diagnostics_reports_unbalanced_parens() {
    let diags = sigmakee_rs_core::kif_parse_diagnostics("(instance Broken\n", "b.kif");
    assert!(diags.iter().any(|d| d.code == "kif/unbalanced-parens"),
        "expected an unbalanced-parens diagnostic, got {diags:?}");
    assert!(sigmakee_rs_core::kif_parse_diagnostics("(instance A B)", "b.kif").is_empty());
}
