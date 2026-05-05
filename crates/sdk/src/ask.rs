//! Single proof query (`AskOp`).
//!
//! Builder for one call to the theorem prover.  Replaces the CLI's
//! `cli::ask::run_ask` orchestration without taking any of its
//! presentation concerns (colourised verdict, profile printing,
//! exit code).  Returns an [`AskReport`] the caller can render
//! however it likes.
//!
//! # Backends
//!
//! Two backends are exposed via [`ProverBackend`]:
//!
//! - **Subprocess** (always available with `feature = "ask"`): spawns
//!   an external `vampire` binary.  The caller supplies the binary's
//!   path; if it's a bare name, `Command::spawn` walks `$PATH`.  The
//!   SDK does not pre-resolve `$PATH` — caller-side fail-fast checks
//!   are the caller's responsibility.
//! - **Embedded** (requires `feature = "integrated-prover"`): runs
//!   Vampire in-process via the C++ FFI.  Holds a global mutex while
//!   running; safe to call but should not be killed mid-flight from
//!   another thread.
//!
//! # Profiling
//!
//! `AskOp` does not own a profiler.  If the caller wants per-phase
//! timings, they install a `Profiler` on the `KnowledgeBase` *before*
//! handing it to `AskOp::new`:
//!
//! ```ignore
//! // Install a progress sink that aggregates phase timings:
//! kb.set_progress_sink(my_phase_aggregator);
//! sigmakee_rs_sdk::AskOp::new(&mut kb, "(holds ?X Animal)").run()?;
//! // The sink has now seen every PhaseStarted/PhaseFinished pair.
//! ```

use std::path::PathBuf;
use std::time::Instant;

use sigmakee_rs_core::{KnowledgeBase, TptpLang, VampireRunner};

use crate::error::{SdkError, SdkResult};
use sigmakee_rs_core::{ProgressEvent, ProgressSink};
use crate::report::ask::AskReport;

/// Selects the prover runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProverBackend {
    /// Spawn an external `vampire` binary.  Caller supplies the
    /// path (or a bare name to be looked up on `$PATH` by the OS).
    Subprocess,

    /// Run Vampire in-process via the C++ FFI.  Requires the
    /// `integrated-prover` feature.
    #[cfg(feature = "integrated-prover")]
    Embedded,
}

impl Default for ProverBackend {
    fn default() -> Self { ProverBackend::Subprocess }
}

impl ProverBackend {
    /// Stable shortcode used in [`ProgressEvent::AskStarted`].
    fn label(self) -> &'static str {
        match self {
            ProverBackend::Subprocess => "subprocess",
            #[cfg(feature = "integrated-prover")]
            ProverBackend::Embedded   => "embedded",
        }
    }
}

/// Builder for a single proof query.
pub struct AskOp<'a> {
    kb:           &'a mut KnowledgeBase,
    query:        String,
    tells:        Vec<String>,
    session:      String,
    timeout_secs: u32,
    backend:      ProverBackend,
    lang:         TptpLang,
    vampire_path: Option<PathBuf>,
    tptp_dump:    Option<PathBuf>,
    progress:     Option<Box<dyn ProgressSink>>,
}

impl<'a> AskOp<'a> {
    /// New op against `kb` with conjecture `query`.  Defaults match
    /// the CLI: subprocess backend, FOF lang, 30 s timeout, session
    /// `"<inline>"`, `vampire_path = "vampire"`.
    pub fn new(kb: &'a mut KnowledgeBase, query: impl Into<String>) -> Self {
        Self {
            kb,
            query:        query.into(),
            tells:        Vec::new(),
            session:      "<inline>".to_string(),
            timeout_secs: 30,
            backend:      ProverBackend::Subprocess,
            lang:         TptpLang::Fof,
            vampire_path: None,
            tptp_dump:    None,
            progress:     None,
        }
    }

    /// Add one extra KIF assertion to load into the session before
    /// the conjecture is asked.  Cumulative across calls.
    pub fn tell(mut self, kif: impl Into<String>) -> Self {
        self.tells.push(kif.into());
        self
    }

    /// Add many KIF assertions at once.
    pub fn tells<I, S>(mut self, iter: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tells.extend(iter.into_iter().map(Into::into));
        self
    }

