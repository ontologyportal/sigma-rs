use log;
use inline_colorization::*;
use sumo_parser_core::KnowledgeBase;
use crate::ask::{ask as native_ask, AskOptions};
use crate::cli::args::KbArgs;
use crate::cli::util::{build_store, maybe_save_cache, read_stdin};

pub fn run_ask(
    formula: Option<String>,
    tell: Vec<String>,
    timeout: u32,
    session: String,
    kb_args: KbArgs,
) -> bool {
    log::trace!(
        "run_ask(
        formula={:?},
        tell={:?},
        vampire={:?},
        timeout={:?},	
        kb_ags={:#?}
    )",
        formula,
        tell,
        kb_args.vampire,
        timeout,
        kb_args
    );
    let conjecture = match formula.or_else(read_stdin) {
        Some(f) => f,
        None => {
            log::error!(
                "error: ask requires a conjecture formula 
                       (supply as argument or via stdin)"
            );
            return false;
        }
    };

    let store = match build_store(&kb_args) {
        Ok(s) => s,
        Err(..) => {
            return false;
        }
    };
    log::info!(
        "Completed parsing knowledge base ({} axioms)",
        store.roots.len()
    );
    maybe_save_cache(&store, kb_args.cache.as_deref());

    let mut kb = KnowledgeBase::new(store);

    // Apply tell statements into the KB under the specified session.
    for kif in &tell {
        log::debug!("Telling KB (session={:?}): {}", session, kif);
        let r = kb.tell(&session, kif);
        if !r.ok {
            for e in &r.errors {
                log::error!("tell error: {}", e);
            }
            return false;
            // else: skip this tell statement but continue
        }
    }
    log::debug!("Completed telling axioms to the KB");

    let result = native_ask(
        &mut kb,
        &conjecture,
        AskOptions {
            vampire_path: kb_args.vampire,
            timeout_secs: Some(timeout),
            keep_tmp_file: false,
            session: Some(session),
            ..AskOptions::default()
        },
    );

    if !result.errors.is_empty() {
        for e in &result.errors {
            log::error!("error: {}", e);
        }
        return false;
    }

    print!(
        "{style_bold}Theorem prover completed successfully: {style_reset}{}",
        result.output
    );
    result.proved
}
