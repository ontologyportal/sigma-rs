// crates/sumo-kb/src/vampire/native_proof.rs
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
// `source_sid` is left as `None`: unlike the subprocess path, the
// embedded prover doesn't preserve our `kb_<sid>` names (Vampire's
// `--output_axiom_names` only applies to TPTP parsing, not the
// in-process API).  Downstream `print_step_source` falls back to the
// canonical-fingerprint lookup in `AxiomSourceIndex`, which is
// alpha-equivalence tolerant.  Future work could build a per-call
// `tptp_string -> SentenceId` map from the filtered IR problem and
// resolve axiom-role steps directly.
//
// Gated on `integrated-prover` because the native proof type only
// exists when the embedded FFI backend is compiled in.

#![cfg(feature = "integrated-prover")]

use vampire_prover::{Proof, ProofRule};

use crate::tptp::kif::{formula_to_ast, KifProofStep};

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
pub(crate) fn native_proof_to_kif_steps(proof: &Proof) -> Vec<KifProofStep> {
    proof.steps().iter().enumerate().map(|(i, step)| {
        let tptp = step.conclusion().to_tptp();
        let formula = formula_to_ast(&tptp).unwrap_or_else(|| {
            crate::parse::ast::AstNode::Symbol {
                name: format!("; [unparseable] {}", tptp),
                span: crate::parse::ast::Span::point(String::new(), 0, 0, 0),
            }
        });
        KifProofStep {
            index:      i,
            rule:       rule_name(step.rule()).to_string(),
            premises:   step.premises().to_vec(),
            formula,
            // Embedded path has no preserved `kb_<sid>` name — the
            // CLI's print_step_source falls back to canonical-hash
            // lookup, which is correct though slower than the sid
            // direct path.
            source_sid: None,
        }
    }).collect()
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
        ProofRule::Axiom                        => "axiom",
        ProofRule::NegatedConjecture            => "negated_conjecture",
        ProofRule::Rectify                      => "rectify",
        ProofRule::Flatten                      => "flatten",
        ProofRule::EENFTransformation           => "ennf_transformation",
        ProofRule::CNFTransformation            => "cnf_transformation",
        ProofRule::NNFTransformation            => "nnf_transformation",
        ProofRule::SkolemSymbolIntroduction     => "skolem_symbol_introduction",
        ProofRule::Skolemize                    => "skolemize",
        ProofRule::Superposition                => "superposition",
        ProofRule::ForwardDemodulation          => "forward_demodulation",
        ProofRule::BackwardDemodulation         => "backward_demodulation",
        ProofRule::ForwardSubsumptionResolution => "forward_subsumption_resolution",
        ProofRule::Resolution                   => "resolution",
        ProofRule::TrivialInequalityRemoval     => "trivial_inequality_removal",
        ProofRule::Avatar                       => "avatar",
        ProofRule::Other                        => "plain",
    }
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
        assert_eq!(rule_name(ProofRule::NegatedConjecture), "negated_conjecture");
    }

    #[test]
    fn non_input_rules_are_informative() {
        // Non-axiom rules don't affect the source-traceback logic
        // but appear in the `[rule]` label.  A spot check that the
        // enum → string mapping is populated (not just the "Other"
        // catch-all).
        assert_eq!(rule_name(ProofRule::Resolution),     "resolution");
        assert_eq!(rule_name(ProofRule::Superposition),  "superposition");
        assert_eq!(rule_name(ProofRule::CNFTransformation), "cnf_transformation");
        assert_eq!(rule_name(ProofRule::Other),          "plain");
    }
}
