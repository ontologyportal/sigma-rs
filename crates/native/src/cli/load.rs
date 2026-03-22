use log;
use sumo_kb::KnowledgeBase;

use crate::cli::args::KbArgs;
use crate::cli::util::{collect_kif_files, read_kif_file};
use crate::semantic_error;

/// Entry point for `sumo load`.
///
/// The only command that writes to the database.
///
/// 1. Opens (or creates) the LMDB at `--db`.
/// 2. Parses all `-f`/`-d` files as session assertions -- parse errors abort immediately.
/// 3. Validates the loaded assertions -- promoted warnings (`-W`) count as errors and abort.
/// 4. If clean, promotes assertions to axioms and commits them to the database.
pub fn run_load(kb_args: KbArgs) -> bool {
    let has_files = !kb_args.files.is_empty() || !kb_args.dirs.is_empty();

    // Open or create the database.
    let mut kb = match KnowledgeBase::open(&kb_args.db) {
        Ok(k) => k,
        Err(e) => {
            log::error!("Failed to open/create database at '{}': {}", kb_args.db.display(), e);
            return false;
        }
    };

    if !has_files {
        log::info!("load: no files specified -- database initialised at '{}'", kb_args.db.display());
        return true;
    }

    let all_files = match collect_kif_files(&kb_args) {
        Ok(f)   => f,
        Err(()) => return false,
    };

    // -- Phase 1: parse all files as assertions --------------------------------
    const SESSION: &str = "__load__";
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
            return false; // parse errors -> abort, don't touch DB
        }
    }
    log::info!("load: parsed {} file(s)", all_files.len());

    // -- Phase 2: validate all loaded assertions --------------------------------
    // validate_session collects hard errors (including warnings promoted via -W).
    // Regular warnings are logged as WARN by handle() but don't block the commit.
    let errors = kb.validate_session(SESSION);
    if !errors.is_empty() {
        log::error!(
            "load: {} validation error(s) -- database not modified",
            errors.len()
        );
        for (sid, e) in &errors {
            semantic_error!(e, kb);
            let _ = sid;
        }
        return false;
    }

    // Also run the full validate_all so any cross-file warnings are surfaced.
    let all_errors = kb.validate_all();
    let blocking: Vec<_> = all_errors.iter()
        .filter(|(_, e)| !e.is_warn())
        .collect();
    if !blocking.is_empty() {
        log::error!(
            "load: {} hard validation error(s) in combined KB -- database not modified",
            blocking.len()
        );
        for (_, e) in &blocking {
            semantic_error!(e, kb);
        }
        return false;
    }
    // Log non-blocking warnings so the user sees them.
    for (_, e) in &all_errors {
        if e.is_warn() {
            semantic_error!(e, kb);
        }
    }

    // -- Phase 3: commit to database -------------------------------------------
    match kb.promote_assertions_unchecked(SESSION) {
        Ok(report) => {
            log::info!(
                "load: committed {} assertion(s) to '{}' ({} duplicate(s) skipped)",
                report.promoted.len(),
                kb_args.db.display(),
                report.duplicates_removed.len(),
            );
            true
        }
        Err(e) => {
            log::error!("load: failed to commit to database: {}", e);
            false
        }
    }
}
