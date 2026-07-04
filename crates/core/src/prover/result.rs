// crates/core/src/prover/result.rs
//
// Prover result types and handling

use std::{fmt, time::Duration};
use serde::{Deserialize, Serialize};

/// Why the prover stopped, when it stopped *without* a definitive
/// Proved/Disproved verdict.  Lets callers (notably the autoscaling loop)
/// tell a wall-clock/resource exhaustion (search space too big → *narrow*
/// the premise set) apart from a logical exhaustion (search saturated /
/// strategy gave up without a proof → likely *missing* premises → *widen*).
///
/// Backend-agnostic: each [`ProverRunner`] maps its own termination
/// markers here (Vampire's `% Termination reason:` line, etc.).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminationReason {
    /// Wall-clock time limit hit (Vampire `Time limit` / `SZS Timeout`).
    TimeLimit,
    /// Memory / other resource limit hit.
    ResourceOut,
    /// Saturation: the calculus ran to completion with no refutation — the
    /// conjecture does not follow from the *selected* axioms (Vampire emits
    /// this alongside `SZS CounterSatisfiable` for a complete strategy).
    Saturation,
    /// The prover stopped without a verdict for some other reason — an
    /// incomplete strategy gave up, all clauses discarded, etc.
    GaveUp,
    /// A reason was reported but didn't match any of the above.
    Other,
}

/// Per-query timing breakdown, populated on every call.
#[derive(Debug, Clone, Default)]
pub struct ProverTimings {
    /// Time spent building the theorem-prover input (TPTP string or native Problem).
    pub input_gen:    Duration,
    /// Time spent inside the theorem prover itself.
    pub prover_run:   Duration,
    /// Time spent parsing the prover output / extracting bindings.
    pub output_parse: Duration,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProverResult {
    /// `Some(true)` when a Saturated verdict ran with NO capacity
    /// discards (depth / width / literal-count caps): the saturation
    /// genuinely exhausted the loaded theory.  Under
    /// `Strategy.strict_saturation` the bar rises to refutation-
    /// completeness proper (full saturation over the whole theory, no
    /// generation cap hit, complete equality calculus when equality is
    /// present).  `Some(false)` when any of that failed (the verdict
    /// means "no proof found", not "countermodel exists").  `None` for
    /// non-saturation outcomes.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub complete_saturation: Option<bool>,
    /// Given-clause steps the native saturation loop executed —
    /// deterministic and machine-independent, so it is the sweep /
    /// benchmark objective of choice (wall time only tie-breaks).
    /// `None` for subprocess backends and synthesized results.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub given_steps: Option<usize>,
    pub status:     ProverStatus,
    pub raw_output: String,
    /// Why the prover stopped, when no definitive verdict was reached.
    /// `None` for definitive Proved/Disproved/Consistent/Inconsistent
    /// results and for backends that don't report a reason.
    #[serde(default)]
    pub termination: Option<TerminationReason>,
    pub bindings:   Vec<Binding>,
    /// Proof steps converted to SUO-KIF, populated when a proof is found.
    pub proof_kif:  Vec<crate::prover::proof::KifProofStep>,
    /// Proof steps as structured IR formulas — backend-agnostic output
    /// from both the subprocess and embedded paths.  Empty when no proof
    /// was produced.  KIF steps can be derived from these via
    /// `step.formula.to_tptp()` → `proof::formula_to_kif(...)`.
    #[serde(skip)]
    pub ir_proof:   Vec<crate::prover::proof::IrProofStep>,
    /// Raw TSTP proof section as emitted by Vampire (the text between
    /// `SZS output start` and `SZS output end`, minus the markers
    /// themselves).  Empty when no proof was produced.  Preserved so
    /// the `--proof tptp` CLI format can display the prover's output
    /// verbatim without re-parsing `raw_output`.
    #[serde(default)]
    pub proof_tptp: String,
    /// The TPTP language a *generic* (`--proof tptp`) proof dump should use —
    /// mirrored from the input problem's dialect (`cnf`/`fof`/`tff`), defaulting
    /// to `Fof` for non-TPTP inputs (KIF/TQ).  Set by [`KnowledgeBase::solve_tptp`];
    /// `Fof` elsewhere.  Derived, not serialized.
    #[serde(skip)]
    pub proof_tptp_lang: crate::parse::dialect::TptpLang,
    /// Per-phase timing breakdown (not serialized).
    #[serde(skip)]
    pub timings:    ProverTimings,
    /// Named sub-phase durations from inside the prover (native
    /// backend's saturation-loop mechanisms; empty unless the run was
    /// profiled).  Coarser pipeline phases flow through the progress
    /// sink instead — this carries only what no sink span can see.
    #[serde(skip)]
    pub phase_profile: Vec<(String, std::time::Duration)>,
    /// Proof transcripts of INPUT contradictions discovered (and
    /// suppressed) during the run — the axioms/hypotheses derive ⊥
    /// without the conjecture's involvement.  Each is a complete
    /// citable derivation; empty when the inputs were consistent as
    /// far as the search saw.  (Native backend; not serialized.)
    #[serde(skip)]
    pub contradiction_proofs: Vec<Vec<crate::prover::proof::KifProofStep>>,
}

impl ProverResult {
    /// Input-completeness gate, caller side: `failures` input formulas never
    /// made it into the prover's clause set (assembly drop, staging error,
    /// load failure).  A missing input can only HIDE a refutation, never
    /// fabricate one — so a Proved/Inconsistent verdict stands, but a
    /// confident "no" (Disproved / Consistent) is demoted to Unknown/GaveUp
    /// with a loud reason, and `complete_saturation` is forced off so no
    /// downstream consumer reads the run as a certified countermodel.
    pub fn withhold_countermodel(&mut self, failures: usize, why: &str) {
        if failures == 0 {
            return;
        }
        self.raw_output.push_str(&format!(
            "\nWARNING: {failures} input formula(s) failed to load ({why}) — \
             Satisfiable/countermodel verdicts withheld (GaveUp)"));
        if self.complete_saturation == Some(true) {
            self.complete_saturation = Some(false);
        }
        if matches!(self.status, ProverStatus::Disproved | ProverStatus::Consistent) {
            self.status      = ProverStatus::Unknown;
            self.termination = Some(TerminationReason::GaveUp);
            self.complete_saturation = Some(false);
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProverStatus {
    Proved,
    Disproved,
    Consistent,
    Inconsistent,
    Timeout,
    /// The prover **rejected the input as malformed** before reaching any
    /// verdict — a parse/syntax error or a type-check error (e.g. an
    /// ill-typed TFF problem).  This is distinct from [`Unknown`], which
    /// means the prover accepted the input, ran, and simply reached no
    /// conclusion (incompleteness, gave up, etc.).
    ///
    /// Backend-agnostic: every [`ProverRunner`] implementation should map
    /// its "could not consume the problem" failure here (Vampire's
    /// `User error: …`, E's `SZS status SyntaxError`, a native lowering
    /// failure, …).  Surfacing it as its own status lets callers print
    /// the actionable diagnostic instead of a generic "gave up", and
    /// lets consistency checks treat it as uncertain rather than
    /// silently "consistent".
    ///
    /// [`Unknown`]: ProverStatus::Unknown
    InputError,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Binding {
    pub variable: String,
    pub value:    String,
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} = {}", self.variable, self.value)
    }
}