//! Phase 1 smoke tests for `sumo-sdk`'s `IngestOp`.
//!
//! Covers the three ingest postures: already-in-memory text, a file
//! the SDK reads, and a directory the SDK walks.  Mixed builders are
//! exercised in `mixed_sources_in_one_op`.

use sumo_kb::KnowledgeBase;
use sumo_sdk::{IngestOp, ProgressEvent, ProgressSink, SdkError};

#[test]
fn empty_op_is_a_noop() {
    let mut kb = KnowledgeBase::new();
    let report = IngestOp::new(&mut kb).run().unwrap();
    assert_eq!(report.sources.len(), 0);
    assert_eq!(report.total_added, 0);
    assert!(kb.manpage("Animal").is_none());
}

#[test]
fn single_source_ingests_and_promotes() {
    let mut kb = KnowledgeBase::new();
    let kif = r#"
        (subclass Animal Organism)
        (subclass Organism PhysicalObject)
    "#;
    let report = IngestOp::new(&mut kb)
        .add_source("<base>", kif)
        .run()
        .unwrap();

    assert_eq!(report.sources.len(), 1);
    assert!(!report.sources[0].was_reconciled, "first ingest is fresh-load");
    assert!(report.sources[0].added > 0);

    // Symbols resolve, proving parse → intern → axiom-promote completed.
    assert!(kb.manpage("Animal").is_some());
    assert!(kb.manpage("PhysicalObject").is_some());
}

#[test]
fn multiple_sources_are_each_reported() {
    let mut kb = KnowledgeBase::new();
    let report = IngestOp::new(&mut kb)
        .add_source("<a>", "(subclass A B)")
        .add_source("<b>", "(subclass C D)")
        .run()
        .unwrap();

    assert_eq!(report.sources.len(), 2);
    assert_eq!(report.sources[0].tag, "<a>");
    assert_eq!(report.sources[1].tag, "<b>");
    assert!(report.total_added >= 2);
}

#[test]
fn re_ingest_same_tag_takes_reconcile_path() {
    let mut kb = KnowledgeBase::new();
    IngestOp::new(&mut kb)
        .add_source("<x>", "(subclass A B)")
        .run()
        .unwrap();

    // Re-ingest the same tag with a different body — the SDK should
    // diff via reconcile, not reload from scratch.
    let report = IngestOp::new(&mut kb)
        .add_source("<x>", "(subclass A C)")
        .run()
        .unwrap();
    assert!(report.sources[0].was_reconciled, "second ingest of same tag should reconcile");
}

#[test]
fn parse_error_aborts_with_kb_error() {
    let mut kb = KnowledgeBase::new();
    let result = IngestOp::new(&mut kb)
        .add_source("<bad>", "(subclass A")
        .run();
    assert!(matches!(result, Err(sumo_sdk::SdkError::Kb(_))));
}

#[test]
fn add_sources_iter_works() {
    let mut kb = KnowledgeBase::new();
    let pairs: Vec<(String, String)> = vec![
        ("<a>".into(), "(subclass A B)".into()),
        ("<b>".into(), "(subclass C D)".into()),
    ];
    let report = IngestOp::new(&mut kb).add_sources(pairs).run().unwrap();
    assert_eq!(report.sources.len(), 2);
}

#[test]
fn progress_callback_fires_for_each_source() {
    use std::sync::{Arc, Mutex};

    let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let collected = events.clone();

    struct Sink(Arc<Mutex<Vec<&'static str>>>);
    impl ProgressSink for Sink {
        fn emit(&self, e: &ProgressEvent) {
            let label = match *e {
                ProgressEvent::LoadStarted     { .. } => "load-started",
                ProgressEvent::SourceIngested  { .. } => "source-ingested",
                _ => "other",
            };
            self.0.lock().unwrap().push(label);
        }
    }

    let mut kb = KnowledgeBase::new();
    let _ = IngestOp::new(&mut kb)
        .add_source("<a>", "(subclass A B)")
        .add_source("<b>", "(subclass C D)")
        .progress(Box::new(Sink(collected)))
        .run()
        .unwrap();

    let evs = events.lock().unwrap().clone();
    assert_eq!(evs.first().copied(), Some("load-started"));
    assert_eq!(evs.iter().filter(|&&s| s == "source-ingested").count(), 2);
}

// ---------------------------------------------------------------------------
// File / Dir paths (SDK does the I/O)
// ---------------------------------------------------------------------------

fn write_kif_file(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn add_file_reads_from_disk_and_uses_path_as_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_kif_file(tmp.path(), "base.kif", "(subclass Animal Organism)");

    let mut kb = KnowledgeBase::new();
    let report = IngestOp::new(&mut kb).add_file(&path).run().unwrap();

    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].tag, path.display().to_string());
    assert!(kb.manpage("Animal").is_some());
}

