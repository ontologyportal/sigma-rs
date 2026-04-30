use log;
use sumo_kb::{KbError, KnowledgeBase};
use sumo_sdk::ValidateOp;

use crate::cli::args::KbArgs;
use crate::cli::util::{open_or_build_kb, read_stdin, source_tag};
use crate::{parse_error, semantic_error, semantic_warning};

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

    let mut op = ValidateOp::formula(&mut kb, tag, text);
    op = op.parse_only(parse_only).skip_kb_check(no_kb_check);
    let report = match op.run() {
        Ok(r)  => r,
        Err(e) => {
            log::error!("validate: {}", e);
            return false;
        }
    };

    // Surface parse errors with the colourised macro that knows
    // about the source text in scope.  The SDK's report is plain
    // KbError values; we restore the rich rendering here.
    if !report.parse_errors.is_empty() {
        for e in &report.parse_errors {
            match e {
                KbError::Parse(p) => parse_error!(p.get_span(), p, text),
                _ => log::error!("{}: {}", "Inline Query: ", e),
            }
        }
        return false;
    }

    // Render findings (the KB-pre-pass plus the inline-formula pass
    // both show up here).  Errors fail the run; warnings don't.
    for (_, e) in &report.semantic_errors   { semantic_error!(e, kb); }
    for (_, e) in &report.semantic_warnings { semantic_warning!(e, kb); }

    if parse_only {
        println!("Parse check passed: OK");
        return true;
    }

    if !report.is_clean() {
        return false;
    }

    if report.inspected == 0 && !report.parse_errors.is_empty() {
        log::error!("no sentences were parsed from input");
        return false;
    }
    true
}

// -- Validate all formulas in the KB ------------------------------------------

pub fn validate_all_roots(kb: &KnowledgeBase) -> bool {
    // ValidateOp's whole-KB pass calls validate_all_findings under
    // the hood; we render via the classified macros.  `&KnowledgeBase`
    // is read-only here but ValidateOp wants &mut for its formula
    // path; we hand-roll the rendering instead to avoid a redundant
    // mutable borrow.
    let findings = kb.validate_all_findings();

    for (_, e) in &findings.errors   { semantic_error!(e, kb); }
    for (_, e) in &findings.warnings { semantic_warning!(e, kb); }

    if findings.is_clean() {
        println!("All formulas validated: OK");
        true
    } else {
        log::warn!(
            "{} validation error(s), {} warning(s)",
            findings.errors.len(), findings.warnings.len(),
        );
        false
    }
}
