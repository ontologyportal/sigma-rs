// crates/core/src/prover/subprocess.rs
//
// VampireRunner -- subprocess-based Vampire prover.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use once_cell::sync::Lazy;
use regex::Regex;

use super::super::{ProverMode, ProverOpts, ProverRunner};
use super::super::super::super::result::{
    ProverResult,
    ProverStatus,
    ProverTimings,
    TerminationReason
};

use crate::prover::tptp_proof::{ProofStep, TptpProofProcessor, kif_proof_inputs, proof_steps_to_ir};

// -- Pre-compiled regexes ------------------------------------------------------

/// Vampire names every proof step `f<n>`; used to pull parent references out
/// of an inference tail's final `[...]` bracket.
static RE_PARENT:    Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(f\d+)\b").unwrap());
static RE_FOF:       Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?s)(fof|cnf|tff|thf)\((f\d+),\s*(\w+),\s*\((.*?)\),\s*(.*?)\)\."
).unwrap());
/// Extract our `kb_<sid>` axiom name from Vampire's source annotation.
///
/// With `--output_axiom_names on` Vampire emits an axiom step's tail as
///   `file('<path>', kb_<sid>)`
/// — the second component of `file(..)` carries the axiom's original
/// TPTP name.  Without the flag the tail becomes
///   `file('<path>', unknown)`
/// and this regex simply doesn't match (axiom traceback falls back to
/// the canonical-hash path).
static RE_AXIOM_NAME: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"file\('[^']*',\s*(kb_\d+)\s*\)"
).unwrap());

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
            tptp_dump_path: None
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
        "--mode".into(),            "vampire".into(),
        "--input_syntax".into(),    "tptp".into(),
        "--sine_selection".into(),  "off".into(),
        // Emit proofs in TSTP/TPTP format.  Without this Vampire
        // defaults to `--proof on` which prints steps as
        //     `36373. FORMULA [input(axiom)]`
        // — a human-readable format that `parse_vampire_output`'s
        // `fof(...)` regex can't parse.  Setting `-p tptp` produces
        //     `fof(f36373, axiom, (FORMULA), inference(...,[],[...])).`
        // which our parser *does* understand, and the `--proof`
        // CLI flag's SUO-KIF translation (`proof_kif`) depends on
        // that parse succeeding.  Kept on unconditionally: proof-
        // parsing is cheap and only happens when Vampire actually
        // emitted an "SZS output start" block.
        "-p".into(),                "tptp".into(),
        "-t".into(),                timeout_secs.into(),
        // Preserve our `kb_<sid>` axiom names in the proof
        // transcript's source annotation.  Vampire's default strips
        // them (axiom tails become `file('/dev/stdin', unknown)`);
        // with this option on the tails become
        // `file('/dev/stdin', kb_42)`, letting the proof-display
        // path map each axiom-role step back to its source sid in
        // O(1) via `AxiomSourceIndex::lookup_by_sid` — much cheaper
        // and more robust (survives CNF transforms and alpha-
        // renaming) than the canonical-fingerprint fallback.
        "--output_axiom_names".into(), "on".into(),
    ]
}

impl ProverRunner for VampireRunner {
    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
        // Optionally dump TPTP to a file for inspection.
        if let Some(path) = &self.tptp_dump_path {
            if let Err(e) = write_file(path, tptp) {
                crate::log!(Warn, "sigmakee_rs_core::prover", format!("failed to write TPTP dump to {}: {}", path.display(), e));
            } else {
                crate::log!(Info, "sigmakee_rs_core::prover", format!("wrote TPTP dump: {}", path.display()));
            }
        }

        // Per-call timeout from `opts` takes precedence (the autoscaling
        // loop varies it run-to-run); fall back to the runner's own field
        // when the caller left it at 0.
        let secs    = opts.timeout();
        let timeout = secs.to_string();
        let args    = build_vampire_args(&timeout);

        crate::log!(Debug, "sigmakee_rs_core::prover", format!("vampire: {} {} /dev/stdin", self.vampire_path.display(), args.join(" ")));
        crate::log!(Info, "sigmakee_rs_core::prover", format!("starting vampire prover"));