#[test]
fn add_dir_walks_kif_files_sorted() {
    let tmp = tempfile::tempdir().unwrap();
    write_kif_file(tmp.path(), "b.kif", "(subclass C D)");
    write_kif_file(tmp.path(), "a.kif", "(subclass A B)");
    // Non-`.kif` files in the dir must be ignored.
    write_kif_file(tmp.path(), "readme.txt", "ignore me");

    let mut kb = KnowledgeBase::new();
    let report = IngestOp::new(&mut kb).add_dir(tmp.path()).run().unwrap();

    assert_eq!(report.sources.len(), 2);
    // Sorted: a.kif before b.kif.
    assert!(report.sources[0].tag.ends_with("a.kif"));
    assert!(report.sources[1].tag.ends_with("b.kif"));
}

#[test]
fn add_file_missing_returns_io_error() {
    let mut kb = KnowledgeBase::new();
    let result = IngestOp::new(&mut kb)
        .add_file("/tmp/sumo-sdk-no-such-file-xxxxx.kif")
        .run();
    assert!(matches!(result, Err(SdkError::Io { .. })));
}

#[test]
fn add_dir_missing_returns_dir_read_error() {
    let mut kb = KnowledgeBase::new();
    let result = IngestOp::new(&mut kb)
        .add_dir("/tmp/sumo-sdk-no-such-dir-xxxxx")
        .run();
    assert!(matches!(result, Err(SdkError::DirRead { .. })));
}

#[test]
fn mixed_sources_in_one_op() {
    let tmp = tempfile::tempdir().unwrap();
    let f = write_kif_file(tmp.path(), "from-disk.kif", "(subclass Disk Storage)");

    let mut kb = KnowledgeBase::new();
    let report = IngestOp::new(&mut kb)
        .add_file(&f)
        .add_source("ws://network/q1", "(subclass Q1 Query)")
        .run()
        .unwrap();

    assert_eq!(report.sources.len(), 2);
    assert!(report.sources[0].tag.ends_with("from-disk.kif"));
    assert_eq!(report.sources[1].tag, "ws://network/q1");
    assert!(kb.manpage("Disk").is_some());
    assert!(kb.manpage("Q1").is_some());
}

#[test]
fn file_read_progress_event_fires_only_for_disk_sources() {
    use std::sync::{Arc, Mutex};

    let tmp = tempfile::tempdir().unwrap();
    let f = write_kif_file(tmp.path(), "f.kif", "(subclass A B)");

    let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let collected = events.clone();

    struct Sink(Arc<Mutex<Vec<&'static str>>>);
    impl ProgressSink for Sink {
        fn emit(&self, e: &ProgressEvent) {
            let label = match *e {
                ProgressEvent::LoadStarted    { .. } => "load-started",
                ProgressEvent::FileRead       { .. } => "file-read",
                ProgressEvent::SourceIngested { .. } => "source-ingested",
                _ => "other",
            };
            self.0.lock().unwrap().push(label);
        }
    }

    let mut kb = KnowledgeBase::new();
    let _ = IngestOp::new(&mut kb)
        .add_file(&f)                                 // expects FileRead
        .add_source("<inline>", "(subclass C D)")     // no FileRead
        .progress(Box::new(Sink(collected)))
        .run()
        .unwrap();

    let evs = events.lock().unwrap().clone();
    assert_eq!(evs.iter().filter(|&&s| s == "file-read").count(), 1,
        "FileRead fires once (only for the disk source), got {:?}", evs);
    assert_eq!(evs.iter().filter(|&&s| s == "source-ingested").count(), 2);
}
