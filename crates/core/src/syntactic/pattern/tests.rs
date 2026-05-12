//! Tests for the pattern-matching subsystem.

use super::types::{Bindings, MatchKey, PatternElement, SentencePattern, instantiate_pattern};
use super::build::PatternFromKifError;
use crate::parse::ast::OpKind;
use crate::syntactic::SyntacticLayer;
use crate::syntactic::sentence::Sentence;
use crate::types::{Element, ElementVec, InternedSym, Literal, SentenceId, Symbol};
use smallvec::smallvec;

fn make_sentence(elements: ElementVec) -> Sentence {
    Sentence { parent: Vec::new(), elements }
}

/// A symbol element from a name.
fn esym(name: &str) -> Element {
    Element::Symbol(InternedSym(Symbol::from(name)))
}

/// A symbol `MatchKey` from a name (matching is name-based).
fn mkey(name: &str) -> MatchKey {
    MatchKey::Symbol(Symbol::from(name))
}

/// Assert a captured/instantiated element is the named symbol.
fn is_sym(el: Option<&Element>, name: &str) -> bool {
    matches!(el, Some(Element::Symbol(s)) if &*s.name() == name)
}

// -------------------------------------------------------------------------
// match_pattern — Exact
// -------------------------------------------------------------------------

#[test]
fn match_pattern_exact_symbol_succeeds() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        esym("Foo"),
    ]);
    let pattern = SentencePattern(vec![PatternElement::Exact(mkey("Foo"))]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_some());
}

#[test]
fn match_pattern_exact_symbol_mismatch() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        esym("Foo"),
    ]);
    let pattern = SentencePattern(vec![PatternElement::Exact(mkey("Bar"))]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_none());
}

#[test]
fn match_pattern_exact_op_succeeds() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        Element::Op(OpKind::And),
    ]);
    let pattern = SentencePattern(vec![PatternElement::Exact(MatchKey::Op(OpKind::And))]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_some());
}

#[test]
fn match_pattern_exact_literal_succeeds() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        Element::Literal(Literal::Number("0".to_string())),
    ]);
    let pattern = SentencePattern(vec![
        PatternElement::Exact(MatchKey::Literal(Literal::Number("0".to_string()))),
    ]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_some());
}

#[test]
fn match_pattern_exact_literal_mismatch() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        Element::Literal(Literal::Number("1".to_string())),
    ]);
    let pattern = SentencePattern(vec![
        PatternElement::Exact(MatchKey::Literal(Literal::Number("0".to_string()))),
    ]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_none());
}

#[test]
fn match_pattern_length_mismatch_returns_none() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        esym("A"),
        esym("B"),
    ]);
    let pattern = SentencePattern(vec![PatternElement::Exact(mkey("A"))]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_none());
}

// -------------------------------------------------------------------------
// match_pattern — AnyCapture
// -------------------------------------------------------------------------

#[test]
fn match_pattern_any_capture_binds_element() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        esym("Foo"),
    ]);
    let pattern = SentencePattern(vec![PatternElement::AnyCapture(0)]);
    let bindings = store.patterns().match_pattern(&pattern, &sentence).expect("should match");
    assert!(is_sym(bindings.elements.get(&0), "Foo"));
}

#[test]
fn match_pattern_any_capture_consistency_same_element_passes() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        esym("Foo"),
        esym("Foo"),
    ]);
    let pattern = SentencePattern(vec![
        PatternElement::AnyCapture(0),
        PatternElement::AnyCapture(0),
    ]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_some());
}

#[test]
fn match_pattern_any_capture_consistency_different_elements_fails() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        esym("Foo"),
        esym("Bar"),
    ]);
    let pattern = SentencePattern(vec![
        PatternElement::AnyCapture(0),
        PatternElement::AnyCapture(0),
    ]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_none(),
        "inconsistent capture slot should fail");
}

