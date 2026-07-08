use std::path::PathBuf;

use log;
use sigmakee_rs_sdk::{HasTranslation, TopLayer, TptpOptions};
use sigmakee_rs_sdk::{SdkError, Session, Source};
use sigmakee_rs_sdk::manager::KBManager;

use crate::cli::util::{read_stdin};

/// Entry point for `sumo translate`.
///
/// Delegates to [`sigmakee_rs_sdk::TranslateOp`] for both the inline-formula
/// and whole-KB paths.  Findings rendered via the CLI macros from
/// the report's `semantic_errors` / `semantic_warnings` fields.
///
/// When `test` is `Some`, the inline/whole-KB paths are bypassed and the
/// given `.kif.tq` file is translated into the exact TPTP problem the prover
/// would receive — see [`run_translate_test`] (requires the `ask` feature).
#[allow(clippy::too_many_arguments)]
pub fn run_translate<L>(
    mut session:  Session<L>,
    manager:      KBManager,
    formula:      Option<String>,
    show_numbers: bool,
    test:         Option<PathBuf>,
) -> bool 
where 
    L: HasTranslation + TopLayer {
    if let Some(test_file) = test {
        let test_source = Source::Local(vec![test_file]);
        let prover_opts = manager.external_prover.to_prover_opts();
        let tptp = session.translate_test(
            test_source, 
            manager.into(), 
            prover_opts
        );
        match tptp {
            Ok(tptp) => println!("{}", tptp),
            Err(errs) => {
                for e in errs {
                    match e {
                        SdkError::Kb(e) => {
                            session.kb().pretty_print_error(&e, log::Level::Error);
                        },
                        _ => log::error!("{}", e)
                    }
                }
            }
        };
        return true;
    }

    // Stdin auto-detect: `read_stdin` internally checks `is_terminal()`
    // and returns None when stdin is a TTY, so this is safe when run
    // interactively.  Pipes (`cat foo.kif | sumo translate`) are
    // consumed automatically.
    let formula = formula.or_else(read_stdin);

    let opts: TptpOptions = TptpOptions {
        hide_numbers: !show_numbers,
        ..manager.into()
    };

    match formula {
        Some(text) => {
            match session.translate_formula(&text, opts.lang) {
                Ok(s) => {print!("{}", s); true},
                Err(e) => {
                    match e {
                        SdkError::Kb(diag) => session.kb().pretty_print_error(&diag, log::Level::Error),
                        _ => log::error!("{}", e)
                    }
                    false
                }
            }
        }
        None => {
            match session.translate(opts) {
                Ok(s) => {print!("{}", s); true},
                Err(e) => {
                    match e {
                        SdkError::Kb(diag) => session.kb().pretty_print_error(&diag, log::Level::Error),
                        _ => log::error!("{}", e)
                    }
                    false
                }
            }
        }
    }
}
