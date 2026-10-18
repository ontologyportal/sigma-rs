use super::*;
use crate::syntactic::SyntacticLayer;
use crate::types::{SentenceId, SymbolId};
use std::collections::HashSet;

/// Loads each KIF clause separately and returns root sids in the order the
/// clauses appear in `clauses`.
fn load_in_order(store: &mut SyntacticLayer, clauses: &[&str]) -> Vec<SentenceId> {
    let mut out = Vec::with_capacity(clauses.len());
    // Each clause needs its own file tag, unique across calls; reusing a tag
    // re-ingests as a replacement and retracts the earlier axiom from SInE.
    let base = store.source_files().len();
    for (i, clause) in clauses.iter().enumerate() {
        let before: HashSet<SentenceId> = store.root_sids().into_iter().collect();
        let errs = store.load_kif_assert(clause, &format!("test{}", base + i));
        assert!(errs.is_empty(), "load errors: {:?}", errs);
        let after: HashSet<SentenceId> = store.root_sids().into_iter().collect();
        let mut new_roots: Vec<SentenceId> = after.difference(&before).copied().collect();
        assert_eq!(
            new_roots.len(),
            1,
            "clause {i:?} produced {} roots",
            new_roots.len()
        );
        out.push(new_roots.pop().unwrap());
    }
    out
}

/// Loads a multi-clause KIF string (one clause per line) and returns root
/// sids in the order the clauses appear.
fn store_and_axioms(kif: &str) -> (SyntacticLayer, Vec<SentenceId>) {
    let mut store = SyntacticLayer::default();
    let clauses: Vec<&str> = kif
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let axioms = load_in_order(&mut store, &clauses);
    (store, axioms)
}

/// Builds the SInE index for `store` from `axioms`.
fn build_eager(store: &mut SyntacticLayer, axioms: &[SentenceId]) {
    for &sid in axioms {
        store.sine_add_axiom(sid);
    }
}

/// Asserts the D-relation of `store.sine` matches a from-scratch build
/// over the same axiom set.
fn assert_matches_from_scratch(store: &mut SyntacticLayer, axioms: &[SentenceId], tol: f32) {
    let pairs: Vec<(SentenceId, HashSet<SymbolId>)> = axioms
        .iter()
        .filter_map(|&sid| {
            store
                .sine
                .with_ref(|idx| idx.symbols_of_axiom(sid).map(|s| (sid, s.clone())))
        })
        .collect();
    let mut scratch_idx = SineIndex::default();
    scratch_idx.rebuild_from(pairs);

    let store_axiom_count = store.sine.with_ref(|idx| idx.axiom_count());
    assert_eq!(store_axiom_count, scratch_idx.axiom_count(), "axiom count");

    for &sid in axioms {
        let store_syms = store
            .sine
            .with_ref(|idx| idx.symbols_of_axiom(sid).cloned());
        assert_eq!(
            store_syms,
            scratch_idx.symbols_of_axiom(sid).cloned(),
            "symbols of axiom {} differ",
            sid,
        );
    }
    let all_syms: HashSet<SymbolId> = axioms
        .iter()
        .filter_map(|&sid| scratch_idx.symbols_of_axiom(sid))
        .flat_map(|s| s.iter().copied())
        .collect();
    for s in all_syms {
        let idx_triggered = store.select_axioms(&HashSet::from([s]), tol, Some(1), false);
        let scr_triggered = scratch_idx.select(&HashSet::from([s]), tol, Some(1), false);
        assert_eq!(
            idx_triggered, scr_triggered,
            "triggered axioms mismatch for symbol {} at tol {}",
            s, tol
        );
        let store_gen = store.sine.with_ref(|idx| idx.generality(s));
        assert_eq!(
            store_gen,
            scratch_idx.generality(s),
            "generality for symbol {} differs",
            s
        );
    }
}

#[test]
fn generality_counts_distinct_axiom_occurrences() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)",
    );
    build_eager(&mut store, &axioms);

    let sub = store.sym_id("subclass").unwrap();
    let human = store.sym_id("Human").unwrap();
    let animal = store.sym_id("Animal").unwrap();
    let mammal = store.sym_id("Mammal").unwrap();
    let dog = store.sym_id("Dog").unwrap();

    assert_eq!(store.sine.with_ref(|idx| idx.generality(sub)), 3);
    assert_eq!(store.sine.with_ref(|idx| idx.generality(human)), 1);
    assert_eq!(store.sine.with_ref(|idx| idx.generality(animal)), 2);
    assert_eq!(store.sine.with_ref(|idx| idx.generality(mammal)), 2);
    assert_eq!(store.sine.with_ref(|idx| idx.generality(dog)), 1);

    assert_eq!(store.axiom_sentences_of(sub).len(), 3);
    assert_eq!(store.axiom_sentences_of(human).len(), 1);
    assert_eq!(store.axiom_sentences_of(animal).len(), 2);
}

#[test]
fn trigger_relation_strict_picks_least_general() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)",
    );
    build_eager(&mut store, &axioms);

    let sub = store.sym_id("subclass").unwrap();
    let human = store.sym_id("Human").unwrap();
    let animal = store.sym_id("Animal").unwrap();
    let mammal = store.sym_id("Mammal").unwrap();
    let dog = store.sym_id("Dog").unwrap();

    assert!(
        store
            .select_axioms(&HashSet::from([sub]), 1.0, Some(1), false)
            .is_empty(),
        "subclass should trigger nothing strict",
    );

    assert_eq!(
        store.select_axioms(&HashSet::from([human]), 1.0, Some(1), false),
        HashSet::from([axioms[0]])
    );
    assert_eq!(
        store.select_axioms(&HashSet::from([dog]), 1.0, Some(1), false),
        HashSet::from([axioms[2]])
    );
    assert_eq!(
        store.select_axioms(&HashSet::from([animal]), 1.0, Some(1), false),
        HashSet::from([axioms[1]])
    );
    assert_eq!(
        store.select_axioms(&HashSet::from([mammal]), 1.0, Some(1), false),
        HashSet::from([axioms[1]])
    );
}