        let mut child = match Command::new(&self.vampire_path)
            .args(&args)
            .arg("/dev/stdin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c)  => c,
            Err(e) => return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: format!("Failed to spawn vampire: {}", e),
                ..Default::default()
            },
        };

        // Write TPTP to Vampire's stdin then close it so Vampire sees EOF.
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(tptp.as_bytes()) {
                crate::log!(Warn, "sigmakee_rs_core::prover", format!("failed to write to vampire stdin: {}", e));
            }
        }

        let t_prover = Instant::now();
        let output = child.wait_with_output();
        let prover_run = t_prover.elapsed();

        match output {
            Err(e) => ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: format!("Failed to run vampire: {}", e),
                timings:    ProverTimings { prover_run, ..Default::default() },
                ..Default::default()
            },
            Ok(out) => {
                let t_parse = Instant::now();
                let stdout   = String::from_utf8_lossy(&out.stdout).into_owned();
                let stderr   = String::from_utf8_lossy(&out.stderr).into_owned();
                let combined = format!("{}{}", stdout, stderr);

                let status = determine_status(&combined, &opts.mode);
                let has_proof = combined.contains("SZS output start");
                let parsed = if has_proof { parse_vampire_output(&combined) } else { VampireOutput::default() };
                // Distinguish a genuine Theorem from ContradictoryAxioms that
                // Vampire mislabels: some schedules report `SZS status Theorem`
                // even when the refutation never used the negated conjecture
                // (the axioms alone derive ⊥ — SUMO carries known
                // inconsistencies).  Mirror the embedded backend's guard: a
                // proof without a negated-conjecture STEP (checked on the
                // parsed roles, not the raw text — the substring can appear
                // in echoed input or schedule chatter) is an Inconsistent
                // verdict, not a Proved one.
                let status = if matches!(opts.mode, ProverMode::Prove)
                    && matches!(status, ProverStatus::Proved)
                    && has_proof
                    && !parsed.proof_steps.iter().any(|s| s.role == "negated_conjecture")
                {
                    ProverStatus::Inconsistent
                } else {
                    status
                };
                crate::log!(Info, "sigmakee_rs_core::prover", format!("vampire result: {:?}", status_label(&status)));

                // Surface Vampire's `User error: …` (parse/type-check) detail
                // that would otherwise be buried in raw_output behind `Unknown`.
                if matches!(status, ProverStatus::InputError) {
                    let detail = extract_input_error(&combined)
                        .unwrap_or_else(|| combined.trim().to_string());
                    crate::log!(Warn, "sigmakee_rs_core::prover",
                        format!("vampire rejected the input: {}", detail));
                }

                // Only extract bindings when Vampire proved the conjecture via
                // a genuine refutation (SZS Theorem).  ContradictoryAxioms /
                // Unsatisfiable proofs derive contradiction purely from the
                // axioms and carry no negated-conjecture steps, so the
                // TptpProofProcessor cannot find any variable bindings there.
                let bindings = if matches!(opts.mode, ProverMode::Prove)
                    && combined.contains("SZS status Theorem")
                {
                    let mut proc = TptpProofProcessor::new();
                    proc.load_proof(&parsed.proof_steps);
                    proc.extract_answers()
                } else {
                    Vec::new()
                };

                // Preserve the raw SZS proof section verbatim so the
                // `--proof tptp` CLI path can emit Vampire's output
                // without re-parsing.  Empty when no proof was found.
                let proof_tptp = if has_proof {
                    combined
                        .find("SZS output start")
                        .and_then(|s| combined[s..].find('\n').map(|nl| s + nl + 1))
                        .and_then(|body_start| {
                            combined[body_start..]
                                .find("SZS output end")
                                .map(|len| combined[body_start..body_start + len].to_string())
                        })
                        .unwrap_or_default()
                } else {
                    String::new()
                };

                let proof_kif = if has_proof {
                    // `kif_proof_inputs` resolves each step's pre-parsed
                    // `parents` into positional premise indices and carries
                    // the `kb_<sid>` source name through; see
                    // `proof_steps_to_kif` for how the tuples are consumed.
                    let inputs = kif_proof_inputs(&parsed.proof_steps);
                    crate::prover::proof::proof_steps_to_kif(&inputs)
                } else {
                    Vec::new()
                };

                let ir_proof = if has_proof {
                    proof_steps_to_ir(&parsed.proof_steps)
                } else {
                    Vec::new()
                };

                let output_parse = t_parse.elapsed();
                let termination = extract_termination_reason(&combined);
                ProverResult {
                    complete_saturation: None,
                    given_steps: None,
                    phase_profile: Vec::new(),
                contradiction_proofs: Vec::new(),
                    status, raw_output: combined, termination, bindings, proof_kif, ir_proof,
                    proof_tptp,
                    proof_tptp_lang: crate::parse::dialect::TptpLang::default(),
                    timings: ProverTimings { prover_run, output_parse, ..Default::default() },
                }
            }
        }
    }
}

