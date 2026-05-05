//! Batch test runner (`TestOp`).
//!
//! Replaces the CLI's `cli::test::run_test` orchestration.  Reads
//! `.kif.tq` files (or accepts pre-loaded text), parses each into a
//! [`sigmakee_rs_core::TestCase`], runs the conjecture against the configured
//! prover with each case's axioms loaded into a per-case session,
//! compares the verdict to the case's `(answer ...)` directive, and
//! returns a [`TestSuiteReport`].
//!
//! Each case gets its own session (`"sigmakee-rs-sdk-test-{idx}"`) which is
//! flushed after the case completes — axioms cannot leak between
//! tests.
//!
//! # I/O posture
//!
//! Same as `IngestOp`: caller picks per source.
//!
//! - [`TestOp::add_file`] — SDK reads it.  Path → tag.
//! - [`TestOp::add_dir`]  — SDK lists `*.kif.tq`, reads each.
//! - [`TestOp::add_text`] — caller-provided `.kif.tq` content.

use std::path::{Path, PathBuf};
use std::time::Instant;

use sigmakee_rs_core::{parse_test_content, KnowledgeBase, TestCase, TptpLang, VampireRunner};

use crate::ask::ProverBackend;
use crate::error::{SdkError, SdkResult};
use sigmakee_rs_core::{ProgressEvent, ProgressSink};
use crate::report::test::{TestCaseReport, TestOutcome, TestSuiteReport};

/// Internal source representation for batch tests.
enum Source {
    File(PathBuf),
    Dir(PathBuf),
    Text { tag: String, content: String },
    Parsed { tag: String, case: Box<TestCase> },
}

/// Builder for a batch test run.
pub struct TestOp<'a> {
    kb:               &'a mut KnowledgeBase,
    sources:          Vec<Source>,
    timeout_override: Option<u32>,
    backend:          ProverBackend,
    lang:             TptpLang,
    vampire_path:     Option<PathBuf>,
    tptp_dump:        Option<PathBuf>,
    progress:         Option<Box<dyn ProgressSink>>,
}

impl<'a> TestOp<'a> {
    /// New op against `kb`.  Defaults match `AskOp`: subprocess
    /// backend, FOF lang, no per-case timeout override.
    pub fn new(kb: &'a mut KnowledgeBase) -> Self {
        Self {
            kb,
            sources:          Vec::new(),
            timeout_override: None,
            backend:          ProverBackend::Subprocess,
            lang:             TptpLang::Fof,
            vampire_path:     None,
            tptp_dump:        None,
            progress:         None,
        }
    }

    /// Add a single `.kif.tq` file.  SDK reads it during `run()`.
    pub fn add_file<P: Into<PathBuf>>(mut self, p: P) -> Self {
        self.sources.push(Source::File(p.into()));
        self
    }

    /// Add a directory of `.kif.tq` files.  SDK enumerates
    /// (non-recursive), filters to `*.kif.tq`, sorts.
    pub fn add_dir<P: Into<PathBuf>>(mut self, p: P) -> Self {
        self.sources.push(Source::Dir(p.into()));
        self
    }

    /// Add already-resident `.kif.tq` text.  `tag` is the synthetic
    /// identifier (e.g. `"network/test-7"`); `content` is the raw
    /// `.kif.tq` body.
    pub fn add_text(
        mut self,
        tag:     impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        self.sources.push(Source::Text {
            tag:     tag.into(),
            content: content.into(),
        });
        self
    }

    /// Add a pre-parsed [`sigmakee_rs_core::TestCase`].  Useful when the
    /// caller already parsed the test (e.g. from a JSON-RPC payload)
    /// and just wants the SDK to run it.  `tag` identifies the case
    /// for reporting.
    pub fn add_case(mut self, tag: impl Into<String>, case: TestCase) -> Self {
        self.sources.push(Source::Parsed {
            tag:  tag.into(),
            case: Box::new(case),
        });
        self
    }

    /// Override every case's timeout.  When unset (the default),
    /// each case uses the value from its `(time ...)` directive (or
    /// 30 s if absent).
    pub fn timeout_override(mut self, secs: u32) -> Self {
        self.timeout_override = Some(secs);
        self
    }

    /// Prover backend.  Default [`ProverBackend::Subprocess`].
    pub fn backend(mut self, b: ProverBackend) -> Self {
        self.backend = b;
        self
    }

    /// TPTP dialect.  Default [`TptpLang::Fof`].
    pub fn lang(mut self, l: TptpLang) -> Self {
        self.lang = l;
        self
    }

    /// Path to the Vampire binary (subprocess backend).
    pub fn vampire_path<P: Into<PathBuf>>(mut self, p: P) -> Self {
        self.vampire_path = Some(p.into());
        self
    }