#[test]
fn incremental_add_is_transitively_correct() {
    let kif = "(subclass Human Animal)\n\
                   (subclass Mammal Animal)\n\
                   (subclass Dog Mammal)\n\
                   (instance Rex Dog)";
    let (mut store, axioms) = store_and_axioms(kif);
    build_eager(&mut store, &axioms);
    assert_matches_from_scratch(&mut store, &axioms, 1.0);
}

#[test]
fn incremental_add_only_touches_affected_axioms() {
    let mut store = SyntacticLayer::default();
    let first_two = load_in_order(
        &mut store,
        &["(subclass Human Animal)", "(subclass Dog Mammal)"],
    );
    for &sid in &first_two {
        store.sine_add_axiom(sid);
    }

    let human = store.sym_id("Human").unwrap();
    let animal = store.sym_id("Animal").unwrap();
    let before_human = store.select_axioms(&HashSet::from([human]), 1.0, Some(1), false);
    let before_animal = store.select_axioms(&HashSet::from([animal]), 1.0, Some(1), false);

    let new_sid = load_in_order(&mut store, &["(instance Pi Constant)"])[0];
    store.sine_add_axiom(new_sid);

    assert_eq!(
        store.select_axioms(&HashSet::from([human]), 1.0, Some(1), false),
        before_human
    );
    assert_eq!(
        store.select_axioms(&HashSet::from([animal]), 1.0, Some(1), false),
        before_animal
    );

    assert!(store.sine.with_ref(|idx| idx.contains(new_sid)));
    let pi = store.sym_id("Pi").unwrap();
    assert!(store
        .select_axioms(&HashSet::from([pi]), 1.0, Some(1), false)
        .contains(&new_sid));
}

#[test]
fn incremental_add_shifts_trigger_entries_for_shared_symbols() {
    let mut store = SyntacticLayer::default();
    let a0 = load_in_order(&mut store, &["(subclass Human Animal)"])[0];

    store.sine_add_axiom(a0);
    let human_id = store.sym_id("Human").unwrap();
    let animal_id = store.sym_id("Animal").unwrap();
    assert!(store
        .select_axioms(&HashSet::from([human_id]), 1.0, Some(1), false)
        .contains(&a0));
    assert!(store
        .select_axioms(&HashSet::from([animal_id]), 1.0, Some(1), false)
        .contains(&a0));

    let a1 = load_in_order(&mut store, &["(subclass Dog Animal)"])[0];
    store.sine_add_axiom(a1);

    assert!(
        store
            .select_axioms(&HashSet::from([human_id]), 1.0, Some(1), false)
            .contains(&a0),
        "Human should still trigger axiom 0",
    );
    assert!(
        !store
            .select_axioms(&HashSet::from([animal_id]), 1.0, Some(1), false)
            .contains(&a0),
        "Animal should no longer trigger axiom 0 after occ bump; \
             triggered={:?}",
        store.select_axioms(&HashSet::from([animal_id]), 1.0, Some(1), false),
    );
}

#[test]
fn remove_axiom_restores_pre_add_state() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Human Animal)\n\
             (subclass Mammal Animal)",
    );
    build_eager(&mut store, &axioms);
    let target = axioms[1];
    store.sine_remove_axiom(target);
    store.sine_add_axiom(target);
    assert_matches_from_scratch(&mut store, &axioms, 1.0);
}

#[test]
fn select_is_stateless_and_monotone_in_tolerance() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)",
    );
    build_eager(&mut store, &axioms);

    let human = store.sym_id("Human").unwrap();
    let seed = HashSet::from([human]);

    let at_1 = store.select_axioms(&seed, 1.0, None, false);
    let at_2 = store.select_axioms(&seed, 2.0, None, false);
    let at_3 = store.select_axioms(&seed, 3.0, None, false);

    assert!(at_1.is_subset(&at_2), "t=1 result should be subset of t=2");
    assert!(at_2.is_subset(&at_3), "t=2 result should be subset of t=3");

    let at_1_again = store.select_axioms(&seed, 1.0, None, false);
    assert_eq!(at_1, at_1_again, "select must not mutate index state");
}

#[test]
fn threshold_correct_after_occ_bump() {
    let mut store = SyntacticLayer::default();
    let ax0 = load_in_order(&mut store, &["(instance Rex Dog)"])[0];
    store.sine_add_axiom(ax0);

    let rex = store.sym_id("Rex").unwrap();
    assert!(store
        .select_axioms(&HashSet::from([rex]), 1.0, Some(1), false)
        .contains(&ax0));

    let ax1 = load_in_order(&mut store, &["(instance Rex Cat)"])[0];
    store.sine_add_axiom(ax1);

    assert!(
        !store
            .select_axioms(&HashSet::from([rex]), 1.5, Some(1), false)
            .contains(&ax0),
        "ax0 should not be triggered by Rex at tolerance 1.5 after occ bump",
    );
    assert!(
        store
            .select_axioms(&HashSet::from([rex]), 2.0, Some(1), false)
            .contains(&ax0),
        "ax0 should be triggered by Rex at tolerance 2.0",
    );
}

#[test]
fn selection_reaches_transitive_axioms() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)\n\
             (instance Rex Dog)",
    );
    build_eager(&mut store, &axioms);

    let dog_id = store.sym_id("Dog").unwrap();
    let seed: HashSet<SymbolId> = [dog_id].into_iter().collect();
    let selected = store.select_axioms(&seed, 1.0, None, false);

    let expected: HashSet<SentenceId> = [axioms[1], axioms[2]].into_iter().collect();
    assert_eq!(
        selected, expected,
        "got {:?}, expected {:?}",
        selected, expected
    );
}