// -------------------------------------------------------------------------
// match_pattern — AnySubSentence
// -------------------------------------------------------------------------

#[test]
fn match_pattern_any_sub_sentence_binds_sid() {
    let store = SyntacticLayer::default();
    let sub_sid: SentenceId = 999;
    let sentence = make_sentence(smallvec![
        Element::Sub(sub_sid),
    ]);
    let pattern = SentencePattern(vec![PatternElement::AnySubSentence(0)]);
    let bindings = store.patterns().match_pattern(&pattern, &sentence).expect("should match");
    assert_eq!(bindings.sub_sids.get(&0), Some(&sub_sid));
}

#[test]
fn match_pattern_any_sub_sentence_rejects_non_sub() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        esym("Foo"),
    ]);
    let pattern = SentencePattern(vec![PatternElement::AnySubSentence(0)]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_none());
}

// -------------------------------------------------------------------------
// instantiate_pattern
// -------------------------------------------------------------------------

#[test]
fn instantiate_pattern_exact_produces_correct_element() {
    let pattern  = SentencePattern(vec![PatternElement::Exact(mkey("Foo"))]);
    let bindings = Bindings::default();
    let elems = instantiate_pattern(&pattern, &bindings).expect("should instantiate");
    assert!(is_sym(elems.get(0), "Foo"));
}

#[test]
fn instantiate_pattern_any_capture_uses_binding() {
    let pattern  = SentencePattern(vec![PatternElement::AnyCapture(0)]);
    let mut bindings = Bindings::default();
    bindings.elements.insert(0, esym("Foo"));
    let elems = instantiate_pattern(&pattern, &bindings).expect("should instantiate");
    assert!(is_sym(elems.get(0), "Foo"));
}

#[test]
fn instantiate_pattern_missing_capture_returns_none() {
    let pattern  = SentencePattern(vec![PatternElement::AnyCapture(0)]);
    let bindings = Bindings::default();
    assert!(instantiate_pattern(&pattern, &bindings).is_none());
}

#[test]
fn instantiate_pattern_any_sub_sentence_uses_binding() {
    let sub_sid: SentenceId = 42;
    let pattern  = SentencePattern(vec![PatternElement::AnySubSentence(0)]);
    let mut bindings = Bindings::default();
    bindings.sub_sids.insert(0, sub_sid);
    let elems = instantiate_pattern(&pattern, &bindings).expect("should instantiate");
    assert!(matches!(&elems[0], Element::Sub(sid) if *sid == sub_sid));
}

// -------------------------------------------------------------------------
// match_pattern — SubPattern (recursive nested matching)
// -------------------------------------------------------------------------

#[test]
fn sub_pattern_matches_nested_antecedent_by_structure() {
    let mut store = SyntacticLayer::default();
    store.load_kif(
        "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))", "test");

    let pat = SentencePattern(vec![
        PatternElement::Exact(MatchKey::Op(OpKind::Implies)),
        PatternElement::SubPattern(Box::new(SentencePattern(vec![
            PatternElement::Exact(mkey("instance")),
            PatternElement::AnyCapture(0),
            PatternElement::Exact(mkey("PositiveInteger")),
        ]))),
        PatternElement::AnySubSentence(1),
    ]);

    let results = store.patterns().find_by_pattern(&pat, None, None);
    assert_eq!(results.len(), 1, "should match the implication");
    let (_, ref bindings) = results[0];
    assert!(bindings.elements.contains_key(&0),
        "slot 0 should be bound to the typed variable (?X)");
    assert!(bindings.sub_sids.contains_key(&1),
        "slot 1 should be bound to the consequent SentenceId");
}

