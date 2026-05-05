// crates/core/tests/file_diff.rs
//
// Integration tests for the incremental-reload diff primitives:
//   * `compute_file_diff`          -- pure function; positional-greedy match
//   * `KnowledgeBase::apply_file_diff` -- mutation of an in-memory KB
//
// Core post-condition: the state after
// `apply_file_diff(compute(old, new))` equals the state after loading
// `new_text` from scratch, for every observable query.

use sigmakee_rs_core::{
    compute_file_diff, parse_document, sentence_fingerprint, FileDiff, KnowledgeBase,
};

// -- Helpers ------------------------------------------------------------------

fn kb_with(text: &str, file: &str) -> KnowledgeBase {
    let mut kb = KnowledgeBase::new();
    let r = kb.load_kif(text, file, None);
    assert!(r.ok, "initial load failed: {:?}", r.errors);
    kb
}

fn diff_for(kb: &KnowledgeBase, file: &str, new_text: &str) -> FileDiff {
    let old_sids   = kb.file_roots(file).to_vec();
    let old_hashes = kb.file_hashes(file).to_vec();
    let parsed     = parse_document(file.to_owned(), new_text);
    compute_file_diff(file, &old_sids, &old_hashes, &parsed.root_hashes,
                      &parsed.ast, &parsed.root_spans)
}

// -- Pure-function `compute_file_diff` tests ----------------------------------

#[test]
fn compute_diff_noop_edit_retains_everything() {
    let kb = kb_with(
        "(subclass Human Animal)\n(subclass Dog Animal)",
        "t.kif",
    );

    let diff = diff_for(&kb, "t.kif",
        "(subclass Human Animal)\n(subclass Dog Animal)");
    assert_eq!(diff.retained.len(), 2);
    assert!(diff.removed.is_empty());
    assert!(diff.added.is_empty());
}

#[test]
fn compute_diff_whitespace_shift_still_retains() {
    // Adding a leading blank line / comment must not change the
    // fingerprint for any sentence.
    let kb = kb_with("(subclass Human Animal)", "t.kif");

    let diff = diff_for(&kb, "t.kif",
        "\n; a comment\n(subclass  Human  Animal)");
    assert_eq!(diff.retained.len(), 1);
    assert!(diff.added.is_empty());
    assert!(diff.removed.is_empty());

    // Retained span must reflect the new position.
    let (sid, new_span) = &diff.retained[0];
    let old_span = {
        let file = sigmakee_rs_core::Span::default();
        let _ = file;  // silence
        // Fetch old span via KB lookup; we know the sid must be stable.
        kb.file_roots("t.kif")[0]
    };
    assert_eq!(*sid, old_span);
    assert!(new_span.offset > 0, "new span should have shifted down");
}

#[test]
fn compute_diff_add_one_sentence() {
    let kb = kb_with("(subclass Human Animal)", "t.kif");

    let diff = diff_for(&kb, "t.kif",
        "(subclass Human Animal)\n(subclass Dog Animal)");
    assert_eq!(diff.retained.len(), 1);
    assert!(diff.removed.is_empty());
    assert_eq!(diff.added.len(), 1);
}

#[test]
fn compute_diff_remove_one_sentence() {
    let kb = kb_with(
        "(subclass Human Animal)\n(subclass Dog Animal)",
        "t.kif",
    );

    let diff = diff_for(&kb, "t.kif", "(subclass Human Animal)");
    assert_eq!(diff.retained.len(), 1);
    assert_eq!(diff.removed.len(), 1);
    assert!(diff.added.is_empty());
}

#[test]
fn compute_diff_replace_one_sentence() {
    let kb = kb_with(
        "(subclass Human Animal)\n(subclass Dog Animal)",
        "t.kif",
    );

    let diff = diff_for(&kb, "t.kif",
        "(subclass Human Animal)\n(subclass Cat Animal)");
    assert_eq!(diff.retained.len(), 1);
    assert_eq!(diff.removed.len(), 1);
    assert_eq!(diff.added.len(),   1);
}

#[test]
fn compute_diff_duplicates_pair_by_source_order() {
    // Two identical sentences in the old text, one identical + one
    // different in the new text -- the first identical pairs, the
    // second becomes removed, and the different one becomes added.
    let kb = kb_with(
        "(instance A B)\n(instance A B)",
        "t.kif",
    );

    let diff = diff_for(&kb, "t.kif",
        "(instance A B)\n(instance A C)");
    assert_eq!(diff.retained.len(), 1);
    assert_eq!(diff.removed.len(), 1);
    assert_eq!(diff.added.len(),   1);
}

// -- `apply_file_diff` state-equivalence tests --------------------------------