#[test]
fn selection_respects_depth_limit() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)\n\
             (instance Rex Dog)",
    );
    build_eager(&mut store, &axioms);
    let rex = store.sym_id("Rex").unwrap();
    let seed: HashSet<SymbolId> = [rex].into_iter().collect();
    let shallow = store.select_axioms(&seed, 1.0, Some(1), false);
    assert_eq!(shallow, HashSet::from([axioms[3]]));
}

#[test]
fn selection_empty_seed_yields_empty_result() {
    let (mut store, axioms) = store_and_axioms("(subclass Human Animal)");
    build_eager(&mut store, &axioms);
    assert!(store
        .select_axioms(&HashSet::new(), 1.2, None, false)
        .is_empty());
}

#[test]
fn add_axiom_tolerates_unknown_sid() {
    let (mut store, axioms) = store_and_axioms("(subclass Human Animal)");
    build_eager(&mut store, &axioms);
    store.sine_add_axiom(99_999_999);
    assert_eq!(store.sine.with_ref(|idx| idx.axiom_count()), 1);
}

#[test]
fn add_axiom_is_idempotent() {
    let (mut store, axioms) = store_and_axioms("(subclass Human Animal)");
    store.sine_add_axiom(axioms[0]);
    store.sine_add_axiom(axioms[0]);
    assert_eq!(store.sine.with_ref(|idx| idx.axiom_count()), 1);
}

#[test]
fn remove_unknown_sid_is_noop() {
    let (mut store, axioms) = store_and_axioms("(subclass Human Animal)");
    build_eager(&mut store, &axioms);
    let before = store.sine.with_ref(|idx| idx.axiom_count());
    store.sine_remove_axiom(99_999_999);
    assert_eq!(store.sine.with_ref(|idx| idx.axiom_count()), before);
}

#[test]
fn remove_then_readd_matches_from_scratch() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Human Animal)\n\
             (subclass Dog Mammal)\n\
             (=> (instance ?X Dog) (instance ?X Animal))",
    );
    build_eager(&mut store, &axioms);
    store.sine_remove_axiom(axioms[1]);
    store.sine_add_axiom(axioms[1]);
    assert_matches_from_scratch(&mut store, &axioms, 1.2);
}

#[test]
fn remove_axiom_decrements_generality() {
    let (mut store, axioms) = store_and_axioms("(subclass Dog Mammal)\n(subclass Cat Mammal)");
    build_eager(&mut store, &axioms);
    let mammal = store.sym_id("Mammal").expect("Mammal interned");
    assert_eq!(store.sine.with_ref(|idx| idx.generality(mammal)), 2);
    store.sine_remove_axiom(axioms[0]);
    assert_eq!(store.sine.with_ref(|idx| idx.generality(mammal)), 1);
    assert!(!store.sine.with_ref(|idx| idx.contains(axioms[0])));
    assert!(store.sine.with_ref(|idx| idx.contains(axioms[1])));
}

#[test]
fn remove_axiom_unindexes_its_triggers() {
    let (mut store, axioms) = store_and_axioms("(subclass Dog Mammal)\n(subclass Cat Mammal)");
    build_eager(&mut store, &axioms);
    let dog = store.sym_id("Dog").expect("Dog interned");
    assert!(
        store
            .select_axioms(&HashSet::from([dog]), 1.0, Some(1), false)
            .contains(&axioms[0]),
        "Dog should trigger axiom 0 before removal",
    );
    store.sine_remove_axiom(axioms[0]);
    assert!(
        !store
            .select_axioms(&HashSet::from([dog]), 1.0, Some(1), false)
            .contains(&axioms[0]),
        "Dog must not trigger axiom 0 after removal",
    );
    assert!(store
        .sine
        .with_ref(|idx| idx.symbols_of_axiom(axioms[0]).is_none()));
}

#[test]
fn remove_axiom_recomputes_affected_triggers() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Dog Mammal)\n\
             (subclass Mammal Animal)\n\
             (subclass Cat Mammal)\n\
             (subclass Animal Entity)",
    );
    build_eager(&mut store, &axioms);
    store.sine_remove_axiom(axioms[1]);
    let kept: Vec<_> = [axioms[0], axioms[2], axioms[3]].into_iter().collect();
    assert_matches_from_scratch(&mut store, &kept, 1.2);
}

#[test]
fn remove_is_idempotent() {
    let (mut store, axioms) = store_and_axioms("(subclass Human Animal)\n(subclass Dog Mammal)");
    build_eager(&mut store, &axioms);
    store.sine_remove_axiom(axioms[0]);
    store.sine_remove_axiom(axioms[0]);
    assert_eq!(store.sine.with_ref(|idx| idx.axiom_count()), 1);
    assert!(!store.sine.with_ref(|idx| idx.contains(axioms[0])));
}

#[test]
fn remove_axioms_batch_equivalent_to_singletons() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Dog Mammal)\n\
             (subclass Cat Mammal)\n\
             (subclass Mammal Animal)\n\
             (subclass Animal Entity)",
    );
    build_eager(&mut store, &axioms);
    store.sine_remove_axiom(axioms[0]);
    store.sine_remove_axiom(axioms[2]);
    let kept = [axioms[1], axioms[3]];
    assert_matches_from_scratch(&mut store, &kept, 1.2);

    let (mut store_b, axioms_b) = store_and_axioms(
        "(subclass Dog Mammal)\n\
             (subclass Cat Mammal)\n\
             (subclass Mammal Animal)\n\
             (subclass Animal Entity)",
    );
    build_eager(&mut store_b, &axioms_b);
    store_b
        .sine
        .modify(|idx| idx.remove_axioms([axioms_b[0], axioms_b[2]]));
    assert_matches_from_scratch(&mut store_b, &kept, 1.2);
}

