use log;
use sumo_parser_core::{load_kif, KnowledgeBase};
use crate::cli::args::KbArgs;
use crate::cli::util::{
    load_and_commit_files, load_kb_from_db,
    open_existing_db, read_stdin, source_tag,
};
use crate::{parse_error, semantic_error};

// ── Validate subcommand ───────────────────────────────────────────────────────

/// Entry point for `sumo validate`.
///
/// Behaviour:
/// * With KIF `-f`/`-d` files: parse → validate → commit to `--db`.
/// * With a `formula` argument and no KIF files: validate the formula against
///   the existing `--db` (the DB must exist).
/// * With both KIF files and a formula: parse+commit files first, then
///   validate the formula against the resulting DB.
pub fn run_validate(formula: Option<String>, kb_args: KbArgs) -> bool {
    log::debug!("run_validate: formula={:?}, db={}", formula.is_some(), kb_args.db.display());

    let has_files = !kb_args.files.is_empty() || !kb_args.dirs.is_empty();
    let formula   = formula.or_else(read_stdin);

    if has_files {
        // Parse KIF files, validate them, and commit to the database.
        log::info!("validate: loading KIF files and committing to database");
        let env = match load_and_commit_files(&kb_args) {
            Ok(e)  => e,
            Err(()) => return false,
        };

        match formula {
            Some(text) => {
                // Also validate an inline formula against the newly committed DB.
                let kb = match load_kb_from_db(&env) {
                    Ok(k)   => k,
                    Err(()) => return false,
                };
                validate_single_formula(kb, &text, source_tag())
            }
            None => {
                // Validate every formula now in the database.
                let kb = match load_kb_from_db(&env) {
                    Ok(k)   => k,
                    Err(()) => return false,
                };
                validate_all_roots(&kb)
            }
        }
    } else if formula.is_some() {
        // No KIF files — validate formula against existing DB.
        log::info!("validate: validating inline formula against existing database");
        let env = match open_existing_db(&kb_args) {
            Ok(e)  => e,
            Err(e) => { log::error!("{}", e); return false; }
        };
        let kb = match load_kb_from_db(&env) {
            Ok(k)   => k,
            Err(()) => return false,
        };
        validate_single_formula(kb, &formula.unwrap(), source_tag())
    } else {
        // No files, no formula — validate all formulas in existing DB.
        log::info!("validate: validating all formulas in existing database");
        let env = match open_existing_db(&kb_args) {
            Ok(e)  => e,
            Err(e) => { log::error!("{}", e); return false; }
        };
        let kb = match load_kb_from_db(&env) {
            Ok(k)   => k,
            Err(()) => return false,
        };
        validate_all_roots(&kb)
    }
}

// ── Validate a single inline formula ─────────────────────────────────────────

/// Validate a single formula string against the KB.
pub fn validate_single_formula(
    mut kb:  KnowledgeBase,
    text:    &str,
    tag:     &str,
) -> bool {
    log::debug!("validate_single_formula: {}", text);
    kb.validate_kb_once();

    let parse_errors = load_kif(&mut kb.store, text, tag);
    let mut ok = true;
    for (span, e) in &parse_errors {
        parse_error!(span, e, text);
        ok = false;
    }
    if !ok { return false; }

    let sids: Vec<_> = kb.store.file_roots.get(tag).cloned().unwrap_or_default();
    if sids.is_empty() {
        log::error!("no sentences were parsed from input");
        return false;
    }

    for sid in sids {
        log::trace!("validating sid={}", sid);
        let span = kb.store.sentences[sid as usize].span.clone();
        if let Err(e) = kb.validate_sentence(sid) {
            semantic_error!(span, e, sid, kb);
            ok = false;
        }
    }
    ok
}

// ── Validate all root formulas in the KB ─────────────────────────────────────

pub fn validate_all_roots(kb: &KnowledgeBase) -> bool {
    log::debug!("validate_all_roots: {} root(s)", kb.store.roots.len());
    let failures: Vec<_> = kb.validate_all().into_iter().collect();

    for (sid, e) in &failures {
        let sent  = &kb.store.sentences[*sid as usize];
        semantic_error!(sent.span, e, *sid, kb);
    }

    let total = kb.store.roots.len();
    let n_err = failures.len();
    if n_err == 0 {
        println!("{} formula(s) validated: all OK", total);
        true
    } else {
        log::warn!("{} formula(s) validated: {} error(s)", total, n_err);
        false
    }
}
