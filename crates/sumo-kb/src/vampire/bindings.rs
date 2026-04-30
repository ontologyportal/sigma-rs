// crates/sumo-kb/src/vampire/bindings.rs
//
// Extract variable bindings from a native Vampire `Proof`.
//
// Refutation proofs produced by Vampire for an existential conjecture
// `?[X0, X1, ...] : goal(...)` eliminate each free variable via a
// sequence of unification steps.  The substitutions are scattered
// across the proof DAG: any single proof step tells us about only a
// subset of variables, and the rest flow through intermediate aliases
// that must be followed to later steps.
//
// Extraction proceeds in three passes:
//
// 1. Identify the conjecture step -- either a step tagged
//    `ProofRule::NegatedConjecture` (rare in vampire-prover's current
//    release; TODO in TODO.md) or the first input step whose
//    conclusion starts with `~?` / `~!` (a negated-quantifier prefix
//    only appears when Vampire has inverted an input conjecture).
//
// 2. Forward walk: for every step whose clause carries a current
//    "alias" for a conjecture variable, follow that alias.  When a
//    Resolution-family step (Resolution, ForwardSubsumptionResolution,
//    Superposition, ...) unifies an alias with a constant or with
//    another variable, update the alias table.  Bindings to
//    constants are recorded as answers; variable-to-variable
//    substitutions keep the alias alive under a new name.
//
// 3. Assemble: for each conjecture variable recorded in the
//    QueryVarMap, look up its final value and emit a `ProofBinding`.
//    Variables that never got bound to a ground value are dropped
//    (we return partial results rather than silently succeeding).
//
// Gated: requires both the `ask` and `integrated-prover` features via
// the parent module.

use std::collections::HashMap;

use once_cell::sync::Lazy;
use regex::Regex;

use vampire_prover::{Proof, ProofRule};

use crate::vampire::converter::QueryVarMap;

/// A single variable-to-value substitution extracted from a proof.
///
/// Mirrors the `Binding` type returned by the subprocess prover so both
/// paths populate `ProverResult::bindings` with the same shape.  Kept as
/// a standalone struct (not an alias for `crate::prover::Binding`) so
/// this module doesn't depend on the `ask` feature's gating.
#[derive(Debug, Clone)]
pub struct ProofBinding {
    pub variable: String,
    pub value:    String,
}

static RE_UNBOUND: Lazy<Regex> = Lazy::new(|| Regex::new(r"\bX\d+\b").unwrap());
static RE_VAR:     Lazy<Regex> = Lazy::new(|| Regex::new(r"^X\d+$").unwrap());

/// Top-level (non-nested) literal in either FOF or TFF encoding.
/// Captures polarity, predicate name, and the flat argument list.  The
/// argument list is `[^()]*`, so this only matches literals with no
/// nested function calls -- fine for the conjectures users actually
/// ask for bindings on.
static RE_LITERAL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(~?)(\w+)\(([^()]*)\)").unwrap());

/// Entry point: walk a proof and extract bindings for every free
/// variable recorded in `qvm`.
pub fn extract_bindings(proof: &Proof, qvm: &QueryVarMap) -> Vec<ProofBinding> {
    if qvm.free_var_indices.is_empty() {
        return Vec::new();
    }

    // Snapshot the proof as (formula_str, rule, premises) tuples so
    // subsequent passes can read by index.
    let steps: Vec<(String, ProofRule, Vec<usize>)> = proof
        .steps()
        .iter()
        .map(|s| (s.conclusion().to_string(), s.rule(), s.premises().to_vec()))
        .collect();

    let target_vars: Vec<String> = qvm
        .free_var_indices
        .iter()
        .map(|&i| format!("X{}", i))
        .collect();
    crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sumo_kb::bindings", message: format!("extract_bindings: {} steps, target_vars={:?}, idx_to_kif={:?}", steps.len(), target_vars, qvm.idx_to_kif) });
    for (i, (f, rule, prems)) in steps.iter().enumerate() {
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Trace, target: "sumo_kb::bindings", message: format!("  step {}: rule={:?} prems={:?} conclusion={:?}", i, rule, prems, f) });
    }

    // Identify the conjecture-input step.
    let Some(neg_conj_idx) = find_negated_conjecture(&steps) else {
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sumo_kb::bindings", message: format!("no conjecture-like step in proof; cannot extract bindings") });
        return Vec::new();
    };

    // Forward walk: track each target variable's current alias (or
    // bound constant) across the proof.
    let mut tracker = AliasTracker::new(&target_vars, neg_conj_idx);
    for step_idx in 0..steps.len() {
        if step_idx == neg_conj_idx {
            continue;
        }
        tracker.process_step(&steps, step_idx);
    }

    tracker.finalise(qvm)
}

