// crates/core/src/prover/eprover/subprocess.rs
//
// EproverRunner -- subprocess-based E (eprover) prover.
//
// Mirrors `vampire::subprocess::VampireRunner`: take a TPTP string, run the
// prover, return a backend-agnostic `ProverResult`.  The differences from the
// Vampire runner are all in E's surface dialect:
//
//   * stdin is fed via the filename `-` (E rejects `/dev/stdin` as "not a
//     regular file", unlike Vampire).
//   * verdict markers are `#`-prefixed and E declares time/memory exhaustion
//     with `# Failure: Resource limit exceeded (…)` rather than an SZS line.
//   * proof steps are named `c_0_N` / `i_0_N`, and the `inference(…)` terms
//     nest and carry a trailing `['proof']` annotation — so parent references
//     are resolved by name-membership over the whole annotation (see
//     `resolve_parents`) rather than Vampire's "last `[...]` bracket" rule.
//
// Proof-graph → bindings / KIF / IR translation is shared with Vampire via
// `crate::prover::tptp_proof`.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use std::collections::HashSet;

use once_cell::sync::Lazy;
use regex::Regex;


use super::super::{ProverMode, ProverOpts, ProverRunner};
use super::super::super::super::{
    result::{
        ProverResult,
        ProverStatus,
        ProverTimings,
        TerminationReason
    },
    tptp_proof::{
        ProofStep,
        TptpProofProcessor,
        kif_proof_inputs,
        proof_steps_to_ir
    }
};

// -- Pre-compiled regexes ------------------------------------------------------

/// Any TPTP identifier token; intersected with the set of known step names to
/// pick parent references out of an `inference(…)` / `file(…)` annotation.
static RE_IDENT: Lazy<Regex> = Lazy::new(|| Regex::new(r"[A-Za-z_][A-Za-z0-9_]*").unwrap());
/// Our `kb_<sid>` axiom name, as preserved in E's leaf source annotation
/// `file('<stdin>', kb_42)`.  Mirrors the Vampire runner's regex; E preserves
/// input formula names automatically (no flag needed).
static RE_AXIOM_NAME: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"file\('[^']*',\s*(kb_\d+)\s*\)"
).unwrap());

// -- EproverRunner -------------------------------------------------------------

/// Spawns E (`eprover`) as a subprocess.
#[derive(Debug, Clone)]
pub struct EproverRunner {
    pub eprover_path: PathBuf,
    /// If set, write the generated TPTP to this path before running E.  When
    /// `None` the TPTP is piped directly to E's stdin with no intermediate file.
    pub tptp_dump_path: Option<PathBuf>,
}

impl Default for EproverRunner {
    fn default() -> Self {
        Self {
            eprover_path:   PathBuf::from("eprover"),
            tptp_dump_path: None,
        }
    }
}

/// Construct E's command-line arguments.
///
/// * `--auto` — single-strategy auto mode (E picks one heuristic by problem
///   class).  The KB performs its own SInE selection before handing TPTP to
///   the prover, so we want E's *single*-strategy mode (not the
///   `--auto-schedule` portfolio) and E's own SInE stays off by default —
///   matching the intent of the Vampire runner's `--mode vampire`.
/// * `--proof-object` — emit the TSTP refutation between
///   `# SZS output start CNFRefutation` / `… end CNFRefutation`, which
///   `parse_eprover_proof` consumes.
/// * `--tstp-format` — force TPTP-3 input *and* output, so the proof object is
///   TSTP (`fof(…, inference(…))`) regardless of input-format auto-detection.
/// * `--cpu-limit=<secs>` — proof-search time budget (omitted when 0).
fn build_eprover_args(timeout_secs: u64) -> Vec<String> {
    let mut args = vec![
        "--auto".to_string(),
        "--proof-object".to_string(),
        "--tstp-format".to_string(),
    ];
    if timeout_secs > 0 {
        args.push(format!("--cpu-limit={timeout_secs}"));
    }
    args
}

