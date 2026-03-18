use std::path::PathBuf;

use log;
use inline_colorization::*;
use sumo_kb::ProverStatus;
#[cfg(feature = "integrated-prover")]
use sumo_kb::TptpLang;
use crate::cli::util::parse_lang;

use crate::cli::args::KbArgs;
use crate::cli::util::{open_or_build_kb, read_stdin};

pub fn run_ask(
    formula:  Option<String>,
    tell:     Vec<String>,
    timeout:  u32,
    session:  String,
    backend:  String,
    lang:     String,
    kb_args:  KbArgs,
    keep:     bool,
    show_proof: bool,
) -> bool {
    log::debug!(
        "run_ask: formula={:?}, tell={}, timeout={}, session={:?}, backend={:?}, lang={:?}",
        formula.is_some(), tell.len(), timeout, session, backend, lang
    );

    let tptp_lang = parse_lang(&lang);

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

    let result = match backend.as_str() {
        #[cfg(feature = "integrated-prover")]
        "embedded" => {
            if matches!(tptp_lang, TptpLang::Tff) {
                log::error!("ask: TFF is not yet supported with the embedded backend");
                return false;
            }
            log::info!("ask: using embedded Vampire backend");
            kb.ask_embedded(&conjecture, Some(&session), timeout)
        }
        "subprocess" | "" => {
            use sumo_kb::VampireRunner;
            let vampire_path = kb_args.vampire.unwrap_or_else(|| PathBuf::from("vampire"));
            let runner = VampireRunner { vampire_path, timeout_secs: timeout };
            kb.ask(&conjecture, Some(&session), &runner, tptp_lang)
        }
        other => {
            log::error!("ask: unknown backend '{}' (supported: subprocess, embedded)", other);
            return false;
        }
    };

    if !result.bindings.is_empty() {
        for b in &result.bindings {
            println!("  {style_bold}{}{style_reset}", b);
        }
    }

    if show_proof && !result.proof_kif.is_empty() {
        println!("\n{style_bold}Proof (SUO-KIF):{style_reset}");
        for step in &result.proof_kif {
            let premises = if step.premises.is_empty() {
                String::new()
            } else {
                format!(" ← [{}]", step.premises.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", "))
            };
            // Header line: index, rule, premises
            println!("  {:>3}. [{}]{}", step.index + 1, step.rule, premises);
            // Formula pretty-printed with 8-space indent so it reads as body of the step
            println!("        {}", step.formula.pretty_print(2).replace('\n', "\n        "));
        }
    }

    log::debug!(
        "{style_bold}Theorem prover output: {style_reset}{}",
        result.raw_output
    );

    matches!(result.status, ProverStatus::Proved)
}
