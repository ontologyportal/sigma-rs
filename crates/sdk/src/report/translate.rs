//! Output of [`crate::TranslateOp::run`].

use sigmakee_rs_core::{SemanticError, SentenceId};

/// Findings from a KIF→TPTP translation.
#[derive(Debug, Default)]
pub struct TranslateReport {
    /// The full TPTP output.  For whole-KB translation this is the
    /// concatenated `to_tptp(...)` string; for inline-formula
    /// translation it's the per-sentence TPTP output joined with `\n`.
    /// Either form is directly write-to-file ready.
    pub tptp: String,

    /// Per-sentence breakout, populated only when an inline formula
    /// was translated.  Empty for whole-KB translation — that path
    /// emits a single combined string and consumers don't typically
    /// need the per-sentence view.
    pub sentences: Vec<TranslatedSentence>,

    /// Semantic warnings observed during translation.  These are
    /// surfaced even on the success path because TPTP output for
    /// semantically-suspect KIF is still produced (warnings, after
    /// all, are not errors) and consumers may want to flag them.
    pub semantic_warnings: Vec<(SentenceId, SemanticError)>,

    /// Set when the translation ingested an inline formula into a
    /// session.  Consumers may discard via `kb.flush_session(...)`.
    pub session: Option<String>,
}

/// One sentence's translation, surfaced when translating an inline
/// formula so consumers can correlate KIF source with TPTP output.
#[derive(Debug, Clone)]
pub struct TranslatedSentence {
    /// Stable [`SentenceId`] the SDK ingested this sentence under.
    pub sid:  SentenceId,
    /// The original KIF text for this sentence.
    pub kif:  String,
    /// TPTP rendering of the same sentence.
    pub tptp: String,
}