#[test]
fn remove_axiom_bumps_stats_counter() {
    let (mut store, axioms) = store_and_axioms("(subclass Dog Mammal)");
    build_eager(&mut store, &axioms);
    let _ = store.sine.update_with(|idx| idx.take_stats());
    store.sine_remove_axiom(axioms[0]);
    let s = store.sine.update_with(|idx| idx.take_stats());
    assert_eq!(s.removes, 1, "removes counter must tick: {:?}", s);
}

#[test]
fn add_axioms_bulk_matches_incremental() {
    let mut src = String::new();
    for i in 0..60 {
        src.push_str(&format!("(subclass Class{} Entity)\n", i));
    }
    let (mut store, axioms) = store_and_axioms(&src);
    store.sine_add_axioms(axioms.iter().copied());
    assert_matches_from_scratch(&mut store, &axioms, 1.0);

    let stats = store.sine.update_with(|idx| idx.take_stats());
    assert!(
        stats.bulk_rebuilds >= 1,
        "expected bulk rebuild for 60-axiom batch, got {:?}",
        stats
    );
}

#[test]
fn add_axioms_small_batch_takes_incremental_path() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)\n\
             (subclass Cat Mammal)\n\
             (instance Rex Dog)\n\
             (instance Whiskers Cat)",
    );
    store.sine_add_axioms(axioms[..4].iter().copied());
    let _ = store.sine.update_with(|idx| idx.take_stats());

    store.sine_add_axioms(axioms[4..].iter().copied());
    let stats = store.sine.update_with(|idx| idx.take_stats());
    assert_eq!(
        stats.bulk_rebuilds, 0,
        "small follow-up batch should NOT trigger a bulk rebuild"
    );
    assert!(
        stats.calls >= 2,
        "incremental add_axiom should have been called at least twice: {:?}",
        stats
    );

    assert_matches_from_scratch(&mut store, &axioms, 1.0);
}

#[test]
fn add_axioms_large_followup_batch_triggers_rebuild() {
    let mut src = String::new();
    src.push_str("(subclass Human Animal)\n");
    src.push_str("(subclass Mammal Animal)\n");
    src.push_str("(subclass Dog Mammal)\n");
    src.push_str("(subclass Cat Mammal)\n");
    src.push_str("(instance Rex Dog)\n");
    for i in 0..80 {
        src.push_str(&format!("(subclass Class{} Entity)\n", i));
    }
    let (mut store, axioms) = store_and_axioms(&src);
    let (first5, rest): (Vec<_>, Vec<_>) = axioms.iter().enumerate().partition(|(i, _)| *i < 5);
    let first5: Vec<SentenceId> = first5.into_iter().map(|(_, sid)| *sid).collect();
    let rest: Vec<SentenceId> = rest.into_iter().map(|(_, sid)| *sid).collect();

    store.sine_add_axioms(first5.iter().copied());
    let _ = store.sine.update_with(|idx| idx.take_stats());

    store.sine_add_axioms(rest.iter().copied());
    let stats = store.sine.update_with(|idx| idx.take_stats());
    assert!(
        stats.bulk_rebuilds >= 1,
        "80-axiom batch over 5 existing should rebuild, got {:?}",
        stats
    );

    assert_matches_from_scratch(&mut store, &axioms, 1.2);
}

#[test]
fn add_axioms_empty_batch_is_noop() {
    let (mut store, axioms) = store_and_axioms("(subclass Human Animal)");
    store.sine_add_axioms(axioms.iter().copied());
    let before = store.sine.with_ref(|idx| idx.axiom_count());
    let empty: Vec<SentenceId> = Vec::new();
    store.sine_add_axioms(empty);
    assert_eq!(store.sine.with_ref(|idx| idx.axiom_count()), before);
}

#[test]
fn occ_bump_correctly_updates_thresholds() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)\n\
             (subclass Bird Animal)",
    );
    store.sine_add_axioms(axioms[..1].iter().copied());
    let _ = store.sine.update_with(|idx| idx.take_stats());
    for &sid in &axioms[1..] {
        store.sine_add_axiom(sid);
    }
    assert_matches_from_scratch(&mut store, &axioms, 1.2);
}

#[test]
fn remove_axiom_occ_decrease_restores_trigger() {
    let mut store = SyntacticLayer::default();
    let axioms = load_in_order(
        &mut store,
        &["(subclass Human Animal)", "(subclass Dog Animal)"],
    );
    let ax0 = axioms[0];
    let ax1 = axioms[1];

    store.sine_add_axiom(ax0);
    store.sine_add_axiom(ax1);

    let animal = store.sym_id("Animal").unwrap();

    assert!(!store
        .select_axioms(&HashSet::from([animal]), 1.0, Some(1), false)
        .contains(&ax0));

    store.sine_remove_axiom(ax1);
    assert!(
        store
            .select_axioms(&HashSet::from([animal]), 1.0, Some(1), false)
            .contains(&ax0),
        "Animal should trigger ax0 again after occ decrements back to 1",
    );
}

#[test]
fn g_min_cache_matches_manual_computation_after_bulk() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)",
    );
    store.sine_add_axioms(axioms.iter().copied());

    for &sid in &axioms {
        let syms = store
            .sine
            .with_ref(|idx| idx.symbols_of_axiom(sid).cloned())
            .unwrap();
        let min_actual = syms
            .iter()
            .map(|&s| store.sine.with_ref(|idx| idx.generality(s)))
            .min()
            .unwrap();
        let threshold = min_actual as f32;
        for s in syms {
            let g = store.sine.with_ref(|idx| idx.generality(s));
            let is_trigger = store
                .select_axioms(&HashSet::from([s]), 1.0, Some(1), false)
                .contains(&sid);
            assert_eq!(
                g <= threshold as usize,
                is_trigger,
                "sid={} sym={} generality={} threshold={} triggered={}",
                sid,
                s,
                g,
                threshold,
                is_trigger
            );
        }
    }
}

