// crates/sumo-kb/src/prover/subprocess.rs
//
// VampireRunner -- subprocess-based Vampire prover.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use once_cell::sync::Lazy;
use regex::Regex;

use std::time::Instant;
use super::{Binding, ProverMode, ProverOpts, ProverResult, ProverRunner, ProverStatus, ProverTimings};

// -- Pre-compiled regexes ------------------------------------------------------

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
static RE_UNBOUND:   Lazy<Regex> = Lazy::new(|| Regex::new(r"\bX\d+\b").unwrap());
static RE_NEG_HOLDS: Lazy<Regex> = Lazy::new(|| Regex::new(r"~s__holds\(([^()]+)\)").unwrap());
static RE_POS_HOLDS: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?:^|[^~])s__holds\(([^()]+)\)"
).unwrap());
static RE_VAR:       Lazy<Regex> = Lazy::new(|| Regex::new(r"^X\d+$").unwrap());
static RE_VAR_CAP:   Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(X\d+)\b").unwrap());
static RE_CONST:     Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(s__[A-Za-z0-9_]+)\b").unwrap());

// -- VampireRunner -------------------------------------------------------------

/// Default runner -- spawns Vampire as a subprocess.
pub struct VampireRunner {
    pub vampire_path: PathBuf,
    pub timeout_secs: u32,
    /// If set, write the generated TPTP to this path before running Vampire.
    /// When `None` the TPTP is piped directly to Vampire via stdin with no
    /// intermediate file.
    pub tptp_dump_path: Option<PathBuf>,
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
    fn timeout_secs(&self) -> u32 { self.timeout_secs }

    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
        // Optionally dump TPTP to a file for inspection.
        if let Some(path) = &self.tptp_dump_path {
            if let Err(e) = write_file(path, tptp) {
                crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Warn, target: "sumo_kb::prover", message: format!("failed to write TPTP dump to {}: {}", path.display(), e) });
            } else {
                crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Info, target: "sumo_kb::prover", message: format!("wrote TPTP dump: {}", path.display()) });
            }
        }

        let timeout = self.timeout_secs.to_string();
        let args    = build_vampire_args(&timeout);

        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sumo_kb::prover", message: format!("vampire: {} {} /dev/stdin", self.vampire_path.display(), args.join(" ")) });
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Info, target: "sumo_kb::prover", message: format!("starting vampire prover") });

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
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
                proof_tptp: String::new(),
                timings:    ProverTimings::default(),
            },
        };

        // Write TPTP to Vampire's stdin then close it so Vampire sees EOF.
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(tptp.as_bytes()) {
                crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Warn, target: "sumo_kb::prover", message: format!("failed to write to vampire stdin: {}", e) });
            }
        }

        let t_prover = Instant::now();
        let output = child.wait_with_output();
        let prover_run = t_prover.elapsed();

        match output {
            Err(e) => ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: format!("Failed to run vampire: {}", e),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
                proof_tptp: String::new(),
                timings:    ProverTimings { prover_run, ..Default::default() },
            },
            Ok(out) => {
                let t_parse = Instant::now();
                let stdout   = String::from_utf8_lossy(&out.stdout).into_owned();
                let stderr   = String::from_utf8_lossy(&out.stderr).into_owned();
                let combined = format!("{}{}", stdout, stderr);

                let status = determine_status(&combined, &opts.mode);
                crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Info, target: "sumo_kb::prover", message: format!("vampire result: {:?}", status_label(&status)) });

                // Only extract bindings when Vampire proved the conjecture via
                // a genuine refutation (SZS Theorem).  ContradictoryAxioms /
                // Unsatisfiable proofs derive contradiction purely from the
                // axioms and carry no negated-conjecture steps, so the
                // TptpProofProcessor cannot find any variable bindings there.
                let has_proof = combined.contains("SZS output start");
                let parsed = if has_proof { parse_vampire_output(&combined) } else { VampireOutput::default() };

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
                    // Build id->index map so we can resolve parent references.
                    let id_to_idx: HashMap<&str, usize> = parsed.proof_steps
                        .iter()
                        .enumerate()
                        .map(|(i, s)| (s.id.as_str(), i))
                        .collect();
                    // Fourth tuple element is the axiom's `kb_<sid>`
                    // source name when Vampire's source annotation
                    // preserved it (only populated for axiom-role
                    // steps whose origin traces back to an input
                    // axiom we named via `assemble_tptp`).  See
                    // `proof_steps_to_kif` for how it's consumed.
                    let inputs: Vec<(String, String, Vec<usize>, Option<String>)> =
                        parsed.proof_steps
                        .iter()
                        .map(|s| {
                            let premises = s.inference.as_deref()
                                .and_then(|inf| inf.rfind('[').map(|p| &inf[p..]))
                                .map(|bracket_part| {
                                    RE_PARENT.captures_iter(bracket_part)
                                        .filter_map(|c| id_to_idx.get(c.get(1)?.as_str()).copied())
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            (
                                s.formula.clone(),
                                s.role.clone(),
                                premises,
                                s.source_name.clone(),
                            )
                        })
                        .collect();
                    crate::tptp::kif::proof_steps_to_kif(&inputs)
                } else {
                    Vec::new()
                };

                let output_parse = t_parse.elapsed();
                ProverResult {
                    status, raw_output: combined, bindings, proof_kif, proof_tptp,
                    timings: ProverTimings { prover_run, output_parse, ..Default::default() },
                }
            }
        }
    }
}

