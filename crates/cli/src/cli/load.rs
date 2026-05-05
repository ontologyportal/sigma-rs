use std::fs;

use log;
use sigmakee_rs_core::KnowledgeBase;
use sigmakee_rs_sdk::{LoadOp, SdkError};

use crate::cli::args::KbArgs;
use crate::cli::util::{collect_kif_files, read_kif_file};
use crate::{semantic_error, semantic_warning};

/// Entry point for `sumo load`.
///
/// The only command that writes to the LMDB database.  Two flows:
///
/// ## Default (`flush = false`)
///
/// Per-file reconcile + persist.  Each `-f` / `-d` file is diffed
/// against the DB's current contents under that same file tag; the
/// delta (added + removed) is committed to disk, retained sentences
/// are left as-is.  Files unrelated to the supplied set stay
/// untouched.  This is the idempotent "sync the DB to disk state"
/// flow — safe to run repeatedly.
///
/// ## `flush = true`
///
/// Wipe the whole DB directory and rebuild from just the supplied
/// files.  With no files, leaves an empty initialised DB — useful
/// as a reset when the DB has accumulated stale axioms from earlier
/// loads.  Mirrors the old "full rewrite" semantics.
pub fn run_load(kb_args: KbArgs, flush: bool) -> bool {
    let has_files = !kb_args.files.is_empty() || !kb_args.dirs.is_empty();

    if flush {
        return run_flush(kb_args, has_files);
    }

    // -- Default path: per-file reconcile + incremental commit -------------

    if !has_files {
        // Open-or-create the DB and exit cleanly.  Same behaviour
        // as the legacy "sumo load with no files" path.
        match KnowledgeBase::open(&kb_args.db) {
            Ok(_) => {
                log::info!(
                    "load: no files specified -- database initialised at '{}'",
                    kb_args.db.display()
                );
                return true;
            }
            Err(e) => {
                log::error!(
                    "Failed to open/create database at '{}': {}",
                    kb_args.db.display(),
                    e
                );
                return false;
            }
        }
    }

    let mut kb = match KnowledgeBase::open(&kb_args.db) {
        Ok(k) => k,
        Err(e) => {
            log::error!(
                "Failed to open/create database at '{}': {}",
                kb_args.db.display(),
                e
            );
            return false;
        }
    };

    let all_files = match collect_kif_files(&kb_args) {
        Ok(f)   => f,
        Err(()) => return false,
    };

    // Read every file up-front (for parallel I/O when the
    // `parallel` feature is on inside the CLI helpers) so we can
    // hand pre-resident text to `LoadOp::add_sources`.  Per-file
    // read failures abort the whole load — matches the previous
    // behaviour and is the simplest UX ("first bad file" message
    // instead of N races).
    let mut readable: Vec<(String, String)> = Vec::with_capacity(all_files.len());
    for path in &all_files {
        match read_kif_file(path) {
            Ok(text) => readable.push((path.display().to_string(), text)),
            Err(())  => return false,
        }
    }
    let count = readable.len();

    // Drive `LoadOp` for the reconcile + persist pipeline.  Strict
    // mode (the default) preserves the historical "all-or-nothing
    // commit on semantic errors" gate.  The SDK handles the batched
    // reconcile, the per-file commit loop, and the strict-abort
    // path; the CLI just picks a rendering for the report.
    let report = match LoadOp::new(&mut kb).add_sources(readable).run() {
        Ok(r) => r,
        Err(SdkError::Kb(e)) => {
            log::error!("load: aborted — {}; DB not modified", e);
            return false;
        }
        Err(e) => {
            log::error!("load: {}", e);
            return false;
        }
    };

    // Strict-mode abort: report came back with semantic errors,
    // committed=false.  Render via the standard semantic_error!
    // macro so the user sees the same colour/format as elsewhere.
    if !report.committed {
        for (tag, e) in &report.semantic_errors { semantic_error!(e, kb); let _ = tag; }
        log::error!(
            "load: {} semantic error(s) across {} file(s) -- database not modified \
             (in-memory reconcile discarded on exit)",
            report.semantic_errors.len(), report.files.len(),
        );
        return false;
    }

    // Successful commit.  Render advisory warnings (don't block),
    // then per-file reconcile counts at info, then the aggregate.
    for (_, e) in &report.semantic_errors    { semantic_error!(e, kb); }
    for status in &report.files {
        if status.is_noop() {
            log::info!(target: "sigmakee_rs_core::load",
                "reconciled {}: unchanged ({} retained)", status.tag, status.retained);
        } else {
            log::info!(target: "sigmakee_rs_core::load",
                "reconciled {}: +{} -{} ={}",
                status.tag, status.added, status.removed, status.retained);
        }
        for w in &status.semantic_warnings { semantic_warning!(w, kb); }
    }

    log::info!(
        "load: reconciled {} file(s) into '{}' (+{} added, -{} removed)",
        count, kb_args.db.display(),
        report.total_added, report.total_removed,
    );
    true
}

