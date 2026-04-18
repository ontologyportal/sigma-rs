//! Phase B/C baseline: how long does the ingest path (load_kif + tell)
//! take on Merge.kif + Mid-level-ontology.kif?
//!
//! The target is the `extend_taxonomy` call at the end of every
//! ingest, which today unconditionally full-rebuilds the taxonomy
//! even when none of the new sentences are taxonomy relations.
//!
//!   cargo test -p sumo-kb --features "cnf integrated-prover persist ask" \
//!       --release --test ingest_bench -- --test-threads=1 --nocapture --ignored
#![cfg(all(feature = "cnf", feature = "integrated-prover", feature = "persist", feature = "ask"))]

use std::path::PathBuf;
use std::time::Instant;

use sumo_kb::KnowledgeBase;

fn kb_dir() -> PathBuf {
    if let Ok(p) = std::env::var("SUMO_KB_DIR") { return PathBuf::from(p); }
    let home = std::env::var("HOME").expect("HOME unset");
    PathBuf::from(home).join("projects/sumo")
}

#[test]
#[ignore]
fn ingest_merge_and_milo() {
    let dir = kb_dir();
    let merge_txt = std::fs::read_to_string(dir.join("Merge.kif")).unwrap();
    let milo_txt  = std::fs::read_to_string(dir.join("Mid-level-ontology.kif")).unwrap();

    println!("\n==== INGEST BASELINE ====");

    let mut kb = KnowledgeBase::new();

    let t = Instant::now();
    let r1 = kb.load_kif(&merge_txt, "Merge.kif", Some("files"));
    let merge_time = t.elapsed();
    assert!(r1.ok);
    println!("  load Merge.kif:                {:?}", merge_time);

    let t = Instant::now();
    let r2 = kb.load_kif(&milo_txt, "Mid-level-ontology.kif", Some("files"));
    let milo_time = t.elapsed();
    assert!(r2.ok);
    println!("  load MILO.kif:                 {:?}", milo_time);

    let t = Instant::now();
    kb.make_session_axiomatic("files");
    let promote_time = t.elapsed();
    println!("  make_session_axiomatic:        {:?}", promote_time);

    println!("\n  === per-sentence tell() after bulk load ===");
    // Tell a new, non-taxonomy sentence -- this should be the fast
    // path after Phase B lands.  Baseline: each tell full-rebuilds
    // the taxonomy.
    let tells: &[&str] = &[
        "(attribute Alice Warm)",
        "(part Alice Earth)",
        "(documentation Alice EnglishLanguage \"a fine person\")",
        "(instance Bob Human)",          // taxonomy head -- SHOULD rebuild
        "(holdsDuring Now (located Alice Earth))",
    ];
    for (i, kif) in tells.iter().enumerate() {
        let t = Instant::now();
        let _ = kb.tell("single", kif);
        let dt = t.elapsed();
        println!("  tell[{}]: {:40} {:?}", i, kif, dt);
    }

    println!("\n  === summary ===");
    println!("  Merge + MILO total load:       {:?}", merge_time + milo_time);
    println!("  + promote:                     {:?}", merge_time + milo_time + promote_time);

    println!("\n==== END INGEST BASELINE ====\n");
}
