//! Output of [`crate::IngestOp::run`].

use sumo_kb::SemanticError;

/// Aggregate findings from an ingest pass.
#[derive(Debug, Default)]
pub struct IngestReport {
    /// Per-source breakout, in input order.  One entry per source the
    /// caller passed to `IngestOp::add_source`.
    pub sources: Vec<SourceIngestStatus>,

    /// Sum of `added` across every source.  Convenient when the
    /// caller just wants a single "n new sentences" number.
    pub total_added: usize,

    /// Sum of `removed`.  Non-zero when reconcile dropped sentences
    /// from previously-known tags.
    pub total_removed: usize,

    /// Sum of `retained` — sentences in a reconciled tag whose IR
    /// matched verbatim and which therefore weren't re-promoted.
    pub total_retained: usize,
}

/// One source's outcome.
#[derive(Debug)]
pub struct SourceIngestStatus {
    /// The tag the caller supplied for this source.
    pub tag: String,

    /// Sentences newly added to the KB by this source.
    pub added: usize,

    /// Sentences removed (only non-zero on the reconcile path —
    /// fresh loads have nothing to subtract from).
    pub removed: usize,

    /// Sentences whose IR matched verbatim and which were therefore
    /// retained as-is (reconcile path only).
    pub retained: usize,

    /// Semantic warnings observed while ingesting this source.
    /// Populated on the reconcile path; empty on the fresh-load path
    /// (those flow through `KbError` if any are hard failures).
    pub semantic_warnings: Vec<SemanticError>,

    /// `true` if this source took the reconcile path (its tag was
    /// already in `kb.file_roots`); `false` for a fresh load.
    pub was_reconciled: bool,
}
