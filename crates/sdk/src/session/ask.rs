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

/// The SZS ("Success/Zero-info Status") ontology's outcome words, restricted
/// to the ones this harness ever reports — the six the CLI prints on the
/// `% SZS status <X> for <name>` line, plus the two "expected" markers
/// (`ContradictoryAxioms`, and `Open`/`Unknown`, folded into
/// [`ExpectedOutcome`](self::ExpectedOutcome) rather than kept here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SzsStatus {
    /// A conjecture was proved (FOF-style: the problem carries a proper
    /// `conjecture` role).
    Theorem,
    /// A refutation was found with no (or a CNF `negated_conjecture`)
    /// conjecture in play — the axiom set itself is unsatisfiable.
    Unsatisfiable,
    /// Honest, saturation-complete "no" against a FOF conjecture: the
    /// negated conjecture plus axioms saturate with no contradiction.
    CounterSatisfiable,
    /// Honest, saturation-complete "no" with no (or a CNF) conjecture: the
    /// clause set itself saturates satisfiably.
    Satisfiable,
    /// The prover stopped without a confident verdict (incomplete search,
    /// step exhaustion) — not a certificate either way.
    GaveUp,
    /// The wall-clock budget was exhausted before any verdict.
    Timeout,
}

impl std::fmt::Display for SzsStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SzsStatus::Theorem            => "Theorem",
            SzsStatus::Unsatisfiable      => "Unsatisfiable",
            SzsStatus::CounterSatisfiable => "CounterSatisfiable",
            SzsStatus::Satisfiable        => "Satisfiable",
            SzsStatus::GaveUp             => "GaveUp",
            SzsStatus::Timeout            => "Timeout",
        };
        f.write_str(s)
    }
}

/// Map a prover result onto the SZS status word the `% SZS status …` line
/// reports.  `has_fof_conjecture` picks the FOF-conjecture naming
/// (Theorem/CounterSatisfiable) vs. the no-conjecture/CNF-refutation naming
/// (Unsatisfiable/Satisfiable) for the two "definitive" verdicts; the
/// TPTP path's `strict_saturation` strategy means a `Disproved` status is
/// already a completeness *certificate* (see `prove.rs`'s status mapping),
/// so it always maps to CounterSatisfiable/Satisfiable, never GaveUp.
pub fn szs_status(result: &ProverResult, has_fof_conjecture: bool) -> SzsStatus {
    match result.status {
        ProverStatus::Proved if has_fof_conjecture => SzsStatus::Theorem,
        ProverStatus::Proved                       => SzsStatus::Unsatisfiable,
        // A refutation not rooted in the conjecture: the background theory
        // alone is unsatisfiable.
        ProverStatus::Inconsistent                 => SzsStatus::Unsatisfiable,
        ProverStatus::Disproved if has_fof_conjecture => SzsStatus::CounterSatisfiable,
        ProverStatus::Disproved                       => SzsStatus::Satisfiable,
        ProverStatus::Consistent                   => SzsStatus::Satisfiable,
        ProverStatus::Timeout                      => SzsStatus::Timeout,
        // `Unknown` (incomplete search / step exhaustion / no verdict) and
        // `InputError` (malformed input — nothing better to report on this
        // 6-word scale) both fall through to GaveUp.
        ProverStatus::Unknown | ProverStatus::InputError => SzsStatus::GaveUp,
    }
}

/// The bucket a TPTP `% Status : <word>` header maps a run's expectation
/// into — whether the run OUGHT to find a proof, OUGHT NOT to (a confident
/// disproof/countermodel is also a pass), or is merely informational
/// (`Open`/`Unknown` problems: run it, report the SZS outcome, but don't
/// grade pass/fail).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedOutcome {
    /// `Theorem` / `Unsatisfiable` / `ContradictoryAxioms`: the prover ought
    /// to find a proof.
    Proved,
    /// `Satisfiable` / `CounterSatisfiable`: the prover ought NOT to find a
    /// proof — an honest countermodel (or an honest timeout/give-up) passes.
    NotProved,
    /// `Open` / `Unknown` header: run it, report the SZS outcome, but don't
    /// grade pass/fail.
    Informational,
}

