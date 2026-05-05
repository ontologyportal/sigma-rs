//! `sigmakee-rs-sdk` — programmatic Rust API over [`sigmakee_rs_core`].
//!
//! This crate exposes the operations the `sumo` CLI is built on
//! (ingesting KIF text, validating, translating to TPTP, asking
//! proof queries, batch-testing) as plain function / builder calls
//! that return structured reports.  No clap, no stdout, no exit
//! codes, no `log::error!`-then-bail — `sigmakee-rs-sdk` is meant to be
//! embedded inside larger applications: language servers, network
//! daemons, custom CLIs, scripted pipelines, test harnesses.
//!
//! # Quickstart
//!
//! ```no_run
//! use sigmakee_rs_sdk::{IngestOp, ValidateOp, manpage_view};
//!
//! // The caller owns the KB.  Use `KnowledgeBase::new()` for
//! // in-memory or `KnowledgeBase::open(path)` for LMDB-backed.
//! let mut kb = sigmakee_rs_core::KnowledgeBase::new();
//!
//! // Ingest: file path, directory, or resident text — mix freely.
//! IngestOp::new(&mut kb)
//!     .add_file("base.kif")                          // SDK reads this
//!     .add_dir("ontology/")                          // SDK walks *.kif
//!     .add_source("ws://patch", "(subclass A B)")    // already in memory
//!     .run()
//!     .unwrap();
//!
//! // Validate the whole KB.  Findings ride out in the report —
//! // `Err` is reserved for infrastructural failures only.
//! let report = ValidateOp::all(&mut kb).run().unwrap();
//! if !report.is_clean() {
//!     for (sid, err) in &report.semantic_errors {
//!         eprintln!("sentence {sid:?}: {err}");
//!     }
//! }
//!
//! // Structured queries:
//! if let Some(view) = manpage_view(&kb, "Animal") {
//!     println!("Animal has {} doc entries", view.documentation.len());
//! }
//! ```
//!
//! # Operations
//!
//! Each top-level capability is a builder with a `run()` terminal
//! that returns a typed report.  Findings (parse errors, semantic
//! warnings, prover output) ride in the report; only infrastructural
//! failures bubble out as [`SdkError`].
//!
//! | Builder | Purpose | Feature gate |
//! |---|---|---|
//! | [`IngestOp`]    | Layer KIF text into a KB (in-memory)               | always |
//! | [`ValidateOp`]  | Parse + semantic checks on KB or inline formula    | always |
//! | [`TranslateOp`] | KIF → TPTP rendering                               | always |
//! | [`LoadOp`]      | Reconcile + commit to LMDB                         | `persist` (default) |
//! | [`AskOp`]       | One proof query against the KB                     | `ask` (default) |
//! | [`TestOp`]      | Batch `.kif.tq` test runner                        | `ask` (default) |
//! | [`manpage_view`]| Symbol introspection with pre-resolved cross-refs  | always |
//!
//! # I/O posture
//!
//! The SDK doesn't open the [`KnowledgeBase`] for you — call
//! [`sigmakee_rs_core::KnowledgeBase::new`] (in-memory) or
//! [`sigmakee_rs_core::KnowledgeBase::open`] (LMDB) directly and hand the KB
//! to whichever `*Op` you're driving.  For ingest, choose per source:
//!
//! - **Already-resident text** via [`IngestOp::add_source`] — for
//!   network bodies, stdin pipes, in-memory tests.  No I/O on the
//!   SDK side.
//! - **A file path** via [`IngestOp::add_file`] — SDK opens and
//!   reads.  The path's display string becomes the source tag, so
//!   later re-ingests of the same file diff via reconcile.
//! - **A directory** via [`IngestOp::add_dir`] — SDK enumerates
//!   `*.kif` (non-recursive, sorted) and reads each.
//!
//! Mix freely in the same builder.
//!
//! # Progress events
//!
//! Long-running ops can stream events through an installed
//! [`ProgressSink`].  See the [`sigmakee_rs_core::progress`] module for the
//! event taxonomy.  Three usage shapes:
//!
//! ```no_run
//! use std::sync::Arc;
//! use sigmakee_rs_sdk::{IngestOp, ProgressEvent, ProgressSink};
//!
//! // 1. Ad-hoc closure (cheapest for one-off scripts).
//! let mut kb = sigmakee_rs_core::KnowledgeBase::new();
//! kb.set_progress_sink(Arc::new(|e: &ProgressEvent| {
//!     eprintln!("[sumo] {e:?}");
//! }));
//! IngestOp::new(&mut kb).add_file("base.kif").run().unwrap();
//! ```
//!
//! Or a struct that aggregates state — the canonical example is the
//! CLI's `PhaseAggregator` which captures `PhaseStarted` /
//! `PhaseFinished` events to time named phases.  See the [`progress`
//! module's docs][sigmakee_rs_core::progress] for the full event list.
//!
//! [progress-module]: sigmakee_rs_core::progress
//!
//! # Feature flags
//!
//! Each flag forwards 1:1 to the matching `sigmakee-rs-core` feature.
//! Activating a flag may *add* public API surface — code matching on
//! gated types must mirror the same gates.
//!
//! | Flag | Default | Adds |
//! |---|---|---|
//! | `persist` | ✓ | [`LoadOp`], LMDB-backed [`sigmakee_rs_core::KnowledgeBase::open`] |
//! | `ask` | ✓ | [`AskOp`], [`TestOp`], prover-related re-exports |
//! | `parallel` | ✓ | rayon-backed parallel hot paths inside `sigmakee-rs-core` |
//! | `integrated-prover` |   | Embedded Vampire C++ backend; implies `ask`.  Requires CMake + the `vampire-sys` submodule at build time. |
//!
//! Phase timing is **not** a feature flag.  Every `profile_span!` /
//! `profile_call!` site inside `sigmakee-rs-core` emits
//! [`ProgressEvent::PhaseStarted`] and [`ProgressEvent::PhaseFinished`]
//! through the installed sink; consumers aggregate however they want.
//!
//! # Caveats
//!
//! - **Cancellation is out of scope (v1).**  No abort token, no
//!   async API.  For long-running ops:
//!     - *Subprocess prover* (default for `AskOp`): the OS reaps the
//!       child when the calling thread is dropped — safe to
//!       spawn-and-abandon on a worker thread.
//!     - *Embedded prover* (`integrated-prover` feature): holds a
//!       global Vampire mutex.  Killing a thread mid-call would leak
//!       the mutex.  Use [`AskOp::timeout_secs`] and let the prover
//!       exit cleanly.
//!     - *Ingest / reconcile / persist*: bounded by KB size; no abort
//!       path exists in `sigmakee-rs-core`.  Worst-case mitigation is to drop
//!       the [`KnowledgeBase`] (in-memory state vanishes; LMDB state
//!       survives if a commit had already landed).
//! - **Strict mode in [`LoadOp`] is the default.**  Semantic errors
//!   abort the LMDB commit; the in-memory KB is already mutated by
//!   reconcile but disk is untouched.  Drop the KB to revert.
//!   Use [`LoadOp::strict`] with `false` to commit-anyway.
//! - **Tag stability matters for reconcile.**  [`IngestOp::add_source`]
//!   uses the supplied tag as the file identifier.  Re-ingesting the
//!   same tag with new content produces a sentence-level diff via
//!   reconcile; a different tag produces a fresh load.  Pick stable
//!   tags (paths, URIs, buffer IDs) if you want incremental updates.
//! - **Inline KIF in `IngestOp` is promoted to axiomatic status** at
//!   the end of `run()`.  Calls to `AskOp` etc. afterwards include
//!   the new sentences as hypotheses.  This matches what the CLI does;
//!   if you want session-scoped (non-axiomatic) tells, use
//!   [`sigmakee_rs_core::KnowledgeBase::tell`] directly.
//! - **Errors carry findings, success carries findings, both are
//!   useful.**  `Ok(report)` may contain semantic errors —
//!   `report.is_clean()` to short-circuit.  `Err(SdkError)` is for
//!   infrastructure failures (DB unreadable, vampire missing).  Don't
//!   pattern-match for op-level diagnostics on `Err`.
//!
//! # See also
//!
//! - [`sigmakee_rs_core`] — the core knowledge-base library this SDK wraps.
//!   Public types like [`KnowledgeBase`], [`KbError`], [`TptpLang`],
//!   [`ManPage`], [`ProgressSink`], etc. are re-exported here for
//!   convenience.

