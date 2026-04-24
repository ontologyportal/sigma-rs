//! Phase A baseline benchmark — measures the cost of the ask()
//! invalidation pattern on a realistic-size KB.
//!
//! Run with `--nocapture --ignored`:
//!
//!   cargo test -p sumo-kb --features "cnf integrated-prover persist" \
//!       --release --test invalidation_bench -- \
//!       --test-threads=1 --nocapture --ignored
//!
//! The benchmark is `#[ignore]` because it wants the caller-provided
//! Merge.kif + Mid-level-ontology.kif, which may not be on every
//! developer's machine.  Set env var `SUMO_KB_DIR` (defaults to
//! `$HOME/projects/sumo`) to point at a directory containing both
//! files.
#![cfg(all(feature = "cnf", feature = "integrated-prover", feature = "persist", feature = "ask"))]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use sumo_kb::{KnowledgeBase, TptpLang};
use sumo_kb::prover::VampireRunner;

fn kb_dir() -> PathBuf {
    if let Ok(p) = std::env::var("SUMO_KB_DIR") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").expect("HOME unset");
    PathBuf::from(home).join("projects/sumo")
}

fn load_merge_and_milo() -> (KnowledgeBase, Duration) {
    let dir = kb_dir();
    let merge = dir.join("Merge.kif");
    let milo  = dir.join("Mid-level-ontology.kif");
    assert!(merge.exists(), "Merge.kif not found at {:?}", merge);
    assert!(milo.exists(),  "Mid-level-ontology.kif not found at {:?}", milo);

    let merge_txt = std::fs::read_to_string(&merge).unwrap();
    let milo_txt  = std::fs::read_to_string(&milo).unwrap();

    let mut kb = KnowledgeBase::new();

    let t = Instant::now();
    let r1 = kb.load_kif(&merge_txt, "Merge.kif", Some("__files__"));
    assert!(r1.ok, "Merge.kif failed to load: {:?}", r1.errors);
    let r2 = kb.load_kif(&milo_txt,  "Mid-level-ontology.kif", Some("__files__"));
    assert!(r2.ok, "MILO failed to load: {:?}", r2.errors);
    kb.make_session_axiomatic("__files__");
    let elapsed = t.elapsed();

    (kb, elapsed)
}

#[test]
#[ignore]
fn measure_ask_invalidation_cost() {
    println!("\n==== PHASE A BASELINE ====");
    println!("loading Merge.kif + Mid-level-ontology.kif ...");

    let (mut kb, load_time) = load_merge_and_milo();
    println!("  KB load (+ promote to axioms):  {:?}", load_time);

    // Ask the first query.  This triggers:
    //   ingest query -> invalidate_cache -> prove -> remove_file
    //   -> rebuild_taxonomy -> invalidate_cache
    //
    // We expect the warm path (ask 2, 3, ...) to NOT repay the
    // taxonomy-rebuild cost on every call, once Phase A lands.
    let runner = VampireRunner {
        vampire_path:   std::path::PathBuf::from("vampire"),
        timeout_secs:   3,
        tptp_dump_path: None,
    };

    // Measure two query shapes side by side:
    //   - taxonomy-head: Phase A keeps the rebuild (conservative).
    //   - non-taxonomy:  Phase A skips the rebuild (the optimised path).
    //
    // The gap between the two is the Phase A win.
    fn bench(label: &str, kb: &mut KnowledgeBase, query: &str) {
        println!("\n  === {label}: \"{query}\" ===");
        // Warm-up: prime any lazy caches.
        let _ = kb.ask_embedded(query, None, 3);
        let mut times: Vec<Duration> = Vec::with_capacity(5);
        for i in 1..=5 {
            let t = Instant::now();
            let _r = kb.ask_embedded(query, None, 3);
            let dt = t.elapsed();
            times.push(dt);
            println!("    ask_embedded #{i}:  {dt:?}");
        }
        let avg: Duration = times.iter().sum::<Duration>() / times.len() as u32;
        println!("    average:          {avg:?}");
    }

    bench("taxonomy-head (rebuild kept)",    &mut kb, "(subclass Human Animal)");
    bench("non-taxonomy (rebuild skipped)",  &mut kb, "(attribute Alice Warm)");
    bench("non-taxonomy (rebuild skipped)",  &mut kb, "(part Alice Earth)");

    // Subprocess path (vampire binary may not be installed -- the times
    // reflect everything except the prove call).
    println!("\n  === subprocess ask (vampire may be missing) ===");
    let t = Instant::now();
    let _r1 = kb.ask("(attribute Alice Warm)", None, &runner, TptpLang::Fof);
    let ask1 = t.elapsed();
    println!("    ask #1 wall-clock:  {ask1:?}");

    let mut asks: Vec<Duration> = Vec::with_capacity(5);
    for i in 2..=6 {
        let t = Instant::now();
        let _r = kb.ask("(attribute Alice Warm)", None, &runner, TptpLang::Fof);
        let dt = t.elapsed();
        asks.push(dt);
        println!("    ask #{i} wall-clock:  {dt:?}");
    }
    let avg: Duration = asks.iter().sum::<Duration>() / asks.len() as u32;
    println!("    average:             {avg:?}");

    println!("\n==== END PHASE A BENCH ====\n");
}
