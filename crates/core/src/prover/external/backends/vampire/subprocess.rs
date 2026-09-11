// crates/core/src/prover/subprocess.rs
//
// VampireRunner -- subprocess-based Vampire prover.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use super::super::super::super::result::{ProverResult, ProverStatus, ProverTimings};
use super::super::{ProverOpts, ProverRunner};

use crate::prover::vampire_proof::result_from_transcript;

// -- VampireRunner -------------------------------------------------------------

/// Default runner -- spawns Vampire as a subprocess.
#[derive(Debug, Clone)]
pub struct VampireRunner {
    pub vampire_path: PathBuf,
    /// If set, write the generated TPTP to this path before running Vampire.
    /// When `None` the TPTP is piped directly to Vampire via stdin with no
    /// intermediate file.
    pub tptp_dump_path: Option<PathBuf>,
}

impl Default for VampireRunner {
    fn default() -> Self {
        Self {
            vampire_path: PathBuf::from("vampire"),
            tptp_dump_path: None,
        }
    }
}

/// Construct the Vampire command-line arguments.
///
/// **SInE handling.**  The KB already performs SInE axiom selection
/// internally before handing TPTP to Vampire (see
/// `KnowledgeBase::ask`).  To prevent Vampire from re-applying SInE on
/// top of our already-filtered input — which would risk over-selection
/// (dropping axioms our external filter deliberately kept) — we
/// explicitly:
///
/// 1. Set `--mode vampire` (single-strategy, no portfolio).  The
///    `casc` portfolio's strategies are encoded as option-strings like
///    `ss=axioms:st=1.5` which `readFromEncodedOptions` applies per
///    strategy, overriding command-line SInE settings.  The only
///    reliable way to disable SInE across the whole run is to avoid
///    the portfolio entirely.
/// 2. Set `--sine_selection off` as a defensive belt-and-braces
///    measure.  Vampire's default for this option is already `off`,
///    but spelling it out makes the intent explicit and survives any
///    future default change.
///
/// If the single-strategy default proof search turns out to be
/// insufficient on hard queries, options are:
/// - Loosen the external SInE tolerance (`SineParams::benevolent(..)`)
///   to feed more axioms into Vampire.
/// - Switch back to `--mode casc` and accept the minor over-selection
///   risk (CASC portfolio strategies may re-filter; non-SInE
///   strategies still receive the full external-SInE set).
fn build_vampire_args(timeout_secs: &str) -> Vec<String> {
    vec![
        "--mode".into(),
        "vampire".into(),
        "--input_syntax".into(),
        "tptp".into(),
        "--sine_selection".into(),
        "off".into(),
        // Emit proofs in TSTP/TPTP format.  Without this Vampire
        // defaults to `--proof on` which prints steps as
        //     `36373. FORMULA [input(axiom)]`
        // — a human-readable format `parse::szs::parse_szs` (which reuses
        // the TPTP grammar) can't parse.  Setting `-p tptp` produces
        //     `fof(f36373, axiom, (FORMULA), inference(...,[],[...])).`
        // which our parser *does* understand, and the `--proof`
        // CLI flag's SUO-KIF translation (`proof_kif`) depends on
        // that parse succeeding.  Kept on unconditionally: proof-
        // parsing is cheap and only happens when Vampire actually
        // emitted an "SZS output start" block.
        "-p".into(),
        "tptp".into(),
        "-t".into(),
        timeout_secs.into(),
        // Preserve our `kb_<sid>` axiom names in the proof
        // transcript's source annotation.  Vampire's default strips
        // them (axiom tails become `file('/dev/stdin', unknown)`);
        // with this option on the tails become
        // `file('/dev/stdin', kb_42)`, letting the proof-display
        // path map each axiom-role step back to its source sid in
        // O(1) via `AxiomSourceIndex::lookup_by_sid` — much cheaper
        // and more robust (survives CNF transforms and alpha-
        // renaming) than the canonical-fingerprint fallback.
        "--output_axiom_names".into(),
        "on".into(),
    ]
}

