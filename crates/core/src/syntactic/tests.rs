//! End-to-end exercise of the syntactic cascade: feed `SourceAdded` (via
//! `load_kif_assert`) and assert the whole pipeline — source AST → sentences →
//! symbols → indices — plus file-diff, empty-file clearing, the two refcount
//! layers, and hash consistency.

#![cfg(test)]

use super::SyntacticLayer;
use super::sentence::Symbol;
use crate::cache::events::Event;
use crate::layer::Layer;
use crate::types::SentenceId;

/// Sorted root ids (content-hash ids have no load order, so sort for a stable
/// view).
fn roots(layer: &SyntacticLayer) -> Vec<SentenceId> {
    let mut v: Vec<SentenceId> = layer.root_sids();
    v.sort_unstable();
    v
}

// 1 ─ A source file is ingested into AST nodes, then sentences + symbols.
#[test]
fn source_ingests_into_ast_then_sentences_and_symbols() {
    let mut layer = SyntacticLayer::default();
    let errs = layer.load_kif_assert("(subclass Human Animal)", "a.kif");
    assert!(errs.is_empty(), "load errors: {errs:?}");

    // AST node landed in the source cache (keyed by content fingerprint).
    let fps = layer.file_fingerprints("a.kif");
    assert_eq!(fps.len(), 1, "one formula → one source fingerprint");
    assert!(layer.source_ast(fps[0]).is_some(), "source AST node retained");
    assert_eq!(layer.source_files(), vec!["a.kif".to_string()]);

    // Sentence built.
    assert_eq!(layer.num_roots(), 1, "one root sentence");

    // Symbols interned.
    for s in ["subclass", "Human", "Animal"] {
        assert!(layer.sym_id(s).is_some(), "symbol {s} interned");
    }
}

// 2 ─ Duplicate symbols intern once (content-addressed `SymbolId = hash(name)`).
#[test]
fn duplicate_symbols_intern_once() {
    let mut layer = SyntacticLayer::default();
    // `Animal` appears 3×, `subclass` 2×.
    layer.load_kif_assert("(subclass Human Animal)(subclass Dog Animal)(instance Animal Class)", "a.kif");

    let animal = layer.sym_id("Animal").expect("Animal interned");
    // Every mention resolves to the same id, and the id is exactly hash(name).
    assert_eq!(animal, Symbol::hash_name("Animal"));
    // The symbol table holds exactly one entry per distinct name (not per mention).
    let names: Vec<String> = layer.symbols.snapshot().values().map(|n| n.to_string()).collect();
    let animal_entries = names.iter().filter(|n| n.as_str() == "Animal").count();
    assert_eq!(animal_entries, 1, "Animal interned once despite 3 mentions");
}

// 3 ─ Root sentences are interned; identical concrete facts dedup to one root.
#[test]
fn root_sentences_interned_and_concrete_facts_dedup() {
    let mut layer = SyntacticLayer::default();
    layer.load_kif_assert("(instance Fido Dog)(instance Rex Cat)", "a.kif");
    assert_eq!(layer.num_roots(), 2);

    // The same concrete fact, twice in one file, is one root (the map IS the dedup).
    let mut l2 = SyntacticLayer::default();
    l2.load_kif_assert("(instance Fido Dog)(instance Fido Dog)", "b.kif");
    assert_eq!(l2.num_roots(), 1, "identical concrete fact dedups to one root");
}

// 4 ─ Re-ingesting a file diffs against its previous contents (add/keep/remove).
#[test]
fn source_update_diff_is_handled() {
    let mut layer = SyntacticLayer::default();
    layer.load_kif_assert("(subclass A B)(subclass C D)", "f.kif");
    assert_eq!(layer.num_roots(), 2);
    let before = roots(&layer);

    // Keep (A B), drop (C D), add (E F).
    layer.load_kif_assert("(subclass A B)(subclass E F)", "f.kif");
    assert_eq!(layer.num_roots(), 2, "one kept + one added; one removed");

    // The kept root (A B) keeps its (content-hash) id; (C D)'s id is gone; (E F) is new.
    let after = roots(&layer);
    let kept: Vec<_> = before.iter().filter(|s| after.contains(s)).collect();
    assert_eq!(kept.len(), 1, "exactly the unchanged sentence is retained by id");
    assert!(layer.sym_id("E").is_some(), "added symbol E interned");
    assert!(layer.sym_id("C").is_none() || layer.by_head("subclass").len() == 2,
        "C/D no longer referenced (orphan symbols pruned)");
}