#[test]
fn sub_pattern_does_not_match_wrong_class() {
    let mut store = SyntacticLayer::default();
    store.load_kif("(=> (instance ?X Dog) (Animal ?X))", "test");

    let pat = SentencePattern(vec![
        PatternElement::Exact(MatchKey::Op(OpKind::Implies)),
        PatternElement::SubPattern(Box::new(SentencePattern(vec![
            PatternElement::Exact(mkey("instance")),
            PatternElement::AnyCapture(0),
            PatternElement::Exact(mkey("PositiveInteger")),
        ]))),
        PatternElement::AnySubSentence(1),
    ]);

    assert_eq!(store.patterns().find_by_pattern(&pat, None, None).len(), 0,
        "Dog ≠ PositiveInteger; should not match");
}

#[test]
fn sub_pattern_rejects_non_sub_element() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        esym("Foo"),
    ]);
    let pat = SentencePattern(vec![
        PatternElement::SubPattern(Box::new(SentencePattern(vec![
            PatternElement::AnyCapture(0),
        ]))),
    ]);
    assert!(store.patterns().match_pattern(&pat, &sentence).is_none(),
        "SubPattern on a non-Sub element must fail");
}

// -------------------------------------------------------------------------
// match_pattern — AnyElement
// -------------------------------------------------------------------------

#[test]
fn any_element_matches_non_sub() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        esym("Foo"),
    ]);
    let pattern = SentencePattern(vec![PatternElement::AnyElement(0)]);
    let bindings = store.patterns().match_pattern(&pattern, &sentence).expect("should match");
    assert!(is_sym(bindings.elements.get(&0), "Foo"));
}

#[test]
fn any_element_matches_sub() {
    let store = SyntacticLayer::default();
    let sub_sid: SentenceId = 42;
    let sentence = make_sentence(smallvec![
        Element::Sub(sub_sid),
    ]);
    let pattern = SentencePattern(vec![PatternElement::AnyElement(0)]);
    let bindings = store.patterns().match_pattern(&pattern, &sentence).expect("should match Sub");
    assert!(matches!(bindings.elements.get(&0), Some(Element::Sub(sid)) if *sid == sub_sid));
}

#[test]
fn any_element_consistency_same_symbol_passes() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        esym("Foo"),
        esym("Foo"),
    ]);
    let pattern = SentencePattern(vec![
        PatternElement::AnyElement(0),
        PatternElement::AnyElement(0),
    ]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_some());
}

#[test]
fn any_element_consistency_different_symbols_fails() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        esym("Foo"),
        esym("Bar"),
    ]);
    let pattern = SentencePattern(vec![
        PatternElement::AnyElement(0),
        PatternElement::AnyElement(0),
    ]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_none(),
        "inconsistent AnyElement slot should fail");
}

#[test]
fn any_element_consistency_different_subs_fails() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        Element::Sub(1),
        Element::Sub(2),
    ]);
    let pattern = SentencePattern(vec![
        PatternElement::AnyElement(0),
        PatternElement::AnyElement(0),
    ]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_none(),
        "different Sub sids in same slot should fail");
}

// -------------------------------------------------------------------------
// pattern_from_kif
// -------------------------------------------------------------------------

#[test]
fn pattern_from_kif_simple_match() {
    let mut store = SyntacticLayer::default();
    store.load_kif(
        "(instance Dog Animal)(instance Cat Animal)(instance subclass BinaryRelation)",
        "test");

    let pat = store.patterns().pattern_from_kif("(instance ?X Animal)").expect("should parse");
    let results = store.patterns().find_by_pattern(&pat, Some("instance"), None);
    assert_eq!(results.len(), 2, "Dog and Cat are both instances of Animal");
}

#[test]
fn pattern_from_kif_unknown_symbol_returns_err() {
    let mut store = SyntacticLayer::default();
    store.load_kif("(instance Dog Animal)", "test");

    assert!(matches!(
        store.patterns().pattern_from_kif("(instance ?X Unicorn)"),
        Err(PatternFromKifError::UnknownSymbol(ref s)) if s == "Unicorn"
    ));
}