#[test]
fn sym_to_owned_is_empty_for_high_occ_symbol() {
    let mut src = String::new();
    for i in 0..20 {
        src.push_str(&format!("(subclass Class{i} Entity)\n"));
    }
    let (mut store, axioms) = store_and_axioms(&src);
    build_eager(&mut store, &axioms);

    let sub = store.sym_id("subclass").unwrap();
    let owned_count = store
        .sine
        .with_ref(|idx| idx.sym_to_owned.get(&sub).map_or(0, |s| s.len()));
    assert_eq!(
        owned_count, 0,
        "high-occ symbol 'subclass' should not own g_min for any axiom"
    );
}

// -- Auto-tolerance / budget selection -----------------------------------

/// The 4-axiom taxonomy used by the auto-tolerance tests, seeded from
/// `Rex`.  occ: subclass=3, Animal=2, Mammal=2, Dog=2, Human=1,
/// instance=1, Rex=1.  g_min: a0=1, a1=2, a2=2, a3=1.  Selecting from
/// `Rex` yields {a1,a2,a3} at t=1.0 and gains a0 (the `Human` axiom) at
/// t=2.0; the activation breakpoints are 1.5, 2.0, 3.0.
fn rex_kb() -> (SyntacticLayer, Vec<SentenceId>, SymbolId) {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Human Animal)\n\
             (subclass Mammal Animal)\n\
             (subclass Dog Mammal)\n\
             (instance Rex Dog)",
    );
    build_eager(&mut store, &axioms);
    let rex = store.sym_id("Rex").unwrap();
    (store, axioms, rex)
}

#[test]
fn tolerance_breakpoints_are_sorted_distinct_and_exact() {
    let (store, _axioms, rex) = rex_kb();
    let seed = HashSet::from([rex]);
    let bps = store
        .sine
        .with_ref(|idx| idx.tolerance_breakpoints(&seed, None, false, MAX_AUTO_TOLERANCE));
    assert_eq!(
        bps,
        vec![1.5, 2.0, 3.0],
        "unexpected breakpoints: {:?}",
        bps
    );
    assert!(bps.windows(2).all(|w| w[0] < w[1]));
    assert!(bps.iter().all(|&t| t > 1.0));
}

#[test]
fn within_budget_finds_largest_tolerance_under_cap() {
    let (store, axioms, rex) = rex_kb();
    let seed = HashSet::from([rex]);

    // Budget 3: the floor set {a1,a2,a3} fills it; a0 enters at t=2.0 and
    // would overrun.  Binary search returns the *largest* tolerance still
    // in budget — anywhere in [1.5, 2.0) — with the 3-axiom set.
    let (t, set) = store
        .sine
        .with_ref(|idx| idx.select_within_budget(&seed, 3, None, false));
    assert_eq!(set.len(), 3);
    assert!((1.5..2.0).contains(&t), "tolerance {} not in [1.5, 2.0)", t);
    let expected: HashSet<SentenceId> = [axioms[1], axioms[2], axioms[3]].into_iter().collect();
    assert_eq!(set, expected);
}

#[test]
fn within_budget_returns_floor_when_floor_overruns() {
    let (store, _axioms, rex) = rex_kb();
    let seed = HashSet::from([rex]);
    // Budget 2 < floor size 3: nothing smaller exists, so the strict
    // floor (t=1.0) is returned even though it exceeds the budget.
    let (t, set) = store
        .sine
        .with_ref(|idx| idx.select_within_budget(&seed, 2, None, false));
    assert_eq!(t, 1.0);
    assert_eq!(set.len(), 3);
}

#[test]
fn within_budget_large_reaches_fixed_point() {
    let (store, axioms, rex) = rex_kb();
    let seed = HashSet::from([rex]);
    let (_t, set) = store
        .sine
        .with_ref(|idx| idx.select_within_budget(&seed, 10_000, None, false));
    // All four axioms are reachable from Rex at the fixed point.
    let all: HashSet<SentenceId> = axioms.iter().copied().collect();
    assert_eq!(set, all);
}

#[test]
fn within_budget_result_matches_fixed_select_at_chosen_tolerance() {
    let (store, _axioms, rex) = rex_kb();
    let seed = HashSet::from([rex]);
    for budget in [1usize, 2, 3, 4, 100] {
        let (t, set) = store
            .sine
            .with_ref(|idx| idx.select_within_budget(&seed, budget, None, false));
        let direct = store.sine.with_ref(|idx| idx.select(&seed, t, None, false));
        assert_eq!(
            set, direct,
            "budget {} -> tolerance {} mismatch vs direct select",
            budget, t
        );
    }
}

#[test]
fn select_capped_aborts_and_flags_over_budget() {
    let (store, _axioms, rex) = rex_kb();
    let seed = HashSet::from([rex]);
    // At a high tolerance Rex reaches all 4 axioms; cap=2 must abort with
    // over=true and a partial set of at most cap+1.
    let (set, over) = store
        .sine
        .with_ref(|idx| idx.select_capped(&seed, MAX_AUTO_TOLERANCE, None, false, 2));
    assert!(over, "cap=2 should be exceeded reaching 4 axioms");
    assert!(
        set.len() <= 3,
        "partial set should stop near the cap, got {}",
        set.len()
    );
    // With a cap at/above the full reachable size, over is false and the
    // set is complete.
    let (set_full, over_full) = store
        .sine
        .with_ref(|idx| idx.select_capped(&seed, MAX_AUTO_TOLERANCE, None, false, 100));
    assert!(!over_full);
    assert_eq!(set_full.len(), 4);
}

