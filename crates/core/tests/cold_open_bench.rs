//! Phase D baseline: how long does it take to open a persisted KB
//! with Merge.kif + Mid-level-ontology.kif, and where does the time
//! go?
//!
//! Run with:
//!
//!   cargo test -p sigmakee-rs-core --features "cnf integrated-prover persist ask" \
//!       --release --test cold_open_bench -- \
//!       --test-threads=1 --nocapture --ignored
//!
//! Requires Merge.kif + Mid-level-ontology.kif in `$sigmakee_rs_core_DIR`
//! (default: `$HOME/projects/sumo`).
#![cfg(all(feature = "cnf", feature = "integrated-prover", feature = "persist", feature = "ask"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sigmakee_rs_core::KnowledgeBase;

fn kb_dir() -> PathBuf {
    if let Ok(p) = std::env::var("sigmakee_rs_core_DIR") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").expect("HOME unset");
    PathBuf::from(home).join("projects/sumo")
}

fn tmp_dir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("sigmakee-rs-core-coldopen-{}-{}-{}",
        name, std::process::id(), n));
    p
}

fn cleanup(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

fn dir_bytes(path: &Path) -> u64 {
    fs::read_dir(path)
        .ok()
        .map(|rd| rd.flatten()
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum::<u64>())
        .unwrap_or(0)
}

fn fmt_bytes(n: u64) -> String {
    if n < 1024           { return format!("{} B", n); }
    if n < 1024 * 1024    { return format!("{:.1} KiB", n as f64 / 1024.0); }
    if n < 1024_u64.pow(3) { return format!("{:.2} MiB", n as f64 / (1024.0 * 1024.0)); }
    format!("{:.2} GiB", n as f64 / (1024.0_f64.powi(3)))
}

#[test]
#[ignore]
fn cold_open_baseline() {
    let dir = tmp_dir("baseline");
    cleanup(&dir);

    let merge_path = kb_dir().join("Merge.kif");
    let milo_path  = kb_dir().join("Mid-level-ontology.kif");
    assert!(merge_path.exists(), "Merge.kif not at {:?}", merge_path);
    assert!(milo_path.exists(),  "MILO.kif not at {:?}", milo_path);

    println!("\n==== PHASE D BASELINE: cold-open from LMDB ====");

    // -- Phase 1: initial load + promote + close ----------------------
    let merge_txt = fs::read_to_string(&merge_path).unwrap();
    let milo_txt  = fs::read_to_string(&milo_path).unwrap();
    let load_time = {
        let t = Instant::now();
        let mut kb = KnowledgeBase::open(&dir).expect("open new DB");
        let r1 = kb.load_kif(&merge_txt, "Merge.kif", Some("files"));
        assert!(r1.ok, "Merge load failed: {:?}", r1.errors);
        let r2 = kb.load_kif(&milo_txt,  "Mid-level-ontology.kif", Some("files"));
        assert!(r2.ok, "MILO load failed: {:?}", r2.errors);
        let report = kb.promote_assertions_unchecked("files")
            .expect("promote failed");
        println!("  initial load: {} axioms promoted", report.promoted.len());
        let elapsed = t.elapsed();
        drop(kb);  // flushes LMDB
        elapsed
    };
    println!("  initial load+promote+close:     {:?}", load_time);

    let lmdb_size = dir_bytes(&dir);
    println!("  LMDB dir size after commit:     {}", fmt_bytes(lmdb_size));

    // -- Phase 2: cold open ------------------------------------------
    println!("\n  === cold open (no in-memory state) ===");
    let (kb, open_time) = {
        let t = Instant::now();
        let kb = KnowledgeBase::open(&dir).expect("cold open");
        (kb, t.elapsed())
    };
    println!("  cold open wall-clock:           {:?}", open_time);

    // -- Phase 3: first ask_embedded (builds axiom cache) ------------
    // Note: kb is moved out of the Option so we can call &mut on it.
    let mut kb = kb;
    println!("\n  === first ask_embedded after cold open ===");
    let t = Instant::now();
    let _r = kb.ask_embedded("(attribute Alice Warm)", None, 3, sigmakee_rs_core::TptpLang::Tff);
    let first_ask = t.elapsed();
    println!("  first ask_embedded:             {:?}", first_ask);

    // -- Phase 4: repeated asks --------------------------------------
    println!("\n  === repeated asks (axiom cache warm) ===");
    let mut warm_asks = Vec::new();
    for i in 2..=5 {
        let t = Instant::now();
        let _r = kb.ask_embedded("(attribute Alice Warm)", None, 3, sigmakee_rs_core::TptpLang::Tff);
        let dt = t.elapsed();
        warm_asks.push(dt);
        println!("  ask #{i}:                         {dt:?}");
    }
    let warm_avg = warm_asks.iter().sum::<std::time::Duration>() / warm_asks.len() as u32;
    println!("  warm average:                   {:?}", warm_avg);

    println!("\n  === summary ===");
    println!("  cold open -> first ask delta:   {:?}",
        first_ask.saturating_sub(warm_avg));
    println!("  LMDB size on disk:              {}", fmt_bytes(lmdb_size));

    // Second cold open: measure the taxonomy + sort_annotations cache
    // restore path.  (We do NOT persist the axiom cache -- the TPTP
    // reparse cost exceeds the in-memory rebuild cost.)
    drop(kb);
    println!("\n  === SECOND cold open (caches warm on disk) ===");
    let t = Instant::now();
    let mut kb2 = KnowledgeBase::open(&dir).expect("second cold open");
    let second_open = t.elapsed();
    println!("  second cold open:               {:?}", second_open);

    let t = Instant::now();
    let _r = kb2.ask_embedded("(attribute Alice Warm)", None, 3, sigmakee_rs_core::TptpLang::Tff);
    let second_first_ask = t.elapsed();
    println!("  first ask after reopen:         {:?}", second_first_ask);

    let t = Instant::now();
    let _r = kb2.ask_embedded("(attribute Alice Warm)", None, 3, sigmakee_rs_core::TptpLang::Tff);
    let second_warm_ask = t.elapsed();
    println!("  second ask (warm):              {:?}", second_warm_ask);

    println!("\n  === summary ===");
    println!("  Final LMDB size:                {}",
        fmt_bytes(dir_bytes(&dir)));

    println!("\n==== END PHASE D BASELINE ====\n");
    drop(kb2);
    cleanup(&dir);
}