/// `true` if Vampire's output signals that the run terminated because
/// it ran out of time.  Three markers are checked because Vampire 5.x
/// emits them in different combinations depending on which phase the
/// time-out hit (preprocessing vs saturation vs proof-search):
///
///   - `SZS status Timeout` — the canonical SZS line.  Always emitted
///     when Vampire decides the result before the time budget is fully
///     consumed *and* declares a Timeout verdict; commonly missing
///     when the budget runs out mid-saturation.
///   - `% Termination reason: Time limit` — the machine-friendly tail
///     marker; present on every time-limit termination regardless of
///     phase.  This is the most reliable signal.
///   - `% Time limit reached!` — the early-banner line emitted from
///     within the saturation loop when the limit is detected
///     mid-iteration.  Implies the same outcome.
///
/// Matching any of the three avoids the previous failure mode where
/// the absence of `SZS status Timeout` caused the parser to fall
/// through to `Unknown`, which the SDK's test harness then
/// misclassified as a `ProverError`.
fn is_timeout(output: &str) -> bool {
    output.contains("SZS status Timeout")
        || output.contains("Termination reason: Time limit")
        || output.contains("Time limit reached")
}

/// Classify *why* Vampire stopped, for the autoscaling loop.  Parses the
/// `% Termination reason:` tail line (and a couple of phase banners),
/// mapping to a backend-agnostic [`TerminationReason`].
///
/// Wall-clock / resource exhaustion (`Time limit`, `Memory limit`,
/// `Refutation not found, ...`) signals an over-large premise set → narrow.
/// A clean `Saturation` or a `CounterSatisfiable` verdict signals the
/// conjecture isn't entailed by the *selected* axioms → widen.  Returns
/// `None` when no termination marker is present (e.g. a clean proof).
fn extract_termination_reason(output: &str) -> Option<TerminationReason> {
    // A successful refutation isn't a "stopped without a verdict" case.
    if output.contains("SZS status Theorem")
        || output.contains("SZS status Unsatisfiable")
        || output.contains("SZS status ContradictoryAxioms")
        || output.contains("Termination reason: Refutation")
    {
        return None;
    }
    if is_timeout(output) {
        return Some(TerminationReason::TimeLimit);
    }
    if output.contains("Memory limit")
        || output.contains("Refutation not found, incomplete strategy")
        || output.contains("Refutation not found, non-redundant clauses discarded")
    {
        return Some(TerminationReason::ResourceOut);
    }
    if output.contains("Termination reason: Satisfiable")
        || output.contains("Termination reason: Saturation")
        || output.contains("SZS status CounterSatisfiable")
        || output.contains("SZS status Satisfiable")
    {
        return Some(TerminationReason::Saturation);
    }
    if output.contains("SZS status GaveUp")
        || output.contains("Termination reason:")
    {
        return Some(TerminationReason::GaveUp);
    }
    None
}

/// `true` if Vampire's output signals that it could not consume the
/// problem — a parse/syntax error or a type-check failure — as opposed
/// to running and reaching no verdict.
///
/// Vampire emits these as `User error: …` on stderr (e.g. "Failed to
/// create predicate application … is not an instance of sort $real",
/// "Cannot create equality between terms of different types", or a
/// tokeniser `Parse error`/`Syntax error`).  It then exits immediately,
/// so no `SZS status` line is produced and the result would otherwise
/// fall through to `Unknown`.
fn is_input_error(output: &str) -> bool {
    output.contains("User error")
        || output.contains("Parse error")
        || output.contains("Syntax error")
        || output.contains("SZS status SyntaxError")
        || output.contains("SZS status TypeError")
}

/// Pull out the single most informative line of a Vampire input-error
/// report for logging.  Prefers the `User error:` line (and the line
/// after it, which carries the sort/term detail); falls back to the
/// first line mentioning an error.
fn extract_input_error(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.contains("User error")
            || line.contains("Parse error")
            || line.contains("Syntax error")
        {
            // Vampire's type errors span two lines: the `User error:`
            // header and a follow-up describing the offending sort/term.
            let mut msg = line.trim().to_string();
            if let Some(next) = lines.get(i + 1) {
                let next = next.trim();
                if !next.is_empty() && !next.starts_with('%') {
                    msg.push(' ');
                    msg.push_str(next);
                }
            }
            return Some(msg);
        }
    }
    None
}