#[test]
fn within_budget_is_monotone_in_budget() {
    let (store, _axioms, rex) = rex_kb();
    let seed = HashSet::from([rex]);
    let mut prev: Option<HashSet<SentenceId>> = None;
    for budget in [1usize, 2, 3, 4, 5, 100] {
        let (_t, set) = store
            .sine
            .with_ref(|idx| idx.select_within_budget(&seed, budget, None, false));
        if let Some(p) = &prev {
            assert!(
                p.is_subset(&set),
                "larger budget must yield a superset (budget {})",
                budget
            );
        }
        prev = Some(set);
    }
}

// -- Pinned-owner selection ---------------------------------------------------

fn sym(store: &SyntacticLayer, name: &str) -> SymbolId {
    store.sym_id(name).unwrap()
}

fn select(
    store: &SyntacticLayer,
    seed: &[SymbolId],
    depth: Option<usize>,
    pinned: bool,
) -> HashSet<SentenceId> {
    store.select_axioms(&seed.iter().copied().collect(), 1.0, depth, pinned)
}

/// `Animal` is rarer than `Dog` (one occurrence against four), so by
/// generality alone `Animal` owns `(subclass Dog Animal)`.
const RARE_PARENT: &str = "(subclass Dog Animal)\n\
                           (instance Rex Dog)\n\
                           (instance Fido Dog)\n\
                           (instance Spot Dog)";

#[test]
fn pinned_child_owns_edge_when_parent_is_rarer() {
    let (mut store, axioms) = store_and_axioms(RARE_PARENT);
    build_eager(&mut store, &axioms);
    let dog = sym(&store, "Dog");
    let animal = sym(&store, "Animal");

    assert!(select(&store, &[dog], Some(1), false).is_empty());
    assert_eq!(
        select(&store, &[animal], Some(1), false),
        HashSet::from([axioms[0]])
    );

    assert_eq!(
        select(&store, &[dog], Some(1), true),
        HashSet::from([axioms[0]]),
        "the child owns its edge at any generality",
    );
    assert!(
        select(&store, &[animal], Some(1), true).is_empty(),
        "the parent no longer triggers its subclasses' edges",
    );
}

#[test]
fn pinned_instance_fact_is_owned_by_the_instance() {
    let (mut store, axioms) = store_and_axioms(RARE_PARENT);
    build_eager(&mut store, &axioms);
    let rex = sym(&store, "Rex");
    let dog = sym(&store, "Dog");
    assert_eq!(
        store
            .sine
            .with_ref(|i| i.pinned_of(axioms[1]).map(|p| p.owner)),
        Some(rex)
    );
    assert_ne!(
        store
            .sine
            .with_ref(|i| i.pinned_of(axioms[1]).map(|p| p.owner)),
        Some(dog)
    );
    assert!(select(&store, &[rex], Some(1), true).contains(&axioms[1]));
}

#[test]
fn pinned_superclass_chain_ignores_depth_limit() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Poodle Dog)\n\
         (subclass Dog Mammal)\n\
         (subclass Mammal Animal)\n\
         (subclass Animal Entity)",
    );
    build_eager(&mut store, &axioms);
    let poodle = sym(&store, "Poodle");

    let all: HashSet<SentenceId> = axioms.iter().copied().collect();
    assert_eq!(select(&store, &[poodle], Some(1), true), all);
    assert_ne!(select(&store, &[poodle], Some(1), false), all);
    assert!(
        select(&store, &[poodle], Some(0), true).is_empty(),
        "Some(0) still performs no expansion",
    );
}

#[test]
fn pinned_domain_is_owned_by_the_relation_and_brings_its_class_chain() {
    let (mut store, axioms) = store_and_axioms(
        "(domain color 2 Object)\n\
         (subclass Object Physical)\n\
         (subclass Physical Entity)\n\
         (color Sky Blue)\n\
         (color Grass Green)\n\
         (color Snow White)",
    );
    build_eager(&mut store, &axioms);
    let color = sym(&store, "color");
    let object = sym(&store, "Object");

    assert!(select(&store, &[color], Some(1), false).is_empty());

    let chain: HashSet<SentenceId> = axioms[..3].iter().copied().collect();
    assert_eq!(
        select(&store, &[color], Some(1), true),
        chain,
        "domain axiom plus the class's superclass chain, not the color facts",
    );
    assert!(
        !select(&store, &[object], Some(1), true).contains(&axioms[0]),
        "a class does not trigger the signature declarations that mention it",
    );
}

#[test]
fn pinned_range_and_subclass_signature_forms_are_pinned() {
    let (mut store, axioms) = store_and_axioms(
        "(range father Man)\n\
         (domainSubclass subclassOf 1 Class)\n\
         (rangeSubclass subclassOf Class)",
    );
    build_eager(&mut store, &axioms);
    for (&sid, rel) in axioms.iter().zip(["father", "subclassOf", "subclassOf"]) {
        let owner = sym(&store, rel);
        assert_eq!(store.pinned_of(sid).map(|p| p.owner), Some(owner));
        assert_eq!(
            store.sine.with_ref(|i| i.pinned_of(sid).map(|p| p.owner)),
            Some(owner)
        );
    }
}

#[test]
fn only_ground_pinned_facts_with_a_symbol_owner_are_pinned() {
    let (store, axioms) = store_and_axioms(
        "(attribute Rex Loyal)\n\
         (subclass ?X Animal)\n\
         (=> (instance ?X Dog) (instance ?X Animal))\n\
         (subclass Dog Animal)",
    );
    assert_eq!(
        store.pinned_of(axioms[0]).map(|p| p.owner),
        None,
        "unpinned head"
    );
    assert_eq!(
        store.pinned_of(axioms[1]).map(|p| p.owner),
        None,
        "variable owner"
    );
    assert_eq!(
        store.pinned_of(axioms[2]).map(|p| p.owner),
        None,
        "rule, not a fact"
    );
    assert_eq!(
        store.pinned_of(axioms[3]).map(|p| p.owner),
        Some(sym(&store, "Dog"))
    );
}

