// crates/core/src/kb/error.rs
//
// `KbError` is the flow-control error type returned from every
// `Result<T, KbError>` in the public sigmakee-rs-core API.  Presentation goes
// through [`crate::Diagnostic`] — `KbError` implements
// [`crate::ToDiagnostic`], and the convenience `pretty_print`
// helpers on [`KnowledgeBase`] route through that single path.

use thiserror::Error;

use super::KnowledgeBase;

// -- Span and ParseError -------------------------------------------------------
// Defined in parse::kif; re-exported here for backward compatibility.
pub use crate::parse::ParseError;
pub use crate::semantics::errors::*;

// -- KbError -------------------------------------------------------------------

/// Top-level error type for all sigmakee-rs-core operations.
#[derive(Debug, Error)]
pub enum KbError {
    #[error(transparent)]
    Parse(#[from] Box<dyn ParseError>),

    #[error(transparent)]
    Semantic(#[from] SemanticError),

    #[cfg(feature = "persist")]
    #[error("database error: {0}")]
    Db(String),

    /// The on-disk LMDB schema was created by an older build of
    /// `sigmakee-rs-core` and is not compatible with the current one.  There is
    /// no auto-migration pre-1.0 — the caller must delete the DB and
    /// re-import, or downgrade to a compatible build.  The `String`
    /// gives a short human-readable description of what was detected.
    #[cfg(feature = "persist")]
    #[error("schema migration required: {0}")]
    SchemaMigrationRequired(String),

    #[cfg(feature = "ask")]
    #[error("prover error: {0}")]
    Prover(String),

    #[error("{0}")]
    Other(String),
}

impl KbError {
    /// Render and emit this error through the unified [`crate::Diagnostic`]
    /// pipeline using `kb` for source-line context.  The `level`
    /// argument is retained for API compatibility but is no longer
    /// consulted — the diagnostic's own severity drives the log
    /// channel.
    pub fn pretty_print(&self, kb: &KnowledgeBase, _level: log::Level) {
        use crate::diagnostic::ToDiagnostic;
        self.to_diagnostic().emit(Some(kb));
    }
}

#[cfg(feature = "persist")]
impl From<heed::Error> for KbError {
    fn from(e: heed::Error) -> Self {
        KbError::Db(e.to_string())
    }
}

// -- DiagnosticSource impl ----------------------------------------------------
//
// Lets `Diagnostic::render(Some(&kb))` pull source-line context for
// any sentence id the diagnostic mentions.  Wired here (rather than in
// `diagnostic.rs`) so the trait crosses the `kb -> diagnostic` module
// boundary in the right direction.
impl crate::diagnostic::DiagnosticSource for KnowledgeBase {
    fn render_sentence(
        &self,
        sid:           crate::types::SentenceId,
        highlight_arg: i32,
    ) -> Option<String> {
        let store = &self.layer.semantic.syntactic;
        if !store.has_sentence(sid) { return None; }
        Some(format!(
            "{}",
            crate::syntactic::SentenceDisplay {
                sid,
                store,
                indent:        0,
                show_gutter:   true,
                highlight_arg,
            }
        ))
    }
}
