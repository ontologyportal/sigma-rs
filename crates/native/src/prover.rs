/// Helpers for parsing Vampire output
use regex::Regex;
use core::fmt;
use std::collections::{HashMap, HashSet};
use log;

#[derive(Debug)]
pub struct ProofStep {
    pub id: String,
    pub language: String, // e.g., fof, cnf
    pub role: String,     // e.g., axiom, plain, conjecture
    pub formula: String,
    pub inference: Option<String>,
}

#[derive(Debug, Default)]
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


#[derive(Debug, Clone)]
pub struct Binding {
    pub variable: String,
    pub value: String,
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} = {}", self.variable, self.value)
    }   
}


#[derive(Debug, Clone)]
struct GraphNode {
    pub id: String,
    pub formula: String,
    pub parents: Vec<String>,
}

// --- (ProofStep, Binding, GraphNode structs unchanged) ---

pub struct TptpProofProcessor {
    nodes: HashMap<String, GraphNode>,
    conjecture_id: Option<String>,
    negated_conjecture_id: Option<String>,
}

impl TptpProofProcessor {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            conjecture_id: None,
            negated_conjecture_id: None,
        }
    }

    pub fn load_proof(&mut self, steps: &[ProofStep]) {
        let parent_re = Regex::new(r"\b(f\d+)\b").unwrap();
        log::debug!("Loading proof into processor");

        for step in steps {
            log::debug!("Processing proof step: {}", step.formula);
            let mut parents = Vec::new();

            if let Some(inf) = &step.inference {
                if let Some(last_bracket) = inf.rfind('[') {
                    for cap in parent_re.captures_iter(&inf[last_bracket..]) {
                        parents.push(cap[1].to_string());
                    }
                }
            }
            log::debug!("Found {} parent steps", parents.len());

            if step.role == "conjecture" {
                self.conjecture_id = Some(step.id.clone());
            } else if step.role == "negated_conjecture" {
                self.negated_conjecture_id = Some(step.id.clone());
            }

            self.nodes.insert(step.id.clone(), GraphNode {
                id: step.id.clone(),
                formula: step.formula.clone(),
                parents,
            });
        }
    }

    /// Primary extraction strategy (mirrors Java's processAnswersFromProof):
    /// Scan all proof steps for an answer literal with fully-ground arguments.
    ///
    /// In TPTP proofs the prover emits a step like:
    ///   fof(c_0_42, plain, answer(esk1_1(s__JohnDoe)), inference(...))
    /// once all variables in the original query have been resolved to constants.
    /// We find that step, strip any surrounding skolem wrapper, and return bindings
    /// positionally matched to the variables in the negated conjecture.
    pub fn extract_answers(&self) -> Vec<Binding> {
        log::debug!("Extract answers from proof");

        let neg_conj_id = match &self.negated_conjecture_id {
            Some(id) => id,
            None => return Vec::new(),
        };
        let neg_conj_node = match self.nodes.get(neg_conj_id) {
            Some(n) => n,
            None => return Vec::new(),
        };

        log::debug!("Located negated conjecture: {}", neg_conj_node.formula);

        let vars = self.extract_variables_ordered(&neg_conj_node.formula);
        log::debug!("Variables to bind (in order): {}", vars.join(", "));

        if vars.is_empty() {
            return Vec::new();
        }

        // Strategy 1: grounded answer(...) literal in the proof
        if let Some(b) = self.extract_from_answer_literal(&vars) {
            return b;
        }

        // Strategy 2: resolution unification against an external ground fact
        if let Some(b) = self.extract_from_resolution_unification(neg_conj_id, &vars) {
            return b;
        }

        // Strategy 3: na̎ive descendant heuristic (last resort)
        log::debug!("Falling back to descendant heuristic");
        self.extract_from_descendants(neg_conj_id, &vars)
    }

    // -----------------------------------------------------------------------
    // Strategy 1: answer literal extraction
    // -----------------------------------------------------------------------

    fn extract_from_answer_literal(&self, vars: &[String]) -> Option<Vec<Binding>> {
        // Scan every proof node for a grounded answer literal
        for node in self.nodes.values() {
            if !node.formula.contains("answer(") {
                continue;
            }
            // Skip if the formula still has unbound variables (X0, X1 …)
            if Self::has_unbound_vars(&node.formula) {
                continue;
            }

            log::debug!("Found grounded answer literal in node {}: {}", node.id, node.formula);

            let args = Self::extract_answer_args(&node.formula)?;
            log::debug!("Answer args: {:?}", args);

            let bindings = vars
                .iter()
                .enumerate()
                .filter_map(|(i, var)| {
                    args.get(i).map(|val| Binding {
                        variable: var.replace('X', "?Var"),
                        value: Self::unmangle_sumo(val),
                    })
                })
                .collect::<Vec<_>>();

            if !bindings.is_empty() {
                return Some(bindings);
            }
        }
        None
    }

    /// True if the formula still contains an unbound TPTP variable (X followed by digits).
    fn has_unbound_vars(formula: &str) -> bool {
        let re = Regex::new(r"\bX\d+\b").unwrap();
        re.is_match(formula)
    }

    /// Extract the argument list from the innermost `answer(...)` call.
    ///
    /// Examples:
    ///   "answer(esk1_1(s__JohnDoe))"       → ["s__JohnDoe"]
    ///   "answer(s__JohnDoe, s__Monday)"     → ["s__JohnDoe", "s__Monday"]
    ///   "(~answer(esk2_0(s__Foo)) | $false)"→ ["s__Foo"]
    fn extract_answer_args(formula: &str) -> Option<Vec<String>> {
        let start = formula.find("answer(")?;
        let after = &formula[start + "answer(".len()..];

        // Walk to find the matching ')'
        let mut depth = 1usize;
        let mut end = 0usize;
        for (i, c) in after.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        if depth != 0 {
            return None; // Malformed
        }

        let inner = &after[..end];

        // Split by top-level commas, then strip any surrounding skolem wrapper
        // e.g. esk2_1(s__JohnDoe) → s__JohnDoe
        let args = Self::split_top_level(inner)
            .into_iter()
            .map(|a| Self::unwrap_skolem(a.trim()))
            .collect();

        Some(args)
    }

    /// Split `s` by commas that are at parenthesis depth 0.
    fn split_top_level(s: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut depth = 0usize;
        let mut current = String::new();

        for c in s.chars() {
            match c {
                '(' => {
                    depth += 1;
                    current.push(c);
                }
                ')' => {
                    depth -= 1;
                    current.push(c);
                }
                ',' if depth == 0 => {
                    result.push(current.trim().to_string());
                    current = String::new();
                }
                _ => current.push(c),
            }
        }
        let tail = current.trim().to_string();
        if !tail.is_empty() {
            result.push(tail);
        }
        result
    }

    /// Strip a surrounding skolem function applied to a single argument.
    ///
    /// `esk2_1(s__JohnDoe)` → `s__JohnDoe`  
    /// `sK3(s__Foo)`        → `s__Foo`  
    /// `s__JohnDoe`         → `s__JohnDoe`  (unchanged)
    fn unwrap_skolem(s: &str) -> String {
        let is_skolem = s.starts_with("esk") || s.starts_with("sK");
        if is_skolem {
            if let Some(lp) = s.find('(') {
                if s.ends_with(')') {
                    return s[lp + 1..s.len() - 1].to_string();
                }
            }
        }
        s.to_string()
    }

    // -----------------------------------------------------------------------
    // Strategy 2: descendant-tracing fallback (original approach, improved)
    // -----------------------------------------------------------------------

    fn extract_from_descendants(&self, neg_conj_id: &str, vars: &[String]) -> Vec<Binding> {
        let mut bindings = Vec::new();
        let descendants = self.get_all_descendants(neg_conj_id);

        for var in vars {
            if let Some(val) = self.find_binding_in_descendants(var, &descendants) {
                bindings.push(Binding {
                    variable: var.replace('X', "?Var"),
                    value: Self::unmangle_sumo(&val),
                });
            }
        }
        bindings
    }

    // ------------------------------------------------------------------
    // Strategy 3 (new): resolution unification
    // ------------------------------------------------------------------

    /// Finds the first resolution step where:
    ///   - one parent is a *variadic* (has variables) descendant of the negated conjecture
    ///   - the other parent is a *ground* external fact
    ///   - the resolvent itself is ground
    ///
    /// Matches the negative literal that was cancelled in the variadic parent
    /// against the positive ground fact to extract variable → constant substitutions.
    fn extract_from_resolution_unification(
        &self,
        neg_conj_id: &str,
        vars: &[String],
    ) -> Option<Vec<Binding>> {

        // Build the set of all descendants of the negated conjecture (inclusive).
        let neg_conj_set: HashSet<String> = {
            let mut s = HashSet::new();
            s.insert(neg_conj_id.to_string());
            for n in self.get_all_descendants(neg_conj_id) {
                s.insert(n.id.clone());
            }
            s
        };

        // The "variadic" subset: descendants that still contain unbound variables.
        let variadic_ids: HashSet<String> = neg_conj_set
            .iter()
            .filter(|id| {
                self.nodes
                    .get(*id)
                    .map(|n| Self::has_unbound_vars(&n.formula))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();

        // Walk every node looking for ground resolvent steps.
        for node in self.nodes.values() {
            if Self::has_unbound_vars(&node.formula) {
                continue; // This step still has variables — skip.
            }

            // Find a variadic parent from the negated-conjecture chain.
            let variadic_parent_id = node
                .parents
                .iter()
                .find(|p| variadic_ids.contains(*p));

            let variadic_parent_id = match variadic_parent_id {
                Some(id) => id,
                None => continue,
            };

            let variadic_parent = match self.nodes.get(variadic_parent_id) {
                Some(n) => n,
                None => continue,
            };

            // Find a ground *external* parent (not in the negated-conjecture chain).
            for resolvent_id in &node.parents {
                if variadic_ids.contains(resolvent_id) {
                    continue; // Same chain — not an external resolvent.
                }
                let resolvent = match self.nodes.get(resolvent_id) {
                    Some(n) => n,
                    None => continue,
                };
                if Self::has_unbound_vars(&resolvent.formula) {
                    continue;
                }

                // Try to match a negative literal in the variadic parent against
                // the positive literal in the resolvent.
                if let Some(sub) =
                    Self::unify_negative_with_positive(&variadic_parent.formula, &resolvent.formula)
                {
                    log::debug!(
                        "Resolution unification: {} + {} → {:?}",
                        variadic_parent.id, resolvent.id, sub
                    );

                    let bindings: Vec<Binding> = vars
                        .iter()
                        .filter_map(|var| {
                            sub.get(var).map(|val| Binding {
                                variable: var.replace('X', "?Var"),
                                value: Self::unmangle_sumo(val),
                            })
                        })
                        .collect();

                    if bindings.len() == vars.len() {
                        return Some(bindings);
                    }
                }
            }
        }
        None
    }

    /// Tries to unify a negative `~s__holds(pred, a0, a1, …)` literal found
    /// anywhere in `variadic` with the positive `s__holds(pred, c0, c1, …)` in
    /// `resolvent`, where the `c_i` are ground constants and the `a_i` may be
    /// variables (`X\d+`) or constants.
    ///
    /// Returns `Some(var → constant)` on the first successful match.
    fn unify_negative_with_positive(
        variadic: &str,
        resolvent: &str,
    ) -> Option<HashMap<String, String>> {
        let neg_lit_re = Regex::new(r"~s__holds\(([^()]+)\)").unwrap();
        let pos_lit_re = Regex::new(r"(?:^|[^~])s__holds\(([^()]+)\)").unwrap();
        let var_re     = Regex::new(r"^X\d+$").unwrap();

        // Parse the single positive literal from the resolvent.
        let res_cap = pos_lit_re.captures(resolvent)?;
        let res_args: Vec<&str> = res_cap[1].split(',').map(str::trim).collect();

        // Try each negative literal in the variadic formula.
        for cap in neg_lit_re.captures_iter(variadic) {
            let var_args: Vec<&str> = cap[1].split(',').map(str::trim).collect();

            if var_args.len() != res_args.len() {
                continue;
            }
            // First element is the predicate name — must match.
            if var_args[0] != res_args[0] {
                continue;
            }

            let mut sub: HashMap<String, String> = HashMap::new();
            let mut consistent = true;

            for (va, ra) in var_args.iter().zip(res_args.iter()).skip(1) {
                if var_re.is_match(va) {
                    // Variable: record the substitution.
                    sub.insert(va.to_string(), ra.to_string());
                } else if va != ra {
                    // Constant mismatch — this literal pair doesn't unify.
                    consistent = false;
                    break;
                }
            }

            if consistent && !sub.is_empty() {
                return Some(sub);
            }
        }
        None
    }

    // -----------------------------------------------------------------------
    // Shared helpers
    // -----------------------------------------------------------------------

    /// Extract variables in **order of first appearance** (not a HashSet).
    /// Variable ordering is required so that answer arguments map correctly:
    /// vars[0] → answer_arg[0], vars[1] → answer_arg[1], …
    fn extract_variables_ordered(&self, formula: &str) -> Vec<String> {
        let var_re = Regex::new(r"\b(X\d+)\b").unwrap();
        let mut seen = HashSet::new();
        let mut vars = Vec::new();
        for cap in var_re.captures_iter(formula) {
            let v = cap[1].to_string();
            if seen.insert(v.clone()) {
                vars.push(v);
            }
        }
        vars
    }

    /// Collect all nodes reachable (as children) from `parent_id`.
    /// Uses an explicit stack to avoid stack-overflow on long proofs.
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
            log::debug!("Searching descendant proof step: {}", node.formula);
            if !node.formula.contains(var) {
                for cap in const_re.captures_iter(&node.formula) {
                    let candidate = cap[1].to_string();
                    if !candidate.ends_with("__m") {
                        return Some(candidate);
                    }
                }
            }
        }
        None
    }

    /// Convert TPTP-mangled term `s__JohnsCarry` → KIF `JohnsCarry`
    pub fn unmangle_sumo(term: &str) -> String {
        let mut clean = term.to_string();
        if clean.starts_with("s__") {
            clean = clean[3..].to_string();
        }
        if clean.ends_with("__m") {
            clean = clean[..clean.len() - 3].to_string();
        }
        clean
    }
}