#[test]
fn pinned_selection_is_monotone_in_tolerance() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Dog Animal)\n\
         (subclass Animal Entity)\n\
         (instance Rex Dog)\n\
         (=> (instance ?X Dog) (attribute ?X Loyal))\n\
         (domain owns 1 Human)\n\
         (subclass Human Animal)",
    );
    build_eager(&mut store, &axioms);
    let seed: HashSet<SymbolId> = [sym(&store, "Rex"), sym(&store, "owns")].into();
    let mut prev: HashSet<SentenceId> = HashSet::new();
    for tol in [1.0, 1.5, 2.0, 4.0, 16.0, 64.0] {
        let cur = store.select_axioms(&seed, tol, None, true);
        assert!(prev.is_subset(&cur), "selection shrank at tolerance {tol}");
        prev = cur;
    }
}

#[test]
fn pinned_budget_search_keeps_the_chain() {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Poodle Dog)\n\
         (subclass Dog Animal)\n\
         (subclass Animal Entity)",
    );
    build_eager(&mut store, &axioms);
    let seed: HashSet<SymbolId> = [sym(&store, "Poodle")].into();
    let (_t, sel) = store.select_axioms_within_budget(&seed, 10, Some(1), true);
    assert_eq!(sel, axioms.iter().copied().collect::<HashSet<_>>());
}

#[test]
fn pins_follow_removal_and_rebuild() {
    let (mut store, axioms) = store_and_axioms(RARE_PARENT);
    build_eager(&mut store, &axioms);
    let dog = sym(&store, "Dog");
    assert_eq!(
        store
            .sine
            .with_ref(|i| i.pinned_of(axioms[0]).map(|p| p.owner)),
        Some(dog)
    );

    store.sine_rebuild();
    assert_eq!(
        store
            .sine
            .with_ref(|i| i.pinned_of(axioms[0]).map(|p| p.owner)),
        Some(dog),
        "rebuild re-registers pins",
    );

    store.sine_remove_axiom(axioms[0]);
    assert_eq!(
        store
            .sine
            .with_ref(|i| i.pinned_of(axioms[0]).map(|p| p.owner)),
        None
    );
    assert!(select(&store, &[dog], Some(1), true).is_empty());
}

#[test]
fn pins_survive_the_bulk_rebuild_path() {
    let clauses: Vec<String> = (0..80).map(|i| format!("(instance Pup{i} Dog)")).collect();
    let mut kif = clauses.join("\n");
    kif.push_str("\n(subclass Dog Animal)");
    let (mut store, axioms) = store_and_axioms(&kif);
    store.sine_add_axioms(axioms.iter().copied());
    let dog = sym(&store, "Dog");
    let edge = *axioms.last().unwrap();
    assert_eq!(
        store.sine.with_ref(|i| i.pinned_of(edge).map(|p| p.owner)),
        Some(dog)
    );
    assert_eq!(select(&store, &[dog], Some(1), true), HashSet::from([edge]));
}

#[test]
fn pinned_relation_groups_are_the_configured_symbols() {
    use super::pins::{always_pinned_by_head, SIGNATURE_PINS, TAXONOMY_PINS};
    use crate::semantics::consts::*;
    let by_head = always_pinned_by_head();
    assert_eq!(
        by_head.len(),
        TAXONOMY_PINS.len() + SIGNATURE_PINS.len(),
        "heads are unique across the groups",
    );
    for s in [
        &SUBCLASS_RELATION,
        &INSTANCE_RELATION,
        &SUBRELATION_RELATION,
        &SUBATTRIBUTE_RELATION,
    ] {
        assert!(TAXONOMY_PINS.iter().any(|p| p.relation.id() == s.id()));
    }
    for s in [
        &DOMAIN_RELATION,
        &DOMAIN_SUBCLASS_RELATION,
        &RANGE_RELATION,
        &RANGE_SUB_REL_CLASS,
    ] {
        assert!(SIGNATURE_PINS.iter().any(|p| p.relation.id() == s.id()));
    }
    for pin in by_head.values() {
        assert_eq!(pin.owner_arg, 1);
        assert!(pin.depth_exempt);
    }
}

#[test]
fn pinned_relation_owner_arg_selects_the_owning_element() {
    use super::pins::PinnedRelation;
    use crate::semantics::consts::SUBCLASS_RELATION;
    let (store, axioms) = store_and_axioms("(subclass Dog Animal)\n(instance Rex Dog)");
    let sentence = |i: usize| store.sentence(axioms[i]).unwrap();

    let first = PinnedRelation::new(&SUBCLASS_RELATION, 1, true);
    let second = PinnedRelation::new(&SUBCLASS_RELATION, 2, true);
    let third = PinnedRelation::new(&SUBCLASS_RELATION, 3, true);
    assert_eq!(first.owner_of(&sentence(0)), Some(sym(&store, "Dog")));
    assert_eq!(second.owner_of(&sentence(0)), Some(sym(&store, "Animal")));
    assert_eq!(third.owner_of(&sentence(0)), None, "no such argument");
    assert_eq!(first.owner_of(&sentence(1)), None, "different head");
}