impl ProverRunner for VampireRunner {
    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
        // Optionally dump TPTP to a file for inspection.
        if let Some(path) = &self.tptp_dump_path {
            if let Err(e) = write_file(path, tptp) {
                crate::log!(
                    Warn,
                    "sigmakee_rs_core::prover",
                    format!("failed to write TPTP dump to {}: {}", path.display(), e)
                );
            } else {
                crate::log!(
                    Info,
                    "sigmakee_rs_core::prover",
                    format!("wrote TPTP dump: {}", path.display())
                );
            }
        }

        // Per-call timeout from `opts` takes precedence (the autoscaling
        // loop varies it run-to-run); fall back to the runner's own field
        // when the caller left it at 0.
        let secs = opts.timeout();
        let timeout = secs.to_string();
        let args = build_vampire_args(&timeout);

        crate::log!(
            Debug,
            "sigmakee_rs_core::prover",
            format!(
                "vampire: {} {} /dev/stdin",
                self.vampire_path.display(),
                args.join(" ")
            )
        );
        crate::log!(
            Info,
            "sigmakee_rs_core::prover",
            "starting vampire prover".to_string()
        );

        let mut child = match Command::new(&self.vampire_path)
            .args(&args)
            .arg("/dev/stdin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                return ProverResult {
                    status: ProverStatus::Unknown,
                    raw_output: format!("Failed to spawn vampire: {}", e),
                    ..Default::default()
                }
            }
        };

        // Write TPTP to Vampire's stdin then close it so Vampire sees EOF.
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(tptp.as_bytes()) {
                crate::log!(
                    Warn,
                    "sigmakee_rs_core::prover",
                    format!("failed to write to vampire stdin: {}", e)
                );
            }
        }

        let t_prover = Instant::now();
        let output = child.wait_with_output();
        let prover_run = t_prover.elapsed();

        match output {
            Err(e) => ProverResult {
                status: ProverStatus::Unknown,
                raw_output: format!("Failed to run vampire: {}", e),
                timings: ProverTimings {
                    prover_run,
                    ..Default::default()
                },
                ..Default::default()
            },
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                result_from_transcript(&stdout, &stderr, opts.mode, prover_run)
            }
        }
    }
}

fn write_file(path: &Path, content: &str) -> std::io::Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(content.as_bytes())
}

// -- Vampire args construction tests -----------------------------------------

#[cfg(test)]
mod args_tests {
    use super::build_vampire_args;

    #[test]
    fn args_use_single_strategy_vampire_mode() {
        // `casc` would pull the CASC portfolio, whose per-strategy
        // encoded options include `ss=axioms` and thus override any
        // command-line `--sine_selection off`.  We therefore use the
        // single-strategy `vampire` mode so SInE is genuinely disabled
        // across the entire run.
        let args = build_vampire_args("60");
        let mode_idx = args
            .iter()
            .position(|a| a == "--mode")
            .expect("--mode flag must be present");
        assert_eq!(
            args[mode_idx + 1],
            "vampire",
            "must use vampire mode to prevent CASC portfolio strategies \
             from re-applying SInE on our already-filtered input"
        );
        assert!(
            !args.iter().any(|a| a == "casc"),
            "must not invoke CASC portfolio: {:?}",
            args
        );
    }

    #[test]
    fn args_explicitly_disable_sine_selection() {
        // Defensive belt-and-braces: Vampire's default is off, but we
        // spell it out so the intent survives any future default change
        // and is self-documenting in logs.
        let args = build_vampire_args("60");
        let ss_idx = args
            .iter()
            .position(|a| a == "--sine_selection")
            .expect("--sine_selection flag must be present");
        assert_eq!(
            args[ss_idx + 1],
            "off",
            "SInE must be explicitly disabled on Vampire's side; \
             the KB applies its own SInE filter before invoking the prover"
        );
    }

    #[test]
    fn args_include_timeout() {
        let args = build_vampire_args("42");
        let t_idx = args
            .iter()
            .position(|a| a == "-t")
            .expect("-t flag must be present");
        assert_eq!(args[t_idx + 1], "42");
    }

    #[test]
    fn args_use_tptp_input_syntax() {
        let args = build_vampire_args("60");
        let is_idx = args
            .iter()
            .position(|a| a == "--input_syntax")
            .expect("--input_syntax flag must be present");
        assert_eq!(args[is_idx + 1], "tptp");
    }