fn determine_status(output: &str, mode: &ProverMode) -> ProverStatus {
    // An input rejection (parse/type error) is neither a Prove nor a
    // CheckConsistency verdict — the prover never actually ran on the
    // problem.  Check it first, before the mode-specific SZS markers, so
    // it can't be misread as `Unknown` (or, in consistency mode,
    // silently accepted as "consistent").
    if is_input_error(output) {
        return ProverStatus::InputError;
    }
    match mode {
        ProverMode::Prove => {
            if output.contains("SZS status Theorem")
                || output.contains("SZS status Unsatisfiable")
            {
                ProverStatus::Proved
            } else if output.contains("SZS status ContradictoryAxioms") {
                // The axiom set itself is contradictory; the conjecture was never
                // tested.  Report Inconsistent so the caller knows the KB is broken.
                ProverStatus::Inconsistent
            } else if output.contains("SZS status CounterSatisfiable") {
                ProverStatus::Disproved
            } else if is_timeout(output) {
                ProverStatus::Timeout
            } else {
                ProverStatus::Unknown
            }
        }
        ProverMode::CheckConsistency => {
            if output.contains("SZS status Satisfiable")
                || output.contains("SZS status CounterSatisfiable")
            {
                ProverStatus::Consistent
            } else if output.contains("SZS status Unsatisfiable")
                || output.contains("SZS status Theorem")
                || output.contains("SZS status ContradictoryAxioms")
            {
                ProverStatus::Inconsistent
            } else if is_timeout(output) {
                ProverStatus::Timeout
            } else {
                ProverStatus::Unknown
            }
        }
    }
}

fn status_label(s: &ProverStatus) -> &'static str {
    match s {
        ProverStatus::Proved       => "Proved",
        ProverStatus::Disproved    => "Disproved",
        ProverStatus::Consistent   => "Consistent",
        ProverStatus::Inconsistent => "Inconsistent",
        ProverStatus::Timeout      => "Timeout",
        ProverStatus::InputError   => "InputError",
        ProverStatus::Unknown      => "Unknown",
    }
}

fn write_file(path: &Path, content: &str) -> std::io::Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(content.as_bytes())
}

// -- Vampire output parsing ----------------------------------------------------

#[derive(Debug, Default)]
pub(crate) struct VampireOutput {
    pub proof_steps: Vec<ProofStep>,
}

