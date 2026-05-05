//! Structured error type for the SDK.
//!
//! Design choices:
//! - **Single enum, not per-op enums.**  Consumers (LSP, daemons,
//!   scripts) want to match-and-route on variants without a
//!   conversion layer between every call site.
//! - **`Result<T, ()>` is rejected.**  Every error path carries
//!   enough context that the consumer can format it for the user
//!   (CLI prints colourised diagnostics; LSP maps to LSP
//!   `Diagnostic`s; scripts log `Display`).
//! - **`Ok(report)` for diagnostic-bearing operations.**  A
//!   `ValidateOp::run` that finds 30 parse errors returns
//!   `Ok(ValidationReport { parse_errors: vec![...], .. })`, NOT
//!   `Err(SdkError::Parse(...))`.  Errors here are for *infrastructural*
//!   failures (config mismatch, prover spawn).  Operation-level
//!   findings ride out in the report.
//! - **Filesystem variants are opt-in for the caller.**  Most SDK
//!   operations are pure data orchestration over already-in-memory
//!   text — the caller does its own I/O.  When the caller chooses
//!   to use `IngestOp::add_file` / `add_dir`, the SDK reads on their
//!   behalf, and any failures surface as `Io` / `DirRead` here.  If
//!   you only call `add_source(tag, text)` (network / stdin / etc.)
//!   you'll never see those variants.

/// All errors the SDK can return.  Variants are feature-gated to
/// match the operations they cover — code that branches on them
/// must mirror the same gates.
#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    /// Errors from the underlying `sigmakee-rs-core` layer.  Most often this is
    /// a parse failure or a propagated DB error.  `KbError` already
    /// implements `Display` with adequate detail.
    #[error("KB error: {0}")]
    Kb(#[from] sigmakee_rs_core::KbError),

    /// Reading a file the caller asked the SDK to ingest failed.
    /// Only produced by `IngestOp::add_file` and `IngestOp::add_dir`;
    /// callers that exclusively use `add_source(tag, text)` will
    /// never see this.
    #[error("I/O error reading {path}: {source}")]
    Io {
        /// File path the SDK tried to read.
        path:   std::path::PathBuf,
        /// The underlying [`std::io::Error`] from the OS.
        #[source]
        source: std::io::Error,
    },

    /// Walking a directory the caller asked the SDK to ingest failed.
    /// Only produced by `IngestOp::add_dir`.
    #[error("cannot read directory '{path}': {message}")]
    DirRead {
        /// Directory path the SDK tried to enumerate.
        path:    std::path::PathBuf,
        /// Human-readable description of the failure.
        message: String,
    },

    /// An SDK operation was given a configuration that doesn't make
    /// sense (e.g. mutually exclusive flags both set, or a required
    /// input missing).  The string is a human-readable description
    /// of the conflict.
    #[error("invalid configuration: {0}")]
    Config(String),

    /// Promoting in-memory assertions to the LMDB store failed.  The
    /// underlying error is preserved so callers can match on it.
    #[cfg(feature = "persist")]
    #[error("commit failed: {0}")]
    Persist(#[source] sigmakee_rs_core::PromoteError),

    /// The configured Vampire binary could not be located.  Carries
    /// the candidate path or name for the consumer to surface.
    #[cfg(feature = "ask")]
    #[error("vampire binary not found: {0}")]
    VampireNotFound(String),

    /// The prover ran but returned an unrecoverable error before a
    /// proof or refutation could be produced.  Distinct from a
    /// `ProverStatus::Unknown` outcome — that's a successful run
    /// with a "couldn't decide" verdict.
    #[cfg(feature = "ask")]
    #[error("prover failure: {0}")]
    Prover(String),
}

/// Convenience alias used throughout the SDK.
pub type SdkResult<T> = Result<T, SdkError>;

#[cfg(feature = "persist")]
impl From<sigmakee_rs_core::PromoteError> for SdkError {
    fn from(e: sigmakee_rs_core::PromoteError) -> Self {
        SdkError::Persist(e)
    }
}
