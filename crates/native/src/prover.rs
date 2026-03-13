/// Helpers for parsing Vampire output
use regex::Regex;

#[derive(Debug)]
pub struct ProofStep {
    pub id: String,
    pub language: String, // e.g., fof, cnf
    pub role: String,     // e.g., axiom, plain, conjecture
    pub formula: String,
    pub inference: Option<String>,
}

#[derive(Debug)]
pub struct VampireOutput {
    pub proof_steps: Vec<ProofStep>,
    pub termination_reason: String,
    pub time_elapsed: String,
}

pub fn parse_vampire_output(input: &str) -> VampireOutput {
    let mut proof_steps = Vec::new();
    let mut termination_reason = String::new();
    let mut time_elapsed = String::new();

    // Regex to capture: language(id, role, formula, [inference/file info])
    // This handles multi-line formulas by using the DOT_ALL flag (?s)
    let fof_re = Regex::new(r"(?s)(fof|cnf|tff|thf)\((f\d+),\s*(\w+),\s*\((.*?)\),\s*(.*?)\)\.").unwrap();

    for line in input.lines() {
        let line = line.trim();

        // 1. Extract Termination Reason
        if line.starts_with("% Termination reason:") {
            termination_reason = line.replace("% Termination reason:", "").trim().to_string();
        }

        // 2. Extract Time Elapsed (using the formal footer)
        if line.starts_with("% Time elapsed:") {
            time_elapsed = line.replace("% Time elapsed:", "").trim().to_string();
        }
    }

    // 3. Extract Proof Steps (between SZS start and end)
    if let Some(start_idx) = input.find("SZS output start") {
        if let Some(end_idx) = input.find("SZS output end") {
            let proof_section = &input[start_idx..end_idx];
            
            for cap in fof_re.captures_iter(proof_section) {
                proof_steps.push(ProofStep {
                    language: cap[1].to_string(),
                    id: cap[2].to_string(),
                    role: cap[3].to_string(),
                    formula: cap[4].trim().replace('\n', " ").to_string(),
                    inference: Some(cap[5].trim().to_string()),
                });
            }
        }
    }

    VampireOutput {
        proof_steps,
        termination_reason,
        time_elapsed,
    }
}