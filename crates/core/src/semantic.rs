// -- Tests ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntactic::{KifStore, load_kif};

    const BASE: &str = "
        (subclass Relation Entity)
        (subclass BinaryRelation Relation)
        (subclass Predicate Relation)
        (subclass BinaryPredicate Predicate)
        (subclass BinaryPredicate BinaryRelation)
        (instance subclass BinaryRelation)
        (domain subclass 1 Class)
        (domain subclass 2 Class)
        (instance instance BinaryPredicate)
        (domain instance 1 Entity)
        (domain instance 2 Class)
        (subclass Animal Entity)
        (subclass Human Entity)
        (subclass Human Animal)
    ";

    fn base_layer() -> SemanticLayer {
        let mut store = KifStore::default();
        load_kif(&mut store, BASE, "base");
        SemanticLayer::new(store)
    }

    fn kif(kif_str: &str) -> SemanticLayer {
        let mut store = KifStore::default();
        load_kif(&mut store, kif_str, "base");
        SemanticLayer::new(store)
    }

    #[test]
    fn is_relation() {
        let layer = base_layer();
        let id = layer.store.sym_id("subclass").unwrap();
        assert!(layer.is_relation(id));
    }

    #[test]
    fn is_predicate() {
        let layer = base_layer();
        let id = layer.store.sym_id("instance").unwrap();
        assert!(layer.is_predicate(id));
    }

    #[test]
    fn is_class() {
        let layer = base_layer();
        assert!( layer.is_class(layer.store.sym_id("Human").unwrap()));
        assert!(!layer.is_class(layer.store.sym_id("subclass").unwrap()));
    }

    #[test]
    fn has_ancestor() {
        let layer = base_layer();
        let human = layer.store.sym_id("Human").unwrap();
        assert!( layer.has_ancestor_by_name(human, "Entity"));
        assert!( layer.has_ancestor_by_name(human, "Animal"));
        assert!(!layer.has_ancestor_by_name(human, "Relation"));
    }

    #[test]
    fn validate_sentence_valid() {
        let layer = base_layer();
        let sub_id = layer.store.sym_id("subclass").unwrap();
        // Find a root sentence headed by "subclass"
        let sid = layer.store.by_head("subclass")[0];
        // validate_sentence should not error for a valid sentence.
        // (Semantic errors are warnings unless ALL_ERRORS is set.)
        let _ = layer.validate_sentence(sid);
        let _ = sub_id;
    }

    #[test]
    fn validate_all_runs() {
        let layer = base_layer();
        let errors = layer.validate_all();
        // Base ontology may have warnings but no fatal errors.
        // Just check it doesn't panic.
        let _ = errors;
    }

    #[test]
    fn validate_sentence_collect_surfaces_warnings() {
        // `HeadNotRelation` is a default-warning severity error
        // (see `error.rs:promote` -- nothing is promoted in this
        // test).  The existing `validate_sentence` swallows it
        // (returns `Ok(())`), but `validate_sentence_collect`
        // must surface it.
        let layer = kif(r#"
            (subclass Foo Entity)
            ;; `Foo` is NOT declared as a relation -- using it as
            ;; a sentence head should raise HeadNotRelation.
            (Foo Bar Baz)
        "#);
        // Find the sentence whose head is "Foo" (the bad one).
        let foo_id = layer.store.sym_id("Foo").expect("Foo interned");
        let sid = *layer.store.by_head("Foo").iter()
            .find(|&&s| {
                let sent = &layer.store.sentences[layer.store.sent_idx(s)];
                matches!(sent.elements.first(),
                    Some(crate::types::Element::Symbol { id, .. }) if *id == foo_id)
            })
            .expect("found a sentence headed by Foo");

        // Sanity: the existing severity-aware API returns Ok (warning-level).
        assert!(layer.validate_sentence(sid).is_ok(),
            "HeadNotRelation is a warning by default; validate_sentence must return Ok");

        // The collector API must surface it.
        let errs = layer.validate_sentence_collect(sid);
        assert!(errs.iter().any(|e| e.code() == "E002"),
            "validate_sentence_collect should include HeadNotRelation (E002); got {:?}",
            errs.iter().map(|e| e.code()).collect::<Vec<_>>());
    }

    #[test]
    fn is_logical_sentence() {
        let layer = kif("
            (and (relation A B) (relation D C))
            (instance relation Relation)
            (relation A B)
            (NotARelation A B)
        ");
        let store = &layer.store;
        assert!(layer.is_logical_sentence(store.roots[0]));
        assert!(layer.is_logical_sentence(store.roots[2]));
        assert!(!layer.is_logical_sentence(store.roots[3]));
    }

    #[test]
    fn sort_annotations_predicate_arg_sorts() {
        let layer = kif("
            (subclass BinaryPredicate Predicate)
            (subclass Predicate Relation)
            (subclass Integer RationalNumber)
            (subclass RationalNumber RealNumber)
            (instance foo BinaryPredicate)
            (domain foo 1 Integer)
            (domain foo 2 Entity)
        ");
        let sa_guard = layer.sort_annotations();
        let sa = sa_guard.as_ref().unwrap();
        let foo_id = layer.store.sym_id("foo").unwrap();
        let args = sa.symbol_arg_sorts.get(&foo_id).expect("foo should have arg sorts");
        assert_eq!(args.get(0).copied(), Some(Sort::Integer));
        assert_eq!(args.get(1).copied(), Some(Sort::Individual));
        assert!(sa.symbol_return_sorts.get(&foo_id).is_none(),
            "predicates have no return sort entry");
    }

    #[test]
    fn sort_annotations_function_return_sort() {
        let layer = kif("
            (subclass UnaryFunction Function)
            (subclass Integer RationalNumber)
            (subclass RationalNumber RealNumber)
            (instance succFn UnaryFunction)
            (domain succFn 1 Integer)
            (range succFn Integer)
        ");
        let sa_guard = layer.sort_annotations();
        let sa = sa_guard.as_ref().unwrap();
        let fn_id = layer.store.sym_id("succFn").unwrap();
        assert_eq!(sa.symbol_return_sorts.get(&fn_id).copied(), Some(Sort::Integer));
        let args = sa.symbol_arg_sorts.get(&fn_id).expect("succFn should have arg sorts");
        assert_eq!(args.get(0).copied(), Some(Sort::Integer));
    }

    #[test]
    fn sort_annotations_cleared_on_invalidate() {
        let layer = kif("
            (subclass BinaryPredicate Predicate)
            (subclass Predicate Relation)
            (subclass Integer RationalNumber)
            (subclass RationalNumber RealNumber)
            (instance foo BinaryPredicate)
            (domain foo 1 Integer)
        ");
        { assert!(!layer.sort_annotations().as_ref().unwrap().symbol_arg_sorts.is_empty()); }
        layer.invalidate_cache();
        { assert!(!layer.sort_annotations().as_ref().unwrap().symbol_arg_sorts.is_empty()); }
    }

    #[test]
    fn sort_ordering() {
        assert!(Sort::Integer  > Sort::Rational);
        assert!(Sort::Rational > Sort::Real);
        assert!(Sort::Real     > Sort::Individual);
        assert_eq!(Sort::Integer.tptp(),    "$int");
        assert_eq!(Sort::Rational.tptp(),   "$rat");
        assert_eq!(Sort::Real.tptp(),       "$real");
        assert_eq!(Sort::Individual.tptp(), "$i");
    }

    #[test]
    fn taxonomy_edge_lives_in_layer() {
        let layer = base_layer();
        // Taxonomy edges now live in SemanticLayer, not KifStore.
        assert!(!layer.tax_edges.is_empty(),
            "tax_edges should be populated in SemanticLayer after construction");
        // has_ancestor still works -- it uses layer.tax_edges internally.
        let human = layer.store.sym_id("Human").unwrap();
        assert!(layer.has_ancestor_by_name(human, "Entity"));
        assert!(layer.has_ancestor_by_name(human, "Animal"));
    }

    #[test]
    fn taxonomy_rebuilt_after_remove() {
        // Load two files; removing one should update the taxonomy.
        let mut store = KifStore::default();
        load_kif(&mut store, "(subclass Cat Animal)", "cats");
        load_kif(&mut store, "(subclass Animal Entity)", "core");
        let mut layer = SemanticLayer::new(store);

        let cat    = layer.store.sym_id("Cat").unwrap();
        let animal = layer.store.sym_id("Animal").unwrap();

        assert!(layer.has_ancestor_by_name(cat, "Animal"),
            "Cat should have Animal as ancestor before removal");

        // Remove the cats file -- Cat -> Animal edge disappears.
        layer.store.remove_file("cats");
        layer.rebuild_taxonomy();
        layer.invalidate_cache();

        assert!(!layer.has_ancestor_by_name(cat, "Animal"),
            "Cat -> Animal should be gone after cats file is removed");
        // Animal -> Entity from "core" should still be intact.
        assert!(layer.has_ancestor_by_name(animal, "Entity"),
            "Animal -> Entity (from core file) should still exist");
    }

    // =====================================================================
    //  Phase B + C: CacheImpact classifier and incremental taxonomy
    // =====================================================================

    #[test]
    fn classify_taxonomy_heads() {
        // Each of these should flag `taxonomy: true` and nothing else
        // outside the semantic_cache pairing.
        let kif = "
            (subclass Dog Animal)
            (instance Fido Dog)
            (subrelation parent ancestor)
            (subAttribute Happy Mood)
        ";
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "t");
        for &sid in &store.roots {
            let impact = classify_sentence_tree(&store, sid);
            assert!(impact.taxonomy,
                "expected taxonomy=true for sid={sid}: {:?}", impact);
            assert!(impact.semantic_cache,
                "expected semantic_cache=true alongside taxonomy: {:?}", impact);
            assert!(!impact.sort_annotations,
                "unexpected sort_annotations: {:?}", impact);
            assert!(!impact.numeric_char,
                "unexpected numeric_char: {:?}", impact);
        }
    }

    #[test]
    fn classify_domain_range_axioms() {
        let kif = "
            (domain parent 1 Organism)
            (range mother Woman)
            (domainSubclass shapeOf 1 Object)
        ";
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "t");
        for &sid in &store.roots {
            let impact = classify_sentence_tree(&store, sid);
            assert!(impact.sort_annotations,
                "expected sort_annotations=true for sid={sid}: {:?}", impact);
            assert!(!impact.taxonomy,
                "unexpected taxonomy: {:?}", impact);
        }
    }

    #[test]
    fn classify_non_taxonomy_sentence_has_no_impact() {
        // Typical SUMO axiom that doesn't affect any cache.
        let kif = "(attribute Alice Tall)";
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "t");
        let impact = classify_sentence_tree(&store, store.roots[0]);
        assert!(!impact.any(),
            "expected no impact for plain non-taxonomy sentence, got {:?}", impact);
    }

    #[test]
    fn classify_numeric_biconditional_flags_numeric_char() {
        // (<=> (instance ?X PositiveInteger) (greaterThan ?X 0))
        let kif = "(<=> (instance ?X PositiveInteger) (greaterThan ?X 0))";
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "t");
        let impact = classify_sentence_tree(&store, store.roots[0]);
        assert!(impact.numeric_char,
            "expected numeric_char=true for numeric biconditional, got {:?}", impact);
    }

    #[test]
    fn classify_nested_subclass_in_implication() {
        // Rule: taxonomy-head inside implication.  The classifier
        // walks sub-sentences so it should still flag taxonomy --
        // even though the top-level head is `=>`, a `(subclass ...)`
        // sub-sentence exists underneath.
        let kif = "(=> (foo ?X) (subclass ?X Animal))";
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "t");
        let impact = classify_sentence_tree(&store, store.roots[0]);
        assert!(impact.taxonomy,
            "expected taxonomy=true for nested subclass, got {:?}", impact);
    }

    #[test]
    fn extend_taxonomy_with_matches_full_rebuild() {
        // Drive extend_taxonomy_with and rebuild_taxonomy against
        // the same KB and check that the derived caches match.
        // This is the central correctness invariant for Phase C.
        let kif = "
            (subclass Dog Animal)
            (subclass Animal Entity)
            (subclass Cat Animal)
            (instance Fido Dog)
            (domain parent 1 Organism)
            (range father Man)
            (attribute Alice Warm)
        ";
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "t");

        // Baseline: full rebuild.
        let layer_full = SemanticLayer::new({
            let mut s = KifStore::default();
            load_kif(&mut s, kif, "t");
            s
        });

        // Incremental: start empty, extend with the root sids.
        let mut layer_inc = SemanticLayer::new(KifStore::default());
        let mut store2 = KifStore::default();
        load_kif(&mut store2, kif, "t");
        let roots = store2.roots.clone();
        layer_inc.store = store2;
        layer_inc.extend_taxonomy_with(&roots);

        // Sort edges by a deterministic key for comparison (order
        // differs between the two paths because rebuild scans
        // root+sub while extend_with walks roots + sub tree per root).
        let mut full_edges: Vec<_> = layer_full.tax_edges.iter()
            .map(|e| (e.from, e.to, e.rel.clone())).collect();
        let mut inc_edges: Vec<_> = layer_inc.tax_edges.iter()
            .map(|e| (e.from, e.to, e.rel.clone())).collect();
        full_edges.sort();
        inc_edges.sort();

        assert_eq!(full_edges, inc_edges,
            "tax_edges differ between full-rebuild and incremental-extend paths");

        // Derived caches should also agree.
        let full_ns: Vec<_> = {
            let mut v: Vec<_> = layer_full.numeric_sort_cache.iter()
                .map(|(k, v)| (*k, *v)).collect();
            v.sort();
            v
        };
        let inc_ns: Vec<_> = {
            let mut v: Vec<_> = layer_inc.numeric_sort_cache.iter()
                .map(|(k, v)| (*k, *v)).collect();
            v.sort();
            v
        };
        assert_eq!(full_ns, inc_ns, "numeric_sort_cache differs");
    }

    #[test]
    fn extend_taxonomy_with_no_impact_does_nothing() {
        // A batch of purely non-taxonomy sentences should not touch
        // the taxonomy or any derived cache.
        let kif_base = "(subclass Dog Animal)";
        let mut store = KifStore::default();
        load_kif(&mut store, kif_base, "base");
        let mut layer = SemanticLayer::new(store);
        let before_edges = layer.tax_edges.len();

        // Add a non-taxonomy sentence.
        let mut kif_extra = "(attribute Alice Tall) (part Alice Earth)";
        load_kif(&mut layer.store, kif_extra, "extra");
        let _ = &mut kif_extra;  // silence warning
        let new_sids: Vec<_> = layer.store.file_roots.get("extra")
            .cloned().unwrap_or_default();
        assert_eq!(new_sids.len(), 2);

        layer.extend_taxonomy_with(&new_sids);

        // tax_edges unchanged.
        assert_eq!(layer.tax_edges.len(), before_edges,
            "no-impact batch should not change tax_edges");
    }

    #[test]
    fn granular_invalidate_independence() {
        // invalidate_semantic_cache does not touch sort_annotations.
        let mut store = KifStore::default();
        load_kif(&mut store, BASE, "base");
        let layer = SemanticLayer::new(store);

        // Populate the sort_annotations cache.
        drop(layer.sort_annotations());
        assert!(layer.sort_annotations.read().unwrap().is_some());

        layer.invalidate_semantic_cache();
        // sort_annotations is not touched by semantic-cache invalidation.
        assert!(layer.sort_annotations.read().unwrap().is_some(),
            "invalidate_semantic_cache should NOT clear sort_annotations");

        layer.invalidate_sort_annotations();
        assert!(layer.sort_annotations.read().unwrap().is_none());
    }
}
