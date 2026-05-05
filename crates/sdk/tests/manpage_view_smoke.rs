//! Phase 2 — `ManPageView` proves the cross-ref parser delivers
//! pre-resolved `DocSpan::Link` entries to consumers.  These tests
//! exercise the SDK end-to-end: ingest KIF that contains
//! `(documentation X EnglishLanguage "...&%Foo...")` and verify the
//! resulting view exposes a structured link for `Foo`.

use sigmakee_rs_core::KnowledgeBase;
use sigmakee_rs_sdk::{manpage_view, DocSpan, IngestOp};

const KIF_WITH_DOC: &str = r#"
    (subclass Animal Organism)
    (subclass Organism PhysicalObject)
    (documentation Animal EnglishLanguage
        "An &%Organism that is alive — not a &%Plant.")
"#;

fn build_kb() -> KnowledgeBase {
    let mut kb = KnowledgeBase::new();
    IngestOp::new(&mut kb)
        .add_source("<base>", KIF_WITH_DOC)
        .run()
        .unwrap();
    kb
}

#[test]
fn manpage_view_resolves_inline_cross_refs() {
    let kb = build_kb();
    let view = manpage_view(&kb, "Animal").expect("Animal manpage should resolve");
    assert_eq!(view.name, "Animal");

    // Parents survived as structured edges.
    assert!(view.parents.iter().any(|p| p.parent == "Organism"));

    // The doc block should expose a link to Organism and a link to Plant,
    // each as a `DocSpan::Link { text, target }` — the consumer never
    // sees raw `&%` markers.
    let block = &view.documentation[0];
    let mut targets: Vec<&str> = Vec::new();
    for span in &block.spans {
        if let DocSpan::Link { target, .. } = span {
            targets.push(target.as_str());
        }
    }
    assert!(targets.contains(&"Organism"), "expected link to Organism, got {:?}", targets);
    assert!(targets.contains(&"Plant"),    "expected link to Plant, got {:?}", targets);

    // No raw markers anywhere in the resolved spans.
    for span in &block.spans {
        match span {
            DocSpan::Text(t) => assert!(!t.contains("&%"), "raw marker leaked: {:?}", t),
            DocSpan::Link { text, .. } => assert!(!text.contains("&%"), "marker in link text"),
        }
    }
}

#[test]
fn manpage_view_link_targets_includes_parents_and_doc_links() {
    let kb = build_kb();
    let view = manpage_view(&kb, "Animal").unwrap();
    let targets = view.link_targets();
    // Order: parents first, then doc cross-refs.
    assert!(targets.iter().any(|t| *t == "Organism"));
    assert!(targets.iter().any(|t| *t == "Plant"));
    // Parent comes before doc reference (depth-first parents, then doc).
    let pos_parent_org = targets.iter().position(|t| *t == "Organism").unwrap();
    // Plant is doc-only, so it should be after the parent block.
    let pos_plant = targets.iter().position(|t| *t == "Plant").unwrap();
    assert!(pos_parent_org < pos_plant, "parents should be enumerated before doc links");
}

#[test]
fn manpage_view_returns_none_for_unknown_symbol() {
    let kb = KnowledgeBase::new();
    assert!(manpage_view(&kb, "NoSuchSymbol").is_none());
}
