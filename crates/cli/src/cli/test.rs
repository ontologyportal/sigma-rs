use std::path::PathBuf;
use std::time::{Duration, Instant};

use sigmakee_rs_sdk::{KnowledgeBase, Parser, ProverStatus, ProvingLayer};
use sigmakee_rs_sdk::manager::{KBManager, ProverOptsFor};
use sigmakee_rs_sdk::{ExpectedOutcome, Session, Source, SzsStatus, TestCaseOutcome, TestOutcome};

use crate::cli::proof::print_proof;
use crate::style::*;

/// Entry point for `sumo test`.
///
/// The base KB is already loaded into `session` by `dispatch`.  Each discovered
/// test file (`.kif.tq` / `.p` / `.tptp`) runs on a fresh [`Session::fork`] of
/// it, so one test's ingested + promoted axioms can never leak into the next.
/// [`Session::test`] does the rest: split the conjecture from the background
/// theory, promote that background, prove, and grade against the expectation.
pub fn run_test<L>(
    session: Session<L>,
    manager: KBManager,
    paths:   Vec<PathBuf>,
    keep:    Option<PathBuf>,
) -> bool
where
    L: ProvingLayer,
    L::Opts: ProverOptsFor,
{
    log::debug!("run_test(paths={:?})", paths);
    let _ = keep;

    let test_sources = match discover_test_sources(&paths) {
        Ok(s) => s,
        Err(()) => return false,
    };
    if test_sources.is_empty() {
        log::error!("no test files found");
        return false;
    }

    let opts = <L::Opts as ProverOptsFor>::from_manager(&manager);

    let total = test_sources.len();
    let mut passed = 0usize;
    let mut informational = 0usize;
    let mut false_verdicts = 0usize;
    let mut all_passed = true;
    let t_all = Instant::now();

    for (label, src) in test_sources {
        println!("Running test: {label}");
        let mut case = match session.fork() {
            Ok(c) => c,
            Err(e) => {
                println!("  {color_bright_red}ERROR{color_reset}  (could not fork session: {e})");
                all_passed = false;
                continue;
            }
        };
        let t_case = Instant::now();
        match case.test(src, Some(opts.clone())) {
            Ok(outcome) => {
                match render_case(&outcome, t_case.elapsed(), &manager, case.kb()) {
                    CaseVerdict::Passed        => passed += 1,
                    CaseVerdict::Informational => informational += 1,
                    CaseVerdict::FalseVerdict  => { false_verdicts += 1; all_passed = false; }
                    CaseVerdict::Failed        => all_passed = false,
                }
            }
            Err(errs) => {
                println!("  {color_bright_red}ERROR{color_reset}");
                for e in errs { log::error!("  {e}"); }
                all_passed = false;
            }
        }
    }

    let graded = total - informational;
    print!("\nTest Summary: {passed} / {graded} passed");
    if informational > 0 {
        print!("  ({informational} informational, not graded)");
    }
    if false_verdicts > 0 {
        print!("  {color_bright_red}{false_verdicts} FALSE VERDICT{}{color_reset}",
            if false_verdicts == 1 { "" } else { "S" });
    }
    println!("  (tests {:.2}s)", t_all.elapsed().as_secs_f64());
    all_passed
}

/// What one rendered case counted as, for the suite-level summary tally.
enum CaseVerdict {
    Passed,
    Failed,
    /// A confident, wrong claim (see [`TestOutcome::FalseVerdict`]) — always
    /// counted as a suite failure, but tallied separately so it stands out
    /// from an ordinary timeout/give-up `Failed`.
    FalseVerdict,
    /// `Open`/`Unknown` header (or no header) — reported, not graded.
    Informational,
}