impl ProverRunner for EproverRunner {
    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
        if let Some(path) = &self.tptp_dump_path {
            if let Err(e) = write_file(path, tptp) {
                crate::log!(Warn, "sigmakee_rs_core::prover", format!("failed to write TPTP dump to {}: {}", path.display(), e));
            } else {
                crate::log!(Info, "sigmakee_rs_core::prover", format!("wrote TPTP dump: {}", path.display()));
            }
        }

        // Per-call timeout from `opts` takes precedence (the autoscaling loop
        // varies it run-to-run); fall back to the runner's own field at 0.
        let secs = opts.timeout();
        let args = build_eprover_args(secs);

        crate::log!(Debug, "sigmakee_rs_core::prover", format!("eprover: {} {} -", self.eprover_path.display(), args.join(" ")));
        crate::log!(Info, "sigmakee_rs_core::prover", "starting eprover prover".to_string());

        // E reads the problem from stdin when handed the filename `-`.
        let mut child = match Command::new(&self.eprover_path)
            .args(&args)
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c)  => c,
            Err(e) => return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: format!("Failed to spawn eprover: {}", e),
                ..Default::default()
            },
        };

        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(tptp.as_bytes()) {
                crate::log!(Warn, "sigmakee_rs_core::prover", format!("failed to write to eprover stdin: {}", e));
            }
        }

        let t_prover = Instant::now();
        let output = child.wait_with_output();
        let prover_run = t_prover.elapsed();

        match output {
            Err(e) => ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: format!("Failed to run eprover: {}", e),
                timings:    ProverTimings { prover_run, ..Default::default() },
                ..Default::default()
            },
            Ok(out) => {
                let t_parse  = Instant::now();
                let stdout   = String::from_utf8_lossy(&out.stdout).into_owned();
                let stderr   = String::from_utf8_lossy(&out.stderr).into_owned();
                let combined = format!("{}{}", stdout, stderr);

                let status = determine_status(&combined, &opts.mode);
                crate::log!(Info, "sigmakee_rs_core::prover", format!("eprover result: {:?}", status));

                if matches!(status, ProverStatus::InputError) {
                    let detail = extract_input_error(&combined)
                        .unwrap_or_else(|| combined.trim().to_string());
                    crate::log!(Warn, "sigmakee_rs_core::prover",
                        format!("eprover rejected the input: {}", detail));
                }

                let has_proof = combined.contains("SZS output start");
                let proof_steps = if has_proof { parse_eprover_proof(&combined) } else { Vec::new() };

                // Only extract bindings from a genuine refutation of the
                // (negated) conjecture.  ContradictoryAxioms proofs carry no
                // negated-conjecture step, so there is nothing to bind.
                let bindings = if matches!(opts.mode, ProverMode::Prove)
                    && combined.contains("SZS status Theorem")
                {
                    let mut proc = TptpProofProcessor::new();
                    proc.load_proof(&proof_steps);
                    proc.extract_answers()
                } else {
                    Vec::new()
                };

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
                    let inputs = kif_proof_inputs(&proof_steps);
                    crate::prover::proof::proof_steps_to_kif(&inputs)
                } else {
                    Vec::new()
                };

                let ir_proof = if has_proof {
                    proof_steps_to_ir(&proof_steps)
                } else {
                    Vec::new()
                };

                let output_parse = t_parse.elapsed();
                let termination  = extract_termination_reason(&combined);
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

fn write_file(path: &Path, content: &str) -> std::io::Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(content.as_bytes())
}

// -- Status / termination classification --------------------------------------

/// `true` if E signalled that it ran out of its CPU-time budget.  E reports
/// this as `# Failure: Resource limit exceeded (time)` (it does *not* emit an
/// `SZS status Timeout` line in 3.x); the SZS check is a defensive fallback.
fn is_timeout(output: &str) -> bool {
    output.contains("Resource limit exceeded (time)")
        || output.contains("SZS status Timeout")
}

