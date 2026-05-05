//! Output of [`crate::TestOp::run`].

use sigmakee_rs_core::prover::ProverTimings;

/// Findings from a batch test run.
#[derive(Debug, Default)]
pub struct TestSuiteReport {
    /// Per-case results in the order the cases were run.
    pub cases: Vec<TestCaseReport>,

    /// Cases whose actual outcome matched the expectation in the
    /// `.kif.tq` `(answer ...)` directive.
    pub passed: usize,

    /// Cases that ran to completion but mismatched expectation, or
    /// that errored out before the prover could decide.
    pub failed: usize,

    /// Cases that couldn't be run at all (bad input, missing query).
    /// Not failures *per se* — they didn't get a verdict either way.
    pub skipped: usize,
}

impl TestSuiteReport {
    /// `true` iff every case passed and nothing was skipped.
    pub fn all_passed(&self) -> bool {
        self.failed == 0 && self.skipped == 0 && self.passed == self.cases.len()
    }
}

/// One test case's result.
#[derive(Debug, Clone)]
pub struct TestCaseReport {
    /// Display name for the case.  Pulled from the `(note ...)`
    /// directive in the `.kif.tq` file when present, otherwise the
    /// source tag (path or caller-supplied label).
    pub name: String,

    /// Source tag the case was loaded from.  For file-driven cases
    /// this is the path's display string; for inline cases it's
    /// whatever the caller passed.
    pub tag: String,

    /// What happened.
    pub outcome: TestOutcome,

    /// Per-phase timing for the proof attempt.  Zero `Duration` when
    /// the case was skipped before reaching the prover.
    pub timings: ProverTimings,
}

/// The classification of one test result.
#[derive(Debug, Clone)]
pub enum TestOutcome {
    /// `(answer yes/no)` matched the prover's verdict, and any
    /// `(answer X Y)` expected bindings were all present.
    Passed,

    /// Verdict mismatch — the prover proved/disproved opposite to
    /// what the test file declared.
    Failed {
        /// Verdict the test file declared via `(answer yes|no)`.
        expected: bool,
        /// Verdict the prover actually returned (`true` if proved).
        got:      bool,
    },

    /// The prover proved the conjecture but only some expected
    /// bindings were inferred.  `missing` lists the bindings that
    /// were declared in `(answer ...)` but not produced.
    Incomplete {
        /// Bindings the prover did infer (one entry per binding
        /// produced).
        inferred: Vec<String>,
        /// Bindings declared in `(answer ...)` that the prover did
        /// NOT produce.
        missing:  Vec<String>,
    },

    /// Parse failure on the `.kif.tq` content itself.
    ParseError(String),

    /// Semantic validation failed on the case's axioms before the
    /// prover was invoked.
    SemanticError(String),

    /// The prover errored out before producing a verdict.
    ProverError(String),

    /// The case had no `(query ...)` directive — nothing to ask.
    NoQuery,
}