/// Classify a TPTP `% Status` header word into its [`ExpectedOutcome`]
/// bucket.  `None` — a `.tq` case (no such header), or a TPTP file with no
/// recognized `% Status` line at all — defaults to
/// [`ExpectedOutcome::Proved`], preserving this harness's long-standing
/// "absent header ⇒ graded as a Theorem" behavior.  An unrecognized-but-present
/// word is treated like `Open`/`Unknown`: informational only.
fn classify_expected_status(status: Option<&str>) -> ExpectedOutcome {
    match status {
        None => ExpectedOutcome::Proved,
        Some("Theorem") | Some("Unsatisfiable") | Some("ContradictoryAxioms") =>
            ExpectedOutcome::Proved,
        Some("Satisfiable") | Some("CounterSatisfiable") =>
            ExpectedOutcome::NotProved,
        Some(_) => ExpectedOutcome::Informational,
    }
}

/// `true` when the prover's verdict is a CONFIDENT disproof / countermodel —
/// not merely "no proof found within budget", but a certificate that no
/// proof exists (the honest `Disproved` the TPTP strict-saturation strategy
/// only emits once `complete_saturation` holds; see `prove.rs`).  Mirrors how
/// the SZS `CounterSatisfiable`/`Satisfiable` verdicts arrive from the native
/// prover: a `Consistent` audit result counts too (whole-theory satisfiable).
fn is_confident_disproof(result: &ProverResult) -> bool {
    match result.status {
        ProverStatus::Disproved  => result.complete_saturation != Some(false),
        ProverStatus::Consistent => true,
        _ => false,
    }
}

/// How one test case turned out, after comparing the prover's verdict to the
/// case's `expected_proof` / `(answer …)` directives (or, for a TPTP problem
/// carrying a `% Status` header, its [`ExpectedOutcome`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestOutcome {
    /// The prover's verdict matched the case's expectation and every
    /// expected answer binding (if any) was produced.
    Passed,
    /// Provability matched, but some expected `(answer …)` bindings were not
    /// among the prover's results.
    Incomplete { inferred: Vec<String>, missing: Vec<String> },
    /// The prover's verdict contradicted the case's expectation — an
    /// ordinary failure (timeout / gave-up / wrong side of an honest
    /// countermodel), not a confidently wrong claim.
    Failed { expected: bool, got: bool, status: ProverStatus },
    /// The prover made a CONFIDENT claim that contradicts the case's
    /// `% Status` header — proved a problem whose header says
    /// Satisfiable/CounterSatisfiable, or produced a certified disproof of
    /// one whose header says Theorem/Unsatisfiable.  Distinct from
    /// `Failed`: this is not "ran out of budget", it's "got the wrong
    /// answer with confidence" — the harness's most serious finding.
    FalseVerdict { expected: ExpectedOutcome, status: ProverStatus },
    /// The case's expectation is merely informational (`Open`/`Unknown`
    /// header, or no header at all beyond the `.tq` yes/no directive) — the
    /// prover ran and reported an outcome, but it is neither a pass nor a
    /// fail.
    Informational,
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
    /// The `% SZS status <X> for <name>` word this case's result maps to —
    /// always populated (independent of whether the case carried a `%
    /// Status` header), so the CLI can print the line unconditionally.
    pub szs:     SzsStatus,
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
    /// - **`.p` / `.tptp`** TPTP problems: graded against the file's `%
    ///   Status` header (see [`ExpectedOutcome`]) when present, falling back
    ///   to `expected_proof` (always `None` for TPTP — see
    ///   [`classify_expected_status`]) otherwise.  `include(...)` directives
    ///   are already spliced by `Source::read`.
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
        let is_tptp = matches!(Parser::from_filename(&tc.file_name), Some(Parser::Tptp { .. }));
        if is_tptp {
            prover_opts.set_tptp_problem();
        }

        let name = tc.file_name.to_string();
        let has_fof_conjecture = tc.has_fof_conjecture;
        // `.tq` cases carry `expected_proof` (a plain yes/no directive, no
        // SZS vocabulary); TPTP cases carry `expected_status` (the `%
        // Status` header, read back off the `status` Meta the tokenizer
        // recorded — see `parse::tptp::tokenizer::record_status_pragma`).
        let expected = if is_tptp {
            classify_expected_status(tc.expected_status.as_deref())
        } else {
            match tc.expected_proof {
                Some(true) | None => ExpectedOutcome::Proved,
                Some(false)       => ExpectedOutcome::NotProved,
            }
        };
        // Peel the `.tq`/TPTP `Annotated{Conjecture}` wrapper to the bare
        // formula (mirrors `ask_case`) so the conjecture normalizer interns
        // it; otherwise the prover reports "No query sentence parsed".
        tc.query = tc.query.map(|q| q.formula().clone());
        let result = self.kb.ask(tc, Some(&self.name), &prover_opts);
        let szs = szs_status(&result, has_fof_conjecture);

        let proved = matches!(result.status, ProverStatus::Proved);
        let confident_no = is_confident_disproof(&result);
        let outcome = match expected {
            ExpectedOutcome::Informational => TestOutcome::Informational,
            ExpectedOutcome::Proved => {
                if proved {
                    TestOutcome::Passed
                } else if confident_no {
                    // A CERTIFIED disproof/countermodel of a problem the
                    // header claims is a Theorem — confidently wrong, not
                    // merely "ran out of budget".
                    TestOutcome::FalseVerdict { expected, status: result.status }
                } else {
                    TestOutcome::Failed { expected: true, got: false, status: result.status.clone() }
                }
            }
            ExpectedOutcome::NotProved => {
                if proved {
                    // Claimed a proof of a problem the header says is
                    // Satisfiable/CounterSatisfiable — confidently wrong.
                    TestOutcome::FalseVerdict { expected, status: result.status }
                } else {
                    // Any honest non-proof passes: a certified countermodel,
                    // a gave-up, or an honest timeout are all the designed
                    // outcome for a Satisfiable/CounterSatisfiable problem.
                    TestOutcome::Passed
                }
            }
        };
        Ok(TestCaseOutcome { name, outcome, result, szs })
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