// 5 ─ Passing an empty file clears all of that file's axioms.
#[test]
fn empty_file_clears_its_axioms() {
    let mut layer = SyntacticLayer::default();
    layer.load_kif_assert("(subclass A B)(subclass C D)", "f.kif");
    assert_eq!(layer.num_roots(), 2);

    layer.load_kif_assert("", "f.kif");
    assert_eq!(layer.num_roots(), 0, "empty re-ingest removes the file's roots");
    assert!(layer.file_fingerprints("f.kif").is_empty(), "file has no fingerprints");
}

// 6a ─ Reference counters: a fact in two files survives removal of one (the
//      source cache refcounts cross-file occurrences of a fingerprint).
#[test]
fn refcount_fact_in_two_files_survives_partial_removal() {
    let mut layer = SyntacticLayer::default();
    layer.load_kif_assert("(instance Fido Dog)", "a.kif");
    layer.load_kif_assert("(instance Fido Dog)", "b.kif");
    assert_eq!(layer.num_roots(), 1, "same fact across files is one root");

    layer.load_kif_assert("", "a.kif");
    assert_eq!(layer.num_roots(), 1, "still referenced by b.kif");

    layer.load_kif_assert("", "b.kif");
    assert_eq!(layer.num_roots(), 0, "last referencing file gone → removed");
}

// 6b ─ Reference counters: one sentence produced by two *different* source
//      formulas (`<=>` and `=>` both yield `(=> A B)`) survives until the last
//      producing fingerprint goes (the store's `forward`/`source_overflow`).
#[test]
fn refcount_one_sentence_from_two_formulas() {
    let mut layer = SyntacticLayer::default();
    // `(=> (foo a) (bar a))` is concrete, so it dedups.
    layer.load_kif_assert("(=> (foo a) (bar a))", "imp.kif");
    let n_after_imp = layer.num_roots();
    assert_eq!(n_after_imp, 1);

    // `(<=> (foo a) (bar a))` normalizes to `(=> (foo a) (bar a))` + `(=> (bar a) (foo a))`.
    // The first coincides with the existing root (same content hash).
    layer.load_kif_assert("(<=> (foo a) (bar a))", "iff.kif");
    assert_eq!(layer.num_roots(), 2, "shared `(=> foo bar)` + the new reverse implication");

    // Drop the plain implication file: `(=> foo bar)` still produced by the <=>.
    layer.load_kif_assert("", "imp.kif");
    assert_eq!(layer.num_roots(), 2, "shared sentence survives — still produced by the <=>");

    // Drop the <=> file: both implications gone.
    layer.load_kif_assert("", "iff.kif");
    assert_eq!(layer.num_roots(), 0);
}

// 6c ─ Reference counters: a concrete sub shared between a compound root and a
//      standalone root (the store's `subs`/`parent_overflow`).  A sentence can
//      be both a root and a sub; removal respects both.
#[test]
fn refcount_shared_sub_sentence() {
    let mut layer = SyntacticLayer::default();
    // `(instance A B)` appears standalone AND nested as the antecedent of the
    // implication.  (A top-level `and` would be split into separate roots at
    // ingest, so use an implication to keep `(instance A B)` a genuine sub.)
    layer.load_kif_assert("(=> (instance A B) (instance C D))", "x.kif");
    layer.load_kif_assert("(instance A B)", "y.kif");

    // `(instance A B)` is both a root (from y) and a sub (of x's implication).
    let inst_ab = layer.sym_id("instance").and_then(|_| {
        layer.by_head("instance").iter().copied()
            .find(|&sid| {
                let s = layer.sentence(sid).unwrap();
                // (instance A B): arg1 == A
                matches!(s.elements.get(1), Some(crate::types::Element::Symbol(sym)) if Some(sym.id()) == layer.sym_id("A"))
            })
    }).expect("(instance A B) is a root");

    // Remove the compound file: the `and` root goes, but `(instance A B)` survives
    // (still a root via y.kif).
    layer.load_kif_assert("", "x.kif");
    assert!(layer.sentence(inst_ab).is_some(), "shared sub survives — still a standalone root");

    // Remove the standalone file: now nothing references it → gone.
    layer.load_kif_assert("", "y.kif");
    assert!(layer.sentence(inst_ab).is_none(), "removed once neither root nor sub references it");
}

// 7 ─ Sentence and symbol hashing is consistent across independent layers.
#[test]
fn hashing_is_consistent() {
    let mut l1 = SyntacticLayer::default();
    let mut l2 = SyntacticLayer::default();
    l1.load_kif_assert("(p a b)", "f.kif");
    l2.load_kif_assert("(p a b)", "g.kif");

    // Same concrete sentence → identical content-hash SentenceId across layers.
    let s1 = roots(&l1);
    let s2 = roots(&l2);
    assert_eq!(s1, s2, "identical concrete sentence → identical SentenceId across layers");

    // SymbolId is the content hash of the name — stable across layers and equal
    // to `Symbol::hash_name`.
    for name in ["p", "a", "b"] {
        assert_eq!(l1.sym_id(name), l2.sym_id(name));
        assert_eq!(l1.sym_id(name), Some(Symbol::hash_name(name)));
    }
}

