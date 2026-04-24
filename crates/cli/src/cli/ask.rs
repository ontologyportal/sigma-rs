use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use log;
use inline_colorization::*;
use sumo_kb::{ProverStatus, Profiler};
use crate::cli::util::parse_lang;

use crate::cli::args::KbArgs;
use crate::cli::proof::print_proof;
use crate::cli::util::{open_or_build_kb_profiled, read_stdin, resolve_vampire_path};

pub fn run_ask(
    formula:  Option<String>,
    tell:     Vec<String>,
    timeout:  u32,
    session:  String,
    backend:  String,
    lang:     String,
    kb_args:  KbArgs,
    keep:     Option<PathBuf>,
    show_proof: Option<String>,
    profile:  bool,
) -> bool {
    log::debug!(
        "run_ask: formula={:?}, tell={}, timeout={}, session={:?}, backend={:?}, lang={:?}",
        formula.is_some(), tell.len(), timeout, session, backend, lang
    );

    let tptp_lang = parse_lang(&lang);

    let conjecture = match formula.or_else(read_stdin) {
        Some(f) => f,
        None => {
            log::error!("ask requires a conjecture formula (supply as argument or via stdin)");
            return false;
        }
    };

    // Build the profiler first if --profile was passed, so it is
    // installed BEFORE KB load — this way the initial `load_kif` /
    // `make_session_axiomatic` phases are also captured.  When
    // `profiling` is off at build time this is still safe: the
    // profiler is zero-sized and every record call is a no-op.
    let profiler = if profile { Some(Arc::new(Profiler::new())) } else { None };

    let t_kb = Instant::now();
    let mut kb = match open_or_build_kb_profiled(&kb_args, profiler.clone()) {
        Ok(k)   => k,
        Err(()) => return false,
    };
    let kb_load = t_kb.elapsed();

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
            log::info!("ask: using embedded Vampire backend ({:?})", tptp_lang);
            kb.ask_embedded(&conjecture, Some(&session), timeout, tptp_lang)
        }
        "subprocess" | "" => {
            use sumo_kb::VampireRunner;
            let candidate = kb_args.vampire.unwrap_or_else(|| PathBuf::from("vampire"));
            // Fail fast when the external binary is missing — otherwise the
            // spawn error is buried inside the ProverResult's raw_output and
            // only visible with `-v`.
            let vampire_path = match resolve_vampire_path(&candidate) {
                Ok(p)   => p,
                Err(()) => return false,
            };
            let runner = VampireRunner { vampire_path, timeout_secs: timeout, tptp_dump_path: keep };
            kb.ask(&conjecture, Some(&session), &runner, tptp_lang)
        }
        other => {
            log::error!("ask: unknown backend '{}' (supported: subprocess, embedded)", other);
            return false;
        }
    };

    // Always surface the verdict to the user, regardless of `-v`.  The
    // exit code also reflects this (see the final `matches!` below),
    // but scripting against stdout is much friendlier with an explicit
    // line — and interactive users shouldn't have to pass `-v` just to
    // see whether their query was proved.
    let (verdict, colour) = match result.status {
        ProverStatus::Proved       => ("Proved",       color_bright_green),
        ProverStatus::Disproved    => ("Disproved",    color_bright_yellow),
        ProverStatus::Consistent   => ("Consistent",   color_bright_green),
        ProverStatus::Inconsistent => ("Inconsistent", color_bright_red),
        ProverStatus::Timeout      => ("Timeout",      color_bright_yellow),
        ProverStatus::Unknown      => ("Unknown",      color_bright_red),
    };
    println!("{style_bold}Result:{style_reset} {colour}{}{color_reset}", verdict);

    if !result.bindings.is_empty() {
        for b in &result.bindings {
            println!("  {style_bold}{}{style_reset}", b);
        }
    }

    if let Some(format) = show_proof.as_deref() {
        print_proof(&kb, &result, format);
    }


    // Promote the raw Vampire transcript to `info` so `-v` (one `v`) is
    // enough to inspect it.  Previously this required `-vv` (debug) and
    // lived next to tens of thousands of unrelated debug lines.
    log::info!(
        "{style_bold}Theorem prover output:{style_reset}\n{}",
        result.raw_output
    );

    if profile {
        // Coarse four-phase summary (the pre-existing `--profile`
        // output — kept for familiarity and because `kb_load` is
        // measured externally, outside the profiler's view).
        let t = &result.timings;
        println!("\n{style_bold}Profile (coarse):{style_reset}");
        println!("  KB load      {:>10.3} ms", kb_load.as_secs_f64() * 1000.0);
        println!("  Input gen    {:>10.3} ms", t.input_gen.as_secs_f64() * 1000.0);
        println!("  Prover run   {:>10.3} ms", t.prover_run.as_secs_f64() * 1000.0);
        println!("  Output parse {:>10.3} ms", t.output_parse.as_secs_f64() * 1000.0);
        let total = kb_load + t.input_gen + t.prover_run + t.output_parse;
        println!("  ─────────────────────────");
        println!("  Total        {:>10.3} ms", total.as_secs_f64() * 1000.0);

        // Fine-grained per-phase report (requires the `profiling`
        // cargo feature to be on at sumo-kb build time; otherwise
        // the report is just a one-line "feature off" placeholder).
        if let Some(p) = profiler.as_ref() {
            println!("\n{style_bold}Profile (fine-grained, from sumo-kb `profiling` feature):{style_reset}");
            println!("{}", p.report());
        }
    }

    matches!(result.status, ProverStatus::Proved)
}

