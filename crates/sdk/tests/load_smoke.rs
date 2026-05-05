//! Phase 4 — `LoadOp` LMDB roundtrip tests.
//!
//! Each test creates a temp directory, opens an LMDB-backed KB
//! against it, drives `LoadOp::run`, then re-opens and verifies the
//! committed state.  Idempotency is exercised by running `LoadOp`
//! twice with identical inputs and asserting the second run reports
//! all-retained, zero-added.

#![cfg(feature = "persist")]

use std::path::Path;

use sigmakee_rs_core::KnowledgeBase;
use sigmakee_rs_sdk::{LoadOp, SdkError};

const KIF_BASE: &str = r#"
    (subclass Animal Organism)
    (subclass Organism PhysicalObject)
    (instance Fido Animal)
"#;

const KIF_EXTRA: &str = r#"
    (subclass Mammal Animal)
"#;

fn write_kif(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn load_persists_to_lmdb_and_reopens_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let db  = tmp.path().join("kb");
    let f1  = write_kif(tmp.path(), "base.kif", KIF_BASE);

    // First load: ingest + commit.
    {
        let mut kb = KnowledgeBase::open(&db).unwrap();
        let report = LoadOp::new(&mut kb).add_file(&f1).run().unwrap();
        assert!(report.committed, "expected committed=true on clean load");
        assert!(report.is_clean());
        assert!(report.total_added > 0);
    }

    // Reopen — sentences should still resolve.
    {
        let kb = KnowledgeBase::open(&db).unwrap();
        assert!(kb.manpage("Animal").is_some());
        assert!(kb.manpage("Organism").is_some());
        assert!(kb.manpage("Fido").is_some());
    }
}

#[test]
fn second_run_is_idempotent_no_new_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let db  = tmp.path().join("kb");
    let f1  = write_kif(tmp.path(), "base.kif", KIF_BASE);

    // First load.
    {
        let mut kb = KnowledgeBase::open(&db).unwrap();
        let r = LoadOp::new(&mut kb).add_file(&f1).run().unwrap();
        assert!(r.total_added > 0);
    }
    // Second load — same content, same path, nothing should change.
    {
        let mut kb = KnowledgeBase::open(&db).unwrap();
        let r = LoadOp::new(&mut kb).add_file(&f1).run().unwrap();
        assert!(r.committed);
        assert_eq!(r.total_added,   0,  "no new sentences on idempotent run");
        assert_eq!(r.total_removed, 0,  "no removals on idempotent run");
        assert!(r.total_retained > 0,    "everything retained verbatim");
        assert!(r.files[0].is_noop());
    }
}

#[test]
fn second_run_with_extra_file_adds_only_the_delta() {
    let tmp = tempfile::tempdir().unwrap();
    let db  = tmp.path().join("kb");
    let base  = write_kif(tmp.path(), "base.kif",  KIF_BASE);
    let extra = write_kif(tmp.path(), "extra.kif", KIF_EXTRA);

    // First load: just the base.
    {
        let mut kb = KnowledgeBase::open(&db).unwrap();
        LoadOp::new(&mut kb).add_file(&base).run().unwrap();
    }
    // Second load: base + extra.  Base should be retained verbatim;
    // extra should be all-added.
    {
        let mut kb = KnowledgeBase::open(&db).unwrap();
        let r = LoadOp::new(&mut kb)
            .add_file(&base)
            .add_file(&extra)
            .run()
            .unwrap();
        assert!(r.committed);

        let base_status = r.files.iter().find(|f| f.tag.ends_with("base.kif")).unwrap();
        assert!(base_status.is_noop(), "base.kif should be a no-op on the second run");

        let extra_status = r.files.iter().find(|f| f.tag.ends_with("extra.kif")).unwrap();
        assert!(extra_status.added > 0, "extra.kif should add sentences");
    }

    // Reopen and verify Mammal is now resolvable.
    {
        let kb = KnowledgeBase::open(&db).unwrap();
        assert!(kb.manpage("Mammal").is_some(),
            "Mammal should survive reopen — proving extra.kif was committed");
    }
}

