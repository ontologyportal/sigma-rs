//! Phase 2 — `TranslateOp` round-trips against tiny inline KIF.

use sumo_kb::KnowledgeBase;
use sumo_sdk::{IngestOp, TptpLang, TranslateOp};

const KB_KIF: &str = r#"
    (subclass Animal Organism)
"#;

fn build_kb() -> KnowledgeBase {
    let mut kb = KnowledgeBase::new();
    IngestOp::new(&mut kb)
        .add_source("<base>", KB_KIF)
        .run()
        .unwrap();
    kb
}

#[test]
fn translate_kb_emits_non_empty_tptp() {
    let mut kb = build_kb();
    let report = TranslateOp::kb(&mut kb).run().unwrap();
    assert!(!report.tptp.is_empty(), "TPTP output should be non-empty for a non-empty KB");
    // Whole-KB path doesn't fill in per-sentence breakouts.
    assert!(report.sentences.is_empty());
}

#[test]
fn translate_formula_breaks_out_each_sentence() {
    let mut kb = build_kb();
    let formula = "(subclass Mammal Animal)";
    let report = TranslateOp::formula(&mut kb, "<inline>", formula)
        .lang(TptpLang::Fof)
        .run()
        .unwrap();
    assert_eq!(report.sentences.len(), 1);
    assert!(report.sentences[0].kif.contains("Mammal"));
    assert!(!report.sentences[0].tptp.is_empty());
    assert_eq!(report.session.as_deref(), Some("<inline>"));
}

#[test]
fn translate_formula_with_kif_comments_prefixes_each() {
    let mut kb = build_kb();
    let report = TranslateOp::formula(&mut kb, "<inline>", "(subclass Mammal Animal)")
        .show_kif_comments(true)
        .run()
        .unwrap();
    assert!(report.tptp.contains("% (subclass"), "kif comment line should be present");
}

#[test]
fn translate_formula_with_unparseable_input_returns_err() {
    let mut kb = build_kb();
    // Translate-vs-validate divergence: garbage input bubbles out as
    // Err here because we can't translate text we couldn't parse.
    let result = TranslateOp::formula(&mut kb, "<bad>", "(subclass Mammal").run();
    assert!(result.is_err());
}
