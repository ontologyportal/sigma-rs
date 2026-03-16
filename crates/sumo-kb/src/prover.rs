// crates/sumo-kb/src/prover.rs
//
// ProverRunner trait + VampireRunner implementation.
// Gated: #[cfg(feature = "ask")] in lib.rs.
//
// Ported from sumo-native/src/prover.rs and sumo-native/src/ask.rs.

#[cfg(all(feature = "ask", target_arch = "wasm32"))]
compile_error!("sumo-kb: the `ask` feature is not available on wasm32 targets");

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::Regex;

// ── Public API types ──────────────────────────────────────────────────────────

pub trait ProverRunner: Send + Sync {
    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult;
}

pub struct ProverOpts {
    pub timeout_secs: u32,
    pub mode: ProverMode,
}

pub enum ProverMode {
    Prove,
    CheckConsistency,
}

pub struct ProverResult {
    pub status:     ProverStatus,
    pub raw_output: String,
    pub bindings:   Vec<Binding>,
}

pub enum ProverStatus {
    Proved,
    Disproved,
    Consistent,
    Inconsistent,
    Timeout,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub variable: String,
    pub value:    String,
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} = {}", self.variable, self.value)
    }
}

// ── VampireRunner ─────────────────────────────────────────────────────────────

/// Default runner — spawns Vampire as a subprocess.
pub struct VampireRunner {
    pub vampire_path: PathBuf,
    pub timeout_secs: u32,
}

impl ProverRunner for VampireRunner {
    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
        // Write TPTP to a temp file
        let tmp_path = {
            let mut p = std::env::temp_dir();
            p.push(format!("sumo_ask_{}.tptp", std::process::id()));
            p
        };

        if let Err(e) = write_file(&tmp_path, tptp) {
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: format!("Failed to write TPTP tmp file: {}", e),
                bindings:   Vec::new(),
            };
        }

        let timeout = self.timeout_secs.to_string();
        let args    = ["--mode", "casc", "--input_syntax", "tptp", "-t", &timeout];

        log::debug!(target: "sumo_kb::prover",
            "vampire: {} {} {}",
            self.vampire_path.display(), args.join(" "), tmp_path.display());

        let output = Command::new(&self.vampire_path)
            .args(args)
            .arg(&tmp_path)
            .output();

        // Keep the file if SUMO_KEEP_TPTP is set (for debugging).
        if std::env::var("SUMO_KEEP_TPTP").is_err() {
            let _ = fs::remove_file(&tmp_path);
        } else {
            log::info!(target: "sumo_kb::prover", "SUMO_KEEP_TPTP: kept {}", tmp_path.display());
        }

        match output {
            Err(e) => ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: format!("Failed to run vampire: {}", e),
                bindings:   Vec::new(),
            },
            Ok(out) => {
                let stdout   = String::from_utf8_lossy(&out.stdout).into_owned();
                let stderr   = String::from_utf8_lossy(&out.stderr).into_owned();
                let combined = format!("{}{}", stdout, stderr);

                let status = determine_status(&combined, &opts.mode);
                log::info!(target: "sumo_kb::prover", "vampire result: {:?}", status_label(&status));

                let bindings = if matches!(opts.mode, ProverMode::Prove) {
                    let parsed = parse_vampire_output(&combined);
                    let mut proc = TptpProofProcessor::new();
                    proc.load_proof(&parsed.proof_steps);
                    proc.extract_answers()
                } else {
                    Vec::new()
                };

                ProverResult { status, raw_output: combined, bindings }
            }
        }
    }
}

fn determine_status(output: &str, mode: &ProverMode) -> ProverStatus {
    match mode {
        ProverMode::Prove => {
            if output.contains("SZS status Theorem")
                || output.contains("SZS status ContradictoryAxioms")
                || output.contains("SZS status Unsatisfiable")
            {
                ProverStatus::Proved
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

// ── Vampire output parsing ────────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct ProofStep {
    pub id:        String,
    pub role:      String,
    pub formula:   String,
    pub inference: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct VampireOutput {
    pub proof_steps: Vec<ProofStep>,
}

pub(crate) fn parse_vampire_output(input: &str) -> VampireOutput {
    let mut proof_steps = Vec::new();

    let fof_re = Regex::new(
        r"(?s)(fof|cnf|tff|thf)\((f\d+),\s*(\w+),\s*\((.*?)\),\s*(.*?)\)\."
    ).unwrap();

    if let Some(start_idx) = input.find("SZS output start") {
        if let Some(end_idx) = input.find("SZS output end") {
            let proof_section = &input[start_idx..end_idx];
            for cap in fof_re.captures_iter(proof_section) {
                proof_steps.push(ProofStep {
                    id:        cap[2].to_string(),
                    role:      cap[3].to_string(),
                    formula:   cap[4].trim().replace('\n', " ").to_string(),
                    inference: Some(cap[5].trim().to_string()),
                });
            }
        }
    }

    VampireOutput { proof_steps }
}

// ── TptpProofProcessor ────────────────────────────────────────────────────────

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
        let parent_re = Regex::new(r"\b(f\d+)\b").unwrap();
        for step in steps {
            let mut parents = Vec::new();
            if let Some(inf) = &step.inference {
                if let Some(last_bracket) = inf.rfind('[') {
                    for cap in parent_re.captures_iter(&inf[last_bracket..]) {
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

    // ── Strategy 1: answer literal ────────────────────────────────────────────

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
        Regex::new(r"\bX\d+\b").unwrap().is_match(formula)
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

    // ── Strategy 2: resolution unification ───────────────────────────────────

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
        let neg_lit_re = Regex::new(r"~s__holds\(([^()]+)\)").unwrap();
        let pos_lit_re = Regex::new(r"(?:^|[^~])s__holds\(([^()]+)\)").unwrap();
        let var_re     = Regex::new(r"^X\d+$").unwrap();

        let res_cap  = pos_lit_re.captures(resolvent)?;
        let res_args: Vec<&str> = res_cap[1].split(',').map(str::trim).collect();

        for cap in neg_lit_re.captures_iter(variadic) {
            let var_args: Vec<&str> = cap[1].split(',').map(str::trim).collect();
            if var_args.len() != res_args.len() { continue; }
            if var_args[0] != res_args[0] { continue; }

            let mut sub = HashMap::new();
            let mut consistent = true;
            for (va, ra) in var_args.iter().zip(res_args.iter()).skip(1) {
                if var_re.is_match(va) {
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

    // ── Strategy 3: descendant heuristic ─────────────────────────────────────

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

    // ── Shared helpers ────────────────────────────────────────────────────────

    fn extract_variables_ordered(&self, formula: &str) -> Vec<String> {
        let var_re = Regex::new(r"\b(X\d+)\b").unwrap();
        let mut seen = HashSet::new();
        let mut vars = Vec::new();
        for cap in var_re.captures_iter(formula) {
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
        let const_re = Regex::new(r"\b(s__[A-Za-z0-9_]+)\b").unwrap();
        for node in descendants {
            if !node.formula.contains(var) {
                for cap in const_re.captures_iter(&node.formula) {
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