/// `true` if E rejected the input as malformed.  E does not emit an
/// `SZS status SyntaxError` line — a parse error is printed to stderr as
/// `eprover: <stdin>:L:(Column C):(just read '…'): … expected, but … read`.
/// We match `eprover:` lines carrying parse-diagnostic keywords so that
/// genuine fatals of a different kind still fall through to `Unknown`
/// (surfacing their raw output) rather than being mislabelled `InputError`.
fn is_input_error(output: &str) -> bool {
    output.contains("SZS status SyntaxError")
        || output.contains("SZS status TypeError")
        || output.lines().any(|l| {
            let l = l.trim_start();
            l.starts_with("eprover:")
                && (l.contains("expected")
                    || l.contains("Syntax")
                    || l.contains("token")
                    || l.contains("read '"))
        })
}

/// Pull out the most informative line of an E input-error report for logging.
fn extract_input_error(output: &str) -> Option<String> {
    output.lines()
        .map(str::trim)
        .find(|l| l.starts_with("eprover:")
            && (l.contains("expected") || l.contains("Syntax") || l.contains("read '")))
        .map(|l| l.to_string())
}

/// Classify *why* E stopped without a verdict, for the autoscaling loop.
///
/// Wall-clock/resource exhaustion (`Resource limit exceeded`) signals an
/// over-large premise set → narrow.  A `Satisfiable`/`CounterSatisfiable`
/// verdict, or `Out of unprocessed clauses` (saturation), signals the
/// conjecture isn't entailed by the *selected* axioms → widen.  Returns `None`
/// for a clean refutation.
fn extract_termination_reason(output: &str) -> Option<TerminationReason> {
    if output.contains("SZS status Theorem")
        || output.contains("SZS status Unsatisfiable")
        || output.contains("SZS status ContradictoryAxioms")
    {
        return None;
    }
    if is_timeout(output) {
        return Some(TerminationReason::TimeLimit);
    }
    if output.contains("Resource limit exceeded (memory)")
        || output.contains("User resource limit exceeded")
    {
        return Some(TerminationReason::ResourceOut);
    }
    if output.contains("SZS status CounterSatisfiable")
        || output.contains("SZS status Satisfiable")
        || output.contains("Out of unprocessed clauses")
    {
        return Some(TerminationReason::Saturation);
    }
    if output.contains("SZS status GaveUp") {
        return Some(TerminationReason::GaveUp);
    }
    None
}

fn determine_status(output: &str, mode: &ProverMode) -> ProverStatus {
    // Input rejection first: the prover never produced a verdict on the
    // problem, so it must not be read as Unknown (or silently "consistent").
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

// -- E proof-object parsing ----------------------------------------------------

/// Parse E's `# SZS output start CNFRefutation … # SZS output end` block into
/// normalised [`ProofStep`]s.  Two passes: collect every statement's name,
/// then resolve each step's parents by intersecting the identifier tokens in
/// its source annotation with the set of known step names.
fn parse_eprover_proof(input: &str) -> Vec<ProofStep> {
    let section = match (input.find("SZS output start"), input.find("SZS output end")) {
        (Some(s), Some(e)) if e > s => &input[s..e],
        _ => return Vec::new(),
    };

    // First pass: split into TPTP statements and parse the flat fields.
    let raws: Vec<RawStep> = split_statements(section)
        .iter()
        .filter_map(|stmt| parse_statement(stmt))
        .collect();

    let names: HashSet<&str> = raws.iter().map(|r| r.id.as_str()).collect();

    // Second pass: resolve parents + source name now that all names are known.
    raws.iter()
        .map(|r| ProofStep {
            id:          r.id.clone(),
            role:        r.role.clone(),
            formula:     r.formula.clone(),
            parents:     resolve_parents(&r.source, &r.id, &names),
            source_name: RE_AXIOM_NAME.captures(&r.source).map(|c| c[1].to_string()),
        })
        .collect()
}

struct RawStep {
    id:      String,
    role:    String,
    formula: String,
    source:  String,
}

/// Split a proof section into individual `cnf(…).` / `fof(…).` statements.
///
/// E emits one annotated formula per line, but we accumulate across lines until
/// the buffer closes with `).` so a hypothetically wrapped statement is still
/// captured in one piece.  Comment (`#…`) and blank lines are skipped.
fn split_statements(section: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for line in section.lines() {
        let trimmed = line.trim();
        if buf.is_empty() {
            if !(trimmed.starts_with("cnf(") || trimmed.starts_with("fof(")) {
                continue;
            }
        }
        if !buf.is_empty() { buf.push(' '); }
        buf.push_str(trimmed);
        if buf.trim_end().ends_with(").") {
            out.push(std::mem::take(&mut buf));
        }
    }
    out
}

/// Parse one `cnf(name, role, formula, source).` statement into its flat fields.
fn parse_statement(stmt: &str) -> Option<RawStep> {
    let stmt = stmt.trim().strip_suffix('.')?.trim();
    // Strip the `cnf` / `fof` keyword and the outermost parentheses.
    let inner = stmt
        .strip_prefix("cnf")
        .or_else(|| stmt.strip_prefix("fof"))?
        .trim()
        .strip_prefix('(')?
        .strip_suffix(')')?;

    let fields = split_top_level_commas(inner);
    if fields.len() < 3 { return None; }

    let id      = fields[0].trim().to_string();
    let role    = fields[1].trim().to_string();
    let formula = strip_outer_parens(fields[2].trim()).to_string();
    // Everything after the formula is the source annotation (inference/file
    // term, plus any trailing `['proof']`).  Re-join with commas.
    let source  = fields[3..].join(",").trim().to_string();

    Some(RawStep { id, role, formula, source })
}

/// Split on top-level commas, ignoring commas nested inside `()`/`[]` or single
/// quotes.
fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut out     = Vec::new();
    let mut depth   = 0i32;
    let mut quoted  = false;
    let mut current = String::new();
    for c in s.chars() {
        match c {
            '\'' => { quoted = !quoted; current.push(c); }
            '(' | '[' if !quoted => { depth += 1; current.push(c); }
            ')' | ']' if !quoted => { depth -= 1; current.push(c); }
            ',' if depth == 0 && !quoted => { out.push(current.trim().to_string()); current.clear(); }
            _ => current.push(c),
        }
    }
    let tail = current.trim().to_string();
    if !tail.is_empty() { out.push(tail); }
    out
}

