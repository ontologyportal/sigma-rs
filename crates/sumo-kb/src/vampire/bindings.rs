// crates/sumo-kb/src/vampire/bindings.rs
//
// Extract variable bindings from a native Vampire `Proof`.
//
// A refutation proof of `?[X0, X1, ...]: goal(X0, X1, ...)` passes through
// a `NegatedConjecture` step whose conclusion still contains the free
// variables `X0, X1, ...`.  As Vampire resolves that step against ground
// axioms, the free variables get substituted away.  We look for a proof
// step that binds every target variable by pairing the NegatedConjecture's
// argument list with a ground resolvent's argument list.
//
// Two strategies, tried in order:
//
//   Strategy A (resolution unification): find a step produced from a
//     variadic ancestor + at least one ground premise.  Unify the
//     negated-conjecture-like literal in the ancestor with the positive
//     literal in the ground premise, position by position, reading off
//     the substitution.  Works on the usual case: conjecture is a single
//     atomic predicate.
//
//   Strategy B (descendant constants): if A can't bind every target
//     variable, scan descendants of the variadic set for ground constants
//     that appeared once the variable disappeared.  Best-effort heuristic.
//
// Gated: requires both the `vampire` and `integrated-prover` features via
// the parent module.

use std::collections::{HashMap, HashSet};

use once_cell::sync::Lazy;
use regex::Regex;

use vampire_prover::{Proof, ProofRule};

use crate::vampire::converter::QueryVarMap;

/// A single variable-to-value substitution extracted from a proof.
///
/// This mirrors the `Binding` type returned by the subprocess prover, so
/// both paths populate `ProverResult::bindings` with the same shape.  A
/// standalone struct (not an alias for `crate::prover::Binding`) keeps
/// this module independent of the `ask` feature's gating.
#[derive(Debug, Clone)]
pub struct ProofBinding {
    pub variable: String,
    pub value:    String,
}

static RE_UNBOUND: Lazy<Regex> = Lazy::new(|| Regex::new(r"\bX\d+\b").unwrap());
static RE_VAR:     Lazy<Regex> = Lazy::new(|| Regex::new(r"^X\d+$").unwrap());
static RE_CONST:   Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(s__[A-Za-z0-9_]+)\b").unwrap());

/// Top-level (non-nested) literal in either FOF or TFF encoding.
///
/// Matches either `pred(args)` or `~pred(args)`; captures polarity,
/// predicate name, and raw argument list.  The argument list is
/// `[^()]*` so this only matches when there are no nested parens.
/// That covers the vast majority of conjectures that users would ask
/// variable bindings about.
static RE_LITERAL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(~?)(\w+)\(([^()]*)\)").unwrap()
});

/// Extract bindings for every free variable recorded in `qvm`, or an
/// empty vector if any variable can't be bound.
pub fn extract_bindings(proof: &Proof, qvm: &QueryVarMap) -> Vec<ProofBinding> {
    let target_vars: Vec<String> = qvm
        .free_var_indices
        .iter()
        .map(|&i| format!("X{}", i))
        .collect();
    if target_vars.is_empty() {
        return Vec::new();
    }

    let steps: Vec<(String, ProofRule, Vec<usize>)> = proof
        .steps()
        .iter()
        .map(|s| (s.conclusion().to_string(), s.rule(), s.premises().to_vec()))
        .collect();
    log::debug!(target: "sumo_kb::bindings",
        "extract_bindings: {} proof step(s), target_vars={:?}, idx_to_kif={:?}",
        steps.len(), target_vars, qvm.idx_to_kif);
    for (i, (f, rule, prems)) in steps.iter().enumerate() {
        log::trace!(target: "sumo_kb::bindings",
            "  step {}: rule={:?} prems={:?} conclusion={:?}", i, rule, prems, f);
    }

    // Find the step corresponding to the (negated) conjecture.
    //
    // First look for a step explicitly tagged `NegatedConjecture`; if
    // vampire-prover didn't set that tag on the input step (current
    // behaviour as of April 2026 -- all inputs come through as `Axiom`),
    // fall back to identifying the first input step whose conclusion
    // starts with a negated-quantifier prefix (`~?` or `~!`).  That shape
    // only appears when Vampire has negated an input conjecture.
    let neg_conj_idx = steps.iter()
        .position(|(_, rule, _)| *rule == ProofRule::NegatedConjecture)
        .or_else(|| steps.iter().position(|(formula, _, premises)| {
            premises.is_empty() && {
                let trimmed = formula.trim_start();
                trimmed.starts_with("~?") || trimmed.starts_with("~!")
            }
        }));
    let Some(neg_conj_idx) = neg_conj_idx else {
        log::debug!(target: "sumo_kb::bindings",
            "no conjecture-like step in proof; cannot extract bindings");
        return Vec::new();
    };

    // Variadic set: NegatedConjecture plus every descendant that still
    // carries at least one `X\d+` token.  These are the steps that need
    // to be resolved against a ground premise to produce bindings.
    let variadic_set = compute_variadic_set(&steps, neg_conj_idx);

    if let Some(bs) = strategy_resolution(&steps, &variadic_set, &target_vars, qvm) {
        return bs;
    }
    strategy_descendants(&steps, &variadic_set, &target_vars, qvm)
}