    /// Dump every case's TPTP problem to this path before running.
    pub fn tptp_dump<P: Into<PathBuf>>(mut self, p: P) -> Self {
        self.tptp_dump = Some(p.into());
        self
    }

    /// Progress sink.  Receives one [`ProgressEvent::TestCase`] per
    /// case.
    pub fn progress(mut self, sink: Box<dyn ProgressSink>) -> Self {
        self.progress = Some(sink);
        self
    }

    /// Run every case in input order.  Returns `Ok(report)` even if
    /// individual cases fail — failures ride out in the report.
    /// `Err` is reserved for errors that prevent the suite from
    /// running at all (a directory listing fails, a file read fails).
    pub fn run(self) -> SdkResult<TestSuiteReport> {
        let TestOp {
            kb, sources, timeout_override, backend, lang,
            vampire_path, tptp_dump, mut progress,
        } = self;

        // Expand File/Dir/Text/Parsed → flat list of "ready to run"
        // entries.  We materialise text up-front so the run loop can
        // simply iterate; per-source I/O errors abort the whole suite
        // (matches the CLI's existing behaviour).
        let cases = expand_sources(sources)?;
        let total = cases.len();
        let mut report = TestSuiteReport::default();

        for (idx, prepared) in cases.into_iter().enumerate() {
            let case_report = run_one(
                kb,
                idx,
                &prepared,
                timeout_override,
                backend,
                lang,
                vampire_path.as_deref(),
                tptp_dump.as_deref(),
            );

            let brief = match &case_report.outcome {
                TestOutcome::Passed                  => "pass",
                TestOutcome::Failed { .. }           => "fail",
                TestOutcome::Incomplete { .. }       => "incomplete",
                TestOutcome::ParseError(_)           => "parse-error",
                TestOutcome::SemanticError(_)        => "semantic-error",
                TestOutcome::ProverError(_)          => "prover-error",
                TestOutcome::NoQuery                 => "no-query",
            };
            if let Some(p) = progress.as_deref_mut() {
                p.emit(&ProgressEvent::TestCase {
                    idx,
                    total,
                    tag: case_report.tag.clone(),
                    brief,
                });
            }

            match case_report.outcome {
                TestOutcome::Passed                                          => report.passed  += 1,
                TestOutcome::Failed { .. } | TestOutcome::Incomplete { .. } |
                TestOutcome::ProverError(_)                                  => report.failed  += 1,
                TestOutcome::ParseError(_) | TestOutcome::SemanticError(_) |
                TestOutcome::NoQuery                                         => report.skipped += 1,
            }
            report.cases.push(case_report);
        }

        Ok(report)
    }
}

// ---------------------------------------------------------------------------
// Source expansion
// ---------------------------------------------------------------------------

/// Each entry is a (tag, content_or_case) tuple ready to drop into
/// `run_one`.
struct Prepared {
    tag:     String,
    parsed:  Result<TestCase, String>,  // Err = parse error message
}

fn expand_sources(sources: Vec<Source>) -> SdkResult<Vec<Prepared>> {
    let mut out: Vec<Prepared> = Vec::new();
    for s in sources {
        match s {
            Source::File(p) => out.push(prepare_from_disk(&p)?),
            Source::Dir(d)  => {
                for child in scan_dir_for_tests(&d)? {
                    out.push(prepare_from_disk(&child)?);
                }
            }
            Source::Text { tag, content } => {
                let parsed = parse_test_content(&content, &tag).map_err(|e| e.to_string());
                out.push(Prepared { tag, parsed });
            }
            Source::Parsed { tag, case } => {
                out.push(Prepared { tag, parsed: Ok(*case) });
            }
        }
    }
    Ok(out)
}

fn prepare_from_disk(path: &Path) -> SdkResult<Prepared> {
    let content = std::fs::read_to_string(path).map_err(|e| SdkError::Io {
        path:   path.to_path_buf(),
        source: e,
    })?;
    let tag = path.display().to_string();
    let parsed = parse_test_content(&content, &tag).map_err(|e| e.to_string());
    Ok(Prepared { tag, parsed })
}

/// List `*.kif.tq` files in `dir` (non-recursive), sorted.
fn scan_dir_for_tests(dir: &Path) -> SdkResult<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir).map_err(|e| SdkError::DirRead {
        path:    dir.to_path_buf(),
        message: e.to_string(),
    })?;
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.to_string_lossy().ends_with(".kif.tq"))
        .collect();
    files.sort();
    Ok(files)
}

// ---------------------------------------------------------------------------
// Per-case execution
// ---------------------------------------------------------------------------