fn determine_status(output: &str, mode: &ProverMode) -> ProverStatus {
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
            } else if output.contains("SZS status Timeout") {
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
            } else if output.contains("SZS status Timeout") {
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
        ProverStatus::Unknown      => "Unknown",
    }
}

fn write_file(path: &Path, content: &str) -> std::io::Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(content.as_bytes())
}

// -- Vampire output parsing ----------------------------------------------------

#[derive(Debug)]
pub(crate) struct ProofStep {
    pub id:        String,
    pub role:      String,
    pub formula:   String,
    pub inference: Option<String>,
    /// Original axiom name as preserved by Vampire's
    /// `--output_axiom_names on` flag, e.g. `Some("kb_42")` when this
    /// step descended from an input axiom named `kb_42`.  `None` for
    /// derived steps (CNF transforms, resolution, etc.) and for older
    /// Vampire builds that don't support the flag.
    pub source_name: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct VampireOutput {
    pub proof_steps: Vec<ProofStep>,
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
                    inference: Some(inference_raw),
                    source_name,
                });
            }
        }
    }

    VampireOutput { proof_steps }
}

// -- TptpProofProcessor --------------------------------------------------------

struct GraphNode {
    id:      String,
    formula: String,
    parents: Vec<String>,
}

pub(crate) struct TptpProofProcessor {
    nodes:                  HashMap<String, GraphNode>,
    conjecture_id:          Option<String>,
    negated_conjecture_id:  Option<String>,
}

impl TptpProofProcessor {
    pub(crate) fn new() -> Self {
        Self { nodes: HashMap::new(), conjecture_id: None, negated_conjecture_id: None }
    }

    pub(crate) fn load_proof(&mut self, steps: &[ProofStep]) {
        for step in steps {
            let mut parents = Vec::new();
            if let Some(inf) = &step.inference {
                if let Some(last_bracket) = inf.rfind('[') {
                    for cap in RE_PARENT.captures_iter(&inf[last_bracket..]) {
                        parents.push(cap[1].to_string());
                    }
                }
            }
            if step.role == "conjecture" {
                self.conjecture_id = Some(step.id.clone());
            } else if step.role == "negated_conjecture" {
                self.negated_conjecture_id = Some(step.id.clone());
            }
            self.nodes.insert(step.id.clone(), GraphNode {
                id: step.id.clone(), formula: step.formula.clone(), parents,
            });
        }
    }

