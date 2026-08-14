// crates/core/src/prover/vampire_proof.rs
//
// Status classification + proof-step extraction for a captured Vampire
// transcript, lowered to this crate's shared `KifProofStep` proof-graph
// vocabulary. NO subprocess spawning here (no `std::process`, no
// `std::fs`) — everything operates on an already-captured `&str`, so this
// compiles on wasm32.
//
// The proof-step extraction itself is `parse::szs::parse_szs` — real TPTP
// grammar (reusing `parse::tptp`), not text scraping. This module's own job
// is the thin layer on top: pull the SZS status word and proof steps out of
// `parse_szs`'s `Vec<DocItem>`, adapt them to the `ProofStep` shape the
// existing (backend-agnostic) `proof::tstp` machinery already consumes
// (`kif_proof_inputs`, `TptpProofProcessor`), and apply the
// Theorem-without-a-negated-conjecture → Inconsistent correction.
//
// This is the parsing half of what `external`'s `ask`-gated
// `vampire::subprocess::VampireRunner` also uses: that backend spawns the
// `vampire` binary as a subprocess (impossible on wasm32) and then calls
// straight into functions defined here (`is_timeout`/`is_input_error`) to
// classify its stdout. A standalone Vampire build compiled to WASM has no
// subprocess to spawn — its stdout is captured directly by the caller (e.g.
// the wasm crate's JS glue) — so [`parse_vampire_result`] is the equivalent
// entry point for that path, producing the same status + proof-graph shape.

use crate::parse::ast::{AstNode, Role, Source};
use crate::parse::doc::DocItem;
use crate::parse::szs::parse_szs;

use super::proof::{KifProofStep, proof_steps_to_kif_ast};
use super::result::{Binding, ProverMode, ProverStatus};
use super::tptp_proof::{ProofStep, TptpProofProcessor, kif_proof_inputs};

// -- Status classification -----------------------------------------------------

/// `true` if Vampire's output signals that the run terminated because
/// it ran out of time.  Three markers are checked because Vampire 5.x
/// emits them in different combinations depending on which phase the
/// time-out hit (preprocessing vs saturation vs proof-search). These are
/// NOT part of the SZS/TPTP proof-step vocabulary `parse_szs` understands —
/// they're free-form banner text — so this stays a raw scan.
pub(crate) fn is_timeout(output: &str) -> bool {
    output.contains("SZS status Timeout")
        || output.contains("Termination reason: Time limit")
        || output.contains("Time limit reached")
}

/// `true` if Vampire's output signals that it could not consume the
/// problem — a parse/syntax error or a type-check failure — as opposed
/// to running and reaching no verdict. Vampire emits these on stderr with
/// no `%` prefix (`User error: …`), so — unlike the SZS status line —
/// there's no structured pragma for `parse_szs` to recognize here either.
pub(crate) fn is_input_error(output: &str) -> bool {
    output.contains("User error")
        || output.contains("Parse error")
        || output.contains("Syntax error")
        || output.contains("SZS status SyntaxError")
        || output.contains("SZS status TypeError")
}

/// Classify a transcript into this crate's backend-agnostic [`ProverStatus`].
/// `status_word` is the SZS status (`Some("Theorem")`, `Some("ContradictoryAxioms")`,
/// …) already extracted from `parse_szs`'s `Meta` items — `None` when no `%
/// SZS status …` line was found at all. `output` is still needed for the
/// timeout/input-error banner checks above, which aren't part of the SZS
/// vocabulary.
pub(crate) fn determine_status(
    output: &str,
    status_word: Option<&str>,
    mode: &ProverMode,
) -> ProverStatus {
    // An input rejection (parse/type error) is neither a Prove nor a
    // CheckConsistency verdict — the prover never actually ran on the
    // problem.  Check it first, before the mode-specific SZS markers, so
    // it can't be misread as `Unknown` (or, in consistency mode,
    // silently accepted as "consistent").
    if is_input_error(output) {
        return ProverStatus::InputError;
    }
    match mode {
        ProverMode::Prove => match status_word {
            Some("Theorem") | Some("Unsatisfiable") => ProverStatus::Proved,
            // The axiom set itself is contradictory; the conjecture was never
            // tested.  Report Inconsistent so the caller knows the KB is broken.
            Some("ContradictoryAxioms") => ProverStatus::Inconsistent,
            Some("CounterSatisfiable") => ProverStatus::Disproved,
            _ if is_timeout(output) => ProverStatus::Timeout,
            _ => ProverStatus::Unknown,
        },
        ProverMode::CheckConsistency => match status_word {
            Some("Satisfiable") | Some("CounterSatisfiable") => ProverStatus::Consistent,
            Some("Unsatisfiable") | Some("Theorem") | Some("ContradictoryAxioms") =>
                ProverStatus::Inconsistent,
            _ if is_timeout(output) => ProverStatus::Timeout,
            _ => ProverStatus::Unknown,
        },
    }
}