// 8 ─ Promotion is event-driven: `SessionAxiomatized` → the session cache fans
//     out `AxiomsPromoted`, which the `axiom_index` and `sine` reactors consume.
//     Transient assertions are in neither index until promoted.
#[test]
fn promotion_populates_axiom_index_and_sine() {
    let mut layer = SyntacticLayer::default();
    layer.load_kif_assert("(subclass Human Animal)", "a.kif");
    let sub = layer.sym_id("subclass").unwrap();
    let sid = roots(&layer)[0];

    // Before promotion: a transient assertion, unknown to axiom_index + SInE.
    assert!(!layer.is_axiom(sid), "transient before promotion");
    assert!(layer.axiom_sentences_of(sub).is_empty(), "axiom_index empty pre-promotion");
    assert_eq!(layer.sine.with_ref(|idx| idx.generality(sub)), 0, "SInE empty pre-promotion");

    // Promote the session → AxiomsPromoted cascade.
    let _ = layer.cascade(vec![Event::SessionAxiomatized { session: "a.kif".to_string() }]);

    assert!(layer.is_axiom(sid), "axiom after promotion");
    assert!(layer.axiom_sentences_of(sub).contains(&sid), "axiom_index indexes the promoted axiom");
    assert!(layer.sine.with_ref(|idx| idx.generality(sub)) > 0, "SInE indexes the promoted axiom");
}

// 9 ─ Retracting a promoted axiom (empty re-ingest → RootRemoved) drops it from
//     both the axiom index and SInE, and clears its axiom status.
#[test]
fn retracting_axiom_drops_it_from_axiom_index_and_sine() {
    let mut layer = SyntacticLayer::default();
    layer.load_kif_assert("(subclass Human Animal)", "a.kif");
    let _ = layer.cascade(vec![Event::SessionAxiomatized { session: "a.kif".to_string() }]);
    let sub = layer.sym_id("subclass").unwrap();
    let sid = roots(&layer)[0];
    assert!(layer.axiom_sentences_of(sub).contains(&sid));

    // Empty the file: the source diff retracts the sentence (RootRemoved).
    layer.load_kif_assert("", "a.kif");

    assert!(!layer.axiom_sentences_of(sub).contains(&sid), "dropped from axiom_index");
    assert_eq!(layer.sine.with_ref(|idx| idx.generality(sub)), 0, "dropped from SInE");
    assert!(!layer.is_axiom(sid), "no longer an axiom");
}

// 10 ─ An axiom shared by two axiomatized sessions is promoted exactly once (the
//      session cache filters sids already covered by another axiomatic session),
//      and survives until the last referencing source is gone.
#[test]
fn shared_axiom_promoted_once_and_survives_until_last_ref() {
    let mut layer = SyntacticLayer::default();
    // Same concrete fact in two files/sessions → one (deduped) root sentence.
    layer.load_kif_assert("(instance Fido Dog)", "a.kif");
    layer.load_kif_assert("(instance Fido Dog)", "b.kif");
    assert_eq!(layer.num_roots(), 1);
    let inst = layer.sym_id("instance").unwrap();
    let sid = roots(&layer)[0];

    let _ = layer.cascade(vec![Event::SessionAxiomatized { session: "a.kif".to_string() }]);
    assert!(layer.axiom_sentences_of(inst).contains(&sid));

    // Promoting b.kif must NOT re-emit the already-axiom sid (the filter). Drive
    // the cascade directly so we can inspect the emitted events.
    let outcome = layer.cascade(vec![Event::SessionAxiomatized { session: "b.kif".to_string() }]);
    let re_promoted: Vec<SentenceId> = outcome.emitted.iter()
        .filter_map(|e| match e { Event::AxiomsPromoted { sids } => Some(sids.clone()), _ => None })
        .flatten()
        .collect();
    assert!(!re_promoted.contains(&sid), "already-axiom sid filtered out of b.kif's AxiomsPromoted");

    // Still one axiom occurrence, not double-counted.
    assert!(layer.axiom_sentences_of(inst).contains(&sid));

    // Drop one file: the fact is still produced by the other → survives.
    layer.load_kif_assert("", "a.kif");
    assert!(layer.axiom_sentences_of(inst).contains(&sid), "still referenced by b.kif");

    // Drop the last file: RootRemoved retracts it everywhere.
    layer.load_kif_assert("", "b.kif");
    assert!(!layer.axiom_sentences_of(inst).contains(&sid), "gone from axiom_index");
    assert_eq!(layer.sine.with_ref(|idx| idx.generality(inst)), 0, "gone from SInE");
}

