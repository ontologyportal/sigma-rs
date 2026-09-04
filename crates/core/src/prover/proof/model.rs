// crates/core/src/prover/proof/model.rs
//
// The proof-step data model — the backend-agnostic vocabulary every prover
// (native `saturate` + external subprocess) produces and downstream consumers
// (CLI, SDK, prose/NL rendering) read.  Two representations:
//
//   * `KifProofStep` — the formula as a KIF `AstNode`, ready for pretty-printing
//     and dialect emission (see `super::emit`);
//   * `IrProofStep`  — the formula as a structured `trans::ir::Formula`, for
//     consumers that inspect proof structure without re-parsing.
//
// Plus `parse_kb_axiom_name`, the `kb_<sid>` source-name decoder both paths use
// to recover a step's originating `SentenceId`.

use crate::parse::{ast::AstNode, Role, Source};

/// One step of a proof carrying a structured IR formula — backend-agnostic.
///
/// Produced by both the subprocess path (via TPTP round-trip through
/// [`crate::trans::ir::parse_tptp`]) and the embedded path (via FFI proof
/// extraction in [`crate::prover::vampire::native_proof`]).  The IR representation
/// allows downstream consumers to inspect proof structure without re-parsing
/// KIF or TPTP strings.
#[derive(Debug, Clone)]
pub struct IrProofStep {
    /// Position in the proof (0-based).
    pub index: usize,
    /// Human-readable rule name (e.g. "Axiom", "Resolution").
    pub rule: String,
    /// Indices of the premises this step was derived from.
    pub premises: Vec<usize>,
    /// The formula for this step as a structured IR node.
    pub formula: crate::trans::ir::Formula,
    /// Source [`crate::SentenceId`] when this step traces back to an input
    /// axiom whose name matches our `kb_<sid>` convention.
    pub source_sid: Option<crate::types::SentenceId>,
}

/// One step of a proof rendered in SUO-KIF.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct KifProofStep {
    /// Position in the proof (0-based).
    pub index: usize,
    /// Human-readable rule name (e.g. "Axiom", "Resolution").
    pub rule: String,
    /// Indices of the premises this step was derived from.
    pub premises: Vec<usize>,
    /// The formula for this step as a KIF AST, ready for pretty-printing.
    pub formula: AstNode,
    /// Source [`crate::SentenceId`] when this step traces directly back to an
    /// input axiom whose name Vampire preserved (requires
    /// `--output_axiom_names on`).  `None` for derived steps, for
    /// older Vampire builds, and for anonymous axioms.  Downstream
    /// consumers (e.g. proof-display in the CLI) should prefer this
    /// for O(1) source lookup when present and fall back to the
    /// canonical-hash path on [`crate::axiom_source::AxiomSourceIndex`]
    /// when `None`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sid: Option<crate::types::SentenceId>,
}

impl KifProofStep {
    pub(crate) fn to_ast(&self, problem: &str) -> AstNode {
        let role = Role::from_str_plain(&self.rule.to_lowercase());
        let source = if self.premises.is_empty() {
            match self.rule.as_str() {
                "negated_conjecture" => Source::Inference {
                    rule: "negate_conjecture".into(),
                    parents: Vec::new(),
                },
                // Genuine inputs cite the problem; any other premise-less
                // step is prover-synthesized (subrel_schema, list_theory,
                // modal_k, …) and must not masquerade as a stated axiom.
                "axiom" | "hypothesis" | "conjecture" => Source::Input {
                    file: problem.to_string(),
                    name: None,
                },
                other => Source::Introduced(other.to_string()),
            }
        } else {
            // The negated conjecture cites the `negate_conjecture` inference
            // (whose TPTP status is `cth`, see `render_source`) over its
            // conjecture parent — not its own role word as a rule name.
            let rule = if self.rule == "negated_conjecture" {
                "negate_conjecture".to_string()
            } else {
                self.rule.clone()
            };
            Source::Inference {
                rule,
                parents: self
                    .premises
                    .iter()
                    .map(|p| format!("f{}", p + 1))
                    .collect(),
            }
        };
        AstNode::Annotated {
            role,
            name: Some(format!("f{}", self.index + 1)),
            source: Some(source),
            formula: Box::new(self.formula.clone()),
            span: crate::parse::Span::synthetic(),
        }
    }
}

/// Parse an axiom name of the form `"kb_<digits>"` into a
/// [`crate::SentenceId`](crate::types::SentenceId).  Anything else —
/// including `"kb_anon_0"` and names from other prover conventions —
/// returns `None`.
pub(crate) fn parse_kb_axiom_name(name: &str) -> Option<crate::types::SentenceId> {
    let body = name.strip_prefix("kb_")?;
    // Variant-expansion copies are named `kb_<sid>_v<n>` — same origin sid.
    let body = body.split("_v").next().unwrap_or(body);
    body.parse().ok()
}