// -- DocItem -> ProofStep adapter -----------------------------------------------

/// The SZS status word (`Theorem`, `ContradictoryAxioms`, …), if `parse_szs`
/// found a `% SZS status …` line.
pub(crate) fn szs_status_word(doc: &[DocItem]) -> Option<&str> {
    doc.iter()
        .filter_map(DocItem::as_meta)
        .find(|m| m.key == "szs_status")
        .and_then(|m| m.args.first())
        .and_then(|a| match a {
            AstNode::Symbol { name, .. } => Some(name.as_str()),
            _ => None,
        })
}

/// This crate's TPTP role words, verbatim — matching what the raw-text
/// extraction this replaces used to capture directly (`ProofStep::role`,
/// and the `s.role == "negated_conjecture"` check downstream, are plain
/// string comparisons against these exact words).
fn role_str(role: &Role) -> &str {
    match role {
        Role::Axiom             => "axiom",
        Role::Hypothesis        => "hypothesis",
        Role::Definition        => "definition",
        Role::Lemma             => "lemma",
        Role::Conjecture        => "conjecture",
        Role::NegatedConjecture => "negated_conjecture",
        Role::Plain             => "plain",
        Role::Type              => "type",
        Role::Other(word)       => word,
    }
}

/// Adapt `parse_szs`'s `Vec<DocItem>` into the `ProofStep` shape
/// `proof::tstp`'s (backend-agnostic, already shared with the `ask`-gated
/// subprocess backends) `kif_proof_inputs`/`TptpProofProcessor` consume —
/// this is a narrow, lossy field-mapping (a `Meta` item has no `ProofStep`
/// representation at all; neither does an unnamed statement), not a general
/// `AstNode -> ProofStep` conversion, so it stays a plain function rather
/// than a `TryFrom` impl that would imply a broader contract than this one
/// proof-transcript-specific mapping actually has.
///
/// `pub(crate)`: shared with the `ask`-gated subprocess `VampireRunner`
/// (same backend, different transport — spawns the binary instead of
/// running it as WASM), which uses `parse::szs::parse_szs` + this adapter
/// the same way this module's own [`parse_vampire_result`] does.
pub(crate) fn docitems_to_proof_steps(doc: &[DocItem]) -> Vec<ProofStep> {
    doc.iter()
        .filter_map(DocItem::as_stmt)
        .filter_map(|node| match node {
            AstNode::Annotated { role, name: Some(id), source, formula, .. } => Some(ProofStep {
                id:      id.clone(),
                role:    role_str(role).to_string(),
                formula: crate::parse::tptp::dis::tptp_formula(formula),
                parents: match source {
                    Some(Source::Inference { parents, .. }) => parents.clone(),
                    _ => Vec::new(),
                },
                // Only our own `kb_<sid>` naming convention is meaningful
                // here — Vampire builds without `--output_axiom_names` (or
                // older ones) emit `file('/dev/stdin', unknown)`, which must
                // stay `None` so the canonical-hash fallback path takes over
                // downstream (matching `ProofStep::source_name`'s documented
                // contract), not read as a literal axiom name "unknown".
                source_name: match source {
                    Some(Source::Input { name: Some(n), .. }) if n.starts_with("kb_") => Some(n.clone()),
                    _ => None,
                },
            }),
            _ => None,
        })
        .collect()
}

// -- Public entry point ---------------------------------------------------------

