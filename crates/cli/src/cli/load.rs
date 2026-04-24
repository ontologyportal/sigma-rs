use std::fs;

use log;
use sumo_kb::{KnowledgeBase, SentenceId};

use crate::cli::args::KbArgs;
use crate::cli::util::{collect_kif_files, read_kif_file};
use crate::semantic_error;

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

    let mut total_added   = 0usize;
    let mut total_removed = 0usize;

    // Gate every persistent mutation on "no hard validation errors
    // anywhere in the batch" so `-W <code>` and `-Wall` behave the
    // same way they do under `--flush`: reconcile all files into
    // memory, then either commit every delta or commit none.
    //
    // The `parse_errors` path still aborts early per-file — the
    // KIF parser's failure modes (unterminated list, bad literal,
    // etc.) are local to a single file and don't benefit from
    // whole-batch context.
    let mut pending: Vec<(String, Vec<SentenceId>, Vec<SentenceId>)> =
        Vec::with_capacity(all_files.len());
    let mut total_semantic_errors = 0usize;

    for path in &all_files {
        let text = match read_kif_file(path) {
            Ok(t)   => t,
            Err(()) => return false,
        };
        let tag = path.display().to_string();

        let report = kb.reconcile_file(&tag, &text);

        if !report.parse_errors.is_empty() {
            for e in &report.parse_errors {
                log::error!("{}: {}", path.display(), e);
            }
            log::error!("load: aborted — parse errors in {}; DB not modified", tag);
            return false;
        }

        // Surface every semantic error.  An entry lands in
        // `semantic_errors` only when `validate_sentence` returned
        // `Err` — i.e. a true hard error *or* a warning promoted
        // via `-W` / `-Wall`.  Plain warnings are logged by
        // `SemanticError::handle` and never reach us here.
        for e in &report.semantic_errors {
            semantic_error!(e, kb);
        }
        total_semantic_errors += report.semantic_errors.len();

        if report.is_noop() {
            log::info!(target: "sumo_kb::load",
                "reconciled {}: unchanged ({} retained)", tag, report.retained);
        } else {
            log::info!(target: "sumo_kb::load",
                "reconciled {}: +{} -{} ={}",
                tag, report.added(), report.removed(), report.retained);
        }

        total_added   += report.added();
        total_removed += report.removed();
        pending.push((tag, report.removed_sids, report.added_sids));
    }

    // All-or-nothing commit gate.  Matches `--flush` semantics:
    // a single hard validation error anywhere aborts the whole
    // load and leaves the DB exactly as it was on disk.
    if total_semantic_errors > 0 {
        log::error!(
            "load: {} semantic error(s) across {} file(s) -- database not modified \
             (in-memory reconcile discarded on exit)",
            total_semantic_errors, all_files.len(),
        );
        return false;
    }

    // Commit pass — per-file.  Each `persist_reconcile_diff` is its
    // own pair of transactions (delete + write), so a mid-batch
    // failure leaves earlier files committed and later ones
    // untouched.  Not atomic across files, but each file is
    // individually consistent; an interrupted load resumes cleanly
    // on the next invocation via reconcile's idempotence.
    for (tag, removed_sids, added_sids) in &pending {
        if let Err(e) = kb.persist_reconcile_diff(removed_sids, added_sids) {
            log::error!("load: failed to commit delta for {}: {}", tag, e);
            return false;
        }
    }

    log::info!(
        "load: reconciled {} file(s) into '{}' (+{} added, -{} removed)",
        all_files.len(),
        kb_args.db.display(),
        total_added,
        total_removed,
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
        log::info!(target: "sumo_kb::load",
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

    // Files supplied: parse-into-session → validate → promote —
    // identical to the pre-reconcile `sumo load` flow, but against
    // a guaranteed-empty DB.
    let all_files = match collect_kif_files(&kb_args) {
        Ok(f)   => f,
        Err(()) => return false,
    };

    const SESSION: &str = sumo_kb::session_tags::SESSION_LOAD;
    for path in &all_files {
        let text = match read_kif_file(path) {
            Ok(t)   => t,
            Err(()) => return false,
        };
        let tag = path.display().to_string();
        let result = kb.load_kif(&text, &tag, Some(SESSION));
        if !result.ok {
            for e in &result.errors {
                log::error!("{}: {}", path.display(), e);
            }
            return false;
        }
    }
    log::info!("load --flush: parsed {} file(s)", all_files.len());

    // Validate (same semantics as the legacy load path — promoted
    // warnings abort the commit).
    let errors = kb.validate_session(SESSION);
    if !errors.is_empty() {
        log::error!(
            "load --flush: {} validation error(s) -- database not modified",
            errors.len()
        );
        for (sid, e) in &errors {
            semantic_error!(e, kb);
            let _ = sid;
        }
        return false;
    }
    let all_errors = kb.validate_all();
    let blocking: Vec<_> = all_errors.iter().filter(|(_, e)| !e.is_warn()).collect();
    if !blocking.is_empty() {
        log::error!(
            "load --flush: {} hard validation error(s) -- database not modified",
            blocking.len()
        );
        for (_, e) in &blocking {
            semantic_error!(e, kb);
        }
        return false;
    }
    for (_, e) in &all_errors {
        if e.is_warn() {
            semantic_error!(e, kb);
        }
    }

    match kb.promote_assertions_unchecked(SESSION) {
        Ok(report) => {
            log::info!(
                "load --flush: committed {} assertion(s) to '{}' ({} duplicate(s) skipped)",
                report.promoted.len(),
                kb_args.db.display(),
                report.duplicates_removed.len(),
            );
            true
        }
        Err(e) => {
            log::error!("load --flush: failed to commit to database: {}", e);
            false
        }
    }
}

