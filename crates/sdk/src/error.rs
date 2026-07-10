//! Structured error type for the SDK.
//!
//! Infrastructural failures (config mismatch, prover spawn, unreadable file)
//! surface as [`SdkError`]. Parse/validation findings ride in the return value
//! as `Diagnostic`s rather than in `Err`.

use std::path::PathBuf;

#[cfg(feature = "git")]
use git2::Error as GitError;
use sigmakee_rs_core::Severity;

#[cfg(any(feature = "http", feature = "git"))]
use std::io::Error as IOError;

/// All errors the SDK can return.
///
/// Variants are feature-gated to match the operations they cover; code that
/// branches on them must mirror the same gates.
#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    /// An error from the underlying `sigmakee-rs-core` layer, typically a parse
    /// failure or a propagated DB error.
    #[error("KB error: {0}")]
    Kb(#[from] sigmakee_rs_core::Diagnostic),

    /// Reading a file the caller asked the SDK to ingest failed.
    ///
    /// Produced when a [`Source::Local`](crate::Source) names a file (or a
    /// directory child) the SDK cannot read.
    #[error("I/O error reading {path}: {source}")]
    Io {
        /// File path the SDK tried to read.
        path:   std::path::PathBuf,
        /// The underlying [`std::io::Error`] from the OS.
        #[source]
        source: std::io::Error,
    },

    /// Walking a directory the caller asked the SDK to ingest failed.
    /// Produced when a [`Source::Local`](crate::Source) names a directory the
    /// SDK cannot enumerate.
    #[error("cannot read directory '{path}': {message}")]
    DirRead {
        /// Directory path the SDK tried to enumerate.
        path:    std::path::PathBuf,
        /// Human-readable description of the failure.
        message: String,
    },

    /// An SDK operation was given an invalid configuration (e.g. mutually
    /// exclusive flags both set, or a required input missing). The string
    /// describes the conflict.
    #[error("invalid configuration: {0}")]
    Config(String),

    /// Promoting in-memory assertions to the LMDB store failed. The underlying
    /// error is preserved so callers can match on it.
    #[cfg(feature = "persist")]
    #[error("commit failed: {0}")]
    Persist(#[source] sigmakee_rs_core::PromoteError),

    /// The configured Vampire binary could not be located. Carries the
    /// candidate path or name.
    #[cfg(feature = "ask")]
    #[error("vampire binary not found: {0}")]
    VampireNotFound(String),

    /// The prover ran but returned an unrecoverable error before a proof or
    /// refutation could be produced. Distinct from a `ProverStatus::Unknown`
    /// outcome, which is a successful run with a "couldn't decide" verdict.
    #[cfg(feature = "ask")]
    #[error("prover failure: {0}")]
    Prover(String),

    /// The system did not allow the SDK to create local resources to 
    /// temporarily store remotely fetched sources
    #[cfg(any(feature = "http", feature = "git"))]
    #[error("failed to create temporary local resources to store remote files: {0}: {1}")]
    TempDir(PathBuf, IOError),

    /// A generic git error
    #[cfg(feature = "git")]
    #[error("There was an error using the git subsystem: {0}")]
    Git(GitError),

    /// A remote HTTP fetch error.
    #[cfg(feature = "http")]
    #[error("There was an error fetching a remote source over HTTP: {0}")]
    Http(String),

    /// The supplied file was not recognized as a valid input
    #[error("The supplied file does not conform to any known input format: {0}")]
    Input(PathBuf),

    /// The requested operation is not supported on the given session
    #[error("The requestion operation is unsupported by the current session")]
    Unsupported,

    /// Multiple tests were included with a single problem. This is currently
    /// unsupported in this context
    #[error("Multiple tests were included with a single problem source file. This is unsupported in this context")]
    MultipleProblems,

    /// No problems were found
    #[error("No problems were found in the supplied source")]
    NoProblem,
}

/// Convenience alias used throughout the SDK.
pub type SdkResult<T> = Result<T, SdkError>;

#[cfg(feature = "persist")]
impl From<sigmakee_rs_core::PromoteError> for SdkError {
    fn from(e: sigmakee_rs_core::PromoteError) -> Self {
        SdkError::Persist(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigmakee_rs_core::Diagnostic;

    #[test]
    fn infra_variants_are_hard_errors() {
        assert!(SdkError::Config("bad flag".into()).is_err());
        assert!(SdkError::Input(PathBuf::from("mystery.bin")).is_err());
        assert!(SdkError::Unsupported.is_err());
    }

    #[test]
    fn kb_error_is_classified_by_diagnostic_severity() {
        let err = Diagnostic::new_error("kind", "E000", "boom");
        assert!(SdkError::Kb(err).is_err());

        let mut warn = Diagnostic::new_error("kind", "W000", "heads up");
        warn.severity = Severity::Warning;
        assert!(!SdkError::Kb(warn).is_err());
    }

    #[test]
    fn display_carries_context() {
        assert!(SdkError::Config("oops".into()).to_string().contains("oops"));
        assert!(SdkError::Unsupported.to_string().to_lowercase().contains("unsupported"));
        assert!(SdkError::Input(PathBuf::from("f.dat")).to_string().contains("f.dat"));
    }

    #[test]
    fn from_diagnostic_yields_kb_variant() {
        let d = Diagnostic::new_error("k", "E1", "x");
        let e: SdkError = d.into();
        assert!(matches!(e, SdkError::Kb(_)));
    }
}

impl SdkError {
    /// Whether a given SDK error is a hard error that should abort processing
    pub fn is_err(&self) -> bool {
        self.severity() == Severity::Error
    }

    /// The severity this error should be reported at. Only [`SdkError::Kb`]
    /// can carry a non-`Error` severity (it forwards the wrapped
    /// [`Diagnostic`](sigmakee_rs_core::Diagnostic)'s own severity); every
    /// other variant is an infrastructural failure and is always `Error`.
    pub fn severity(&self) -> Severity {
        match self {
            SdkError::Kb(diagnostic) => diagnostic.severity,
            SdkError::Io { .. } => Severity::Error,
            SdkError::DirRead { .. } => Severity::Error,
            SdkError::Config(_) => Severity::Error,
            #[cfg(feature = "persist")]
            SdkError::Persist(_) => Severity::Error,
            #[cfg(feature = "ask")]
            SdkError::VampireNotFound(_) => Severity::Error,
            #[cfg(feature = "ask")]
            SdkError::Prover(_) => Severity::Error,
            #[cfg(any(feature = "http", feature = "git"))]
            SdkError::TempDir(_, _) => Severity::Error,
            #[cfg(feature = "git")]
            SdkError::Git(_) => Severity::Error,
            #[cfg(feature = "http")]
            SdkError::Http(_) => Severity::Error,
            SdkError::Input(_) => Severity::Error,
            SdkError::Unsupported => Severity::Error,
            SdkError::MultipleProblems => Severity::Error,
            SdkError::NoProblem => Severity::Error
        }
    }
}