    #[test]
    fn args_preserve_axiom_names() {
        // Without this flag the proof transcript's axiom tails read
        // `file('/dev/stdin', unknown)` and we lose the mapping from
        // proof step back to input sid.  Must stay on.
        let args = build_vampire_args("60");
        let idx = args
            .iter()
            .position(|a| a == "--output_axiom_names")
            .expect("--output_axiom_names flag must be present");
        assert_eq!(args[idx + 1], "on");
    }
}

// -- Vampire output parsing tests --------------------------------------------
//
// Exercises this backend's own `parse::szs::parse_szs` +
// `docitems_to_proof_steps` pipeline end to end (the lower-level TPTP
// annotation grammar itself — `file(...)`/`inference(...)` shapes — has its
// own coverage in `parse::tptp::parser`'s tests; this is regression coverage
// for the adapter this subprocess backend specifically depends on).

#[cfg(test)]
mod parse_tests {
    use crate::prover::tptp_proof::ProofStep;
    use crate::prover::vampire_proof::docitems_to_proof_steps;

    fn parse_block(body: &str) -> Vec<ProofStep> {
        let input = format!(
            "% SZS output start Proof\n{}\n% SZS output end Proof\n",
            body,
        );
        let (doc, _errors) = crate::parse::szs::parse_szs(&input, "test");
        docitems_to_proof_steps(&doc)
    }

    #[test]
    fn source_name_extracted_from_file_annotation() {
        // `--output_axiom_names on` emits the axiom name as the second
        // component of `file(..)`.  We want that captured into
        // `ProofStep.source_name`.
        let body = "\
fof(f1,axiom,(
  s__holds(s__foo__m,s__A)),
  file('/dev/stdin',kb_42)).
fof(f2,axiom,(
  ~s__holds(s__foo__m,s__A)),
  file('/dev/stdin',kb_99)).
fof(f3,plain,(
  $false),
  inference(forward_subsumption_resolution,[],[f1,f2])).
";
        let steps = parse_block(body);
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].source_name.as_deref(), Some("kb_42"));
        assert_eq!(steps[1].source_name.as_deref(), Some("kb_99"));
        // Derived step — no source name.
        assert_eq!(steps[2].source_name, None);
    }

    #[test]
    fn source_name_none_when_vampire_strips_names() {
        // Older Vampire builds, or invocations without
        // `--output_axiom_names`, emit `file('/dev/stdin', unknown)`
        // — must degrade to `None` and let the canonical-hash path
        // take over downstream.
        let body = "\
fof(f1,axiom,(
  s__holds(s__foo__m,s__A)),
  file('/dev/stdin',unknown)).
";
        let steps = parse_block(body);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].source_name, None);
    }
}

// =====================================================================
//  Status-parser tests
// =====================================================================
//
// Locks in the timeout-detection heuristics: Vampire 5.x emits
// different terminator markers depending on which phase the time
// limit hit, and the SDK's test harness misclassifies an `Unknown`
// status with non-empty output as `ProverError`.  Each scenario below
// is a verbatim trailer from a real Vampire run; if any of them
// regresses to `Unknown`, `sumo test` will start surfacing timeouts
// as "prover error" again.
#[cfg(test)]
mod status_tests {
    use crate::prover::vampire_proof::{determine_status, extract_input_error, is_input_error};
    use crate::{ProverMode, ProverStatus};

    #[test]
    fn timeout_via_szs_line() {
        let out = "% SZS status Timeout for stdin\n";
        assert!(matches!(
            determine_status(out, Some("Timeout"), &ProverMode::Prove),
            ProverStatus::Timeout
        ));
        assert!(matches!(
            determine_status(out, Some("Timeout"), &ProverMode::CheckConsistency),
            ProverStatus::Timeout
        ));
    }