/// The observable-state check: after applying a diff, every external
/// query must yield the same result as loading the new text from
/// scratch in a fresh KB.
fn assert_equivalent_after_diff(old: &str, new: &str) {
    let file = "t.kif";

    // Path 1: load old, apply diff to new.
    let mut kb_diff = kb_with(old, file);
    let diff        = diff_for(&kb_diff, file, new);
    let r           = kb_diff.apply_file_diff(diff);
    assert!(r.ok, "apply_file_diff errors: {:?}", r.errors);

    // Path 2: load new from scratch.
    let kb_fresh = kb_with(new, file);

    // Compare observable state: file_roots length, file_hashes (as
    // multisets -- sid allocation differs between paths so we can't
    // compare sid lists directly), and the KIF round-trip of every
    // root sentence.

    // 1. Same number of roots in this file.
    assert_eq!(kb_diff.file_roots(file).len(), kb_fresh.file_roots(file).len(),
               "root count differs: diff={} fresh={}",
               kb_diff.file_roots(file).len(), kb_fresh.file_roots(file).len());

    // 2. Multisets of file_hashes match -- the whole point of the
    //    fingerprint is that textually-identical source produces
    //    identical hash multisets regardless of SentenceId assignment.
    let mut h_diff:  Vec<u64> = kb_diff.file_hashes(file).to_vec();
    let mut h_fresh: Vec<u64> = kb_fresh.file_hashes(file).to_vec();
    h_diff.sort_unstable();
    h_fresh.sort_unstable();
    assert_eq!(h_diff, h_fresh,
               "file_hashes multiset differs after diff vs fresh load");

    // 3. KIF string round-trip of every root in both KBs -- as sorted
    //    multisets, since SentenceIds may differ.
    let mut kif_diff: Vec<String> = kb_diff.file_roots(file).iter()
        .map(|&sid| kb_diff.sentence_kif_str(sid))
        .collect();
    let mut kif_fresh: Vec<String> = kb_fresh.file_roots(file).iter()
        .map(|&sid| kb_fresh.sentence_kif_str(sid))
        .collect();
    kif_diff.sort();
    kif_fresh.sort();
    assert_eq!(kif_diff, kif_fresh,
               "sentence multiset differs after diff");
}

#[test]
fn apply_diff_noop_preserves_everything() {
    assert_equivalent_after_diff(
        "(subclass Human Animal)\n(subclass Dog Animal)",
        "(subclass Human Animal)\n(subclass Dog Animal)",
    );
}

#[test]
fn apply_diff_add_at_end() {
    assert_equivalent_after_diff(
        "(subclass Human Animal)",
        "(subclass Human Animal)\n(subclass Dog Animal)",
    );
}

#[test]
fn apply_diff_add_at_start() {
    assert_equivalent_after_diff(
        "(subclass Human Animal)",
        "(subclass Dog Animal)\n(subclass Human Animal)",
    );
}

#[test]
fn apply_diff_remove_first() {
    assert_equivalent_after_diff(
        "(subclass Human Animal)\n(subclass Dog Animal)",
        "(subclass Dog Animal)",
    );
}

#[test]
fn apply_diff_remove_last() {
    assert_equivalent_after_diff(
        "(subclass Human Animal)\n(subclass Dog Animal)",
        "(subclass Human Animal)",
    );
}

#[test]
fn apply_diff_replace_middle() {
    assert_equivalent_after_diff(
        "(subclass A X)\n(subclass B X)\n(subclass C X)",
        "(subclass A X)\n(subclass B2 X)\n(subclass C X)",
    );
}

#[test]
fn apply_diff_whitespace_only_edit() {
    assert_equivalent_after_diff(
        "(subclass Human Animal)\n(subclass Dog Animal)",
        "\n\n(subclass  Human   Animal)\n; a new comment\n(subclass Dog Animal)",
    );
}

#[test]
fn apply_diff_reorder() {
    assert_equivalent_after_diff(
        "(subclass A X)\n(subclass B X)",
        "(subclass B X)\n(subclass A X)",
    );
}

// -- SentenceId stability -----------------------------------------------------

#[test]
fn retained_sentences_keep_their_sentence_ids() {
    let mut kb = kb_with(
        "(subclass Human Animal)\n(subclass Dog Animal)",
        "t.kif",
    );
    let sids_before: Vec<_> = kb.file_roots("t.kif").to_vec();

    // A pure whitespace / comment edit retains everything.
    let diff = diff_for(&kb, "t.kif",
        ";; a new comment\n(subclass Human Animal)\n(subclass Dog Animal)");
    let r = kb.apply_file_diff(diff);
    assert!(r.ok);

    let sids_after: Vec<_> = kb.file_roots("t.kif").to_vec();
    assert_eq!(sids_before, sids_after,
        "retained sentences must keep their SentenceIds across a no-op edit");
}

#[test]
fn added_sentences_get_fresh_ids_not_colliding_with_retained() {
    let mut kb = kb_with("(subclass Human Animal)", "t.kif");
    let before: Vec<_> = kb.file_roots("t.kif").to_vec();

    let diff = diff_for(&kb, "t.kif",
        "(subclass Human Animal)\n(subclass Dog Animal)");
    let r = kb.apply_file_diff(diff);
    assert!(r.ok);

    let after: Vec<_> = kb.file_roots("t.kif").to_vec();
    assert_eq!(after.len(), 2);
    // First sid is retained from before.
    assert_eq!(after[0], before[0]);
    // Second is brand new.
    assert_ne!(after[1], before[0]);
}

// -- Fingerprint regression ---------------------------------------------------

#[test]
fn fingerprint_multiset_matches_after_whitespace_edit() {
    let mut kb = kb_with(
        "(subclass Human Animal)\n(subclass Dog Animal)",
        "t.kif",
    );
    let before: Vec<u64> = kb.file_hashes("t.kif").to_vec();

    let diff = diff_for(&kb, "t.kif",
        "\n\n(subclass Human Animal)\n\n(subclass Dog Animal)\n");
    kb.apply_file_diff(diff);

    let mut after: Vec<u64>  = kb.file_hashes("t.kif").to_vec();
    let mut before2           = before.clone();
    before2.sort_unstable();
    after.sort_unstable();
    assert_eq!(before2, after);
}

#[test]
fn sentence_fingerprint_agrees_with_compute_file_diff_bucketing() {
    // Direct sanity check that compute_file_diff's hash bucketing
    // keys match sentence_fingerprint's output.
    let parsed = parse_document("t.kif", "(subclass Human Animal)");
    assert_eq!(parsed.root_hashes.len(), 1);
    let direct = sentence_fingerprint(&parsed.ast[0]);
    assert_eq!(parsed.root_hashes[0], direct);
}
