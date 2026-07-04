// crates/sdk/src/session/ask.rs
//
// Proving ops on a `Session`: ask / tell+ask / audit / check / test.  Every one
// funnels into the core primitive `KnowledgeBase::ask` (a `TestCase` + the
// layer's consolidated `L::Opts`) or `audit_consistency`; the SDK only assembles
// the inputs.

use std::path::PathBuf;

use sigmakee_rs_core::{
    AstNode, CommonProverOpts, Parser, ProverResult, ProverStatus, ProvingLayer, SourceFile,
    TestCase, parse_document
};

use super::Session;
use super::super::{SdkError, SdkResult, Source};

/// How one test case turned out, after comparing the prover's verdict to the
/// case's `expected_proof` / `(answer …)` directives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestOutcome {
    /// The prover's verdict matched `expected_proof` and every expected answer
    /// binding (if any) was produced.
    Passed,
    /// Provability matched, but some expected `(answer …)` bindings were not
    /// among the prover's results.
    Incomplete { inferred: Vec<String>, missing: Vec<String> },
    /// The prover's verdict contradicted the case's expectation.
    Failed { expected: bool, got: bool, status: ProverStatus },
}

/// One test case's result: its name, the pass/fail [`TestOutcome`], and the raw
/// [`ProverResult`] (for proof / output / timing detail).
#[derive(Debug, Clone)]
pub struct TestCaseOutcome {
    /// The test's file name (`TestCase::file_name`).
    pub name:    String,
    /// The verdict, derived from the case's expectations.
    pub outcome: TestOutcome,
    /// The underlying prover result.
    pub result:  ProverResult,
}

/// A `Session` with accumulated hypotheses, returned by [`Session::tell`].  Add
/// more with [`OpenSession::tell`], then discharge a conjecture with
/// [`OpenSession::ask`] — the hypotheses ride in as the `TestCase`'s support.
pub struct OpenSession<'a, L: ProvingLayer> {
    session:    &'a mut Session<L>,
    hypotheses: Vec<AstNode>,
}

impl<L: ProvingLayer> Session<L> {
    /// Begin an incremental query: parse `kif` into hypotheses and return an
    /// [`OpenSession`] carrying them.  Chain more [`tell`](OpenSession::tell)s
    /// and finish with [`ask`](OpenSession::ask).
    pub fn tell(&mut self, kif: &str) -> Result<OpenSession<'_, L>, Vec<SdkError>> {
        Ok(OpenSession { session: self, hypotheses: parse_formulas(kif)? })
    }

    /// Create an empty [`OpenSession`] for this session, used mainly for type
    /// checking convenience
    pub fn open_session(&mut self) -> OpenSession<'_, L> {
        OpenSession { session: self, hypotheses: vec![] }
    }

    /// Prove a KIF conjecture against the KB.  Builds a synthetic [`TestCase`]
    /// (the conjecture, no hypotheses) and calls the core `ask` primitive.
    /// Selection / session ride in on `opts` — the layer's consolidated params.
    /// Errors on non-proving backends.
    pub fn ask(&self, query_kif: &str, opts: Option<L::Opts>) -> Result<ProverResult, Vec<SdkError>> {
        let tc = synthetic_case(parse_one(query_kif)?, Vec::new());
        self.ask_case(tc, opts)
    }

    /// Saturate the KB for up to `limit` distinct contradictions (whole base —
    /// `&[]` focus).  `limit = 1` is the usual satisfiability check.  Selection /
    /// session ride in on `opts`.
    pub fn audit(&self, opts: L::Opts, limit: usize) -> SdkResult<ProverResult> {
        Ok(self.kb.audit_consistency(&[], opts, limit))
    }

    /// Single-contradiction satisfiability check (`audit` with `limit = 1`):
    /// `Inconsistent` if the KB derives a contradiction, else `Consistent`.
    pub fn check_consistency(&self, opts: L::Opts) -> SdkResult<ProverResult> {
        self.audit(opts, 1)
    }

    /// Run each test the `Source` yields, one [`TestCaseOutcome`] per file in
    /// order.  Two kinds of test are accepted (anything else — plain KIF, a bare
    /// `.ax` library — is rejected):
    ///
    /// - **`.tq`** SUMO tests: prove the query with the case's axioms as
    ///   force-included support, graded against `expected_proof` / `(answer …)`.
    /// - **`.p` / `.tptp`** TPTP problems: the full [`tptp::solve`](crate::tptp::solve)
    ///   orchestration (role split, background promotion, proof-dialect stamping,
    ///   Disproved→whole-theory escalation).  `include(...)` directives are
    ///   already spliced by `Source::read`.
    pub fn test(
        &mut self,
        src: Source,
        opts: Option<L::Opts>,
    ) -> Result<TestCaseOutcome, Vec<SdkError>> {
        let mut tc = self.source_to_test_case(src)?;
        // Precedence: an explicit (non-zero) caller timeout — e.g. CLI
        // `--timeout` — pins the budget for every case; otherwise each case uses
        // its own `(time N)` directive (`tc.timeout`).
        let pin_timeout = opts.as_ref().map_or(false, |o| o.timeout() != 0);
        let mut prover_opts = opts.unwrap_or_default();
        if !pin_timeout {
            prover_opts.set_timeout(tc.timeout as u64);
        }
        // A standalone TPTP problem (`.p` / `.tptp`) runs under the backend's
        // complete-calculus configuration (native: full saturation +
        // superposition with strict, honest saturation verdicts).  `.tq` KIF
        // tests keep the backend's configured strategy.
        if matches!(Parser::from_filename(&tc.file_name), Some(Parser::Tptp { .. })) {
            prover_opts.set_tptp_problem();
        }

        let name = tc.file_name.to_string();
        let expected = tc.expected_proof.unwrap_or(true);
        // Peel the `.tq`/TPTP `Annotated{Conjecture}` wrapper to the bare
        // formula (mirrors `ask_case`) so the conjecture normalizer interns
        // it; otherwise the prover reports "No query sentence parsed".
        tc.query = tc.query.map(|q| q.formula().clone());
        let result = self.kb.ask(tc, Some(&self.name), &prover_opts);
        let proved = matches!(result.status, ProverStatus::Proved);
        // Grade against the expectation in BOTH directions: an expected-no case
        // passes when no proof is found (including by exhausting its time
        // budget — "fails by timeout" is that case's designed outcome).
        let outcome = if proved == expected {
            TestOutcome::Passed
        } else {
            TestOutcome::Failed { expected, got: proved, status: result.status.clone() }
        };
        Ok(TestCaseOutcome { name, outcome, result })
    }

    // -- internals -----------------------------------------------------------

    /// Discharge a fully-assembled `TestCase` through the core `ask` primitive.
    fn ask_case(&self, mut tc: TestCase, opts: Option<L::Opts>) -> Result<ProverResult, Vec<SdkError>> {
        // `ask`'s conjecture normalizer interns the bare formula; the `.tq` /
        // TPTP parsers attach an `Annotated{Conjecture}` wrapper it doesn't peel
        // (a negated conjecture's extra `not` is already baked into the formula
        // by `renegate`, so the bare formula is still correct).
        tc.query = tc.query.map(|q| q.formula().clone());
        let prover_opts = opts.unwrap_or_default();
        Ok(self.kb.ask(tc, Some(&self.name), &prover_opts))
    }
}