    pub(crate) fn extract_answers(&self) -> Vec<Binding> {
        let neg_conj_id = match &self.negated_conjecture_id {
            Some(id) => id,
            None => return Vec::new(),
        };
        let neg_conj_node = match self.nodes.get(neg_conj_id) {
            Some(n) => n,
            None => return Vec::new(),
        };
        let vars = self.extract_variables_ordered(&neg_conj_node.formula);
        if vars.is_empty() { return Vec::new(); }

        if let Some(b) = self.extract_from_answer_literal(&vars) { return b; }
        if let Some(b) = self.extract_from_resolution_unification(neg_conj_id, &vars) { return b; }
        self.extract_from_descendants(neg_conj_id, &vars)
    }

    // -- Strategy 1: answer literal --------------------------------------------

    fn extract_from_answer_literal(&self, vars: &[String]) -> Option<Vec<Binding>> {
        for node in self.nodes.values() {
            if !node.formula.contains("answer(") { continue; }
            if Self::has_unbound_vars(&node.formula) { continue; }
            let args = Self::extract_answer_args(&node.formula)?;
            let bindings: Vec<Binding> = vars.iter().enumerate()
                .filter_map(|(i, var)| {
                    args.get(i).map(|val| Binding {
                        variable: var.replace('X', "?Var"),
                        value:    Self::unmangle_sumo(val),
                    })
                })
                .collect();
            if !bindings.is_empty() { return Some(bindings); }
        }
        None
    }

    fn has_unbound_vars(formula: &str) -> bool {
        RE_UNBOUND.is_match(formula)
    }