    #[test]
    fn timeout_via_termination_reason_tail() {
        // The form the user hit on TQG23: SZS line absent, machine-
        // readable Termination block present at the end.
        let out = "\
% Time limit reached!
% ------------------------------
% Termination reason: Time limit
% Termination phase: Saturation
% Time elapsed: 10.0000 s
% Peak memory usage: 186 MB
% ------------------------------
";
        assert!(
            matches!(
                determine_status(out, None, &ProverMode::Prove),
                ProverStatus::Timeout
            ),
            "Prove mode must classify Termination-reason output as Timeout"
        );
        assert!(
            matches!(
                determine_status(out, None, &ProverMode::CheckConsistency),
                ProverStatus::Timeout
            ),
            "CheckConsistency mode must classify Termination-reason output as Timeout"
        );
    }

    #[test]
    fn timeout_via_time_limit_reached_banner() {
        // Some preprocessing-phase timeouts emit only the banner
        // without the Termination block.
        let out = "% Time limit reached!\n";
        assert!(matches!(
            determine_status(out, None, &ProverMode::Prove),
            ProverStatus::Timeout
        ));
        assert!(matches!(
            determine_status(out, None, &ProverMode::CheckConsistency),
            ProverStatus::Timeout
        ));
    }

    #[test]
    fn timeout_does_not_misclassify_proved_run() {
        // A real "Proved" run should never be mistaken for a timeout
        // even if a Time-limit banner appears earlier in the log
        // (e.g. when Vampire emits both because the proof landed just
        // under the limit).  `SZS status Theorem` wins by precedence.
        let out = "\
% Time limit reached!
% SZS status Theorem for stdin
% ------------------------------
% Termination reason: Time limit
";
        assert!(matches!(
            determine_status(out, Some("Theorem"), &ProverMode::Prove),
            ProverStatus::Proved
        ));
    }

    #[test]
    fn input_error_detected_for_vampire_user_error_type_mismatch() {
        // The exact failure mode from the SP01 regression: Vampire
        // rejects an ill-typed TFF problem with a two-line `User error`.
        let out = "\
User error: Failed to create function application for s__MeasureFn__1ReFn of type ($real * $i) > $i
The sort $int of the intended term argument 2500000 (at index 0) is not an instance of sort $real (detected at or around line 1326)
";
        // Both modes must report InputError, not Unknown — the prover
        // never produced a verdict.
        assert!(matches!(
            determine_status(out, None, &ProverMode::Prove),
            ProverStatus::InputError
        ));
        assert!(matches!(
            determine_status(out, None, &ProverMode::CheckConsistency),
            ProverStatus::InputError
        ));
    }

    #[test]
    fn input_error_detected_for_parse_error() {
        let out = "Parse error: unexpected token ')' at line 5\n";
        assert!(matches!(
            determine_status(out, None, &ProverMode::Prove),
            ProverStatus::InputError
        ));
    }

    #[test]
    fn input_error_extraction_joins_detail_line() {
        let out = "\
User error: Cannot create equality between terms of different types.
X0 is $real (detected at or around line 27602)
";
        let msg = extract_input_error(out).expect("should extract a message");
        assert!(
            msg.contains("Cannot create equality between terms of different types"),
            "extracted message should carry the User error header: {msg}"
        );
        assert!(
            msg.contains("X0 is $real"),
            "extracted message should also carry the follow-up detail line: {msg}"
        );
    }

    #[test]
    fn input_error_takes_precedence_over_absent_szs_status() {
        // A genuine timeout (no User error) must NOT be misread as
        // InputError — the detector only fires on actual rejection text.
        let out = "% Termination reason: Time limit\n";
        assert!(!is_input_error(out));
        assert!(matches!(
            determine_status(out, None, &ProverMode::Prove),
            ProverStatus::Timeout
        ));
    }

    #[test]
    fn contradictory_axioms_still_wins_over_time_limit_in_consistency_mode() {
        // The Pair / ViralPartFn scenario the user has been
        // debugging: Vampire detects the contradiction during
        // preprocessing and may run out of time afterwards.  The
        // contradiction is the more informative verdict.
        let out = "\
% SZS status ContradictoryAxioms for stdin
% Time limit reached!
% Termination reason: Time limit
";
        assert!(matches!(
            determine_status(
                out,
                Some("ContradictoryAxioms"),
                &ProverMode::CheckConsistency
            ),
            ProverStatus::Inconsistent
        ));
    }
}
