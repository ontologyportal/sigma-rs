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

#[cfg(feature = "external-prover")]
use super::proof::proof_steps_to_kif;
use super::proof::{proof_steps_to_kif_ast, KifProofStep};
use super::result::{Binding, ProverMode, ProverStatus};
#[cfg(feature = "external-prover")]
use super::result::{ProverResult, ProverTimings, TerminationReason};
#[cfg(feature = "external-prover")]
use super::tptp_proof::proof_steps_to_ir;
use super::tptp_proof::{kif_proof_inputs, ProofStep, TptpProofProcessor};

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
            Some("Unsatisfiable") | Some("Theorem") | Some("ContradictoryAxioms") => {
                ProverStatus::Inconsistent
            }
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
        Role::Axiom => "axiom",
        Role::Hypothesis => "hypothesis",
        Role::Definition => "definition",
        Role::Lemma => "lemma",
        Role::Conjecture => "conjecture",
        Role::NegatedConjecture => "negated_conjecture",
        Role::Plain => "plain",
        Role::Type => "type",
        Role::Other(word) => word,
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
            AstNode::Annotated {
                role,
                name: Some(id),
                source,
                formula,
                ..
            } => Some(ProofStep {
                id: id.clone(),
                role: role_str(role).to_string(),
                formula: crate::parse::tptp::dis::tptp_formula(formula),
                parents: match source {
                    Some(Source::Inference { parents, .. }) => parents.clone(),
                    _ => Vec::new(),
                },
                rule: match source {
                    Some(Source::Inference { rule, .. }) => Some(rule.clone()),
                    _ => None,
                },
                // Only our own `kb_<sid>` naming convention is meaningful
                // here — Vampire builds without `--output_axiom_names` (or
                // older ones) emit `file('/dev/stdin', unknown)`, which must
                // stay `None` so the canonical-hash fallback path takes over
                // downstream (matching `ProofStep::source_name`'s documented
                // contract), not read as a literal axiom name "unknown".
                source_name: match source {
                    Some(Source::Input { name: Some(n), .. }) if n.starts_with("kb_") => {
                        Some(n.clone())
                    }
                    _ => None,
                },
            }),
            _ => None,
        })
        .collect()
}

// -- Transcript -> ProverResult -------------------------------------------------

/// Everything a Vampire runner does after capturing the CLI's stdout and
/// stderr: SZS classification, the Theorem-vs-ContradictoryAxioms
/// correction, answer-binding extraction, both proof lowerings, and the
/// termination reason the autoscaling loop reads.  Shared by every runner
/// that drives Vampire as a TPTP-text CLI -- the subprocess runner and the
/// browser bridge to the Emscripten build -- so their results are identical
/// for identical transcripts.  `prover_run` is the wall-clock time the caller
/// measured around the run.
#[cfg(feature = "external-prover")]
pub fn result_from_transcript(
    stdout: &str,
    stderr: &str,
    mode: ProverMode,
    prover_run: std::time::Duration,
) -> ProverResult {
    let t_parse = crate::clock::Instant::now();
    let combined = format!("{}{}", stdout, stderr);

    let (doc, _parse_errors) = parse_szs(&combined, "vampire");
    let status_word = szs_status_word(&doc);
    let status = determine_status(&combined, status_word, &mode);
    let parsed_steps = docitems_to_proof_steps(&doc);
    let has_proof = !parsed_steps.is_empty();
    // Distinguish a genuine Theorem from ContradictoryAxioms that Vampire
    // mislabels: some schedules report `SZS status Theorem` even when the
    // refutation never used the negated conjecture (the axioms alone derive
    // bottom -- SUMO carries known inconsistencies).  A proof without a
    // negated-conjecture STEP (checked on the parsed roles, not the raw
    // text -- the substring can appear in echoed input or schedule chatter)
    // is an Inconsistent verdict, not a Proved one.
    let status = if matches!(mode, ProverMode::Prove)
        && matches!(status, ProverStatus::Proved)
        && has_proof
        && !parsed_steps.iter().any(|s| s.role == "negated_conjecture")
    {
        ProverStatus::Inconsistent
    } else {
        status
    };
    crate::log!(
        Info,
        "sigmakee_rs_core::prover",
        format!("vampire result: {:?}", status_label(&status))
    );

    // Surface Vampire's `User error: ...` (parse/type-check) detail that
    // would otherwise be buried in raw_output behind `Unknown`.
    if matches!(status, ProverStatus::InputError) {
        let detail = extract_input_error(&combined).unwrap_or_else(|| combined.trim().to_string());
        crate::log!(
            Warn,
            "sigmakee_rs_core::prover",
            format!("vampire rejected the input: {}", detail)
        );
    }

    // Only extract bindings when Vampire proved the conjecture via a genuine
    // refutation (SZS Theorem).  ContradictoryAxioms / Unsatisfiable proofs
    // derive contradiction purely from the axioms and carry no
    // negated-conjecture steps, so there are no variable bindings to find.
    let bindings = if matches!(mode, ProverMode::Prove) && status_word == Some("Theorem") {
        let mut proc = TptpProofProcessor::new();
        proc.load_proof(&parsed_steps);
        proc.extract_answers()
    } else {
        Vec::new()
    };

    // Preserve the raw SZS proof section verbatim so the `--proof tptp` CLI
    // path can emit Vampire's output without re-parsing.
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
        proof_steps_to_kif(&kif_proof_inputs(&parsed_steps))
    } else {
        Vec::new()
    };
    let ir_proof = if has_proof {
        proof_steps_to_ir(&parsed_steps)
    } else {
        Vec::new()
    };

    let output_parse = t_parse.elapsed();
    let termination = extract_termination_reason(&combined);
    ProverResult {
        complete_saturation: None,
        given_steps: None,
        phase_profile: Vec::new(),
        contradiction_proofs: Vec::new(),
        status,
        raw_output: combined,
        termination,
        bindings,
        proof_kif,
        ir_proof,
        proof_tptp,
        proof_tptp_lang: crate::parse::dialect::TptpLang::default(),
        timings: ProverTimings {
            prover_run,
            output_parse,
            ..Default::default()
        },
    }
}