fn compute_variadic_set(
    steps: &[(String, ProofRule, Vec<usize>)],
    neg_conj_idx: usize,
) -> HashSet<usize> {
    let mut set = HashSet::new();
    set.insert(neg_conj_idx);
    let mut changed = true;
    while changed {
        changed = false;
        for (i, (formula, _, premises)) in steps.iter().enumerate() {
            if set.contains(&i) {
                continue;
            }
            if premises.iter().any(|p| set.contains(p)) && RE_UNBOUND.is_match(formula) {
                set.insert(i);
                changed = true;
            }
        }
    }
    set
}

/// Resolution-unification: find a step derived from a variadic ancestor
/// + a ground premise, and read substitutions off the argument lists.
fn strategy_resolution(
    steps: &[(String, ProofRule, Vec<usize>)],
    variadic_set: &HashSet<usize>,
    target_vars: &[String],
    qvm: &QueryVarMap,
) -> Option<Vec<ProofBinding>> {
    for (_formula, _rule, premises) in steps {
        let Some(&variadic_parent_idx) = premises.iter().find(|&&p| variadic_set.contains(&p)) else {
            continue;
        };
        let variadic_formula = &steps[variadic_parent_idx].0;
        if !RE_UNBOUND.is_match(variadic_formula) {
            continue;
        }

        for &resolvent_idx in premises {
            if variadic_set.contains(&resolvent_idx) {
                continue;
            }
            let resolvent_formula = &steps[resolvent_idx].0;
            if RE_UNBOUND.is_match(resolvent_formula) {
                continue;
            }
            if let Some(sub) = unify_neg_pos(variadic_formula, resolvent_formula) {
                let bindings: Vec<ProofBinding> = target_vars
                    .iter()
                    .filter_map(|v| {
                        sub.get(v).map(|val| ProofBinding {
                            variable: qvm_kif_name(qvm, v),
                            value:    unmangle_sumo(val),
                        })
                    })
                    .collect();
                if bindings.len() == target_vars.len() {
                    log::debug!(target: "sumo_kb::bindings",
                        "resolution strategy bound {} variable(s)", bindings.len());
                    return Some(bindings);
                }
            }
        }
    }
    None
}

/// Descendant fallback: scan every descendant of the variadic set for
/// ground `s__` constants that appear once a variable has disappeared.
/// Lossy but surprisingly useful when Strategy A misses a case.
fn strategy_descendants(
    steps: &[(String, ProofRule, Vec<usize>)],
    variadic_set: &HashSet<usize>,
    target_vars: &[String],
    qvm: &QueryVarMap,
) -> Vec<ProofBinding> {
    let descendants: Vec<usize> = {
        let mut result  = Vec::new();
        let mut visited = variadic_set.clone();
        let mut frontier: Vec<usize> = variadic_set.iter().copied().collect();
        while let Some(idx) = frontier.pop() {
            for (i, (_, _, premises)) in steps.iter().enumerate() {
                if !visited.contains(&i) && premises.contains(&idx) {
                    visited.insert(i);
                    result.push(i);
                    frontier.push(i);
                }
            }
        }
        result
    };

    let bindings: Vec<ProofBinding> = target_vars
        .iter()
        .filter_map(|v| {
            let value = descendants.iter().find_map(|&i| {
                let formula = &steps[i].0;
                if formula.contains(v.as_str()) {
                    return None;
                }
                RE_CONST.captures_iter(formula).find_map(|cap| {
                    let candidate = cap[1].to_string();
                    if candidate.ends_with("__m") { None } else { Some(candidate) }
                })
            })?;
            Some(ProofBinding {
                variable: qvm_kif_name(qvm, v),
                value:    unmangle_sumo(&value),
            })
        })
        .collect();

    if !bindings.is_empty() {
        log::debug!(target: "sumo_kb::bindings",
            "descendant strategy extracted {} binding(s)", bindings.len());
    }
    bindings
}