/// Re-pins the `Poodle -> Dog -> Animal -> Entity` edges with the given
/// exemption flag and selects from `Poodle` at depth one.
fn chain_at_depth_one(depth_exempt: bool) -> (HashSet<SentenceId>, Vec<SentenceId>) {
    let (mut store, axioms) = store_and_axioms(
        "(subclass Poodle Dog)\n\
         (subclass Dog Animal)\n\
         (subclass Animal Entity)",
    );
    build_eager(&mut store, &axioms);
    for &sid in &axioms {
        let owner = store.pinned_of(sid).unwrap().owner;
        store.sine.modify(|idx| {
            idx.pin(
                sid,
                super::Pin {
                    owner,
                    depth_exempt,
                },
            )
        });
    }
    let poodle = sym(&store, "Poodle");
    (select(&store, &[poodle], Some(1), true), axioms)
}

#[test]
fn depth_exempt_pin_follows_the_chain_past_the_depth_limit() {
    let (selected, axioms) = chain_at_depth_one(true);
    assert_eq!(selected, axioms.iter().copied().collect());
}

#[test]
fn non_exempt_pin_takes_an_ordinary_wave_per_hop() {
    let (selected, axioms) = chain_at_depth_one(false);
    assert_eq!(
        selected,
        HashSet::from([axioms[0]]),
        "only the seed's own pinned edge fits in one wave",
    );
}

// -- Depth as a budget fallback -----------------------------------------------

const LONG_CHAIN: &str = "(subclass A B)\n(subclass B C)\n(subclass C D)\n\
                          (subclass D E)\n(subclass E F)\n(subclass F G)";

fn chain_budget(
    budget: usize,
    depth_limit: Option<usize>,
    pinned: bool,
) -> (f32, HashSet<SentenceId>, Vec<SentenceId>) {
    let (mut store, axioms) = store_and_axioms(LONG_CHAIN);
    build_eager(&mut store, &axioms);
    let seed: HashSet<SymbolId> = [sym(&store, "A")].into();
    let (t, sel) = store.select_axioms_within_budget(&seed, budget, depth_limit, pinned);
    (t, sel, axioms)
}

#[test]
fn pinned_budget_truncates_the_chain_by_depth_when_tolerance_cannot_help() {
    let (t, sel, axioms) = chain_budget(3, None, true);
    assert_eq!(t, 1.0);
    assert_eq!(
        sel,
        axioms[..3].iter().copied().collect(),
        "the three edges nearest the seed",
    );
}

#[test]
fn pinned_budget_keeps_the_whole_chain_when_it_fits() {
    let (_t, sel, axioms) = chain_budget(10, None, true);
    assert_eq!(sel, axioms.iter().copied().collect());
}

#[test]
fn explicit_depth_limit_is_the_ceiling_of_the_budget_search() {
    let (_t, sel, axioms) = chain_budget(3, Some(2), true);
    assert_eq!(sel, axioms[..2].iter().copied().collect());
}

#[test]
fn unpinned_budget_still_returns_the_overrunning_floor() {
    let (mut store, axioms) = store_and_axioms(LONG_CHAIN);
    build_eager(&mut store, &axioms);
    let seed: HashSet<SymbolId> = [sym(&store, "A")].into();
    let floor = store.select_axioms(&seed, 1.0, None, false);
    let (_t, sel) = store.select_axioms_within_budget(&seed, 1, None, false);
    assert_eq!(sel, floor);
}

#[test]
fn pinned_budget_returns_one_wave_when_even_that_overruns() {
    let (mut store, axioms) =
        store_and_axioms("(subclass A B1)\n(subclass A B2)\n(subclass A B3)\n(subclass B1 C)");
    build_eager(&mut store, &axioms);
    let seed: HashSet<SymbolId> = [sym(&store, "A")].into();
    let (_t, sel) = store.select_axioms_within_budget(&seed, 2, None, true);
    assert_eq!(sel, axioms[..3].iter().copied().collect());
}

const HIGHER_ORDER: &str = "(equal (?F ?X) (AssignmentFn ?F ?X))\n\
                            (=> (and (range ?F ?C) (equal (AssignmentFn ?F ?X) ?V)) (instance ?V ?C))\n\
                            (forall (?X) (=> (instance ?X Dog) (instance ?X Animal)))\n\
                            (instance Rex Dog)\n\
                            (range FooFn Dog)\n\
                            (range BarFn Dog)\n\
                            (range BazFn Dog)";

#[test]
fn variable_headed_axioms_are_higher_order_but_quantifier_lists_are_not() {
    let (store, axioms) = store_and_axioms(HIGHER_ORDER);
    assert!(store.sentence_has_variable_head(axioms[0]));
    assert!(!store.sentence_has_variable_head(axioms[1]));
    assert!(
        !store.sentence_has_variable_head(axioms[2]),
        "a quantifier's variable list is not a variable head",
    );
    assert!(!store.sentence_has_variable_head(axioms[3]));
}

#[test]
fn pinned_selection_seeds_higher_order_axioms_and_their_symbols() {
    let (mut store, axioms) = store_and_axioms(HIGHER_ORDER);
    build_eager(&mut store, &axioms);
    let rex = sym(&store, "Rex");
    let unpinned = select(&store, &[rex], None, false);
    assert!(!unpinned.contains(&axioms[0]));
    let pinned = select(&store, &[rex], None, true);
    assert!(
        pinned.contains(&axioms[0]),
        "the bridge seeds every selection"
    );
    assert!(
        pinned.contains(&axioms[1]),
        "its AssignmentFn joins the fan-out and triggers the range rule",
    );
}

#[test]
fn higher_order_marks_follow_removal_and_rebuild() {
    let (mut store, axioms) = store_and_axioms(HIGHER_ORDER);
    build_eager(&mut store, &axioms);
    let rex = sym(&store, "Rex");
    store.sine_rebuild();
    assert!(select(&store, &[rex], None, true).contains(&axioms[0]));
    store.sine_remove_axiom(axioms[0]);
    assert!(!select(&store, &[rex], None, true).contains(&axioms[0]));
}