/// Classify *why* Vampire stopped, for the autoscaling loop.  Parses the
/// `% Termination reason:` tail line (and a couple of phase banners),
/// mapping to a backend-agnostic [`TerminationReason`].
///
/// Wall-clock / resource exhaustion (`Time limit`, `Memory limit`,
/// `Refutation not found, ...`) signals an over-large premise set -> narrow.
/// A clean `Saturation` or a `CounterSatisfiable` verdict signals the
/// conjecture isn't entailed by the *selected* axioms -> widen.  Returns
/// `None` when no termination marker is present (e.g. a clean proof).
#[cfg(feature = "external-prover")]
pub(crate) fn extract_termination_reason(output: &str) -> Option<TerminationReason> {
    // A successful refutation isn't a "stopped without a verdict" case.
    if output.contains("SZS status Theorem")
        || output.contains("SZS status Unsatisfiable")
        || output.contains("SZS status ContradictoryAxioms")
        || output.contains("Termination reason: Refutation")
    {
        return None;
    }
    if is_timeout(output) {
        return Some(TerminationReason::TimeLimit);
    }
    if output.contains("Memory limit")
        || output.contains("Refutation not found, incomplete strategy")
        || output.contains("Refutation not found, non-redundant clauses discarded")
    {
        return Some(TerminationReason::ResourceOut);
    }
    if output.contains("Termination reason: Satisfiable")
        || output.contains("Termination reason: Saturation")
        || output.contains("SZS status CounterSatisfiable")
        || output.contains("SZS status Satisfiable")
    {
        return Some(TerminationReason::Saturation);
    }
    if output.contains("SZS status GaveUp") || output.contains("Termination reason:") {
        return Some(TerminationReason::GaveUp);
    }
    None
}

/// Pull out the single most informative line of a Vampire input-error
/// report for logging.  Prefers the `User error:` line (and the line
/// after it, which carries the sort/term detail); falls back to the
/// first line mentioning an error.
#[cfg(feature = "external-prover")]
pub(crate) fn extract_input_error(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.contains("User error")
            || line.contains("Parse error")
            || line.contains("Syntax error")
        {
            // Vampire's type errors span two lines: the `User error:`
            // header and a follow-up describing the offending sort/term.
            let mut msg = line.trim().to_string();
            if let Some(next) = lines.get(i + 1) {
                let next = next.trim();
                if !next.is_empty() && !next.starts_with('%') {
                    msg.push(' ');
                    msg.push_str(next);
                }
            }
            return Some(msg);
        }
    }
    None
}

#[cfg(feature = "external-prover")]
fn status_label(s: &ProverStatus) -> &'static str {
    match s {
        ProverStatus::Proved => "Proved",
        ProverStatus::Disproved => "Disproved",
        ProverStatus::Consistent => "Consistent",
        ProverStatus::Inconsistent => "Inconsistent",
        ProverStatus::Timeout => "Timeout",
        ProverStatus::InputError => "InputError",
        ProverStatus::Unknown => "Unknown",
    }
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
    pub status: ProverStatus,
    pub proof: Vec<KifProofStep>,
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
            .map(|((_, role, premises, source_name), formula)| {
                (formula, role, premises, source_name)
            })
            .collect();
        proof_steps_to_kif_ast(&inputs)
    } else {
        Vec::new()
    };

    VampireProofResult {
        status,
        proof,
        bindings,
    }
}

/// The parsed formula `AstNode` of each named statement in `doc`, in the same
/// order [`docitems_to_proof_steps`] visits them — pairs with
/// [`kif_proof_inputs`]'s per-step tuples by position.
fn docitems_to_ast_formulas(doc: &[DocItem]) -> Vec<AstNode> {
    doc.iter()
        .filter_map(DocItem::as_stmt)
        .filter_map(|node| match node {
            AstNode::Annotated {
                name: Some(_),
                formula,
                ..
            } => Some((**formula).clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::kif::dis::AstKif;

    #[test]
    fn derived_steps_are_labeled_by_their_inference_rule_not_plain() {
        let raw = "\
% SZS status Theorem for input
% SZS output start Proof for input
fof(f1,axiom,(
  s__instance(s__Rex,s__Dog)),
  file('/work/input.p',kb_1)).
fof(f2,conjecture,(
  s__instance(s__Rex,s__Dog)),
  file('/work/input.p',conjecture)).
fof(f3,negated_conjecture,(
  ~s__instance(s__Rex,s__Dog)),
  inference(negated_conjecture,[],[f2])).
fof(f4,plain,(
  ~s__instance(s__Rex,s__Dog)),
  inference(cnf_transformation,[],[f3])).
fof(f5,plain,(
  $false),
  inference(resolution,[],[f1,f4])).
% SZS output end Proof for input
";
        let result = parse_vampire_result(raw, ProverMode::Prove);
        let rules: Vec<&str> = result.proof.iter().map(|s| s.rule.as_str()).collect();
        assert_eq!(
            rules,
            vec![
                "axiom",
                "conjecture",
                "negated_conjecture",
                "cnf_transformation",
                "resolution"
            ]
        );
        assert!(
            !rules.contains(&"plain"),
            "a derived step must never surface the bare TPTP role: {rules:?}"
        );
    }

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
