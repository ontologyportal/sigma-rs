// crates/core/src/prover/proof/tstp.rs
//
// Backend-agnostic TSTP proof machinery, shared by every subprocess
// `ProverRunner` that consumes a TPTP/SZS proof transcript (Vampire, E, …).
//
// What lives here is *not* tied to any one prover: a parsed proof step, the
// SUO-KIF binding extractor, and the bare-formula → IR lowering.  What stays
// in each backend's `subprocess.rs` is the prover-*specific* surface: CLI
// argument construction, SZS/termination marker dialect, and the regex that
// recognises that prover's proof-step naming (Vampire's `f\d+`, E's
// `c_0_N`/`i_0_N`).  Each backend's parser is responsible for resolving a
// step's parent references into [`ProofStep::parents`] — Vampire reads the
// last `[...]` bracket of its inference tail; E does name-membership over the
// whole annotation (its `inference(...)` terms nest and carry a trailing
// `['proof']` that the naive "last bracket" rule would mis-read).

use std::collections::{HashMap, HashSet};

use once_cell::sync::Lazy;
use regex::Regex;

use crate::prover::result::Binding;

// -- SUO-KIF-aware regexes (prover-agnostic) ----------------------------------

/// A clause/formula carrying an as-yet-unbound SUO-KIF variable (`X0`, `X12`).
static RE_UNBOUND:  Lazy<Regex> = Lazy::new(|| Regex::new(r"\bX\d+\b").unwrap());
static RE_NEG_HOLDS: Lazy<Regex> = Lazy::new(|| Regex::new(r"~s__holds\(([^()]+)\)").unwrap());
static RE_POS_HOLDS: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?:^|[^~])s__holds\(([^()]+)\)"
).unwrap());
static RE_VAR:      Lazy<Regex> = Lazy::new(|| Regex::new(r"^X\d+$").unwrap());
static RE_VAR_CAP:  Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(X\d+)\b").unwrap());
static RE_CONST:    Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(s__[A-Za-z0-9_]+)\b").unwrap());

// -- Parsed proof step ---------------------------------------------------------

/// One line of a TSTP proof transcript, normalised across backends.
#[derive(Debug, Clone)]
pub(crate) struct ProofStep {
    /// This step's name (`f37`, `c_0_6`, `kb_42`, …).
    pub id:      String,
    /// TPTP role (`axiom`, `negated_conjecture`, `plain`, …).
    pub role:    String,
    /// The step's formula, whitespace-normalised, parentheses stripped of
    /// surrounding newlines.
    pub formula: String,
    /// Names of the steps this one was derived from, already resolved by the
    /// backend parser.  Empty for input/leaf steps.
    pub parents: Vec<String>,
    /// Original axiom name preserved by the prover's source annotation
    /// (`file('…', kb_42)` → `Some("kb_42")`).  `None` for derived steps and
    /// builds that don't preserve names.
    pub source_name: Option<String>,
}

/// Map each step's resolved [`ProofStep::parents`] to positional indices into
/// `steps`, for the IR/KIF proof representations that address premises by
/// position.  Unknown names (parents that aren't themselves steps) are dropped.
fn premise_indices(steps: &[ProofStep]) -> Vec<Vec<usize>> {
    let id_to_idx: HashMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    steps
        .iter()
        .map(|s| {
            s.parents
                .iter()
                .filter_map(|p| id_to_idx.get(p.as_str()).copied())
                .collect()
        })
        .collect()
}

/// Build the `(formula, role, premise_indices, source_name)` tuples that
/// [`crate::prover::proof::proof_steps_to_kif`] consumes.
pub(crate) fn kif_proof_inputs(
    steps: &[ProofStep],
) -> Vec<(String, String, Vec<usize>, Option<String>)> {
    let premises = premise_indices(steps);
    steps
        .iter()
        .zip(premises)
        .map(|(s, prem)| (s.formula.clone(), s.role.clone(), prem, s.source_name.clone()))
        .collect()
}

/// Lower each step's bare formula string into a structured [`IrProofStep`].
///
/// The formula is wrapped in a minimal FOF envelope and round-tripped through
/// [`crate::trans::ir::parse_tptp`]; steps whose formula can't be parsed fall
/// back to [`crate::trans::ir::Formula::True`].  Premises come from the
/// pre-resolved [`ProofStep::parents`].
pub(crate) fn proof_steps_to_ir(steps: &[ProofStep]) -> Vec<crate::prover::proof::IrProofStep> {
    use crate::prover::proof::IrProofStep;

    let premises = premise_indices(steps);

    steps
        .iter()
        .zip(premises)
        .enumerate()
        .map(|(i, (step, prem))| {
            let wrapped = format!("fof(anon, plain, ({})).\n", step.formula);
            let formula = crate::trans::ir::parse_tptp(&wrapped)
                .ok()
                .and_then(|p| p.axioms().first().cloned())
                .unwrap_or(crate::trans::ir::Formula::True);

            let source_sid = step.source_name.as_deref()
                .and_then(crate::prover::proof::parse_kb_axiom_name);

            IrProofStep {
                index: i,
                rule: step.role.clone(),
                premises: prem,
                formula,
                source_sid,
            }
        })
        .collect()
}

// -- TptpProofProcessor: SUO-KIF binding extraction ---------------------------

struct GraphNode {
    id:      String,
    formula: String,
    parents: Vec<String>,
}

/// Extracts variable bindings (answers) from a refutation proof by walking the
/// negated-conjecture's descendants.  Purely SUO-KIF/`s__holds`-driven, so it
/// is independent of which prover produced the proof — it already understands
/// both Vampire (`sK…`) and E (`esk…`) Skolem prefixes.
pub(crate) struct TptpProofProcessor {
    nodes:                 HashMap<String, GraphNode>,
    conjecture_id:         Option<String>,
    negated_conjecture_id: Option<String>,
}

impl TptpProofProcessor {
    pub(crate) fn new() -> Self {
        Self { nodes: HashMap::new(), conjecture_id: None, negated_conjecture_id: None }
    }

    pub(crate) fn load_proof(&mut self, steps: &[ProofStep]) {
        for step in steps {
            if step.role == "conjecture" {
                self.conjecture_id = Some(step.id.clone());
            } else if step.role == "negated_conjecture" {
                self.negated_conjecture_id = Some(step.id.clone());
            }
            self.nodes.insert(step.id.clone(), GraphNode {
                id: step.id.clone(), formula: step.formula.clone(), parents: step.parents.clone(),
            });
        }
        // Silence the unused-field lint when binding extraction takes an early
        // return path; `conjecture_id` documents intent and is cheap to keep.
        let _ = &self.conjecture_id;
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