    /// Session whose axioms are included as TPTP hypotheses.  Default
    /// is `"<inline>"` (matches the CLI's `ask` default).
    pub fn session(mut self, s: impl Into<String>) -> Self {
        self.session = s.into();
        self
    }

    /// Prover timeout in seconds.  Default 30.
    pub fn timeout_secs(mut self, secs: u32) -> Self {
        self.timeout_secs = secs;
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

    /// Path to the Vampire binary (subprocess backend).  Default is
    /// `"vampire"` — `Command::spawn` will resolve it via `$PATH`.
    /// Passing an absolute path bypasses `$PATH` lookup.
    pub fn vampire_path<P: Into<PathBuf>>(mut self, p: P) -> Self {
        self.vampire_path = Some(p.into());
        self
    }

    /// If set, the generated TPTP problem is written to this path
    /// before the prover is invoked (subprocess backend only).
    /// Useful for reproducing in a Vampire shell.
    pub fn tptp_dump<P: Into<PathBuf>>(mut self, p: P) -> Self {
        self.tptp_dump = Some(p.into());
        self
    }

    /// Progress sink.  Receives [`ProgressEvent::AskStarted`] before
    /// the prover is invoked and [`ProgressEvent::AskFinished`] after.
    pub fn progress(mut self, sink: Box<dyn ProgressSink>) -> Self {
        self.progress = Some(sink);
        self
    }

    /// Run the query.  Returns `Ok(report)` on any successful prover
    /// invocation — including `Unknown`/`Timeout` verdicts, which are
    /// successful runs with an undecided outcome.  Only
    /// infrastructural failures (parse error in the query, tells that
    /// don't load, prover spawn failure) bubble out as `Err`.
    pub fn run(self) -> SdkResult<AskReport> {
        let AskOp {
            kb, query, tells, session, timeout_secs, backend, lang,
            vampire_path, tptp_dump, mut progress,
        } = self;

        // Apply tells into the named session before invoking the
        // prover.  Failures here abort: the session would be in an
        // inconsistent state and the conjecture would be asked
        // against an incomplete hypothesis set.
        for kif in &tells {
            log::debug!(target: "sigmakee_rs_sdk::ask", "tell (session={}): {}", session, kif);
            let r = kb.tell(&session, kif);
            if !r.ok {
                if let Some(first) = r.errors.into_iter().next() {
                    return Err(SdkError::Kb(first));
                }
                return Err(SdkError::Config(format!(
                    "kb.tell reported failure for session '{}' but produced no errors",
                    session
                )));
            }
        }

        if let Some(p) = progress.as_deref_mut() {
            p.emit(&ProgressEvent::AskStarted { backend: backend.label() });
        }
        let t_ask = Instant::now();

        let result = match backend {
            ProverBackend::Subprocess => {
                let path = vampire_path.unwrap_or_else(|| PathBuf::from("vampire"));
                let runner = VampireRunner {
                    vampire_path:   path,
                    timeout_secs,
                    tptp_dump_path: tptp_dump,
                };
                kb.ask(&query, Some(&session), &runner, lang)
            }
            #[cfg(feature = "integrated-prover")]
            ProverBackend::Embedded => {
                kb.ask_embedded(&query, Some(&session), timeout_secs, lang)
            }
        };

        let elapsed = t_ask.elapsed();
        if let Some(p) = progress.as_deref_mut() {
            p.emit(&ProgressEvent::AskFinished { status: result.status, elapsed });
        }

        // Subprocess prover surfaces spawn / I/O failures as
        // `ProverStatus::Unknown` with the OS error in `raw_output`.
        // We translate the most common shape — "vampire binary not
        // found" — into a structured error so consumers can route on
        // it.  The substring sniff is intentionally narrow: we only
        // promote when we're very sure, and let other Unknowns ride
        // out via the report (they may be legitimate undecided
        // outcomes).
        if matches!(result.status, sigmakee_rs_core::ProverStatus::Unknown)
            && backend == ProverBackend::Subprocess
            && (result.raw_output.contains("No such file")
                || result.raw_output.contains("not found")
                || result.raw_output.contains("cannot find"))
        {
            return Err(SdkError::VampireNotFound(result.raw_output));
        }

        Ok(AskReport {
            status:     result.status,
            bindings:   result.bindings,
            raw_output: result.raw_output,
            proof_kif:  result.proof_kif,
            proof_tptp: result.proof_tptp,
            timings:    result.timings,
        })
    }
}
