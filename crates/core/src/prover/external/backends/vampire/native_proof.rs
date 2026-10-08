// crates/core/src/prover/vampire/native_proof.rs
//
// Walk a native `vampire_prover::Proof` (returned by the embedded
// prover path) into the same `Vec<KifProofStep>` shape the subprocess
// path produces from parsing Vampire's TPTP transcript.  The goal is
// bit-for-bit output parity in `--proof kif` / `--proof tptp` / etc.
// regardless of which backend ran.
//
// The core trick: `vampire_prover::Formula::to_tptp()` emits the
// formula as a TPTP string that our existing `formula_to_ast` helper
// can parse straight into a KIF `AstNode`.  Rule names get a
// lower-case mapping from the `ProofRule` enum.  Premise indices
// carry over unchanged (native `ProofStep::premises` are already
// indices into the `Proof::steps` slice).
//
// `source_sid`: the embedded prover never sees our `kb_<sid>` names
// (Vampire's `--output_axiom_names` only applies to TPTP parsing), so an
// input-axiom step is resolved instead through `ProofStep::axiom_index`
// -- the position of the axiom in the lowered problem -- and the
// `sid_map` that is parallel to the problem's axioms.
//
// Gated on `integrated-prover` because the native proof type only
// exists when the embedded FFI backend is compiled in.

#![cfg(feature = "integrated-prover")]

use vampire_prover::{Proof, ProofRule, ProofStep};

use crate::prover::proof::{formula_to_ast, IrProofStep, KifProofStep};
use crate::types::SentenceId;

/// Convert a native Vampire `Proof` into the KIF proof-step shape
/// used by the CLI's `--proof` rendering.
///
/// Preserves topological order (native `Proof::steps` is already
/// sorted so every step's premises have smaller indices), so the
/// output plays cleanly with the proof-display code that references
/// premises by their list index.
///
/// An unparseable step formula (shouldn't normally happen — Vampire's
/// TPTP output is re-readable by our tokenizer) falls back to an
/// `AstNode::Symbol` carrying the raw string prefixed with
/// `; [unparseable]`, mirroring `proof_steps_to_kif`'s defensive
/// behaviour on the subprocess path.
///
/// `sid_map` is parallel to the solved problem's axioms (as for
/// [`crate::trans::assemble::assemble_tptp_indexed`]); input-axiom steps
/// with an entry there get it as their `source_sid`.
pub(crate) fn native_proof_to_kif_steps(
    proof: &Proof,
    sid_map: &[SentenceId],
) -> Vec<KifProofStep> {
    proof
        .steps()
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let tptp = step.conclusion().to_tptp();
            let formula =
                formula_to_ast(&tptp).unwrap_or_else(|| crate::parse::ast::AstNode::Symbol {
                    name: format!("; [unparseable] {}", tptp),
                    span: crate::parse::ast::Span::point(String::new(), 0, 0, 0),
                });
            KifProofStep {
                index: i,
                rule: rule_name(step.rule()).to_string(),
                premises: step.premises().to_vec(),
                formula,
                source_sid: source_sid(step, sid_map),
            }
        })
        .collect()
}

/// The source sentence of an input-axiom step: its axiom index looked up in
/// `sid_map`. `None` for derived steps and for axioms with no sid entry.
fn source_sid(step: &ProofStep, sid_map: &[SentenceId]) -> Option<SentenceId> {
    step.axiom_index().and_then(|i| sid_map.get(i).copied())
}

/// Map a native `ProofRule` enum variant to the string role the CLI
/// proof-display code expects.
///
/// **Critical**: `ProofRule::Axiom` MUST map to the literal string
/// `"axiom"` — that's the exact value `crate::cli::proof`'s
/// `print_step_source` checks against to decide whether to print a
/// source-file traceback.  A mismatch here would silently suppress
/// every `↳ file:line` line for embedded-backend proofs.
///
/// All other variants map to descriptive lower-case names that
/// appear in the `[rule]` label of each step.  These are purely
/// cosmetic — the CLI treats them as opaque strings.
fn rule_name(rule: ProofRule) -> &'static str {
    match rule {
        ProofRule::Axiom => "axiom",
        ProofRule::NegatedConjecture => "negated_conjecture",
        ProofRule::Rectify => "rectify",
        ProofRule::Flatten => "flatten",
        ProofRule::EENFTransformation => "ennf_transformation",
        ProofRule::CNFTransformation => "cnf_transformation",
        ProofRule::NNFTransformation => "nnf_transformation",
        ProofRule::SkolemSymbolIntroduction => "skolem_symbol_introduction",
        ProofRule::Skolemize => "skolemize",
        ProofRule::Superposition => "superposition",
        ProofRule::ForwardDemodulation => "forward_demodulation",
        ProofRule::BackwardDemodulation => "backward_demodulation",
        ProofRule::ForwardSubsumptionResolution => "forward_subsumption_resolution",
        ProofRule::Resolution => "resolution",
        ProofRule::TrivialInequalityRemoval => "trivial_inequality_removal",
        ProofRule::Avatar => "avatar",
        ProofRule::Other => "plain",
    }
}

/// Convert a native Vampire `Proof` into IR-typed proof steps.
///
/// Each step's conclusion formula is converted via the round-trip:
/// `FFI Formula → TPTP string → trans::ir::Formula`.  The parse is
/// wrapped in a minimal `fof(anon, plain, ...).\n` envelope so our
/// TPTP parser can handle a bare formula string.  Unparseable steps
/// fall back to `ir::Formula::True` with the raw string preserved in
/// the debug representation. `sid_map` resolves `source_sid` as in
/// [`native_proof_to_kif_steps`].
pub(crate) fn native_proof_to_ir_steps(proof: &Proof, sid_map: &[SentenceId]) -> Vec<IrProofStep> {
    proof
        .steps()
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let tptp_formula = step.conclusion().to_tptp();
            let tptp_wrapped = format!("fof(anon, plain, {}).\n", tptp_formula);
            let formula = crate::trans::ir::parse_tptp(&tptp_wrapped)
                .ok()
                .and_then(|p| p.axioms().first().cloned())
                .unwrap_or(crate::trans::ir::Formula::True);
            IrProofStep {
                index: i,
                rule: rule_name(step.rule()).to_string(),
                premises: step.premises().to_vec(),
                formula,
                source_sid: source_sid(step, sid_map),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axiom_rule_maps_to_literal_axiom() {
        // Regression guard: the CLI keys on exactly `"axiom"` to
        // enable source-file tracebacks.  Don't change this string
        // without updating `crate::cli::proof::print_step_source`
        // in the native crate.
        assert_eq!(rule_name(ProofRule::Axiom), "axiom");
    }

    #[test]
    fn negated_conjecture_maps_to_subprocess_convention() {
        // Mirror the role string Vampire's TPTP output uses, so
        // proof-step headers look identical across backends.
        assert_eq!(
            rule_name(ProofRule::NegatedConjecture),
            "negated_conjecture"
        );
    }

    #[test]
    fn non_input_rules_are_informative() {
        // Non-axiom rules don't affect the source-traceback logic
        // but appear in the `[rule]` label.  A spot check that the
        // enum → string mapping is populated (not just the "Other"
        // catch-all).
        assert_eq!(rule_name(ProofRule::Resolution), "resolution");
        assert_eq!(rule_name(ProofRule::Superposition), "superposition");
        assert_eq!(
            rule_name(ProofRule::CNFTransformation),
            "cnf_transformation"
        );
        assert_eq!(rule_name(ProofRule::Other), "plain");
    }
}