    fn extract_answer_args(formula: &str) -> Option<Vec<String>> {
        let start = formula.find("answer(")?;
        let after = &formula[start + "answer(".len()..];
        let mut depth = 1usize;
        let mut end   = 0usize;
        for (i, c) in after.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 { end = i; break; }
                }
                _ => {}
            }
        }
        if depth != 0 { return None; }
        let inner = &after[..end];
        let args = Self::split_top_level(inner)
            .into_iter()
            .map(|a| Self::unwrap_skolem(a.trim()))
            .collect();
        Some(args)
    }

    fn split_top_level(s: &str) -> Vec<String> {
        let mut result  = Vec::new();
        let mut depth   = 0usize;
        let mut current = String::new();
        for c in s.chars() {
            match c {
                '(' => { depth += 1; current.push(c); }
                ')' => { depth -= 1; current.push(c); }
                ',' if depth == 0 => { result.push(current.trim().to_string()); current = String::new(); }
                _ => current.push(c),
            }
        }
        let tail = current.trim().to_string();
        if !tail.is_empty() { result.push(tail); }
        result
    }

    fn unwrap_skolem(s: &str) -> String {
        if (s.starts_with("esk") || s.starts_with("sK")) && s.ends_with(')') {
            if let Some(lp) = s.find('(') {
                return s[lp + 1..s.len() - 1].to_string();
            }
        }
        s.to_string()
    }

    // -- Strategy 2: resolution unification -----------------------------------

    fn extract_from_resolution_unification(
        &self,
        neg_conj_id: &str,
        vars: &[String],
    ) -> Option<Vec<Binding>> {
        let neg_conj_set: HashSet<String> = {
            let mut s = HashSet::new();
            s.insert(neg_conj_id.to_string());
            for n in self.get_all_descendants(neg_conj_id) { s.insert(n.id.clone()); }
            s
        };
        let variadic_ids: HashSet<String> = neg_conj_set.iter()
            .filter(|id| self.nodes.get(*id).map(|n| Self::has_unbound_vars(&n.formula)).unwrap_or(false))
            .cloned()
            .collect();

        for node in self.nodes.values() {
            if Self::has_unbound_vars(&node.formula) { continue; }
            let variadic_parent_id = node.parents.iter().find(|p| variadic_ids.contains(*p));
            let variadic_parent_id = match variadic_parent_id { Some(id) => id, None => continue };
            let variadic_parent = match self.nodes.get(variadic_parent_id) { Some(n) => n, None => continue };

            for resolvent_id in &node.parents {
                if variadic_ids.contains(resolvent_id) { continue; }
                let resolvent = match self.nodes.get(resolvent_id) { Some(n) => n, None => continue };
                if Self::has_unbound_vars(&resolvent.formula) { continue; }

                if let Some(sub) = Self::unify_negative_with_positive(&variadic_parent.formula, &resolvent.formula) {
                    let bindings: Vec<Binding> = vars.iter()
                        .filter_map(|var| sub.get(var).map(|val| Binding {
                            variable: var.replace('X', "?Var"),
                            value:    Self::unmangle_sumo(val),
                        }))
                        .collect();
                    if bindings.len() == vars.len() { return Some(bindings); }
                }
            }
        }
        None
    }

    fn unify_negative_with_positive(
        variadic:  &str,
        resolvent: &str,
    ) -> Option<HashMap<String, String>> {
        let res_cap  = RE_POS_HOLDS.captures(resolvent)?;
        let res_args: Vec<&str> = res_cap[1].split(',').map(str::trim).collect();

        for cap in RE_NEG_HOLDS.captures_iter(variadic) {
            let var_args: Vec<&str> = cap[1].split(',').map(str::trim).collect();
            if var_args.len() != res_args.len() { continue; }
            if var_args[0] != res_args[0] { continue; }

            let mut sub = HashMap::new();
            let mut consistent = true;
            for (va, ra) in var_args.iter().zip(res_args.iter()).skip(1) {
                if RE_VAR.is_match(va) {
                    sub.insert(va.to_string(), ra.to_string());
                } else if va != ra {
                    consistent = false;
                    break;
                }
            }
            if consistent && !sub.is_empty() { return Some(sub); }
        }
        None
    }

    // -- Strategy 3: descendant heuristic -------------------------------------

    fn extract_from_descendants(&self, neg_conj_id: &str, vars: &[String]) -> Vec<Binding> {
        let descendants = self.get_all_descendants(neg_conj_id);
        vars.iter()
            .filter_map(|var| {
                self.find_binding_in_descendants(var, &descendants).map(|val| Binding {
                    variable: var.replace('X', "?Var"),
                    value:    Self::unmangle_sumo(&val),
                })
            })
            .collect()
    }

    // -- Shared helpers --------------------------------------------------------

    fn extract_variables_ordered(&self, formula: &str) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut vars = Vec::new();
        for cap in RE_VAR_CAP.captures_iter(formula) {
            let v = cap[1].to_string();
            if seen.insert(v.clone()) { vars.push(v); }
        }
        vars
    }

    fn get_all_descendants<'a>(&'a self, parent_id: &str) -> Vec<&'a GraphNode> {
        let mut result  = Vec::new();
        let mut stack   = vec![parent_id.to_string()];
        let mut visited = HashSet::new();
        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) { continue; }
            for node in self.nodes.values() {
                if node.parents.contains(&current) {
                    result.push(node);
                    stack.push(node.id.clone());
                }
            }
        }
        result
    }

    fn find_binding_in_descendants(
        &self,
        var: &str,
        descendants: &[&GraphNode],
    ) -> Option<String> {
        for node in descendants {
            if !node.formula.contains(var) {
                for cap in RE_CONST.captures_iter(&node.formula) {
                    let candidate = cap[1].to_string();
                    if !candidate.ends_with("__m") { return Some(candidate); }
                }
            }
        }
        None
    }

    pub(crate) fn unmangle_sumo(term: &str) -> String {
        let mut clean = term.to_string();
        if clean.starts_with("s__") { clean = clean[3..].to_string(); }
        if clean.ends_with("__m")  { clean = clean[..clean.len() - 3].to_string(); }
        clean
    }
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