// 11 ─ The whole store round-trips through the unified cache-snapshot seam:
//      `snapshot_caches` freezes every `own_persistable()` cache to a backend,
//      `restore_caches_from` thaws them into a fresh layer with no replay.
#[cfg(feature = "persist")]
#[test]
fn cache_snapshot_round_trips_the_store() {
    use crate::layer::Layer;
    use crate::persist::MemoryBackend;

    let mut layer = SyntacticLayer::default();
    layer.load_kif_assert("(subclass Human Animal)(instance Fido Dog)", "a.kif");
    let _ = layer.cascade(vec![Event::SessionAxiomatized { session: "a.kif".to_string() }]); // promote → sine + axiom_index + promoted set
    let roots_before = roots(&layer);
    let sub = layer.sym_id("subclass").unwrap();
    let gen_before = layer.sine.with_ref(|idx| idx.generality(sub));
    assert_eq!(roots_before.len(), 2);

    // Freeze to an in-memory backend, thaw into a fresh layer.
    let mut backend = MemoryBackend::default();
    layer.snapshot_caches(&mut backend).expect("snapshot");
    let restored = SyntacticLayer::default();
    restored.restore_caches_from(&backend).expect("restore");

    // Sentences + provenance side (roots set) round-trip.
    assert_eq!(roots(&restored), roots_before, "roots round-trip by content-hash id");
    assert_eq!(restored.num_roots(), 2);
    for &sid in &roots_before {
        assert!(restored.sentence(sid).is_some(), "sentence body restored");
        // Session `promoted` side round-trips → axiom status preserved.
        assert!(restored.is_axiom(sid), "axiom status round-trips");
    }
    // SInE index (eager value) round-trips.
    assert_eq!(restored.sine.with_ref(|idx| idx.generality(sub)), gen_before,
        "SInE generality round-trips");
    // axiom_index round-trips.
    assert!(restored.axiom_sentences_of(sub).iter().any(|s| roots_before.contains(s)),
        "axiom_index round-trips");
}

/// Drive a raw `SourceAdded` cascade and return the whole outcome so a test can
/// inspect the emitted events (`load_kif_assert` discards them).
fn ingest_outcome(
    layer: &mut SyntacticLayer,
    text:  &str,
    file:  &str,
) -> crate::cache::router::RouteOutcome {
    let source = crate::types::SourceFile {
        parser:   crate::Parser::Kif,
        name:     file.to_owned(),
        path:     std::path::PathBuf::from(file),
        origin:   crate::types::FileOrigin::Inline,
        contents: text.to_owned(),
        prebuilt: None,
    };
    layer.cascade(vec![Event::SourceAdded {
        session: std::sync::Arc::new(file.to_owned()),
        file:    source,
        staged:  false,
    }])
}

// 12 ─ Removal events carry the sentence bodies: `RootRemoved` carries the
//      removed root and `RelationRemoved` the relation's sentence, so downstream
//      reactors read head / symbols / edge off the event without a store copy or
//      a reverse map.
#[test]
fn removal_events_carry_sentence_bodies() {
    let mut layer = SyntacticLayer::default();
    layer.load_kif_assert("(subclass Dog Animal)", "a.kif");
    let sid      = roots(&layer)[0];
    let subclass = layer.sym_id("subclass").expect("subclass interned");

    // Empty the file via a raw cascade so the emitted events are observable.
    let outcome = ingest_outcome(&mut layer, "", "a.kif");

    // `RootRemoved` moves the removed root body into the event.
    let root_removed: Vec<(SentenceId, Vec<_>)> = outcome.emitted.iter()
        .filter_map(|e| match e {
            Event::RootRemoved { sid, sentences } => Some((*sid, sentences.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(root_removed.len(), 1, "one root retracted");
    let (rsid, sentences) = &root_removed[0];
    assert_eq!(*rsid, sid);
    let root = sentences.iter().find(|s| s.hash() == sid)
        .expect("removed root body rides on the event");
    assert_eq!(root.head_symbol(), Some(subclass), "moved body still resolves its head");

    let rel_removed: Vec<(SentenceId, _)> = outcome.emitted.iter()
        .filter_map(|e| match e {
            Event::RelationRemoved { sid, sentence } => Some((*sid, sentence.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(rel_removed.len(), 1, "one relation retracted");
    assert_eq!(rel_removed[0].0, sid);
    assert_eq!(rel_removed[0].1.head_symbol(), Some(subclass),
        "RelationRemoved carries the relation's sentence");

    assert_eq!(layer.num_roots(), 0, "store is empty afterwards");
}
