use log;
use sumo_kb::KnowledgeBase;

use crate::cli::args::KbArgs;
use crate::cli::util::{open_or_build_kb, read_stdin, source_tag};
use crate::semantic_error;

/// Entry point for `sumo validate`.
///
/// Opens the DB (if present) and layers any `-f`/`-d` files as in-memory axioms.
/// Never writes to the database.
///
/// - With a `formula` argument: validates that single formula against the KB.
/// - Without a formula: validates every formula in the KB.
pub fn run_validate(formula: Option<String>, kb_args: KbArgs) -> bool {
    log::debug!("run_validate: formula={:?}, db={}", formula.is_some(), kb_args.db.display());

    let formula = formula.or_else(read_stdin);

    let kb = match open_or_build_kb(&kb_args) {
        Ok(k)   => k,
        Err(()) => return false,
    };

    match formula {
        Some(text) => validate_single_formula(kb, &text, source_tag()),
        None       => validate_all_roots(&kb),
    }
}

// ── Validate a single inline formula ─────────────────────────────────────────

pub fn validate_single_formula(mut kb: KnowledgeBase, text: &str, tag: &str) -> bool {
    log::debug!("validate_single_formula: {}", text);

    let result = kb.load_kif(text, tag, Some(tag));
    if !result.ok {
        for e in &result.errors { log::error!("{}", e); }
        return false;
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

// ── Validate all formulas in the KB ──────────────────────────────────────────

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
