use log;
use sumo_kb::TptpOptions;
use sumo_sdk::TranslateOp;

use crate::cli::args::KbArgs;
use crate::cli::util::{open_or_build_kb, parse_lang, read_stdin, source_tag};
use crate::{semantic_error, semantic_warning};

/// Entry point for `sumo translate`.
///
/// Delegates to [`sumo_sdk::TranslateOp`] for both the inline-formula
/// and whole-KB paths.  Findings rendered via the CLI macros from
/// the report's `semantic_errors` / `semantic_warnings` fields.
pub fn run_translate(
    formula:      Option<String>,
    lang:         &str,
    show_numbers: bool,
    show_kif:     bool,
    session:      Option<&str>,
    kb_args:      KbArgs,
) -> bool {
    let formula = formula.or_else(read_stdin);

    let mut kb = match open_or_build_kb(&kb_args) {
        Ok(k)   => k,
        Err(()) => return false,
    };

    let tptp_lang = parse_lang(lang);
    let opts = TptpOptions {
        lang:             tptp_lang,
        hide_numbers:     !show_numbers,
        show_kif_comment: show_kif,
        ..TptpOptions::default()
    };

    match formula {
        Some(text) => {
            let tag = source_tag();
            // Build the TranslateOp.  `.options(opts)` lets us pass
            // through CLI-only flags (hide_numbers, show_kif_comment).
            let mut op = TranslateOp::formula(&mut kb, tag, &text)
                .lang(tptp_lang)
                .show_numbers(show_numbers)
                .show_kif_comments(show_kif);
            if let Some(s) = session { op = op.session(s); }
            // Replace whole options block to pick up any future
            // TptpOptions field additions the dedicated setters
            // don't cover.  Equivalent to the old direct call.
            op = op.options(opts);

            let report = match op.run() {
                Ok(r)  => r,
                Err(e) => {
                    log::error!("{}", e);
                    return false;
                }
            };

            // The SDK's TranslateReport already carries `tptp` as the
            // joined output (with optional KIF comments interleaved
            // when show_kif_comments is on).  Just write it out.
            for (_, e) in &report.semantic_warnings { semantic_warning!(e, kb); }
            print!("{}", report.tptp);
            true
        }
        None => {
            // Whole-KB translate.  Render warnings/errors first so
            // they precede the TPTP output, matching the legacy
            // ordering.
            let f = kb.validate_all_findings();
            for (_, e) in &f.errors   { semantic_error!(e, kb); }
            for (_, e) in &f.warnings { semantic_warning!(e, kb); }

            let mut op = TranslateOp::kb(&mut kb).options(opts);
            if let Some(s) = session { op = op.session(s); }
            let report = match op.run() {
                Ok(r)  => r,
                Err(e) => {
                    log::error!("{}", e);
                    return false;
                }
            };
            print!("{}", report.tptp);
            true
        }
    }
}