#[test]
fn pattern_from_kif_empty_returns_no_root_sentence() {
    let store = SyntacticLayer::default();
    assert_eq!(
        store.patterns().pattern_from_kif(""),
        Err(PatternFromKifError::NoRootSentence)
    );
}

#[test]
fn pattern_from_kif_nested_subformula() {
    let mut store = SyntacticLayer::default();
    store.load_kif(
        "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))",
        "test");

    let pat = store.patterns().pattern_from_kif(
        "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))"
    ).expect("should parse");
    let results = store.patterns().find_by_pattern(&pat, None, None);
    assert_eq!(results.len(), 1, "pattern should match the exact sentence");
}

#[test]
fn pattern_from_kif_variable_wildcard() {
    let mut store = SyntacticLayer::default();
    store.load_kif(
        "(instance Dog Animal)(instance Cat Animal)(instance subclass BinaryRelation)",
        "test");

    let pat = store.patterns().pattern_from_kif("(instance ?X ?C)").expect("should parse");
    let results = store.patterns().find_by_pattern(&pat, Some("instance"), None);
    assert_eq!(results.len(), 3);
}

#[test]
fn pattern_from_kif_same_variable_consistency_check() {
    let mut store = SyntacticLayer::default();
    store.load_kif(
        "(instance Dog Dog)(instance Dog Animal)",
        "test");

    let pat = store.patterns().pattern_from_kif("(instance ?X ?X)").expect("should parse");
    let results = store.patterns().find_by_pattern(&pat, Some("instance"), None);
    assert_eq!(results.len(), 1);
}

// -------------------------------------------------------------------------
// find_by_pattern
// -------------------------------------------------------------------------

#[test]
fn find_by_pattern_with_head_filter_returns_matching_sentences() {
    let mut store = SyntacticLayer::default();
    store.load_kif(
        "(instance subclass BinaryRelation)\n(instance instance BinaryPredicate)",
        "test");

    let pat = SentencePattern(vec![
        PatternElement::Exact(mkey("instance")),
        PatternElement::AnyCapture(0),
        PatternElement::Exact(mkey("BinaryRelation")),
    ]);
    let results = store.patterns().find_by_pattern(&pat, Some("instance"), None);
    assert_eq!(results.len(), 1);
}

#[test]
fn find_by_pattern_any_captures_all_instance_sentences() {
    let mut store = SyntacticLayer::default();
    store.load_kif(
        "(instance subclass BinaryRelation)\n(instance instance BinaryPredicate)",
        "test");

    let pat = SentencePattern(vec![
        PatternElement::Exact(mkey("instance")),
        PatternElement::AnyCapture(0),
        PatternElement::AnyCapture(1),
    ]);
    let results = store.patterns().find_by_pattern(&pat, Some("instance"), None);
    assert_eq!(results.len(), 2);
}

#[test]
fn find_recurssive_and_root_level_patterns() {
    let mut store = SyntacticLayer::default();
    store.load_kif("
      (instance GeorgeWashington President)
      (subclass President Human)
      (instance GeorgeWashington Man)
      (subclass Man Human)
      (=> (instance ?X Human) (exists (?M) (mother ?M ?X)))
      (married Martha GeorgeWashington)
      (domain married 1 Wife)
      (domain married 2 Husband)
      (equals FirstPresident GeorgeWashington)
      (subclass Husband Man)
      (=> 
        (and
            (married ?W ?H)
            (mother ?W ?C))
        (father ?H ?C))
    ", "test");

    let pat = SentencePattern(vec![
        PatternElement::Exact(mkey("instance")),
        PatternElement::Exact(mkey("GeorgeWashington")),
        PatternElement::AnyElement(0)
    ]);

    let results = store.patterns().find_by_pattern(
        &pat,
        Some("instance"), 
        Some(vec![Symbol::hash_name("GeorgeWashington")].into_iter().collect())
    );
    assert_eq!(results.len(), 2);

    let results = store.patterns().find_by_pattern_sub(
        &pat,
        Some(vec![Symbol::hash_name("GeorgeWashington")].into_iter().collect())
    );
    assert_eq!(results.len(), 2)
}

// -------------------------------------------------------------------------
// match_pattern — Glob (variable-arity wildcard)
// -------------------------------------------------------------------------

#[test]
fn glob_matches_target_anywhere() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![
        esym("R"), esym("a"), esym("b"), esym("T"), esym("c"),
    ]);
    let pattern = SentencePattern(vec![
        PatternElement::Exact(mkey("R")),
        PatternElement::Glob,
        PatternElement::Exact(mkey("T")),
        PatternElement::Glob,
    ]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_some());
}

