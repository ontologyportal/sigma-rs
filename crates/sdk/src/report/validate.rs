//! Output of [`crate::ValidateOp::run`].

use sigmakee_rs_core::{KbError, SemanticError, SentenceId};

/// Findings from a validation pass.
///
/// `Ok(ValidationReport)` is returned even when the report contains
/// errors — the operation *ran*, and these are the things the
/// consumer should surface.  `Err(SdkError)` is reserved for
/// infrastructural failure (e.g. KB couldn't be opened).
///
/// # Severity classification
///
/// `sigmakee-rs-core` no longer auto-prints any semantic finding.  Every
/// finding from a validation pass lands in this report — pre-
/// classified by [`sigmakee_rs_core::SemanticError::is_warn`], which honours
/// the global `-Wall` / `-W <code>` / `-q` flags.  Consumers
/// (CLI, LSP, TUIs) decide how to render each side independently.
#[derive(Debug, Default)]
pub struct ValidationReport {
    /// Hard semantic errors discovered.  Each entry is paired with
    /// the `SentenceId` it was raised against so consumers can map
    /// back to source spans via `kb.sentence(sid)`.
    ///
    /// A non-empty list means [`Self::is_clean`] returns `false` —
    /// load commands should treat this as an abort condition.
    pub semantic_errors: Vec<(SentenceId, SemanticError)>,

    /// Semantic warnings discovered.  Same `(SentenceId, Error)`
    /// shape as `semantic_errors` but classified as advisory by
    /// `is_warn()`.  An advisory list does NOT make the report
    /// "unclean" — match on `warnings.is_empty()` separately if
    /// you need a stricter check.
    pub semantic_warnings: Vec<(SentenceId, SemanticError)>,

    /// Parse failures.  Empty for the whole-KB pass (the KB can't
    /// hold un-parsed sentences); populated when `ValidateOp::formula`
    /// is given inline KIF that fails to parse.
    pub parse_errors: Vec<KbError>,

    /// Number of sentences inspected by this pass.  For
    /// `ValidateOp::all` this is the whole-KB count; for
    /// `ValidateOp::formula` it's the count of sentences ingested
    /// from the inline text.
    pub inspected: usize,

    /// Set when the operation ingested an inline formula into a
    /// session for validation.  Consumers may want to discard the
    /// session afterwards via `kb.flush_session(...)` if they're
    /// keeping the KB around.
    pub session: Option<String>,
}

impl ValidationReport {
    /// `true` iff no parse errors and no hard semantic errors were
    /// found.  **Warnings do not affect cleanliness** — a clean
    /// report can still carry advisory warnings the consumer may
    /// want to surface.
    pub fn is_clean(&self) -> bool {
        self.parse_errors.is_empty() && self.semantic_errors.is_empty()
    }

    /// Total finding count: parse errors + hard semantic errors +
    /// semantic warnings.  Useful for "did this pass find anything
    /// at all?" UI prompts.
    pub fn total_findings(&self) -> usize {
        self.parse_errors.len()
            + self.semantic_errors.len()
            + self.semantic_warnings.len()
    }
}