#[cfg(feature = "ask")]
pub mod ask;
pub mod error;
pub mod ingest;
#[cfg(feature = "persist")]
pub mod load;
pub mod man;
pub mod report;
#[cfg(feature = "ask")]
pub mod test;
pub mod translate;
pub mod validate;

// -- Re-exports (crate root) -------------------------------------------------

#[cfg(feature = "ask")]
pub use ask::{AskOp, ProverBackend};
pub use error::{SdkError, SdkResult};
pub use ingest::IngestOp;
#[cfg(feature = "persist")]
pub use load::LoadOp;
pub use man::{
    manpage_view, parse_doc_spans, view_from_manpage, DocBlock, DocSpan, ManPageView,
    ReferenceSet, SignatureView,
};
// Progress types come from sigmakee-rs-core verbatim — the SDK re-exports
// rather than wrapping.  Consumers see one trait + one enum across
// both layers.  See `sigmakee_rs_core::progress` for the full taxonomy.
pub use sigmakee_rs_core::{DynSink, LogLevel, ProgressEvent, ProgressSink};
pub use report::{
    IngestReport, SourceIngestStatus, TranslateReport, TranslatedSentence, ValidationReport,
};
#[cfg(feature = "persist")]
pub use report::{LoadFileStatus, LoadReport};
#[cfg(feature = "ask")]
pub use report::{AskReport, TestCaseReport, TestOutcome, TestSuiteReport};
#[cfg(feature = "ask")]
pub use test::TestOp;
pub use translate::{TranslateOp, TranslateTarget};
pub use validate::{ValidateOp, ValidateTarget};

// `KnowledgeBase` and friends are intentionally re-exported as-is —
// the SDK adds orchestration on top, not gatekeeping.  Power users
// who want raw `kb.load_kif()` access keep it.  Callers must use
// `KnowledgeBase::new()` / `KnowledgeBase::open()` directly to
// construct one (the SDK does not own the I/O).
pub use sigmakee_rs_core::{
    Findings, KbError, KnowledgeBase, ManKind, ManPage, ParentEdge, SortSig,
    SemanticError, SentenceId, TptpLang, TptpOptions,
};

// `ask`-gated re-exports: prover types the caller will need to
// construct or destructure `AskReport` / `TestCaseReport` without
// also importing `sigmakee_rs_core` directly.
#[cfg(feature = "ask")]
pub use sigmakee_rs_core::{Binding, KifProofStep, ProverStatus};
#[cfg(feature = "ask")]
pub use sigmakee_rs_core::TestCase;
#[cfg(feature = "ask")]
pub use sigmakee_rs_core::prover::ProverTimings;