fn run_one(
    kb:               &mut KnowledgeBase,
    idx:              usize,
    prepared:         &Prepared,
    timeout_override: Option<u32>,
    backend:          ProverBackend,
    lang:             TptpLang,
    vampire_path:     Option<&Path>,
    tptp_dump:        Option<&Path>,
) -> TestCaseReport {
    let timings_zero = sigmakee_rs_core::prover::ProverTimings::default();

    let case = match &prepared.parsed {
        Ok(c) => c.clone(),
        Err(msg) => {
            return TestCaseReport {
                name:    prepared.tag.clone(),
                tag:     prepared.tag.clone(),
                outcome: TestOutcome::ParseError(msg.clone()),
                timings: timings_zero,
            };
        }
    };

    let session  = format!("sigmakee-rs-sdk-test-{}", idx);
    let load_tag = format!("sigmakee-rs-sdk-test-src-{}", idx);

    // Load case axioms into a per-case session.
    let axiom_text = case.axioms.join("\n");
    if !axiom_text.is_empty() {
        let load_result = kb.load_kif(&axiom_text, &load_tag, Some(&session));
        if !load_result.ok {
            kb.flush_session(&session);
            let msg = load_result
                .errors
                .into_iter()
                .next()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown parse error in axioms".into());
            return TestCaseReport {
                name:    case.note.clone(),
                tag:     prepared.tag.clone(),
                outcome: TestOutcome::ParseError(msg),
                timings: timings_zero,
            };
        }

        let semantic = kb.validate_session(&session);
        if !semantic.is_empty() {
            kb.flush_session(&session);
            let msg = semantic.iter()
                .map(|(_, e)| e.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            return TestCaseReport {
                name:    case.note.clone(),
                tag:     prepared.tag.clone(),
                outcome: TestOutcome::SemanticError(msg),
                timings: timings_zero,
            };
        }
    }

    let query = match case.query.clone() {
        Some(q) => q,
        None => {
            kb.flush_session(&session);
            return TestCaseReport {
                name:    case.note.clone(),
                tag:     prepared.tag.clone(),
                outcome: TestOutcome::NoQuery,
                timings: timings_zero,
            };
        }
    };

    // Resolve timeout: explicit override > test-file directive > 30s default.
    let timeout = timeout_override.unwrap_or(case.timeout);
    let _t = Instant::now();

    let result = match backend {
        ProverBackend::Subprocess => {
            let path = vampire_path
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("vampire"));
            let runner = VampireRunner {
                vampire_path:   path,
                timeout_secs:   timeout,
                tptp_dump_path: tptp_dump.map(|p| p.to_path_buf()),
            };
            kb.ask(&query, Some(&session), &runner, lang)
        }
        #[cfg(feature = "integrated-prover")]
        ProverBackend::Embedded => {
            kb.ask_embedded(&query, Some(&session), timeout, lang)
        }
    };

    kb.flush_session(&session);

    let proved = matches!(result.status, sigmakee_rs_core::ProverStatus::Proved);
    let expected = case.expected_proof.unwrap_or(true);

    // Prover-side hard failure: ProverStatus::Unknown with non-empty
    // raw_output usually means "couldn't run the prover" rather than
    // "ran but undecided".  Surface as ProverError so suite-level
    // counts treat this as a failure.
    if matches!(result.status, sigmakee_rs_core::ProverStatus::Unknown) && !proved && !result.raw_output.is_empty() {
        // Heuristic: if raw_output contains a binary-not-found marker,
        // this is likely setup (not a real prover-error).  Keep the
        // logic simple here — both shapes record under ProverError —
        // and let the suite consumer route on the message.
        return TestCaseReport {
            name:    case.note.clone(),
            tag:     prepared.tag.clone(),
            outcome: TestOutcome::ProverError(result.raw_output),
            timings: result.timings,
        };
    }

    if proved == expected {
        if let Some(expected_answers) = case.expected_answer.clone() {
            let inferred: Vec<String> = result.bindings.iter().map(|b| b.value.clone()).collect();
            let missing: Vec<String> = expected_answers
                .iter()
                .filter(|e| !inferred.contains(e))
                .cloned()
                .collect();
            if !missing.is_empty() {
                return TestCaseReport {
                    name:    case.note.clone(),
                    tag:     prepared.tag.clone(),
                    outcome: TestOutcome::Incomplete { inferred, missing },
                    timings: result.timings,
                };
            }
        }
        TestCaseReport {
            name:    case.note.clone(),
            tag:     prepared.tag.clone(),
            outcome: TestOutcome::Passed,
            timings: result.timings,
        }
    } else {
        TestCaseReport {
            name:    case.note.clone(),
            tag:     prepared.tag.clone(),
            outcome: TestOutcome::Failed { expected, got: proved },
            timings: result.timings,
        }
    }
}