// -- conjecture step detection ------------------------------------------------

fn find_negated_conjecture(
    steps: &[(String, ProofRule, Vec<usize>)],
) -> Option<usize> {
    // Preferred: an input step explicitly tagged NegatedConjecture.
    if let Some(i) = steps
        .iter()
        .position(|(_, rule, _)| *rule == ProofRule::NegatedConjecture)
    {
        return Some(i);
    }
    // Fallback: first input step whose conclusion starts with `~?` /
    // `~!`.  Vampire's renderer only emits that prefix when an input
    // conjecture has been negated -- axioms never have it.
    steps.iter().position(|(formula, _, premises)| {
        premises.is_empty() && {
            let trimmed = formula.trim_start();
            trimmed.starts_with("~?") || trimmed.starts_with("~!")
        }
    })
}

// -- alias tracker ------------------------------------------------------------

/// For each target variable (conjecture X0, X1, ...), what we currently
/// know about it.
#[derive(Debug, Clone)]
enum AliasState {
    /// Unresolved: the variable still lives as a fresh-named variable
    /// carried in the clause of `alive_in_step`, where it's written
    /// `as_name`.  Most of the time `as_name` is unchanged from the
    /// conjecture step, because transformation rules don't rename.
    Alias { alive_in_step: usize, as_name: String },
    /// Bound to a ground value.
    Bound(String),
}

struct AliasTracker {
    /// Target (conjecture) variable name -> current state.
    states: HashMap<String, AliasState>,
}

impl AliasTracker {
    fn new(target_vars: &[String], neg_conj_idx: usize) -> Self {
        let mut states = HashMap::new();
        for v in target_vars {
            states.insert(
                v.clone(),
                AliasState::Alias { alive_in_step: neg_conj_idx, as_name: v.clone() },
            );
        }
        Self { states }
    }

    /// Which alias names (in the step with index `step_idx`) correspond
    /// to target variables?  Returns a map `alias_name -> target_var`.
    fn aliases_at(&self, step_idx: usize) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for (tv, state) in &self.states {
            if let AliasState::Alias { alive_in_step, as_name } = state {
                if *alive_in_step == step_idx {
                    out.insert(as_name.clone(), tv.clone());
                }
            }
        }
        out
    }

    fn process_step(
        &mut self,
        steps: &[(String, ProofRule, Vec<usize>)],
        idx: usize,
    ) {
        let (conclusion, rule, premises) = &steps[idx];

        // Collect: which premises carry an alias to one of our target
        // variables?  These are the "conjecture-side" inputs.
        let mut conj_side_premises: Vec<(usize, HashMap<String, String>)> = Vec::new();
        for &p in premises {
            let aliases = self.aliases_at(p);
            if !aliases.is_empty() {
                conj_side_premises.push((p, aliases));
            }
        }
        if conj_side_premises.is_empty() {
            return;
        }

        match rule {
            ProofRule::Flatten
            | ProofRule::Rectify
            | ProofRule::NNFTransformation
            | ProofRule::EENFTransformation
            | ProofRule::CNFTransformation => {
                // Transformations preserve variable names (verified
                // empirically on our proofs).  Carry aliases to the
                // new step unchanged.
                for (_, aliases) in &conj_side_premises {
                    for (as_name, tv) in aliases {
                        let still_present = RE_UNBOUND
                            .find_iter(conclusion)
                            .any(|m| m.as_str() == as_name);
                        if still_present {
                            self.states.insert(
                                tv.clone(),
                                AliasState::Alias {
                                    alive_in_step: idx,
                                    as_name: as_name.clone(),
                                },
                            );
                        }
                    }
                }
            }

            _ => {
                // Resolution-family: Resolution, Superposition,
                // ForwardSubsumptionResolution, Forward/Backward
                // Demodulation, Skolemize (introducing skolem fns),
                // SkolemSymbolIntroduction, TrivialInequalityRemoval,
                // Avatar, Other, Axiom (never hit -- has no premises).
                //
                // For unification-bearing rules we pair the
                // conjecture-side premise with a non-conjecture
                // premise, find the resolved literal pair, and read
                // off the MGU.
                self.unify_resolution(steps, idx, &conj_side_premises);
            }
        }
    }

    /// Try to extract a substitution from a resolution-family step at
    /// `idx`, updating `self.states`.
    fn unify_resolution(
        &mut self,
        steps: &[(String, ProofRule, Vec<usize>)],
        idx: usize,
        conj_side_premises: &[(usize, HashMap<String, String>)],
    ) {
        let (conclusion, _, premises) = &steps[idx];

        for (conj_premise_idx, aliases) in conj_side_premises {
            let conj_formula = &steps[*conj_premise_idx].0;

            // Find a partner premise (not the conjecture side).
            for &other_idx in premises {
                if other_idx == *conj_premise_idx {
                    continue;
                }
                let other_formula = &steps[other_idx].0;

                // Try every plausible resolved pair: a literal in the
                // conjecture-side premise and its negation-partner in
                // the other premise.
                if let Some(sub) = find_resolution_substitution(conj_formula, other_formula) {
                    self.apply_substitution(&sub, aliases, idx, conclusion);
                    return;
                }
            }
        }
    }

    /// Apply the `sub` map (produced by unifying one literal pair) to
    /// the conjecture aliases at the current step.
    fn apply_substitution(
        &mut self,
        sub: &HashMap<String, String>,
        aliases: &HashMap<String, String>,
        step_idx: usize,
        conclusion: &str,
    ) {
        for (as_name, tv) in aliases {
            if let Some(value) = sub.get(as_name) {
                if RE_VAR.is_match(value) {
                    // Aliased to another step-local variable.  Only
                    // keep the alias if it still appears in the
                    // conclusion; otherwise it's been consumed or
                    // renamed in a way we can't follow.
                    let still_present = RE_UNBOUND
                        .find_iter(conclusion)
                        .any(|m| m.as_str() == value);
                    if still_present {
                        self.states.insert(
                            tv.clone(),
                            AliasState::Alias {
                                alive_in_step: step_idx,
                                as_name: value.clone(),
                            },
                        );
                    }
                } else {
                    // Bound to a constant.  Record the ground value.
                    self.states.insert(tv.clone(), AliasState::Bound(value.clone()));
                }
            } else {
                // The alias wasn't touched by this substitution.  If
                // it still appears in the conclusion, carry the alias
                // forward under its same name (common case for
                // unchanged literals in a resolution).
                let still_present = RE_UNBOUND
                    .find_iter(conclusion)
                    .any(|m| m.as_str() == as_name);
                if still_present {
                    self.states.insert(
                        tv.clone(),
                        AliasState::Alias {
                            alive_in_step: step_idx,
                            as_name: as_name.clone(),
                        },
                    );
                }
            }
        }
    }

    fn finalise(&self, qvm: &QueryVarMap) -> Vec<ProofBinding> {
        let mut out = Vec::new();
        for (tv, state) in &self.states {
            if let AliasState::Bound(value) = state {
                out.push(ProofBinding {
                    variable: qvm_kif_name(qvm, tv),
                    value:    unmangle_sumo(value),
                });
            }
        }
        // Stable ordering by variable name, so the binding list is
        // deterministic regardless of HashMap iteration order.
        out.sort_by(|a, b| a.variable.cmp(&b.variable));
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sumo_kb::bindings", message: format!("finalise: extracted {} binding(s)", out.len()) });
        out
    }
}

