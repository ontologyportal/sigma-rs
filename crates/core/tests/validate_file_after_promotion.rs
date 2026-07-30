use sigmakee_rs_core::{KnowledgeBase, SourceFile};
use std::path::PathBuf;

#[test]
fn validate_buffer_flow_after_save() {
    let mut kb = KnowledgeBase::default();
    // Boot: base ontology promoted.
    let base = "(subclass Relation Entity)\n(instance instance Relation)\n(instance subclass Relation)";
    kb.stage(SourceFile::kif(PathBuf::from("base.kif"), base.into()), "base.kif");
    kb.commit("base.kif");
    kb.make_session_axiomatic("base.kif").unwrap();

    // Save flow: new file ingested + promoted.
    let text = "(instance MyBrandNewThing MyBrandNewClass)";
    kb.stage(SourceFile::kif(PathBuf::from("e001-test.kif"), text.into()), "e001-test.kif");
    kb.commit("e001-test.kif");
    kb.make_session_axiomatic("e001-test.kif").unwrap();

    // validateBuffer flow: stage identical text, expect empty diff.
    let staged = kb.stage(SourceFile::kif(PathBuf::from("e001-test.kif"), text.into()), "e001-test.kif");
    kb.commit("e001-test.kif");
    eprintln!("staged.ok={} staged.sids={:?}", staged.ok, staged.sids);
    // The wasm validateBuffer empty-diff branch: validate the whole file.
    assert!(staged.sids.is_empty(), "identical buffer should stage an empty diff");
    let diags = kb.validate_file("e001-test.kif");
    for d in &diags { eprintln!("diag: {} {}", d.code, d.message); }
    assert!(diags.iter().any(|d| d.code == "no-entity-ancestor"),
        "expected no-entity-ancestor from validate_file; got {diags:?}");
}
