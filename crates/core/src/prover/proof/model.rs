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

use crate::parse::ast::AstNode;

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
    /// when `None` — the hash path is robust to alpha-renaming and
    /// quantifier-normalisation but requires a whole-KB scan.
    ///
    /// Serialisation: `#[serde(default, skip_serializing_if = …)]`
    /// keeps the JSON wire format compatible — old consumers that
    /// don't know about this field deserialize it as `None`; new
    /// consumers omit the field from the output when it's `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sid: Option<crate::types::SentenceId>,
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