/// Resolve a Vampire inference tail's parent step references.
///
/// Vampire's tail looks like `inference(rule,[flags],[f12,f34])` — the parent
/// clauses are exactly the `f<n>` tokens in the *final* `[...]` bracket, so we
/// scan from the last `[`.  Input/leaf steps (`file('…', kb_42)`) have no such
/// bracket and yield an empty parent list.
fn parse_vampire_parents(inference_raw: &str) -> Vec<String> {
    inference_raw
        .rfind('[')
        .map(|p| &inference_raw[p..])
        .map(|bracket_part| {
            RE_PARENT.captures_iter(bracket_part)
                .map(|c| c[1].to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub(crate) fn parse_vampire_output(input: &str) -> VampireOutput {
    let mut proof_steps = Vec::new();

    if let Some(start_idx) = input.find("SZS output start") {
        if let Some(end_idx) = input.find("SZS output end") {
            let proof_section = &input[start_idx..end_idx];
            for cap in RE_FOF.captures_iter(proof_section) {
                let inference_raw = cap[5].trim().to_string();
                // Extract `kb_<sid>` from a Vampire source annotation
                // like `file('/dev/stdin', kb_42)`.  Only matches when
                // `--output_axiom_names on` is in effect AND the step
                // actually traces back to an input axiom; derived
                // `inference(…)` tails don't match and fall through to
                // `None`, which the consumer handles by falling back
                // to canonical-hash lookup.
                let source_name = RE_AXIOM_NAME
                    .captures(&inference_raw)
                    .map(|c| c[1].to_string());
                proof_steps.push(ProofStep {
                    id:        cap[2].to_string(),
                    role:      cap[3].to_string(),
                    formula:   cap[4].trim().replace('\n', " ").to_string(),
                    parents:   parse_vampire_parents(&inference_raw),
                    source_name,
                });
            }
        }
    }

    VampireOutput { proof_steps }
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
        let mode_idx = args.iter().position(|a| a == "--mode")
            .expect("--mode flag must be present");
        assert_eq!(args[mode_idx + 1], "vampire",
            "must use vampire mode to prevent CASC portfolio strategies \
             from re-applying SInE on our already-filtered input");
        assert!(!args.iter().any(|a| a == "casc"),
            "must not invoke CASC portfolio: {:?}", args);
    }

    #[test]
    fn args_explicitly_disable_sine_selection() {
        // Defensive belt-and-braces: Vampire's default is off, but we
        // spell it out so the intent survives any future default change
        // and is self-documenting in logs.
        let args = build_vampire_args("60");
        let ss_idx = args.iter().position(|a| a == "--sine_selection")
            .expect("--sine_selection flag must be present");
        assert_eq!(args[ss_idx + 1], "off",
            "SInE must be explicitly disabled on Vampire's side; \
             the KB applies its own SInE filter before invoking the prover");
    }

    #[test]
    fn args_include_timeout() {
        let args = build_vampire_args("42");
        let t_idx = args.iter().position(|a| a == "-t")
            .expect("-t flag must be present");
        assert_eq!(args[t_idx + 1], "42");
    }

    #[test]
    fn args_use_tptp_input_syntax() {
        let args = build_vampire_args("60");
        let is_idx = args.iter().position(|a| a == "--input_syntax")
            .expect("--input_syntax flag must be present");
        assert_eq!(args[is_idx + 1], "tptp");
    }

    #[test]
    fn args_preserve_axiom_names() {
        // Without this flag the proof transcript's axiom tails read
        // `file('/dev/stdin', unknown)` and we lose the mapping from
        // proof step back to input sid.  Must stay on.
        let args = build_vampire_args("60");
        let idx = args.iter().position(|a| a == "--output_axiom_names")
            .expect("--output_axiom_names flag must be present");
        assert_eq!(args[idx + 1], "on");
    }
}

// -- Vampire output parsing tests --------------------------------------------

#[cfg(test)]
mod parse_tests {
    use super::*;

    fn parse_block(body: &str) -> Vec<ProofStep> {
        let input = format!(
            "% SZS output start Proof\n{}\n% SZS output end Proof\n",
            body,
        );
        parse_vampire_output(&input).proof_steps
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
    use super::{determine_status, ProverStatus};
    use super::ProverMode;

    #[test]
    fn timeout_via_szs_line() {
        let out = "% SZS status Timeout for stdin\n";
        assert!(matches!(determine_status(out, &ProverMode::Prove),
            ProverStatus::Timeout));
        assert!(matches!(determine_status(out, &ProverMode::CheckConsistency),
            ProverStatus::Timeout));
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
        assert!(matches!(determine_status(out, &ProverMode::Prove),
            ProverStatus::Timeout),
            "Prove mode must classify Termination-reason output as Timeout");
        assert!(matches!(determine_status(out, &ProverMode::CheckConsistency),
            ProverStatus::Timeout),
            "CheckConsistency mode must classify Termination-reason output as Timeout");
    }

    #[test]
    fn timeout_via_time_limit_reached_banner() {
        // Some preprocessing-phase timeouts emit only the banner
        // without the Termination block.
        let out = "% Time limit reached!\n";
        assert!(matches!(determine_status(out, &ProverMode::Prove),
            ProverStatus::Timeout));
        assert!(matches!(determine_status(out, &ProverMode::CheckConsistency),
            ProverStatus::Timeout));
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
        assert!(matches!(determine_status(out, &ProverMode::Prove),
            ProverStatus::Proved));
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
        assert!(matches!(super::determine_status(out, &ProverMode::Prove),
            ProverStatus::InputError));
        assert!(matches!(super::determine_status(out, &ProverMode::CheckConsistency),
            ProverStatus::InputError));
    }

    #[test]
    fn input_error_detected_for_parse_error() {
        let out = "Parse error: unexpected token ')' at line 5\n";
        assert!(matches!(super::determine_status(out, &ProverMode::Prove),
            ProverStatus::InputError));
    }

    #[test]
    fn input_error_extraction_joins_detail_line() {
        let out = "\
User error: Cannot create equality between terms of different types.
X0 is $real (detected at or around line 27602)
";
        let msg = super::extract_input_error(out).expect("should extract a message");
        assert!(msg.contains("Cannot create equality between terms of different types"),
            "extracted message should carry the User error header: {msg}");
        assert!(msg.contains("X0 is $real"),
            "extracted message should also carry the follow-up detail line: {msg}");
    }

    #[test]
    fn input_error_takes_precedence_over_absent_szs_status() {
        // A genuine timeout (no User error) must NOT be misread as
        // InputError — the detector only fires on actual rejection text.
        let out = "% Termination reason: Time limit\n";
        assert!(!super::is_input_error(out));
        assert!(matches!(super::determine_status(out, &ProverMode::Prove),
            ProverStatus::Timeout));
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
        assert!(matches!(determine_status(out, &ProverMode::CheckConsistency),
            ProverStatus::Inconsistent));
    }
}