/// Walk `paths`, collecting one `(label, Source)` per discovered test file
/// (`.kif.tq` / `.p` / `.tptp`).  Linked `.ax` libraries and `include(...)`
/// directives are resolved downstream (by the loaded base KB and
/// [`Source::read`]), not here.
fn discover_test_sources(paths: &[PathBuf]) -> Result<Vec<(String, Source)>, ()> {
    let mut out: Vec<(String, Source)> = Vec::new();
    for path in paths {
        if path.is_dir() {
            let entries = std::fs::read_dir(path).map_err(|e| {
                log::error!("failed to read directory {}: {e}", path.display());
            })?;
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() { push_if_test(p, &mut out); }
            }
        } else if path.is_file() {
            push_if_test(path.clone(), &mut out);
        } else {
            log::error!("path not found: {}", path.display());
            return Err(());
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    Ok(out)
}

fn push_if_test(p: PathBuf, out: &mut Vec<(String, Source)>) {
    let is_test = Parser::from_filename(&p.to_string_lossy())
        .map_or(false, |parser| parser.is_test());
    if is_test {
        out.push((p.display().to_string(), Source::Local(vec![p])));
    }
}

/// Print one case's verdict (+ optional `--proof` / `--prose`) and its `%
/// SZS status` line, returning the suite-tally bucket it counts as.
/// Rendered against the fork's KB, so proof citations resolve to the test's
/// own axioms.
fn render_case<L>(
    oc:      &TestCaseOutcome,
    elapsed: Duration,
    manager: &KBManager,
    kb:      &KnowledgeBase<L>,
) -> CaseVerdict
where
    L: ProvingLayer,
{
    let note = format!("(total {:.2}s)", elapsed.as_secs_f64());
    let verdict = match &oc.outcome {
        TestOutcome::Passed => {
            println!("  {color_bright_green}PASSED{color_reset}  {note}");
            CaseVerdict::Passed
        }
        TestOutcome::Incomplete { inferred, missing } => {
            // The query was proven; only the answer-set enumeration was partial.
            println!("  {color_bright_green}PASSED{color_reset}  {note}");
            println!("    the query was proven but only some answers were inferred");
            println!("    inferred: {}", inferred.join(", "));
            println!("    missing:  {}", missing.join(", "));
            CaseVerdict::Passed
        }
        TestOutcome::Failed { expected, got, status } => {
            println!("  {color_bright_red}FAILED{color_reset}  {note}");
            println!("    expected: {}, got: {} ({})",
                if *expected { "yes" } else { "no" },
                if *got      { "yes" } else { "no" },
                reason_tag(*status));
            CaseVerdict::Failed
        }
        TestOutcome::FalseVerdict { expected, status } => {
            // Distinct from FAILED: the prover didn't run out of budget, it
            // made a CONFIDENT claim that contradicts the file's own `%
            // Status` header — the harness's most serious finding.
            println!("  {color_bright_red}{style_bold}FALSE VERDICT{style_reset}{color_reset}  {note}");
            println!("    expected: {expected:?}, got: {status:?} ({})", reason_tag(*status));
            CaseVerdict::FalseVerdict
        }
        TestOutcome::Informational => {
            println!("  {color_bright_cyan}INFO{color_reset}      {note}");
            println!("    no graded expectation (Open/Unknown status, or none) — reporting only");
            CaseVerdict::Informational
        }
    };
    println!("  % SZS status {} for {}", oc.szs, basename(&oc.name));

    let format = manager.proof.as_str();
    if format != "none" && !oc.result.proof_kif.is_empty() {
        println!("    {style_bold}Proof:{style_reset}");
        print_proof(kb, &oc.result, format);
    }
    if manager.prose && !oc.result.proof_kif.is_empty() {
        let report = kb.render_proof_prose(None, &oc.result.proof_kif, "EnglishLanguage");
        println!("\n    {style_bold}Proof (prose):{style_reset}\n\n{}", report.rendered);
    }
    verdict
}

/// The bare file-stem SZS convention prints (`PUZ001+1`, not the full path
/// or extension).
fn basename(name: &str) -> String {
    std::path::Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| name.to_string())
}

/// Short, lowercase tag describing why the prover landed on its verdict —
/// rendered next to a failed case's `got:` line to distinguish a timeout from a
/// countermodel from contradictory axioms.
fn reason_tag(status: ProverStatus) -> &'static str {
    match status {
        ProverStatus::Proved       => "refutation",
        ProverStatus::Disproved    => "disproved",
        ProverStatus::Consistent   => "countermodel",
        ProverStatus::Inconsistent => "inconsistent",
        ProverStatus::Timeout      => "timeout",
        ProverStatus::InputError   => "input error",
        ProverStatus::Unknown      => "gave up",
    }
}
