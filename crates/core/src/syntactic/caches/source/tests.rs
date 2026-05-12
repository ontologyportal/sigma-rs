// Unit tests for the source store / reactor and its `Side` reconcile.

use super::*;

// -- builders -------------------------------------------------------------

fn sp(file: &str, line: u32) -> Span {
    Span { file: file.into(), line, col: 1, offset: 0, end_line: line, end_col: 1, end_offset: 0 }
}
fn node() -> AstNode {
    AstNode::Symbol { name: "x".into(), span: Span::default() }
}
/// Build a deduped parse from `(fingerprint, file, line)` triples.
fn parse(items: &[(u64, &str, u32)]) -> HashMap<u64, (AstNode, Span)> {
    items.iter().map(|&(h, f, l)| (h, (node(), sp(f, l)))).collect()
}
fn sess() -> Arc<String> {
    Arc::new("sess".to_string())
}

// -- event extractors (Event isn't `PartialEq`, so match on variants) -----

fn added(evs: &[Event]) -> Vec<u64> {
    let mut v: Vec<u64> = evs.iter()
        .filter_map(|e| match e { Event::FormulaAdded { node, .. } => Some(*node), _ => None })
        .collect();
    v.sort_unstable();
    v
}
fn removed(evs: &[Event]) -> Vec<u64> {
    let mut v: Vec<u64> = evs.iter()
        .filter_map(|e| match e { Event::FormulaRemoved { node } => Some(*node), _ => None })
        .collect();
    v.sort_unstable();
    v
}

// -- dedup ----------------------------------------------------------------

#[test]
fn dedup_keeps_first_occurrence_and_warns_on_repeat() {
    let parsed = vec![
        (1u64, node(), sp("a", 1)),
        (1u64, node(), sp("a", 5)), // same fingerprint again within this parse
        (2u64, node(), sp("a", 9)),
    ];
    let (current, warnings) = dedup_parse(parsed);
    assert_eq!(current.len(), 2, "the duplicate of formula 1 is dropped");
    assert_eq!(warnings.len(), 1, "exactly one duplicate warning");
    assert!(matches!(&warnings[0], Event::Diagnostic(d) if d.code == "duplicate-formula"));
    assert_eq!(current[&1].1.line, 1, "the first occurrence (line 1) is canonical");
}

// -- apply_source: refcount / replacement transitions ---------------------

/// A fresh (nodes store, side) pair.
fn store_side() -> (EntryCache<u64, AstNode>, SourceSide) {
    (EntryCache::default(), SourceSide::default())
}

/// Test shim: ordinary (immediate, nothing protected) `apply_source`.
fn apply(
    store: &EntryCache<u64, AstNode>,
    side:  &SourceSide,
    key:   &str,
    session: &Arc<String>,
    current: HashMap<u64, (AstNode, Span)>,
) -> Vec<Event> {
    apply_source(store, side, key, session, current, &HashSet::new())
}

#[test]
fn new_formula_adds_and_stores() {
    let (store, side) = store_side();
    let evs = apply(&store, &side, "a", &sess(), parse(&[(1, "a", 1)]));
    assert_eq!(added(&evs), vec![1]);
    assert!(removed(&evs).is_empty());
    assert!(store.contains_key(&1));
    assert_eq!(side.references.get(&1).unwrap().len(), 1);
    assert!(side.file_hashes.get("a").unwrap().contains(&1));
}

#[test]
fn reingest_unchanged_emits_nothing() {
    let (store, side) = store_side();
    apply(&store, &side, "a", &sess(), parse(&[(1, "a", 1)]));
    let evs = apply(&store, &side, "a", &sess(), parse(&[(1, "a", 1)]));
    assert!(added(&evs).is_empty() && removed(&evs).is_empty());
    assert_eq!(side.references.get(&1).unwrap().len(), 1, "still a single reference");
}

#[test]
fn reingest_without_formula_removes_it() {
    let (store, side) = store_side();
    apply(&store, &side, "a", &sess(), parse(&[(1, "a", 1)]));
    let evs = apply(&store, &side, "a", &sess(), parse(&[]));
    assert_eq!(removed(&evs), vec![1]);
    assert!(store.is_empty(), "node pruned once the last reference is gone");
    assert!(side.references.is_empty());
}

#[test]
fn move_within_file_updates_span_without_event() {
    let (store, side) = store_side();
    apply(&store, &side, "a", &sess(), parse(&[(1, "a", 1)]));
    let evs = apply(&store, &side, "a", &sess(), parse(&[(1, "a", 5)])); // same formula, new line
    assert!(added(&evs).is_empty() && removed(&evs).is_empty(), "a move is not a KB change");
    let refs = side.references.get(&1).unwrap();
    assert_eq!(refs.len(), 1, "old span retracted, new span added");
    assert!(refs.iter().all(|sp| sp.line == 5), "reference now points at the new location");
}

#[test]
fn cross_file_share_adds_once_and_removes_when_last_ref_gone() {
    let (store, side) = store_side();
    // file "a" defines formula 1
    assert_eq!(added(&apply(&store, &side, "a", &sess(), parse(&[(1, "a", 1)]))), vec![1]);
    // file "b" also defines formula 1 — already in the KB, so no second add
    let e2 = apply(&store, &side, "b", &sess(), parse(&[(1, "b", 1)]));
    assert!(added(&e2).is_empty(), "shared formula is not re-added");
    assert_eq!(side.references.get(&1).unwrap().len(), 2, "referenced by both a and b");
    // remove from a — b still references it, so no FormulaRemoved
    let e3 = apply(&store, &side, "a", &sess(), parse(&[]));
    assert!(removed(&e3).is_empty(), "still referenced by b");
    assert_eq!(side.references.get(&1).unwrap().len(), 1);
    // remove from b — last reference gone — removed
    let e4 = apply(&store, &side, "b", &sess(), parse(&[]));
    assert_eq!(removed(&e4), vec![1]);
    assert!(!store.contains_key(&1) && !side.references.contains_key(&1));
}

// -- parse-error recovery (full load pipeline) ----------------------------
// The KIF parser is error-recovering: the source reactor commits every
// well-formed sentence it recovered, surfacing a diagnostic for the bad one.

#[test]
fn parse_error_preserves_recovered_sentences() {
    let mut store = SyntacticLayer::default();
    let errors = store.load_kif(
        "(subclass Human Animal)\n(\"bad\" head)\n(subclass Dog Animal)",
        "mixed");
    assert!(!errors.is_empty(), "expected a parse error");
    assert_eq!(store.by_head("subclass").len(), 2,
        "recovered sentences should be committed despite the parse error");
    assert!(store.sym_id("Human").is_some());
    assert!(store.sym_id("Dog").is_some());
}

#[test]
fn parse_error_leaves_earlier_files_intact() {
    let mut store = SyntacticLayer::default();
    let ok = store.load_kif("(subclass Human Animal)", "good");
    assert!(ok.is_empty());
    assert_eq!(store.by_head("subclass").len(), 1);

    let errs = store.load_kif("(\"broken\"", "bad");
    assert!(!errs.is_empty());
    assert_eq!(store.by_head("subclass").len(), 1,
        "good file's roots disturbed by bad file's parse failure");
}
