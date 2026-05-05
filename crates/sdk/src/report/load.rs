//! Output of [`crate::LoadOp::run`].

use sigmakee_rs_core::SemanticError;

/// Aggregate findings from a load + commit pass.
#[derive(Debug, Default)]
pub struct LoadReport {
    /// Per-source breakout, in input order.
    pub files: Vec<LoadFileStatus>,

    /// Sum of `added` across every source.
    pub total_added: usize,

    /// Sum of `removed` (non-zero only when reconcile dropped
    /// previously-known sentences from a tag).
    pub total_removed: usize,

    /// Sum of `retained` — sentences whose IR matched verbatim and
    /// which were therefore not re-promoted.
    pub total_retained: usize,

    /// Hard semantic errors collected across all sources.  Each
    /// entry is `(source_tag, error)` so consumers can route by
    /// origin.  Plain warnings (`SemanticError::is_warn() == true`)
    /// don't appear here — only errors that `validate_sentence`
    /// returned `Err` for.
    pub semantic_errors: Vec<(String, SemanticError)>,

    /// `true` if the LMDB commit phase ran and completed for every
    /// source.  `false` when:
    /// - strict mode (the default) blocked the commit due to
    ///   semantic errors, OR
    /// - the operation was a no-op (no sources supplied).
    /// On a `persist_reconcile_diff` failure mid-batch the operation
    /// returns `Err(SdkError::Persist)` instead — `committed` is
    /// only ever `false` here for the reasons above.
    pub committed: bool,
}

impl LoadReport {
    /// `true` iff every source was committed and no semantic errors
    /// were reported.
    pub fn is_clean(&self) -> bool {
        self.committed && self.semantic_errors.is_empty()
    }
}

/// One source's reconcile result.
#[derive(Debug)]
pub struct LoadFileStatus {
    /// Tag the source was ingested under (path display string for
    /// `add_file`/`add_dir`; caller-supplied for `add_source`).
    pub tag: String,

    /// Sentences newly added to the KB by this source.
    pub added: usize,

    /// Sentences removed (reconcile path only — non-zero when this
    /// source's tag was already present in the KB and the new text
    /// dropped some sentences).
    pub removed: usize,

    /// Sentences whose IR matched verbatim and which were retained
    /// as-is.
    pub retained: usize,

    /// Per-source semantic warnings.  Same as
    /// [`LoadReport::semantic_errors`] but scoped to this source.
    pub semantic_warnings: Vec<SemanticError>,
}

impl LoadFileStatus {
    /// `true` iff reconcile concluded that this source produced no
    /// changes (zero added, zero removed).
    pub fn is_noop(&self) -> bool {
        self.added == 0 && self.removed == 0
    }
}
