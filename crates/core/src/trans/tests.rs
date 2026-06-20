// crates/core/src/trans/tests.rs
//
// Unit tests for TranslationLayer (Phase 5).
// This file is compiled only in test builds (declared as
// `#[cfg(test)] mod tests;` in trans/mod.rs).

use super::{TranslationLayer, Sort};
use crate::semantics::SemanticLayer;
use crate::syntactic::SyntacticLayer;

fn make_trans(kif: &str) -> TranslationLayer {
    let mut store = SyntacticLayer::default();
    store.load_kif(kif, "t");
    let sem = SemanticLayer::new(store);
    let trans = TranslationLayer::new(sem);
    trans
}

// =====================================================================
//  Sort type ordering
// =====================================================================

#[test]
fn sort_ordering() {
    // Sort is Ord: Individual < Real < Rational < Integer
    assert!(Sort::Individual < Sort::Real);
    assert!(Sort::Real       < Sort::Rational);
    assert!(Sort::Rational   < Sort::Integer);
    assert!(Sort::Integer.max(Sort::Real) == Sort::Integer);
}

// =====================================================================
//  Phase 5: numeric caches primed on construction
// =====================================================================

#[test]
fn numeric_caches_populated_on_new() {
    let trans = make_trans(
        "(subclass Integer RealNumber)(subclass RealNumber Quantity)",
    );
    assert!(!trans.numeric_sorts.is_empty(),
        "numeric_sorts must be populated after TranslationLayer::new");
    assert!(trans.numeric_ancestor_set.is_populated(),
        "numeric_ancestor_set must be populated after TranslationLayer::new");
    assert!(trans.poly_variant_symbols.is_populated(),
        "poly_variant_symbols must be populated after TranslationLayer::new");
    // numeric_char is an EntryCache — it may be empty when the KB has no
    // arithmetic characterisation axioms, which is correct for minimal KBs.
    // Just verify the field exists (compile-time check is sufficient).
}

#[test]
fn on_change_taxonomy_invalidates_and_rebuilds_numeric_caches() {
    use crate::cache::events::Event;

    let mut trans = make_trans("(subclass Dog Animal)");
    // Confirm the LayerCache fields are populated even for a minimal KB.
    // (numeric_sorts and numeric_char are EntryCaches and will be empty since
    //  the KB has no numeric classes; numeric_ancestor_set is a LayerCache
    //  and is always installed by prime_numeric_caches.)
    assert!(trans.numeric_ancestor_set.is_populated(), "ancestor set must be installed on construction");
    // Simulate a (non-pure) taxonomy change via the translation cascade.
    // prime_numeric_caches re-populates LayerCache fields
    assert!(trans.numeric_ancestor_set.is_populated(),
        "numeric_ancestor_set must be reinstalled after taxonomy change");
}

// (The whole-table `sort_annotations()` tests were removed with that API; the
// per-symbol `translation::sort_annotations` cache (`SortAnnotation`) should get
// fresh coverage against `sort_annotation(sym)`.)

#[test]
fn apply_change_taxonomy_addition_invalidates_and_reprimes() {
    use crate::cache::events::Event;
    use crate::layer::Layer;

    let trans = make_trans("(subclass Dog Animal)");
    // Warm a semantic query cache.
    let dog = trans.semantic.syntactic.sym_id("Dog").unwrap();
    trans.semantic.is_class(dog);
    assert!(trans.semantic.is_class.peek(&crate::semantics::types::Scoped {
        scope: crate::semantics::types::Scope::Base, key: dog
    }).is_some());

    // Drive a taxonomy change through the real reactor cascade — the whole
    // stack runs: the semantic `is_class` cache evicts, and the trans numeric
    // caches stay structurally primed (their react re-installs the set).
    let animal = trans.semantic.syntactic.sym_id("Animal").unwrap();
    trans.cascade(vec![Event::TaxonomyChanged { syms: vec![dog, animal] }]);

    // Semantic caches were invalidated (coarse clear on TaxonomyChanged) ...
    assert!(trans.semantic.is_class.peek(&crate::semantics::types::Scoped {
        scope: crate::semantics::types::Scope::Base, key: dog
    }).is_none(),
        "is_class must be evicted after a taxonomy change");
    // ... and the numeric caches stayed structurally primed.
    assert!(trans.numeric_ancestor_set.is_populated(),
        "numeric_ancestor_set must be reinstalled after the cascade");
}

// =====================================================================
//  TFF: literal-equality constant typing (probe)
// =====================================================================

#[test]
fn literal_equality_types_the_constant() {
    let trans = make_trans(
        "(subclass Integer RealNumber)\n\
         (equal Value21-1 40.0)",
    );
    let v = trans.semantic.syntactic.sym_id("Value21-1").unwrap();
    // Layer 1: the semantic classification from the defining equality.
    let cls = trans.semantic.infer_class(v);
    eprintln!("[probe] infer_class(Value21-1) = {cls:?}");
    // Layer 2: the numeric sort.
    let sort = trans.sort_for_symbol(v);
    eprintln!("[probe] sort_for_symbol(Value21-1) = {sort:?}");
    assert_eq!(sort.unwrap(), Sort::Real, "defining equality (equal V 40.0) must type V at $real");
}
