//! Auto-backfill regression: when a KB is opened with `cnf` on but
//! the feature manifest says it was last written with `cnf` off, the
//! cnf tables (`formula_hashes`, `clauses`, `clause_hashes`) must be
//! populated automatically from the existing axioms.
//!
//! The real-world case is upgrading a cnf-off build to a cnf-on one
//! without re-ingesting.  We simulate it by hand-mutating the
//! persisted `FeatureManifest` to record `cnf: false` after writing
//! with cnf on -- semantically equivalent to the upgrade case.
#![cfg(all(feature = "cnf", feature = "integrated-prover", feature = "persist", feature = "ask"))]

use std::fs;
use std::path::{Path, PathBuf};

use sumo_kb::{KnowledgeBase, TellWarning};

fn tmp_dir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("sumo-kb-backfill-{}-{}-{}",
        name, std::process::id(), n));
    p
}

fn cleanup(p: &Path) { let _ = fs::remove_dir_all(p); }

fn dir_bytes(path: &Path) -> u64 {
    fs::read_dir(path).ok()
        .map(|rd| rd.flatten()
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum())
        .unwrap_or(0)
}

/// Stomp the persisted `feature_manifest` to say `cnf: false`.
///
/// Uses the same bincode + LMDB layout the env module uses.  This
/// lets us simulate a "DB was written by a cnf-off build" scenario
/// without actually having to build and run cnf-off tooling inside
/// the test harness.
///
/// The deserialise + reserialise intentionally mirrors the private
/// `FeatureManifest` shape; if that shape changes, this test will
/// break with a bincode deserialise error and we'll have to update
/// the ad-hoc representation below.
fn stomp_manifest_as_cnf_off(db: &Path) {
    use heed::types::{Bytes, Str};
    use heed::EnvOpenOptions;

    let env = unsafe {
        EnvOpenOptions::new()
            .max_dbs(12)
            .map_size(10 * 1024 * 1024 * 1024)
            .open(db)
            .unwrap()
    };

    let mut wtxn = env.write_txn().unwrap();
    let caches = env.open_database::<Str, Bytes>(&wtxn, Some("caches"))
        .unwrap().expect("caches table must exist");

    // Deserialise the current manifest.  We only need to flip the
    // `features.cnf` bool and re-serialise; use a local shape mirror
    // so we don't need env.rs's private type.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct M { schema: u64, kb_version: u64, features: F }
    #[derive(serde::Serialize, serde::Deserialize)]
    struct F { cnf: bool, integrated_prover: bool, ask: bool }

    let bytes = caches.get(&wtxn, "feature_manifest").unwrap()
        .expect("feature_manifest must be present after write_axioms");
    let mut m: M = bincode::deserialize(bytes).unwrap();
    m.features.cnf = false;
    let new_bytes = bincode::serialize(&m).unwrap();
    caches.put(&mut wtxn, "feature_manifest", &new_bytes).unwrap();
    wtxn.commit().unwrap();
}

#[test]
fn cnf_backfill_populates_formula_hashes_and_dedups_on_reopen() {
    let dir = tmp_dir("cnf-backfill");
    cleanup(&dir);

    // -- Phase 1: write a KB with cnf on, then stomp the manifest to
    //    say cnf was off.  The persisted formula_hashes ARE present
    //    (because cnf is really on), but from the open-time manifest
    //    check's perspective we're in a "cnf off -> on" upgrade.
    //    To make the test realistic, we also *erase* the formula_hashes
    //    table contents to simulate a truly cnf-off-written DB.
    {
        let mut kb = KnowledgeBase::open(&dir).expect("open");
        let r = kb.tell("s", "(instance Dog Animal)");
        assert!(r.ok);
        let r = kb.tell("s", "(instance Cat Animal)");
        assert!(r.ok);
        let report = kb.promote_assertions_unchecked("s").expect("promote");
        assert_eq!(report.promoted.len(), 2);
    }

    // Erase the formula_hashes table contents (simulate: cnf-off
    // writer never populated them).
    {
        use heed::types::Bytes;
        use heed::EnvOpenOptions;
        let env = unsafe {
            EnvOpenOptions::new()
                .max_dbs(12)
                .map_size(10 * 1024 * 1024 * 1024)
                .open(&dir)
                .unwrap()
        };
        let mut wtxn = env.write_txn().unwrap();
        let fh = env.open_database::<Bytes, Bytes>(&wtxn, Some("formula_hashes"))
            .unwrap().expect("formula_hashes table must exist");
        fh.clear(&mut wtxn).unwrap();
        wtxn.commit().unwrap();
    }
    stomp_manifest_as_cnf_off(&dir);

    // -- Phase 2: reopen.  Backfill should auto-fire (we see the
    //    cnf-off manifest, the current build has cnf on, so cnf is in
    //    `added_features`), repopulate formula_hashes from the
    //    existing axioms, and re-stamp the manifest as cnf: true.
    {
        let mut kb = KnowledgeBase::open(&dir).expect("reopen triggers backfill");

        // Telling an existing axiom again must now detect it as a dup.
        let r = kb.tell("s2", "(instance Dog Animal)");
        assert!(r.ok);
        let dups: Vec<_> = r.warnings.iter()
            .filter(|w| matches!(w, TellWarning::DuplicateAxiom { .. }))
            .collect();
        assert_eq!(dups.len(), 1,
            "post-backfill, re-telling an existing axiom must raise DuplicateAxiom; got {:?}",
            r.warnings);
    }

    // -- Phase 3: a second reopen should NOT re-backfill (manifest is
    //    now cnf: true).  Verify the behaviour is still correct.
    {
        let mut kb = KnowledgeBase::open(&dir).expect("reopen");
        let r = kb.tell("s3", "(instance Cat Animal)");
        let dups: usize = r.warnings.iter()
            .filter(|w| matches!(w, TellWarning::DuplicateAxiom { .. }))
            .count();
        assert_eq!(dups, 1,
            "second reopen must still recognise existing axiom as dup");
    }

    let _ = dir_bytes(&dir);  // keep helper referenced
    cleanup(&dir);
}
