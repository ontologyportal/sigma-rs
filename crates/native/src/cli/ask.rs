use std::path::PathBuf;

use log;
use inline_colorization::*;
use sumo_kb::{VampireRunner, ProverStatus};

use crate::cli::args::KbArgs;
use crate::cli::util::{open_or_build_kb, read_stdin};

pub fn run_ask(
    formula: Option<String>,
    tell:    Vec<String>,
    timeout: u32,
    session: String,
    kb_args: KbArgs,
    keep:    bool,
) -> bool {
    log::debug!(
        "run_ask: formula={:?}, tell={}, timeout={}, session={:?}",
        formula.is_some(), tell.len(), timeout, session
    );

    if keep {
        log::warn!("--keep is no longer supported; the TPTP temp file will be removed automatically");
    }

    let conjecture = match formula.or_else(read_stdin) {
        Some(f) => f,
        None => {
            log::error!("ask requires a conjecture formula (supply as argument or via stdin)");
            return false;
        }
    };

    let mut kb = match open_or_build_kb(&kb_args) {
        Ok(k)   => k,
        Err(()) => return false,
    };

    // Apply --tell assertions into the named session (in-memory only).
    for kif in &tell {
        log::debug!("ask: tell (session={:?}): {}", session, kif);
        let r = kb.tell(&session, kif);
        if !r.ok {
            for e in &r.errors { log::error!("tell error: {}", e); }
            return false;
        }
    }

    let vampire_path = kb_args.vampire.unwrap_or_else(|| PathBuf::from("vampire"));
    let runner = VampireRunner { vampire_path, timeout_secs: timeout };

    let result = kb.ask(&conjecture, Some(&session), &runner);

    if !result.bindings.is_empty() {
        for b in &result.bindings {
            println!("  {style_bold}{}{style_reset}", b);
        }
    }

    print!(
        "{style_bold}Theorem prover completed: {style_reset}{}",
        result.raw_output
    );

    matches!(result.status, ProverStatus::Proved)
}
