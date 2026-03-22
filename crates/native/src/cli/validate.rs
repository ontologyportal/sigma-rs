use log;
use sumo_kb::{KbError, KnowledgeBase};

use crate::cli::args::KbArgs;
use crate::cli::util::{open_or_build_kb, read_stdin, source_tag};
use crate::{parse_error, semantic_error};

/// Entry point for `sumo validate`.
///
/// Opens the DB (if present) and layers any `-f`/`-d` files as in-memory axioms.
/// Never writes to the database.
///
/// - `parse_only`: skip all semantic checks; only verify the KIF is syntactically valid.
/// - `no_kb_check`: assume loaded KB files are correct; skip semantic validation of them.
/// - With a `formula` argument: validates that single formula against the KB.
/// - Without a formula: validates every formula in the KB.
pub fn run_validate(
    formula:     Option<String>,
    parse_only:  bool,
    no_kb_check: bool,
    kb_args:     KbArgs,
) -> bool {
    log::debug!(
        "run_validate: formula={:?}, parse_only={}, no_kb_check={}, db={}",
        formula.is_some(), parse_only, no_kb_check, kb_args.db.display()
    );

    let formula = formula.or_else(read_stdin);

    let kb = match open_or_build_kb(&kb_args) {
        Ok(k)   => k,
        Err(()) => return false,
    };

    match formula {
        Some(text) => validate_single_formula(kb, &text, source_tag(), parse_only, no_kb_check),
        None       => {
            if parse_only {
                // All files parsed successfully during open_or_build_kb; nothing more to do.
                println!("Parse check passed: OK");
                true
            } else {
                validate_all_roots(&kb)
            }
        }
    }
}

// -- Validate a single inline formula -----------------------------------------

pub fn validate_single_formula(
    mut kb:      KnowledgeBase,
    text:        &str,
    tag:         &str,
    parse_only:  bool,
    no_kb_check: bool,
) -> bool {
    log::debug!("validate_single_formula: parse_only={}, no_kb_check={}", parse_only, no_kb_check);

    // If not skipping KB validation, validate the already-loaded KB sentences first.
    if !parse_only && !no_kb_check {
        let failures = kb.validate_all();
        for (_, e) in &failures {
            semantic_error!(e, kb);
        }
    }

    let result = kb.load_kif(text, tag, Some(tag));
    if !result.ok {
        for e in &result.errors { 
            match e {
                KbError::Parse(p) => parse_error!(p.get_span(), p, text),
                _ => log::error!("{}: {}", "Inline Query: ", e) 
            }
        }
        return false;
    }

    // Parse-only: stop here -- syntax is valid.
    if parse_only {
        println!("Parse check passed: OK");
        return true;
    }

    let sids = kb.session_sids(tag);
    if sids.is_empty() {
        log::error!("no sentences were parsed from input");
        return false;
    }

    let mut ok = true;
    for sid in sids {
        if let Err(e) = kb.validate_sentence(sid) {
            semantic_error!(&e, kb);
            ok = false;
        }
    }
    ok
}

// -- Validate all formulas in the KB ------------------------------------------

pub fn validate_all_roots(kb: &KnowledgeBase) -> bool {
    let failures = kb.validate_all();

    for (_, e) in &failures {
        semantic_error!(e, kb);
    }

    if failures.is_empty() {
        println!("All formulas validated: OK");
        true
    } else {
        log::warn!("{} validation error(s)", failures.len());
        false
    }
}