impl<L: ProvingLayer> OpenSession<'_, L> {
    fn session_mut(&mut self) -> &mut Session<L> {
        return &mut self.session;
    }

    /// Accumulate more hypotheses.
    pub fn tell(mut self, kif: &str) -> Result<Self, Vec<SdkError>> {
        self.hypotheses.extend(parse_formulas(kif)?);
        Ok(self)
    }

    /// Discharge `query_kif` against the KB with the accumulated hypotheses as
    /// force-included support.
    pub fn ask(self, query_kif: &str, opts: Option<L::Opts>) -> Result<ProverResult, Vec<SdkError>> {
        let tc = synthetic_case(parse_one(query_kif)?, self.hypotheses);
        self.session.ask_case(tc, opts)
    }

    /// Validate assertions in the open session
    pub fn validate(&mut self) -> Vec<SdkError> {
        let session_name = format!("__inline_validation({})__", self.session.name);
        let hypotheses = self.hypotheses.clone();
        let kb = self.session_mut().kb_mut();
        let res = kb.load(SourceFile {
            parser: Parser::Kif,
            name: session_name.clone(),
            path: PathBuf::new(),
            origin: sigmakee_rs_core::FileOrigin::Inline,
            contents: String::new(),
            prebuilt: Some(hypotheses),
        }, &session_name);

        if res.has_errors() {
            return res.diagnostics.into_iter().map(|d| SdkError::Kb(d)).collect();
        }

        let diag = kb.validate_session(&session_name).into_iter().map(|d| SdkError::Kb(d)).collect();

        kb.flush_session(&session_name);

        diag
    }
}

/// Parse `kif` to its formulas (statements), erroring on any parse error.
fn parse_formulas(kif: &str) -> Result<Vec<AstNode>, Vec<SdkError>> {
    let doc = parse_document("sdk::session", kif.to_string(), Parser::Kif);
    if doc.has_errors() {
        return Err(doc.parse_errors.iter().map(|(_, e)| SdkError::Kb(e.to_diagnostic())).collect());
    }
    Ok(doc.ast.iter().filter_map(|d| d.as_stmt().cloned()).collect())
}

/// Parse exactly one formula (the conjecture).
fn parse_one(kif: &str) -> Result<AstNode, Vec<SdkError>> {
    parse_formulas(kif)?
        .into_iter().next()
        .ok_or_else(|| vec![SdkError::Config("query parsed to no formula".into())])
}

fn synthetic_case(query: AstNode, hypotheses: Vec<AstNode>) -> TestCase {
    TestCase {
        file_name:       "sdk::session".into(),
        note:            String::new(),
        timeout:         0,
        query:           Some(query),
        expected_proof:  None,
        expected_answer: None,
        axioms:          hypotheses,
        extra_files:     Vec::new(),
    }
}