// -- resolution substitution extraction --------------------------------------

/// Given two clauses that were resolved to produce a third, find the
/// MGU of the resolved literal pair.
///
/// Scans every `(~? pred(args))` literal in the conjecture-side clause
/// and every opposite-polarity same-predicate literal in the other
/// clause; returns the first consistent unifier found.
///
/// Works on literals without nested parentheses -- fine for atomic
/// predicate calls, which is what users ask for bindings on.
fn find_resolution_substitution(
    conj_side: &str,
    other: &str,
) -> Option<HashMap<String, String>> {
    let conj_lits: Vec<(String, String, Vec<String>)> = literals(conj_side);
    let other_lits: Vec<(String, String, Vec<String>)> = literals(other);

    for (pol_a, name_a, args_a) in &conj_lits {
        for (pol_b, name_b, args_b) in &other_lits {
            if name_a != name_b {
                continue;
            }
            // Resolved literals have opposite polarities.
            if pol_a == pol_b {
                continue;
            }
            if args_a.len() != args_b.len() {
                continue;
            }
            if let Some(sub) = unify_arg_lists(args_a, args_b) {
                return Some(sub);
            }
        }
    }
    None
}

/// Parse a clause string into a list of `(polarity, pred_name, args)`
/// tuples.  Polarity is `""` or `"~"`.
fn literals(clause: &str) -> Vec<(String, String, Vec<String>)> {
    RE_LITERAL
        .captures_iter(clause)
        .map(|c| {
            let pol = c[1].to_string();
            let name = c[2].to_string();
            let args: Vec<String> =
                c[3].split(',').map(|s| s.trim().to_string()).collect();
            (pol, name, args)
        })
        .collect()
}

