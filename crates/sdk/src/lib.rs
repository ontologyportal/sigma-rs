//! `sigmakee-rs-sdk` — a programmatic Rust API over [`sigmakee_rs_core`].
//!
//! The SDK's single entry point is a [`Session`]: pick a [`Backend`] — the
//! native saturation prover, an external subprocess prover, or
//! translation-only — then ingest / assert / prove / test / translate /
//! validate against it.  No clap, no stdout, no exit codes: the SDK is meant to
//! be embedded inside larger applications (language servers, daemons, custom
//! CLIs, scripted pipelines, test harnesses).
//!
//! Inputs come from a [`Source`]: a local file or directory, an open reader
//! (e.g. stdin), or — behind the `http` / `git` features — a remote URL or
//! repository.  The parser is auto-detected from each file's name and content.
//!
//! ```no_run
//! use sigmakee_rs_sdk::{Session, Source};
//! # #[cfg(feature = "native-prover")] {
//! use sigmakee_rs_core::{NativeOpts, ProverLayer};
//!
//! let mut s = Session::<ProverLayer>::new("demo".into());        // native prover backend
//! let errs = s.ingest(Source::Local(vec!["Merge.kif".into()]), true);  // file or dir
//! assert!(errs.is_empty());
//! let r = s.ask("(instance Rex Animal)", Some(NativeOpts::default())).unwrap();
//! println!("{:?}", r.status);
//! # }
//! ```
//!
//! # I/O posture
//!
//! Operations return core types directly — proofs as `ProverResult`,
//! diagnostics as [`Diagnostic`], TPTP as `String`, test verdicts as
//! `TestCaseOutcome`.  *Findings* (parse errors, semantic warnings, prover
//! verdicts) ride in the returned value; only *infrastructural* failures
//! (unreadable file, parse failure, missing prover) surface as [`SdkError`].
//!
//! The SDK never opens a [`KnowledgeBase`] behind your back — `Session::new`
//! constructs an in-memory one, and `Session::open` attaches an LMDB-backed
//! store.  Inline KIF asserted via `Session::tell` / [`Session::ingest`] is
//! promoted to axiomatic status, matching the CLI.
//!
//! Symbol introspection is a session op too: [`Session::manpage`]
//! projects a [`ManPage`] with cross-references pre-resolved into typed
//! [`DocSpan`]s, so consumers never parse the `&%Symbol` marker syntax.
//!
//! # Progress events
//!
//! Install a sink with [`Session::set_progress_sink`]; both core and SDK
//! operations emit through it (HTTP fetches, load phases, prover spans).  See
//! the [`sigmakee_rs_core::progress`] module for the event taxonomy.
//!
//! # Feature flags
//!
//! | Flag | Default | Adds |
//! |---|---|---|
//! | `persist` | ✓ | LMDB-backed `Session::open` / `Session::load` |
//! | `ask` | ✓ | the external-prover `Backend::External` selector |
//! | `native-prover` |   | the in-process `Backend::Native` saturation prover + the proving ops (`ask` / `tell` / `audit` / `test`) |
//! | `parallel` | ✓ | rayon-backed hot paths inside `sigmakee-rs-core` |
//! | `http` | ✓ | `Source::Http` remote fetch (native targets only) |
//! | `git` | ✓ | `Source::Git` sparse checkout (native targets only) |
//!
//! # See also
//!
//! - [`sigmakee_rs_core`] — the core knowledge-base library this SDK wraps.
//!   Public types ([`KnowledgeBase`], [`Diagnostic`], [`TptpLang`], [`ManPage`],
//!   [`ProgressSink`], …) are re-exported here for convenience.