/// Strip one balanced outer `(…)` pair if it wraps the entire string.
fn strip_outer_parens(s: &str) -> &str {
    let s = s.trim();
    if !(s.starts_with('(') && s.ends_with(')')) { return s; }
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                // If the opening paren closes before the final char, the outer
                // pair does not wrap the whole expression (e.g. `(a)|(b)`).
                if depth == 0 { return if i == s.len() - 1 { s[1..i].trim() } else { s }; }
            }
            _ => {}
        }
    }
    s
}

/// Resolve parent references: identifier tokens in `source` that name another
/// step.  Excludes the step's own name (E's leaf annotation
/// `file('<stdin>', kb_42)` repeats the formula's own name) and de-duplicates
/// while preserving first-seen order.
fn resolve_parents(source: &str, self_id: &str, names: &HashSet<&str>) -> Vec<String> {
    let mut parents = Vec::new();
    let mut seen = HashSet::new();
    for m in RE_IDENT.find_iter(source) {
        let tok = m.as_str();
        if tok == self_id { continue; }
        if names.contains(tok) && seen.insert(tok.to_string()) {
            parents.push(tok.to_string());
        }
    }
    parents
}

// =============================================================================
//  Tests — fixtures are verbatim E 3.2.5 output captured during calibration.
// =============================================================================

#[cfg(test)]
mod args_tests {
    use super::build_eprover_args;

    #[test]
    fn args_use_single_strategy_auto() {
        let args = build_eprover_args(60);
        assert!(args.iter().any(|a| a == "--auto"),
            "must use single-strategy --auto (not the --auto-schedule portfolio): {:?}", args);
        assert!(!args.iter().any(|a| a == "--auto-schedule"),
            "must not invoke the portfolio schedule: {:?}", args);
    }

    #[test]
    fn args_request_tstp_proof_object() {
        let args = build_eprover_args(60);
        assert!(args.iter().any(|a| a == "--proof-object"),
            "proof object must be requested so a refutation transcript is emitted: {:?}", args);
        assert!(args.iter().any(|a| a == "--tstp-format"),
            "TPTP-3/TSTP I/O must be forced so the proof object is parseable: {:?}", args);
    }