#[test]
fn add_dir_walks_and_commits_all_kif_children() {
    let tmp = tempfile::tempdir().unwrap();
    let db    = tmp.path().join("kb");
    let onto  = tmp.path().join("ontology");
    std::fs::create_dir(&onto).unwrap();
    write_kif(&onto, "01-base.kif",  KIF_BASE);
    write_kif(&onto, "02-extra.kif", KIF_EXTRA);

    let mut kb = KnowledgeBase::open(&db).unwrap();
    let r = LoadOp::new(&mut kb).add_dir(&onto).run().unwrap();
    assert!(r.committed);
    assert_eq!(r.files.len(), 2);
    // Sorted: 01- before 02-.
    assert!(r.files[0].tag.ends_with("01-base.kif"));
    assert!(r.files[1].tag.ends_with("02-extra.kif"));
}

#[test]
fn add_source_commits_inline_text() {
    let tmp = tempfile::tempdir().unwrap();
    let db  = tmp.path().join("kb");

    {
        let mut kb = KnowledgeBase::open(&db).unwrap();
        let r = LoadOp::new(&mut kb)
            .add_source("ws://upload/42", KIF_BASE)
            .run()
            .unwrap();
        assert!(r.committed);
        assert!(r.total_added > 0);
        assert_eq!(r.files[0].tag, "ws://upload/42");
    }

    {
        let kb = KnowledgeBase::open(&db).unwrap();
        assert!(kb.manpage("Animal").is_some());
    }
}

#[test]
fn missing_file_aborts_with_io_error() {
    let tmp = tempfile::tempdir().unwrap();
    let db  = tmp.path().join("kb");
    let mut kb = KnowledgeBase::open(&db).unwrap();
    let r = LoadOp::new(&mut kb)
        .add_file("/tmp/sigmakee-rs-sdk-no-such-file-xxxxx.kif")
        .run();
    assert!(matches!(r, Err(SdkError::Io { .. })));
}

#[test]
fn parse_error_aborts_with_kb_error_and_no_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let db  = tmp.path().join("kb");
    let bad = write_kif(tmp.path(), "bad.kif", "(subclass A");

    let mut kb = KnowledgeBase::open(&db).unwrap();
    let r = LoadOp::new(&mut kb).add_file(&bad).run();
    assert!(matches!(r, Err(SdkError::Kb(_))));
}

#[test]
fn empty_load_returns_clean_uncommitted_report() {
    let tmp = tempfile::tempdir().unwrap();
    let db  = tmp.path().join("kb");
    let mut kb = KnowledgeBase::open(&db).unwrap();
    let r = LoadOp::new(&mut kb).run().unwrap();
    assert!(!r.committed, "no sources → no commit phase ran");
    assert_eq!(r.files.len(), 0);
    assert_eq!(r.total_added, 0);
}

#[test]
fn progress_callback_emits_promote_events() {
    use std::sync::{Arc, Mutex};

    let tmp = tempfile::tempdir().unwrap();
    let db  = tmp.path().join("kb");
    let f   = write_kif(tmp.path(), "base.kif", KIF_BASE);

    let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let collected = log.clone();

    struct Sink(Arc<Mutex<Vec<&'static str>>>);
    impl sigmakee_rs_sdk::ProgressSink for Sink {
        fn emit(&self, e: &sigmakee_rs_sdk::ProgressEvent) {
            let label = match *e {
                sigmakee_rs_sdk::ProgressEvent::LoadStarted     { .. } => "load-started",
                sigmakee_rs_sdk::ProgressEvent::FileRead        { .. } => "file-read",
                sigmakee_rs_sdk::ProgressEvent::SourceIngested  { .. } => "source-ingested",
                sigmakee_rs_sdk::ProgressEvent::PromoteStarted  { .. } => "promote-started",
                sigmakee_rs_sdk::ProgressEvent::PromoteFinished { .. } => "promote-finished",
                _ => "other",
            };
            self.0.lock().unwrap().push(label);
        }
    }

    let mut kb = KnowledgeBase::open(&db).unwrap();
    let _ = LoadOp::new(&mut kb)
        .add_file(&f)
        .progress(Box::new(Sink(collected)))
        .run()
        .unwrap();

    let evs = log.lock().unwrap().clone();
    // The shape we care about: file-read happens before promote-started,
    // and promote-finished closes things out.
    let pos = |label| evs.iter().position(|&e| e == label);
    assert!(pos("file-read").is_some(),         "FileRead should fire for the disk source");
    assert!(pos("promote-started").is_some(),   "PromoteStarted should fire");
    assert!(pos("promote-finished").is_some(),  "PromoteFinished should fire");
    assert!(pos("file-read") < pos("promote-started"),
        "file-read must precede promote-started in {:?}", evs);
}
