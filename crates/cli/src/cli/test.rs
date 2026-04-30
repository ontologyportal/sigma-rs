use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use log;
use inline_colorization::*;
use crate::cli::args::KbArgs;
use crate::cli::util::{open_or_build_kb_profiled, resolve_vampire_path};
use crate::cli::util::parse_lang;
use sumo_kb::parse_test_content;
use sumo_sdk::{ProverBackend, TestOp, TestOutcome};
use crate::cli::profile::PhaseAggregator;

/// Entry point for `sumo test`.
///
/// Walks the supplied paths to discover `.kif.tq` files, builds a
/// base KB, then drives [`sumo_sdk::TestOp`] per test case so the
/// CLI can keep its interleaved "Running test: …" / "PASSED" /
/// "FAILED" / "INCOMPLETE" output between cases — that interleaved
/// presentation isn't expressible through TestOp's progress events
/// alone, so we pre-parse each case and feed it via `add_case`.
pub fn run_test(paths: Vec<PathBuf>, kb_args: KbArgs, keep: Option<PathBuf>, backend: String, lang: String, timeout_override: Option<u32>, profile: bool) -> bool {
    log::trace!("run_test(paths={:?}, kb_args={:#?})", paths, kb_args);
    log::debug!("Test subcommand selected");

    // -- Discover .kif.tq files (kept here for the rich error
    // messages "path not found" and "no .kif.tq files found"; the
    // SDK's add_dir would discover the same set but with terser
    // error reporting).
    let mut test_files = Vec::new();
    for path in &paths {
        if path.is_dir() {
            match std::fs::read_dir(path) {
                Ok(entries) => {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.is_file() && p.to_string_lossy().ends_with(".kif.tq") {
                            log::debug!("Found test file: {}", p.display());
                            test_files.push(p);
                        }
                    }
                }
                Err(e) => {
                    log::error!("failed to read directory {}: {}", path.display(), e);
                    return false;
                }
            }
        } else if path.is_file() {
            log::debug!("Found test file: {}", path.display());
            test_files.push(path.clone());
        } else {
            log::error!("path not found: {}", path.display());
            return false;
        }
    }
    test_files.sort();
    test_files.dedup();

    if test_files.is_empty() {
        log::error!("no .kif.tq files found");
        return false;
    }

    // Resolve the Vampire binary once up-front (subprocess only).
    let prover_backend = match backend.as_str() {
        #[cfg(feature = "integrated-prover")]
        "embedded" => ProverBackend::Embedded,
        "subprocess" | "" => ProverBackend::Subprocess,
        other => {
            log::error!("test: unknown backend '{}' (supported: subprocess, embedded)", other);
            return false;
        }
    };
    let resolved_vampire = if matches!(prover_backend, ProverBackend::Subprocess) {
        let candidate = kb_args.vampire.clone().unwrap_or_else(|| PathBuf::from("vampire"));
        match resolve_vampire_path(&candidate) {
            Ok(p)   => Some(p),
            Err(()) => return false,
        }
    } else {
        None
    };

    // Build the base KB once.  Phase aggregator is installed BEFORE
    // KB load so the initial load/promote phases are captured.
    log::debug!("Building base KB");
    let aggregator = if profile { Some(Arc::new(PhaseAggregator::new())) } else { None };
    let sink: Option<sumo_kb::DynSink> = aggregator.clone()
        .map(|a| a as sumo_kb::DynSink);
    let t_kb = Instant::now();
    let mut kb = match open_or_build_kb_profiled(&kb_args, sink) {
        Ok(k)   => k,
        Err(()) => return false,
    };
    let kb_load = t_kb.elapsed();

    let tptp_lang = parse_lang(&lang);
    let total_tests = test_files.len();
    let mut all_passed = true;
    let mut passed_count = 0;

    // Coarse-summary accumulators kept for `--profile`.
    let mut acc_input_gen    = Duration::ZERO;
    let mut acc_prover_run   = Duration::ZERO;
    let mut acc_output_parse = Duration::ZERO;
    let mut profile_count    = 0usize;

    for test_file in &test_files {
        let content = match std::fs::read_to_string(test_file) {
            Ok(c) => c,
            Err(e) => {
                log::error!("failed to read test file {}: {}", test_file.display(), e);
                all_passed = false;
                continue;
            }
        };

        let mut test_case = match parse_test_content(&content, &test_file.to_string_lossy()) {
            Ok(tc) => tc,
            Err(e) => {
                log::error!("failed to parse test file {}: {}", test_file.display(), e);
                all_passed = false;
                continue;
            }
        };
        log::debug!("Running test from file: {}", test_case.file_name);
        println!("Running test: {} ({})", test_case.note, test_file.display());

        if !test_case.extra_files.is_empty() {
            log::debug!(
                "test {} references extra files (should be in base KB): {}",
                test_case.note,
                test_case.extra_files.join(", ")
            );
        }

        if let Some(t) = timeout_override {
            test_case.timeout = t;
        }

        // Drive TestOp for this single case.  Per-case session
        // creation, axiom load, validation, prover invocation, and
        // post-run flush all happen inside TestOp::run().
        let mut op = TestOp::new(&mut kb)
            .add_case(test_file.display().to_string(), test_case.clone())
            .backend(prover_backend)
            .lang(tptp_lang);
        if let Some(p) = resolved_vampire.clone() { op = op.vampire_path(p); }
        if let Some(p) = keep.clone() { op = op.tptp_dump(p); }

        let suite = match op.run() {
            Ok(s) => s,
            Err(e) => {
                log::error!("test: {}", e);
                all_passed = false;
                continue;
            }
        };

        // The suite has exactly one case (we added one).  Render it.
        let case_report = match suite.cases.into_iter().next() {
            Some(c) => c,
            None => {
                log::error!("test: TestOp returned an empty case list — internal error");
                all_passed = false;
                continue;
            }
        };

        acc_input_gen    += case_report.timings.input_gen;
        acc_prover_run   += case_report.timings.prover_run;
        acc_output_parse += case_report.timings.output_parse;
        profile_count    += 1;

        match case_report.outcome {
            TestOutcome::Passed => {
                println!("  {color_bright_green}PASSED{color_reset}");
                passed_count += 1;
            }
            TestOutcome::Failed { expected, got } => {
                println!("  {color_bright_red}FAILED{color_reset}");
                println!("    expected: {}, got: {}",
                    if expected { "yes" } else { "no" },
                    if got      { "yes" } else { "no" }
                );
                all_passed = false;
            }
            TestOutcome::Incomplete { inferred, missing } => {
                println!("  {color_bright_yellow}INCOMPLETE{color_reset}");
                println!("    the query was proven but only some answers could be inferred");
                println!("    inferred answers: {}", inferred.join(", "));
                println!("    missing answers: {}",  missing.join(", "));
                all_passed = false;
            }
            TestOutcome::ParseError(msg) => {
                log::error!("parse error in test axioms: {}", msg);
                all_passed = false;
            }
            TestOutcome::SemanticError(msg) => {
                log::error!("semantic error in test axioms: {}", msg);
                all_passed = false;
            }
            TestOutcome::ProverError(msg) => {
                log::error!("prover error(s) for test {}:\n  {}", case_report.name, msg);
                all_passed = false;
            }
            TestOutcome::NoQuery => {
                log::error!("no query found in test file");
                all_passed = false;
            }
        }
    }

    println!("\nTest Summary: {} / {} passed", passed_count, total_tests);

    if profile && profile_count > 0 {
        let n = profile_count as f64;
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        println!("\n{style_bold}Profile (coarse, totals / avg over {} test(s)):{style_reset}", profile_count);
        println!("  KB load      {:>10.3} ms  (one-time)", ms(kb_load));
        println!("  Input gen    {:>10.3} ms  ({:.3} ms / test)", ms(acc_input_gen),    ms(acc_input_gen)    / n);
        println!("  Prover run   {:>10.3} ms  ({:.3} ms / test)", ms(acc_prover_run),   ms(acc_prover_run)   / n);
        println!("  Output parse {:>10.3} ms  ({:.3} ms / test)", ms(acc_output_parse), ms(acc_output_parse) / n);
        let total_query = acc_input_gen + acc_prover_run + acc_output_parse;
        println!("  ─────────────────────────────────────────────────");
        println!("  Query total  {:>10.3} ms  ({:.3} ms / test)", ms(total_query), ms(total_query) / n);
        println!("  Grand total  {:>10.3} ms", ms(kb_load + total_query));

        if let Some(a) = aggregator.as_ref() {
            println!("\n{style_bold}Profile (fine-grained, per phase):{style_reset}");
            println!("{}", a.report());
        }
    }

    all_passed
}