/// Try to unify the first negative literal in `variadic` (contains free
/// vars) with the first positive literal in `resolvent` (fully ground),
/// returning a `X<idx> -> value` substitution map if every free-var
/// position resolves to a concrete value.
fn unify_neg_pos(variadic: &str, resolvent: &str) -> Option<HashMap<String, String>> {
    // Find the first positive literal in the resolvent.
    let pos = RE_LITERAL
        .captures_iter(resolvent)
        .find(|cap| &cap[1] == "")?;
    let pos_name  = pos[2].to_string();
    let pos_args: Vec<String> = pos[3].split(',').map(|s| s.trim().to_string()).collect();

    // Try every negative literal in the variadic side that shares the head.
    for cap in RE_LITERAL.captures_iter(variadic) {
        if &cap[1] != "~" {
            continue;
        }
        if cap[2] != pos_name {
            continue;
        }
        let neg_args: Vec<&str> = cap[3].split(',').map(str::trim).collect();
        if neg_args.len() != pos_args.len() {
            continue;
        }

        let mut sub = HashMap::new();
        let mut consistent = true;
        for (neg, pos_a) in neg_args.iter().zip(pos_args.iter()) {
            if RE_VAR.is_match(neg) {
                // Variable on the negated side — bind it.
                if let Some(prev) = sub.insert(neg.to_string(), pos_a.to_string()) {
                    if prev != *pos_a {
                        consistent = false;
                        break;
                    }
                }
            } else if neg != pos_a {
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

/// Strip `s__` prefix and `__m` suffix that the SUMO encoder added.
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
    fn unify_single_variable_against_ground_atom() {
        let neg = "~member(X0, s__Org1)";
        let pos = "member(s__Alice, s__Org1)";
        let sub = unify_neg_pos(neg, pos).expect("unification should succeed");
        assert_eq!(sub.get("X0").unwrap(), "s__Alice");
    }

    #[test]
    fn unify_rejects_head_mismatch() {
        let neg = "~member(X0, s__Org1)";
        let pos = "located(s__Alice, s__Org1)";
        assert!(unify_neg_pos(neg, pos).is_none());
    }

    #[test]
    fn unify_rejects_arity_mismatch() {
        let neg = "~member(X0)";
        let pos = "member(s__Alice, s__Org1)";
        assert!(unify_neg_pos(neg, pos).is_none());
    }

    #[test]
    fn unify_rejects_inconsistent_const() {
        let neg = "~p(X0, s__Foo)";
        let pos = "p(s__A, s__Bar)"; // position 2 mismatches
        assert!(unify_neg_pos(neg, pos).is_none());
    }

    #[test]
    fn unify_keeps_repeated_variable_consistent() {
        // ~p(X0, X0) against p(a, a) binds X0 = a.
        let neg = "~p(X0, X0)";
        let pos = "p(s__A, s__A)";
        let sub = unify_neg_pos(neg, pos).expect("binds");
        assert_eq!(sub.get("X0").unwrap(), "s__A");
    }

    #[test]
    fn unify_rejects_repeated_variable_inconsistency() {
        // ~p(X0, X0) against p(a, b) must fail.
        let neg = "~p(X0, X0)";
        let pos = "p(s__A, s__B)";
        assert!(unify_neg_pos(neg, pos).is_none());
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
        assert_eq!(qvm_kif_name(&qvm, "X7"), "X7"); // fallback
    }
}
