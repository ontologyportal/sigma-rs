// Profiling harness for KnowledgeBase::ask on a SUMO-scale KB.
//
// Usage:
//   cargo run --release --example profile_ask --features ask \
//     -- /path/to/Merge.kif /path/to/Mid-level-ontology.kif
//
// Measures: bootstrap load/promote time, and per-query TPTP-generation
// latency + output size.  The prover itself is deliberately not run
// (pointed at a nonexistent path) — we only care about the KB's own
// work, which happens entirely before the subprocess spawn attempt.
// `VampireRunner` sets `timings.input_gen` before calling `Command::spawn`,
// so the measurement is meaningful even when the spawn fails.
//
// Identical source shape on both pre-SInE and post-SInE builds: uses
// only public APIs that exist on both (`KnowledgeBase::{new, load_kif,
// make_session_axiomatic, lookup, ask}` + `VampireRunner`).  The file
// can be dropped into either checkout's examples/ directory to profile.

use std::path::PathBuf;
use std::time::Instant;

use sumo_kb::{KnowledgeBase, TptpLang, VampireRunner};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: profile_ask <merge.kif> <mid-level-ontology.kif>");
        std::process::exit(2);
    }
    let merge_path = PathBuf::from(&args[1]);
    let mid_path   = PathBuf::from(&args[2]);

    // -- Bootstrap --------------------------------------------------
    let mut kb = KnowledgeBase::new();

    let merge_text = std::fs::read_to_string(&merge_path)
        .expect("read Merge.kif");
    let mid_text   = std::fs::read_to_string(&mid_path)
        .expect("read Mid-level-ontology.kif");

    let t_load = Instant::now();
    let r1 = kb.load_kif(&merge_text, "Merge.kif", Some("bootstrap"));
    let r2 = kb.load_kif(&mid_text,   "Mid-level-ontology.kif", Some("bootstrap"));
    let load_time = t_load.elapsed();

    let t_promote = Instant::now();
    kb.make_session_axiomatic("bootstrap");
    let promote_time = t_promote.elapsed();

    let total_roots = kb.lookup("").len();

    println!("=== Bootstrap ===");
    println!("  Merge.kif warnings:             {}", r1.warnings.len());
    println!("  Mid-level-ontology.kif warns:   {}", r2.warnings.len());
    println!("  load (parse + ingest):          {:?}", load_time);
    println!("  promote (make_session_axiomatic): {:?}", promote_time);
    println!("  total root sentences:           {}", total_roots);
    println!();

    // -- Query set --------------------------------------------------
    let queries: &[&str] = &[
        "(subclass Dog ?X)",
        "(instance ?X Human)",
        "(subclass Mammal Animal)",
        "(=> (instance ?X Mammal) (instance ?X Animal))",
        "(instance ?X ?Y)",              // intentionally broad
        "(subclass Car Artifact)",
        "(holdsDuring ?T (attribute ?X Hot))",
        "(part ?X ?Y)",
    ];

    // Fake prover — we only want TPTP generation numbers.  The dump
    // path captures the TPTP so we can measure its size.
    let tptp_dump = std::env::temp_dir().join("profile_ask_tptp.p");
    let runner = VampireRunner {
        vampire_path:   PathBuf::from("/nonexistent/vampire"),
        timeout_secs:   1,
        tptp_dump_path: Some(tptp_dump.clone()),
    };

    println!("=== Queries (FOF; prover intentionally not run) ===");
    println!("{:<55} {:>12} {:>12} {:>12} {:>10}",
             "query", "input_gen", "wall", "tptp_bytes", "fof_rows");

    // Warm-up query to normalise any first-call caches.
    let _ = kb.ask("(instance Foo Bar)", None, &runner, TptpLang::Fof);

    for q in queries {
        let t = Instant::now();
        let r = kb.ask(q, None, &runner, TptpLang::Fof);
        let wall = t.elapsed();

        let (tptp_bytes, fof_rows) = match std::fs::read_to_string(&tptp_dump) {
            Ok(s) => {
                let bytes = s.len();
                let rows  = s.matches("\nfof(").count()
                          + if s.starts_with("fof(") { 1 } else { 0 };
                (bytes, rows)
            }
            Err(_) => (0, 0),
        };

        // Keep output one-line-per-query for easy diffing.
        println!("{:<55} {:>12} {:>12} {:>12} {:>10}",
                 short(q, 55),
                 fmt_dur(r.timings.input_gen),
                 fmt_dur(wall),
                 tptp_bytes,
                 fof_rows);
    }

    // -- Repeat-query profile ---------------------------------------
    //
    // Pre-SInE builds may cache the full TFF axiom set; post-SInE
    // doesn't (since the filtered set differs per query).  Running
    // the same query twice surfaces any cache effect.
    println!();
    println!("=== Same query 3x (tests any per-query caching) ===");
    for i in 0..3 {
        let t = Instant::now();
        let r = kb.ask("(subclass Dog ?X)", None, &runner, TptpLang::Fof);
        let wall = t.elapsed();
        println!("  run {}: input_gen={}  wall={}", i + 1,
                 fmt_dur(r.timings.input_gen), fmt_dur(wall));
    }
}

fn fmt_dur(d: std::time::Duration) -> String {
    if d.as_secs() > 0 {
        format!("{:.3}s", d.as_secs_f64())
    } else {
        format!("{:.3}ms", d.as_secs_f64() * 1000.0)
    }
}

fn short(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_owned() }
    else { format!("{}...", &s[..n-3]) }
}
