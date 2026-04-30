use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use log;
use inline_colorization::*;
use sumo_kb::ProverStatus;
use sumo_sdk::{AskOp, ProverBackend, SdkError};
use crate::cli::profile::PhaseAggregator;
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

    // Build the phase aggregator first if --profile was passed, so
    // it is installed BEFORE KB load — every emit fires through it
    // from the very first instrumented call.  When --profile is off,
    // no sink is installed and every emit site is a free
    // predicted-None branch.
    let aggregator = if profile { Some(Arc::new(PhaseAggregator::new())) } else { None };
    let sink: Option<sumo_kb::DynSink> = aggregator.clone()
        .map(|a| a as sumo_kb::DynSink);

    let t_kb = Instant::now();
    let mut kb = match open_or_build_kb_profiled(&kb_args, sink) {
        Ok(k)   => k,
        Err(()) => return false,
    };
    let kb_load = t_kb.elapsed();

    // Resolve backend up-front so we can fail fast on either an
    // unknown name or a missing vampire binary BEFORE handing
    // anything to AskOp.
    let prover_backend = match backend.as_str() {
        #[cfg(feature = "integrated-prover")]
        "embedded" => {
            log::info!("ask: using embedded Vampire backend ({:?})", tptp_lang);
            ProverBackend::Embedded
        }
        "subprocess" | "" => ProverBackend::Subprocess,
        other => {
            log::error!("ask: unknown backend '{}' (supported: subprocess, embedded)", other);
            return false;
        }
    };

    // For subprocess: pre-resolve the vampire path so we surface
    // "binary missing" before paying the input-gen cost.
    let resolved_vampire = if matches!(prover_backend, ProverBackend::Subprocess) {
        let candidate = kb_args.vampire.clone().unwrap_or_else(|| PathBuf::from("vampire"));
        match resolve_vampire_path(&candidate) {
            Ok(p)   => Some(p),
            Err(()) => return false,
        }
    } else {
        None
    };

    // Drive AskOp.  It folds the tell-then-ask sequence, vampire
    // path threading, and result assembly into one call.  Tell
    // failures propagate as `SdkError::Kb`; spawn failures as
    // `SdkError::VampireNotFound`; everything else rides out via
    // the typed AskReport.
    let mut op = AskOp::new(&mut kb, &conjecture)
        .session(session.clone())
        .timeout_secs(timeout)
        .backend(prover_backend)
        .lang(tptp_lang)
        .tells(tell.iter().cloned());
    if let Some(p) = resolved_vampire { op = op.vampire_path(p); }
    if let Some(p) = keep             { op = op.tptp_dump(p); }

    let report = match op.run() {
        Ok(r) => r,
        Err(SdkError::Kb(e)) => {
            log::error!("tell error: {}", e);
            return false;
        }
        Err(SdkError::VampireNotFound(msg)) => {
            log::error!("vampire not found: {}", msg);
            return false;
        }
        Err(e) => {
            log::error!("ask: {}", e);
            return false;
        }
    };
    let result = report;

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
        // print_proof takes a `&ProverResult`; AskReport's fields
        // are identical so we synthesise one for the call.  Cheap
        // because the heavy fields (raw_output, proof_kif) are
        // cloned by-Vec / by-String once each.
        let pr = sumo_kb::ProverResult {
            status:     result.status,
            raw_output: result.raw_output.clone(),
            bindings:   result.bindings.clone(),
            proof_kif:  result.proof_kif.clone(),
            proof_tptp: result.proof_tptp.clone(),
            timings:    result.timings.clone(),
        };
        print_proof(&kb, &pr, format);
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

        // Fine-grained per-phase report aggregated from the
        // PhaseStarted / PhaseFinished progress events.
        if let Some(a) = aggregator.as_ref() {
            println!("\n{style_bold}Profile (fine-grained, per phase):{style_reset}");
            println!("{}", a.report());
        }
    }

    matches!(result.status, ProverStatus::Proved)
}