/// `--flush` path: drop the DB directory entirely, then rebuild from
/// the supplied files.  With no files, the result is an empty
/// initialised database at `kb_args.db`.
fn run_flush(kb_args: KbArgs, has_files: bool) -> bool {
    // Wipe the DB directory if it exists.  `remove_dir_all` is
    // atomic per-inode on all supported filesystems; if the path
    // doesn't exist we just fall through to the create path.
    if kb_args.db.exists() {
        if let Err(e) = fs::remove_dir_all(&kb_args.db) {
            log::error!(
                "load --flush: failed to wipe '{}': {}",
                kb_args.db.display(),
                e
            );
            return false;
        }
        log::info!(target: "sigmakee_rs_core::load",
            "load --flush: wiped '{}'", kb_args.db.display());
    }

    // Ensure a fresh empty DB exists even if no files are supplied.
    let mut kb = match KnowledgeBase::open(&kb_args.db) {
        Ok(k) => k,
        Err(e) => {
            log::error!(
                "load --flush: failed to create database at '{}': {}",
                kb_args.db.display(),
                e
            );
            return false;
        }
    };

    if !has_files {
        log::info!(
            "load --flush: database wiped and initialised empty at '{}'",
            kb_args.db.display()
        );
        return true;
    }

    // Files supplied: drive `LoadOp` against the now-empty DB.
    // Reconcile-against-nothing means everything classifies as
    // "added", which is functionally equivalent to the legacy
    // `promote_assertions_unchecked` flow but reuses the SDK's
    // strict-commit gate for free.
    let all_files = match collect_kif_files(&kb_args) {
        Ok(f)   => f,
        Err(()) => return false,
    };
    let mut readable: Vec<(String, String)> = Vec::with_capacity(all_files.len());
    for path in &all_files {
        match read_kif_file(path) {
            Ok(t)   => readable.push((path.display().to_string(), t)),
            Err(()) => return false,
        }
    }
    let count = readable.len();

    let report = match LoadOp::new(&mut kb).add_sources(readable).run() {
        Ok(r) => r,
        Err(SdkError::Kb(e)) => {
            log::error!("load --flush: aborted — {}; DB not modified", e);
            return false;
        }
        Err(e) => {
            log::error!("load --flush: {}", e);
            return false;
        }
    };
    log::info!("load --flush: parsed {} file(s)", count);

    if !report.committed {
        log::error!(
            "load --flush: {} validation error(s) -- database not modified",
            report.semantic_errors.len()
        );
        for (_, e) in &report.semantic_errors { semantic_error!(e, kb); }
        return false;
    }

    for (_, e) in &report.semantic_errors { semantic_error!(e, kb); }
    for status in &report.files {
        for w in &status.semantic_warnings { semantic_warning!(w, kb); }
    }

    log::info!(
        "load --flush: committed {} assertion(s) to '{}' (+{} added, -{} removed)",
        report.total_added, kb_args.db.display(),
        report.total_added, report.total_removed,
    );
    true
}