    #[test]
    fn args_include_cpu_limit_when_set() {
        let args = build_eprover_args(42);
        assert!(args.iter().any(|a| a == "--cpu-limit=42"),
            "timeout must map to --cpu-limit=<secs>: {:?}", args);
    }

    #[test]
    fn args_omit_cpu_limit_when_zero() {
        let args = build_eprover_args(0);
        assert!(!args.iter().any(|a| a.starts_with("--cpu-limit")),
            "no time budget means no --cpu-limit flag: {:?}", args);
    }
}

#[cfg(test)]
mod status_tests {
    use super::{determine_status, is_input_error, ProverStatus};
    use super::ProverMode;

    #[test]
    fn theorem_is_proved() {
        let out = "# Proof found!\n# SZS status Theorem\n";
        assert!(matches!(determine_status(out, &ProverMode::Prove), ProverStatus::Proved));
        // The same run, asked for consistency, means the axioms+~conj are
        // unsatisfiable → the set is Inconsistent.
        assert!(matches!(determine_status(out, &ProverMode::CheckConsistency), ProverStatus::Inconsistent));
    }

    #[test]
    fn countersatisfiable_is_disproved_or_consistent() {
        let out = "# No proof found!\n# SZS status CounterSatisfiable\n";
        assert!(matches!(determine_status(out, &ProverMode::Prove), ProverStatus::Disproved));
        assert!(matches!(determine_status(out, &ProverMode::CheckConsistency), ProverStatus::Consistent));
    }

    #[test]
    fn satisfiable_is_consistent() {
        let out = "# SZS status Satisfiable\n";
        assert!(matches!(determine_status(out, &ProverMode::CheckConsistency), ProverStatus::Consistent));
        // No conjecture to prove → not Proved/Disproved in Prove mode.
        assert!(matches!(determine_status(out, &ProverMode::Prove), ProverStatus::Unknown));
    }

    #[test]
    fn resource_limit_time_is_timeout() {
        // E's actual time-budget marker — note: no `SZS status Timeout` line.
        let out = "# Failure: Resource limit exceeded (time)\n# SZS status GaveUp\n";
        assert!(matches!(determine_status(out, &ProverMode::Prove), ProverStatus::Timeout));
        assert!(matches!(determine_status(out, &ProverMode::CheckConsistency), ProverStatus::Timeout));
    }

    #[test]
    fn contradictory_axioms_is_inconsistent() {
        let out = "# SZS status ContradictoryAxioms\n";
        assert!(matches!(determine_status(out, &ProverMode::Prove), ProverStatus::Inconsistent));
        assert!(matches!(determine_status(out, &ProverMode::CheckConsistency), ProverStatus::Inconsistent));
    }

    #[test]
    fn parse_error_on_stderr_is_input_error() {
        // Verbatim E 3.2.5 stderr for a malformed `fof(a,axiom, p .`.
        let out = "eprover: <stdin>:1:(Column 16):(just read '.'): Closing bracket (')') expected, but Fullstop ('.') read \n";
        assert!(is_input_error(out));
        assert!(matches!(determine_status(out, &ProverMode::Prove), ProverStatus::InputError));
        assert!(matches!(determine_status(out, &ProverMode::CheckConsistency), ProverStatus::InputError));
    }