/// A Vampire transcript parsed the same way the native `ask`-gated
/// subprocess backend parses one: SZS status (with the same
/// Theorem-without-a-negated-conjecture → Inconsistent correction the
/// subprocess backend applies) plus the proof lowered to this crate's shared
/// [`KifProofStep`] proof-graph vocabulary — the same type the native
/// saturation prover's proofs use, so callers can render both through one
/// code path (`render_graphviz`, `render_proof_prose_with`, …).
pub struct VampireProofResult {
    pub status:   ProverStatus,
    pub proof:    Vec<KifProofStep>,
    pub bindings: Vec<Binding>,
}

/// Parse a captured Vampire run's combined stdout+stderr into a
/// [`VampireProofResult`]. `mode` selects Prove-vs-CheckConsistency SZS
/// classification, mirroring [`ProverOpts::mode`](crate::prover::ProverOpts)
/// on the native subprocess path.
pub fn parse_vampire_result(raw_output: &str, mode: ProverMode) -> VampireProofResult {
    let (doc, _errors) = parse_szs(raw_output, "vampire");
    let status_word = szs_status_word(&doc);
    let status = determine_status(raw_output, status_word, &mode);

    let proof_steps = docitems_to_proof_steps(&doc);
    let has_proof = !proof_steps.is_empty();

    // Distinguish a genuine Theorem from ContradictoryAxioms that Vampire
    // mislabels: some schedules report `SZS status Theorem` even when the
    // refutation never used the negated conjecture (the axioms alone derive
    // ⊥ — SUMO carries known inconsistencies). A proof without a
    // negated-conjecture STEP (checked on the parsed roles, not the raw
    // text — the substring can appear in echoed input or schedule chatter)
    // is an Inconsistent verdict, not a Proved one.
    let status = if matches!(mode, ProverMode::Prove)
        && matches!(status, ProverStatus::Proved)
        && has_proof
        && !proof_steps.iter().any(|s| s.role == "negated_conjecture")
    {
        ProverStatus::Inconsistent
    } else {
        status
    };

    // Only extract bindings when Vampire proved the conjecture via a
    // genuine refutation (SZS Theorem). ContradictoryAxioms/Unsatisfiable
    // proofs derive contradiction purely from the axioms and carry no
    // negated-conjecture steps, so there are no variable bindings to find.
    let bindings = if matches!(mode, ProverMode::Prove) && status_word == Some("Theorem") {
        let mut proc = TptpProofProcessor::new();
        proc.load_proof(&proof_steps);
        proc.extract_answers()
    } else {
        Vec::new()
    };

    let proof = if has_proof {
        let inputs = kif_proof_inputs(&proof_steps);
        let formulas = docitems_to_ast_formulas(&doc);
        let inputs: Vec<_> = inputs
            .into_iter()
            .zip(formulas)
            .map(|((_, role, premises, source_name), formula)| (formula, role, premises, source_name))
            .collect();
        proof_steps_to_kif_ast(&inputs)
    } else {
        Vec::new()
    };

    VampireProofResult { status, proof, bindings }
}

/// The parsed formula `AstNode` of each named statement in `doc`, in the same
/// order [`docitems_to_proof_steps`] visits them — pairs with
/// [`kif_proof_inputs`]'s per-step tuples by position.
fn docitems_to_ast_formulas(doc: &[DocItem]) -> Vec<AstNode> {
    doc.iter()
        .filter_map(DocItem::as_stmt)
        .filter_map(|node| match node {
            AstNode::Annotated { name: Some(_), formula, .. } => Some((**formula).clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::kif::dis::AstKif;

    #[test]
    fn proof_step_formulas_render_as_kif_not_quoted_tptp() {
        let raw = "\
% SZS status Theorem for input
% SZS output start Proof for input
fof(f1,axiom,(
  s__instance(s__organizationalObjective,s__BinaryPredicate)),
  file('/work/input.p',kb_1)).
fof(f2,axiom,(
  $false),
  inference(resolution,[],[f1])).
% SZS output end Proof for input
";
        let result = parse_vampire_result(raw, ProverMode::Prove);
        let rendered: Vec<String> = result.proof.iter().map(|s| s.formula.flat()).collect();
        assert_eq!(
            rendered,
            vec![
                "(instance organizationalObjective BinaryPredicate)".to_string(),
                "False".to_string(),
            ]
        );
    }
}