// `http` (ureq, blocking sockets) and `git` (git2/libgit2 C bindings) cannot
// build on `wasm32`; refuse to compile rather than fail deep in a dependency.
#[cfg(all(feature = "http", target_arch = "wasm32"))]
compile_error!(
    "The 'http' feature (ureq) is not supported on wasm32 targets. \
     Browser wasm cannot do blocking HTTP — fetch in JS and ingest the bytes \
     via `Source::Reader`. Remove 'http' from the SDK features for wasm builds."
);
#[cfg(all(feature = "git", target_arch = "wasm32"))]
compile_error!(
    "The 'git' feature (git2/libgit2) is not supported on wasm32 targets. \
     Remove 'git' from the SDK features for wasm builds."
);

pub mod error;
pub mod freshness;
pub mod session;
pub mod source;
pub mod manager;

// -- Re-exports (crate root) -------------------------------------------------

pub use error::{SdkError, SdkResult};
pub use freshness::{
    check_freshness, check_local_freshness, check_git_freshness,
    snapshot_git_tracked, check_git_tracked, Freshness, FreshnessReport,
};
pub use session::man::{
    parse_doc_spans, view_from_manpage, DocBlock, DocSpan, ManPageView,
    ReferenceSet, SignatureView,
};
pub use sigmakee_rs_core::{DynSink, LogLevel, ProgressEvent, ProgressSink};

pub use session::{Backend, Session};
#[cfg(feature = "native-prover")]
pub use session::{ExpectedOutcome, OpenSession, SzsStatus, TestCaseOutcome, TestOutcome};
pub use source::Source;

pub use sigmakee_rs_core::{
    Diagnostic, Findings, KnowledgeBase, ManKind, ManPage, ParentEdge, SemanticError, SentenceId,
    SortSig, TptpLang, TptpOptions,
};

// Layer stack: the concrete top layers plus the traits downstream backend
// dispatch bounds on.
pub use sigmakee_rs_core::{HasTranslation, ProvingLayer, TopLayer, TranslationLayer};
#[cfg(feature = "native-prover")]
pub use sigmakee_rs_core::ProverLayer;
#[cfg(feature = "ask")]
pub use sigmakee_rs_core::ExternalProverLayer;

// Parsing / AST surface: document parsing, the KIF renderer, and the node types.
pub use sigmakee_rs_core::{
    parse_document, parse_test_content, AstKif, AstNode, DocEntry, Parser, SourceFile, Span,
};

// Diagnostics machinery beyond the `Diagnostic` type itself.
pub use sigmakee_rs_core::{
    promote_to_error, set_all_errors, Severity, TellResult, ToDiagnostic,
};

// Proof-source indexing + search + shared prover opts.
pub use sigmakee_rs_core::{AxiomSource, AxiomSourceIndex, CommonProverOpts, SearchOpts, SearchSource};
#[cfg(feature = "ask")]
pub use sigmakee_rs_core::RenderReport;
#[cfg(feature = "native-prover")]
pub use sigmakee_rs_core::Strategy;

// The whole prover module (backends, runners, result types) for path-style
// access (`sigmakee_rs_sdk::prover::external::backends::…`).
pub use sigmakee_rs_core::prover;

// The external-backend selector + the trait for plugging in a custom runner.
#[cfg(feature = "ask")]
pub use sigmakee_rs_core::Prover;
#[cfg(feature = "ask")]
pub use sigmakee_rs_core::prover::ProverRunner;

// Prover-facing types for the native-prover proving ops.
#[cfg(feature = "native-prover")]
pub use sigmakee_rs_core::{
    Binding, KifProofStep, NativeOpts, ProverResult, ProverStatus, SineParams, TestCase,
};

// Proof-emission dialect seam — lets callers (e.g. the `--proof tptp` CLI
// path) reconstruct a TPTP transcript from `proof_kif` when a backend has no
// verbatim prover transcript of its own (native `ProverLayer`, embedded FFI
// Vampire).
#[cfg(feature = "native-prover")]
pub use sigmakee_rs_core::Emitter;
#[cfg(feature = "native-prover")]
pub use sigmakee_rs_core::prover::proof::emit_proof;
#[cfg(feature = "native-prover")]
pub use sigmakee_rs_core::prover::ProverTimings;