/// One-sided unifier: only records substitutions for the conjecture
/// side's variables.  The "other" side's variables are treated as
/// opaque -- they may map to anything without creating an entry, and
/// constants on the other side only matter when the conjecture side
/// has a constant too (in which case they must be equal).
///
/// This avoids a subtle bug where two clauses happen to reuse the same
/// variable *name* for different logical variables (Vampire always
/// starts fresh from `X0` per clause).  Bidirectional unification in
/// that case would incorrectly pin `X0` to two different values.
fn unify_arg_lists(
    conj_args: &[String],
    other_args: &[String],
) -> Option<HashMap<String, String>> {
    let mut sub: HashMap<String, String> = HashMap::new();
    for (a, b) in conj_args.iter().zip(other_args.iter()) {
        let a_var = RE_VAR.is_match(a);
        if a_var {
            // Conjecture-side variable: whatever we see on the other
            // side is recorded (a concrete value, or the other
            // clause's variable name which we'll follow as an alias).
            insert_unique(&mut sub, a.clone(), b.clone())?;
        } else {
            // Conjecture side is a concrete term.  The other side
            // must be equal OR a variable (which Vampire will have
            // unified with this constant; from our POV the alias on
            // the other clause is irrelevant).
            let b_var = RE_VAR.is_match(b);
            if !b_var && a != b {
                return None;
            }
        }
    }
    Some(sub)
}

fn insert_unique(
    sub: &mut HashMap<String, String>,
    key: String,
    value: String,
) -> Option<()> {
    match sub.get(&key) {
        Some(existing) if *existing == value => Some(()),
        Some(_) => None, // inconsistent
        None => {
            sub.insert(key, value);
            Some(())
        }
    }
}

// -- helpers -----------------------------------------------------------------

fn unmangle_sumo(value: &str) -> String {
    let mut clean = value.to_string();
    if let Some(stripped) = clean.strip_prefix("s__") {
        clean = stripped.to_string();
    }
    if let Some(stripped) = clean.strip_suffix("__m") {
        clean = stripped.to_string();
    }
    clean
}

fn qvm_kif_name(qvm: &QueryVarMap, vampire_var: &str) -> String {
    let idx: u32 = vampire_var.trim_start_matches('X').parse().unwrap_or(0);
    qvm.idx_to_kif
        .get(&idx)
        .cloned()
        .unwrap_or_else(|| vampire_var.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qvm_with(idx_to_kif: &[(u32, &str)], free: &[u32]) -> QueryVarMap {
        QueryVarMap {
            idx_to_kif:       idx_to_kif.iter().map(|(i, n)| (*i, n.to_string())).collect(),
            free_var_indices: free.to_vec(),
        }
    }

    #[test]
    fn unify_simple_ground() {
        let sub = find_resolution_substitution(
            "~member(X0, s__Org1)",
            "member(s__Alice, s__Org1)",
        ).expect("should unify");
        assert_eq!(sub.get("X0").unwrap(), "s__Alice");
    }

    #[test]
    fn unify_mixed_var_var_and_var_const() {
        // ~pred(X0, X1) vs pred(X2, s__Carol): X0↔X2, X1↦s__Carol.
        let sub = find_resolution_substitution(
            "~s__grandparent(X1, X0)",
            "s__grandparent(X0, s__Carol)",
        ).expect("should unify");
        // X0 (right-hand's var at position 0) aliases X1 (left's position 0).
        assert!(sub.contains_key("X0") || sub.contains_key("X1"));
        assert_eq!(sub.get("X0").unwrap_or(&"".to_string()), "s__Carol");
    }

    #[test]
    fn unify_head_mismatch_rejected() {
        let sub = find_resolution_substitution(
            "~s__parent(X0, s__Bob)",
            "s__grandparent(s__Alice, s__Bob)",
        );
        assert!(sub.is_none());
    }

    #[test]
    fn unify_arity_mismatch_rejected() {
        let sub = find_resolution_substitution(
            "~p(X0)",
            "p(s__A, s__B)",
        );
        assert!(sub.is_none());
    }

    #[test]
    fn unify_rejects_repeated_var_inconsistency() {
        // ~p(X0, X0) vs p(s__A, s__B) -- X0 can't be both.
        let sub = find_resolution_substitution("~p(X0, X0)", "p(s__A, s__B)");
        assert!(sub.is_none());
    }

    #[test]
    fn unmangle_drops_prefix_and_mention_suffix() {
        assert_eq!(unmangle_sumo("s__Foo"),    "Foo");
        assert_eq!(unmangle_sumo("s__Foo__m"), "Foo");
        assert_eq!(unmangle_sumo("bare"),      "bare");
    }

    #[test]
    fn qvm_kif_name_lookup() {
        let qvm = qvm_with(&[(0, "?X"), (1, "?Y")], &[0, 1]);
        assert_eq!(qvm_kif_name(&qvm, "X0"), "?X");
        assert_eq!(qvm_kif_name(&qvm, "X1"), "?Y");
        assert_eq!(qvm_kif_name(&qvm, "X7"), "X7");
    }
}