#[test]
fn glob_consumes_zero() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![esym("R"), esym("T")]);
    let pattern = SentencePattern(vec![
        PatternElement::Exact(mkey("R")),
        PatternElement::Glob,
        PatternElement::Exact(mkey("T")),
    ]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_some());
}

#[test]
fn glob_trailing_consumes_rest() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![esym("R"), esym("a"), esym("b"), esym("c")]);
    let pattern = SentencePattern(vec![
        PatternElement::Exact(mkey("R")),
        PatternElement::Glob,
    ]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_some());
}

#[test]
fn glob_alone_matches_any_sentence() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![esym("a"), esym("b")]);
    let pattern = SentencePattern(vec![PatternElement::Glob]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_some());
}

#[test]
fn glob_fails_when_next_absent() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![esym("R"), esym("a"), esym("b")]);
    let pattern = SentencePattern(vec![
        PatternElement::Exact(mkey("R")),
        PatternElement::Glob,
        PatternElement::Exact(mkey("T")),
    ]);
    assert!(store.patterns().match_pattern(&pattern, &sentence).is_none());
}

#[test]
fn glob_then_capture_binds_last_element() {
    // End-anchored: the lazy glob slides so the trailing capture lands on the
    // last element, (R a b X) → slot 0 == X.
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![esym("R"), esym("a"), esym("b"), esym("X")]);
    let pattern = SentencePattern(vec![
        PatternElement::Exact(mkey("R")),
        PatternElement::Glob,
        PatternElement::AnyCapture(0),
    ]);
    let b = store.patterns().match_pattern(&pattern, &sentence).expect("should match");
    assert!(is_sym(b.elements.get(&0), "X"));
}

#[test]
fn glob_capture_records_consumed_count() {
    let store = SyntacticLayer::default();
    let sentence = make_sentence(smallvec![esym("R"), esym("a"), esym("b"), esym("T")]);
    let pattern = SentencePattern(vec![
        PatternElement::Exact(mkey("R")),
        PatternElement::GlobCapture(0),
        PatternElement::Exact(mkey("T")),
    ]);
    let b = store.patterns().match_pattern(&pattern, &sentence).expect("should match");
    assert_eq!(b.glob_lens.get(&0), Some(&2));

    let s2 = make_sentence(smallvec![esym("R"), esym("T")]);
    let b2 = store.patterns().match_pattern(&pattern, &s2).expect("should match");
    assert_eq!(b2.glob_lens.get(&0), Some(&0));
}

#[test]
fn glob_consistency_check_holds_across_span() {
    let store = SyntacticLayer::default();
    let pattern = SentencePattern(vec![
        PatternElement::AnyCapture(0),
        PatternElement::Glob,
        PatternElement::AnyCapture(0),
    ]);
    let ok = make_sentence(smallvec![esym("S"), esym("a"), esym("b"), esym("S")]);
    assert!(store.patterns().match_pattern(&pattern, &ok).is_some(), "matching ends → match");
    let bad = make_sentence(smallvec![esym("S"), esym("a"), esym("b"), esym("Z")]);
    assert!(store.patterns().match_pattern(&pattern, &bad).is_none(), "mismatched ends → no match");
}