    #[test]
    fn timeout_does_not_misclassify_proved_run() {
        let out = "# Failure: Resource limit exceeded (time)\n# SZS status Theorem\n";
        assert!(matches!(determine_status(out, &ProverMode::Prove), ProverStatus::Proved));
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    /// Verbatim proof object from `printf 'fof(kb_42, axiom, ![X]:
    /// (s__holds(s__foo,X) => s__bar(X))). fof(kb_43, axiom,
    /// s__holds(s__foo, s__a)). fof(g, conjecture, s__bar(s__a)).' | eprover
    /// --auto --proof-object --tstp-format -`.
    const REFUTATION: &str = "\
# SZS status Theorem
# SZS output start CNFRefutation
fof(g, conjecture, s__bar(s__a), file('<stdin>', g)).
fof(kb_42, axiom, ![X1]:((s__holds(s__foo,X1)=>s__bar(X1))), file('<stdin>', kb_42)).
fof(kb_43, axiom, s__holds(s__foo,s__a), file('<stdin>', kb_43)).
fof(c_0_3, negated_conjecture, ~s__bar(s__a), inference(fof_simplification,[status(thm)],[inference(assume_negation,[status(cth)],[g])])).
fof(c_0_4, plain, ![X2]:((~s__holds(s__foo,X2)|s__bar(X2))), inference(fof_nnf,[status(thm)],[inference(variable_rename,[status(thm)],[inference(fof_nnf,[status(thm)],[kb_42])])])).
fof(c_0_5, negated_conjecture, ~s__bar(s__a), inference(fof_nnf,[status(thm)],[c_0_3])).
cnf(c_0_6, plain, (s__bar(X1)|~s__holds(s__foo,X1)), inference(split_conjunct,[status(thm)],[c_0_4])).
cnf(c_0_7, plain, (s__holds(s__foo,s__a)), inference(split_conjunct,[status(thm)],[kb_43])).
cnf(c_0_8, negated_conjecture, (~s__bar(s__a)), inference(split_conjunct,[status(thm)],[c_0_5])).
cnf(c_0_9, plain, ($false), inference(sr,[status(thm)],[inference(spm,[status(thm)],[c_0_6, c_0_7]), c_0_8]), ['proof']).
# SZS output end CNFRefutation
";

    fn step<'a>(steps: &'a [ProofStep], id: &str) -> &'a ProofStep {
        steps.iter().find(|s| s.id == id).unwrap_or_else(|| panic!("no step {id}"))
    }

    #[test]
    fn parses_all_steps() {
        let steps = parse_eprover_proof(REFUTATION);
        assert_eq!(steps.len(), 10, "all 10 cnf/fof lines should parse");
    }

    #[test]
    fn leaf_source_names_preserved() {
        let steps = parse_eprover_proof(REFUTATION);
        // E echoes the input formula name in `file('<stdin>', NAME)`.
        assert_eq!(step(&steps, "kb_42").source_name.as_deref(), Some("kb_42"));
        assert_eq!(step(&steps, "kb_43").source_name.as_deref(), Some("kb_43"));
        // Non-kb leaf (`g`) and derived steps carry no kb_<sid> source name.
        assert_eq!(step(&steps, "g").source_name, None);
        assert_eq!(step(&steps, "c_0_9").source_name, None);
    }

    #[test]
    fn leaf_steps_have_no_parents() {
        let steps = parse_eprover_proof(REFUTATION);
        // `file('<stdin>', g)` repeats the step's own name — must not become a
        // self-parent.
        assert!(step(&steps, "g").parents.is_empty());
        assert!(step(&steps, "kb_42").parents.is_empty());
    }

    #[test]
    fn parents_resolved_by_name_membership() {
        let steps = parse_eprover_proof(REFUTATION);
        // Original-name parent inside nested inference.
        assert_eq!(step(&steps, "c_0_4").parents, vec!["kb_42"]);
        assert_eq!(step(&steps, "c_0_7").parents, vec!["kb_43"]);
        // The final `$false` step: real clause parents are the three clauses,
        // NOT the trailing `['proof']` annotation that a naive "last bracket"
        // rule would grab.
        assert_eq!(step(&steps, "c_0_9").parents, vec!["c_0_6", "c_0_7", "c_0_8"]);
    }

    #[test]
    fn outer_parens_stripped_from_formula() {
        let steps = parse_eprover_proof(REFUTATION);
        assert_eq!(step(&steps, "c_0_9").formula, "$false");
        assert_eq!(step(&steps, "c_0_8").formula, "~s__bar(s__a)");
        // Quantified formula without a wrapping outer pair is left intact.
        assert!(step(&steps, "c_0_4").formula.starts_with("![X2]:"));
    }

    #[test]
    fn roles_drive_conjecture_detection() {
        let steps = parse_eprover_proof(REFUTATION);
        assert_eq!(step(&steps, "g").role, "conjecture");
        assert_eq!(step(&steps, "c_0_3").role, "negated_conjecture");
    }
}