/// Doxastic queries (contexts-as-sessions, native backend only): reason
/// INSIDE an agent's belief context via a projected prover run — the full
/// calculus over the agent's asserted belief contents, not just the outer
/// K-distribution schemata.  GUARDRAIL: read-only; the projection never
/// asserts inner conclusions back into the KB — verdicts and proofs
/// return to the caller only.
#[cfg(feature = "native-prover")]
impl Session<sigmakee_rs_core::ProverLayer> {
    /// Prove `query_kif` inside `agent`'s belief context (`believes`):
    /// `Proved` — the agent's asserted beliefs entail the query under
    /// full consequence closure; `Disproved` (saturation) — the inner
    /// CounterSatisfiable analogue; `Inconsistent` — the belief base
    /// itself is contradictory; `Unknown`/`Timeout` — budget.  Cited
    /// proof steps ride in `proof_kif` when `opts.want_proof` is set.
    pub fn doxastic_ask(
        &self,
        agent:     &str,
        query_kif: &str,
        opts:      Option<sigmakee_rs_core::NativeOpts>,
    ) -> SdkResult<ProverResult> {
        Ok(self.kb.doxastic_ask(agent, query_kif, opts.unwrap_or_default()))
    }

    /// Is `agent`'s belief base consistent under full consequence
    /// closure?  `Consistent` / `Inconsistent` (cited contradiction
    /// transcripts in `contradiction_proofs`) / `Unknown`-`Timeout`.
    /// An empty belief base is trivially `Consistent`.
    pub fn doxastic_consistent(
        &self,
        agent: &str,
        opts:  Option<sigmakee_rs_core::NativeOpts>,
    ) -> SdkResult<ProverResult> {
        Ok(self.kb.doxastic_consistent(agent, opts.unwrap_or_default()))
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
        expected_status: None,
        has_fof_conjecture: false,
        input_formulas:     0,
        unaccounted_inputs: 0,
    }
}
