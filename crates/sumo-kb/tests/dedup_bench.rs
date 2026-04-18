//! Demonstrates that the clause-based dedup implemented in Phase 5
//! prevents LMDB growth when duplicate formulas are re-ingested, and
//! quantifies the clausification cost.
//!
//! Run with `--nocapture` to see the numbers:
//!
//!   cargo test -p sumo-kb --features "cnf integrated-prover persist" \
//!       --test dedup_bench -- --test-threads=1 --nocapture
//!
//! Gated on `cnf`: dedup is a cnf-feature behaviour.  Also requires
//! `persist` to materialise the LMDB files.
#![cfg(all(feature = "cnf", feature = "persist"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sumo_kb::{KnowledgeBase, TellWarning};

// =========================================================================
//  Helpers
// =========================================================================

fn tmp_dir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("sumo-kb-bench-{}-{}-{}",
        name, std::process::id(), n));
    p
}

fn cleanup(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

/// Find the data.mdb file size (the actual database content, not the
/// pre-allocated map size).
fn data_mdb_bytes(path: &Path) -> Option<u64> {
    fs::metadata(path.join("data.mdb")).ok().map(|m| m.len())
}

fn fmt_bytes(n: u64) -> String {
    if n < 1024             { return format!("{} B", n); }
    if n < 1024 * 1024      { return format!("{:.1} KiB", n as f64 / 1024.0); }
    if n < 1024 * 1024 * 1024 { return format!("{:.2} MiB", n as f64 / (1024.0 * 1024.0)); }
    format!("{:.2} GiB", n as f64 / (1024.0 * 1024.0 * 1024.0))
}

// =========================================================================
//  Dedup demonstration
// =========================================================================

/// Telling the same formula 1001 times produces one stored axiom, one
/// stored clause, and (most importantly) one on-disk formula record --
/// not 1001 of each.  The data.mdb file size at the end is the same as
/// after a single promotion.
#[test]
fn thousand_duplicates_do_not_grow_the_database() {
    const N_DUPLICATES: usize = 1000;
    const FORMULA: &str = "(instance Socrates Mortal)";

    let dir = tmp_dir("dedup-demo");
    cleanup(&dir);

    // -- Round 1: a single formula ----------------------------------------
    println!("\n==== DEDUP DEMO ====");
    println!("round 1: promote 1 formula");

    let (size_after_one, t_one) = {
        let mut kb = KnowledgeBase::open(&dir).expect("open fresh DB");
        let t = Instant::now();
        let r = kb.tell("s1", FORMULA);
        assert!(r.ok, "{:?}", r.errors);
        let tell_elapsed = t.elapsed();

        kb.promote_assertions_unchecked("s1").expect("promote 1");
        drop(kb);

        let size = data_mdb_bytes(&dir).unwrap_or(0);
        (size, tell_elapsed)
    };
    println!("  data.mdb size:      {}", fmt_bytes(size_after_one));
    println!("  first-tell time:    {:?}", t_one);

    // -- Round 2: tell the same formula 1000 more times -------------------
    println!("\nround 2: tell {} duplicates of the same formula", N_DUPLICATES);
    let (size_after_many, tell_total, dup_count) = {
        let mut kb = KnowledgeBase::open(&dir).expect("reopen DB");

        let t = Instant::now();
        let mut dup_count = 0usize;
        for _ in 0..N_DUPLICATES {
            let r = kb.tell("s_dup", FORMULA);
            assert!(r.ok);
            dup_count += r.warnings.iter()
                .filter(|w| matches!(w, TellWarning::DuplicateAxiom { .. }))
                .count();
        }
        let tell_total = t.elapsed();

        // Nothing should make it into the DB -- the formulas are all dups
        // of the existing axiom.  Promote anyway to exercise the full path.
        kb.promote_assertions_unchecked("s_dup").expect("promote dups");
        drop(kb);

        let size = data_mdb_bytes(&dir).unwrap_or(0);
        (size, tell_total, dup_count)
    };
    println!("  data.mdb size:      {}", fmt_bytes(size_after_many));
    println!("  total tell time:    {:?}", tell_total);
    println!("  per-tell avg:       {:?}", tell_total / N_DUPLICATES as u32);
    println!("  dup warnings:       {} / {}", dup_count, N_DUPLICATES);

    // -- Round 3: reopen and confirm the axiom is still exactly one -------
    println!("\nround 3: reopen + re-tell one more time");
    let (final_size, _kb) = {
        let mut kb = KnowledgeBase::open(&dir).expect("reopen final");
        let r = kb.tell("s_final", FORMULA);
        assert!(r.ok);
        let dup = r.warnings.iter()
            .filter(|w| matches!(w, TellWarning::DuplicateAxiom { .. }))
            .count();
        assert_eq!(dup, 1, "one DuplicateAxiom warning after reopen, got {:?}", r.warnings);

        drop(kb);
        (data_mdb_bytes(&dir).unwrap_or(0), ())
    };
    println!("  data.mdb size:      {}", fmt_bytes(final_size));

    // -- Assertions --------------------------------------------------------
    println!("\n==== RESULTS ====");
    println!("  size after 1 formula:       {}", fmt_bytes(size_after_one));
    println!("  size after 1 + {} dups:      {}", N_DUPLICATES, fmt_bytes(size_after_many));
    println!("  delta:                      {} B",
        (size_after_many as i64 - size_after_one as i64));
    println!("  dups caught at tell time:   {} / {}", dup_count, N_DUPLICATES);

    // Every duplicate should have been caught at tell() time.
    assert_eq!(dup_count, N_DUPLICATES,
        "all duplicates should raise DuplicateAxiom, got {}/{}",
        dup_count, N_DUPLICATES);

    // The DB must not have grown.  A tiny LMDB-internal overhead is
    // possible (sequence counter updates, session bookkeeping) but it
    // must be orders of magnitude smaller than what N fresh formulas
    // would cost.
    //
    // Empirically the post-dup size equals the after-one size; guard
    // at 2x as a generous tolerance for LMDB page alignment jitter.
    assert!(
        size_after_many <= size_after_one.saturating_mul(2),
        "DB grew from {} to {} after {} dups -- dedup regressed?",
        size_after_one, size_after_many, N_DUPLICATES,
    );

    cleanup(&dir);
}

// =========================================================================
//  Clausification cost
// =========================================================================

/// Separately measures the clausification cost.  This is the wall-clock
/// time for a single `KnowledgeBase::tell` on a formula the KB has not
/// seen before -- it includes parsing, semantic validation, the
/// NativeConverter pass, Vampire's NewCNF, FFI round-trip, and the
/// canonical-hash bookkeeping.
#[test]
fn clausification_cost_per_formula() {
    // Four formulas of increasing complexity to get a feel for the
    // per-shape cost.  These are tested individually against fresh
    // in-memory KBs so no two clausifications share state.
    let cases: &[(&str, &str)] = &[
        ("ground atom",       "(instance Socrates Mortal)"),
        ("universal",         "(forall (?X) (subclass ?X Entity))"),
        ("implication",       "(forall (?X) (=> (subclass ?X Human) (subclass ?X Animal)))"),
        ("nested existential","(forall (?X) (exists (?Y) (subclass ?Y ?X)))"),
    ];

    println!("\n==== CLAUSIFICATION COST PER FORMULA ====");
    for &(label, kif) in cases {
        // Warm up so the first-ever clausification of the process
        // doesn't skew the number we print.  Vampire's global state
        // and the shared IR lock both amortise across calls.
        let mut warm = KnowledgeBase::new();
        let _ = warm.tell("warm", kif);

        // Measure on a fresh KB so the dedup path actually runs.
        let mut kb = KnowledgeBase::new();
        let t = Instant::now();
        let r = kb.tell("bench", kif);
        let dt = t.elapsed();
        assert!(r.ok, "bench tell failed: {:?}", r.errors);
        println!("  {:<18}  {:?}   (\"{}\")", label, dt, kif);
    }

    // Batch throughput: 100 *distinct* formulas -- each one a fresh
    // clausification, not a dedup hit.  This is the interesting number
    // because it excludes the dedup fast-path.
    println!("\n==== BATCH THROUGHPUT (100 distinct formulas) ====");
    let batch: Vec<String> = (0..100)
        .map(|i| format!("(instance C{} Class{})", i, i % 10))
        .collect();

    let mut kb = KnowledgeBase::new();
    let t = Instant::now();
    for kif in &batch {
        let r = kb.tell("batch", kif);
        assert!(r.ok);
    }
    let dt = t.elapsed();
    println!("  total:       {:?}", dt);
    println!("  per formula: {:?}", dt / batch.len() as u32);
    println!("  throughput:  {:.0} formulas/sec",
        batch.len() as f64 / dt.as_secs_f64());

    // Duplicate-path throughput: feed the same 100 formulas again and
    // measure.  This exercises the dedup fast-path: clausify, hash,
    // look up, skip.  The cost per call is still dominated by the
    // clausification (dedup is a single hash-map probe), so this is
    // almost the same number -- but we print it to show the full
    // story.
    println!("\n==== DEDUP FAST-PATH (same 100 formulas re-told) ====");
    let t = Instant::now();
    let mut dup_total = 0usize;
    for kif in &batch {
        let r = kb.tell("batch2", kif);
        dup_total += r.warnings.iter()
            .filter(|w| matches!(w, TellWarning::DuplicateAssertion { .. }
                                  | TellWarning::DuplicateAxiom      { .. }))
            .count();
    }
    let dt = t.elapsed();
    println!("  total:       {:?}", dt);
    println!("  per formula: {:?}", dt / batch.len() as u32);
    println!("  dups:        {} / {}", dup_total, batch.len());
    assert_eq!(dup_total, batch.len(), "every re-tell should be a dup");
}
