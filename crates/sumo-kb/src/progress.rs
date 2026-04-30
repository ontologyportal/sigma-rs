//! Progress / instrumentation events for the knowledge base.
//!
//! `sumo-kb` does not log directly.  Every operational milestone,
//! diagnostic anomaly, and (in dev builds) per-item internal step is
//! emitted as a [`ProgressEvent`] through an installed
//! [`ProgressSink`].  Consumers decide what to do with them: forward
//! to `log::*`, render in a TUI status bar, publish over LSP
//! `$/progress`, capture in a metrics pipeline, or ignore entirely.
//!
//! # Cost model
//!
//! - **No sink installed**: every emit site is a single
//!   branch-on-`Option::None` (predicted), then nothing.  Cheaper
//!   than `log::*` (which still does an atomic max-level lookup).
//! - **Sink installed**: branch + virtual call + payload allocation.
//!   Bounded by event frequency.
//!
//! Events fired in genuinely hot paths (per-AST-element, per-symbol,
//! per-clause) are wrapped at the call site in
//! `#[cfg(debug_assertions)]` so they compile to nothing in release
//! builds.  See the variants tagged "hot-path" below.
//!
//! # Cancellation
//!
//! Events are reports, not yield points.  An op continues regardless
//! of what the sink does.  No control-flow back-pressure exists.
//!
//! # Two-layer use
//!
//! `sumo-sdk` re-exports [`ProgressSink`] and [`ProgressEvent`]
//! verbatim — consumers see one set of types.  The SDK installs its
//! own sink onto the [`crate::KnowledgeBase`] (via
//! [`crate::KnowledgeBase::set_progress_sink`]) and emits SDK-specific
//! variants directly through that same sink, so the consumer
//! observes a unified event stream.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::error::SemanticError;
use crate::types::SentenceId;

#[cfg(feature = "ask")]
use crate::prover::ProverStatus;

/// Receiver for every progress event the KB emits.  Sinks must be
/// `Send + Sync` because the KB may be used across threads (the
/// embedded prover holds a global mutex; subprocess prover spawns
/// children).  Implementations should be cheap — events are
/// delivered on the operation's hot path.
pub trait ProgressSink: Send + Sync {
    /// Called for each event.  The default operation continues
    /// regardless of what the sink does.
    fn emit(&self, event: &ProgressEvent);
}

/// Convenience: any `Fn(&ProgressEvent) + Send + Sync` is a sink.
impl<F> ProgressSink for F
where
    F: Fn(&ProgressEvent) + Send + Sync,
{
    fn emit(&self, event: &ProgressEvent) {
        self(event)
    }
}

/// Type-erased sink, the kind that lives on a [`crate::KnowledgeBase`].
pub type DynSink = Arc<dyn ProgressSink>;

// ---------------------------------------------------------------------------
// Thread-local sink propagation.
//
// Sub-components (KifStore, LmdbEnv, SineIndex, …) live below the
// KnowledgeBase and don't carry their own sinks.  Rather than thread
// an `Option<&DynSink>` through every internal call site, we expose
// a thread-local: every KB-level entry point that may dispatch to
// sub-components installs the sink as a guard that restores the
// previous value on drop.  Sub-component code emits via the
// `emit_event!` macro (or the raw `with_current_sink`).
//
// Trade-off: per-thread state is implicit, but the alternative is a
// dozen new `&Option<DynSink>` parameters.  The KB is single-thread
// at the call-site level (each method takes `&mut self` or `&self`
// from a single owner; the global Vampire mutex serialises proves);
// concurrent-thread access of sub-components requires a sink set
// per thread, which is exactly what this gives us.
// ---------------------------------------------------------------------------

use std::cell::RefCell;

thread_local! {
    static CURRENT_SINK: RefCell<Option<DynSink>> = const { RefCell::new(None) };
}

/// RAII guard installed by `KnowledgeBase` entry points.  On drop,
/// restores the previous sink (or unsets if none was installed).
/// Crate-internal: callers should use the higher-level
/// `KnowledgeBase` API.
pub(crate) struct SinkGuard {
    prev: Option<DynSink>,
}

impl SinkGuard {
    pub(crate) fn install(sink: Option<DynSink>) -> Self {
        let prev = CURRENT_SINK.with(|cell| {
            let mut slot = cell.borrow_mut();
            std::mem::replace(&mut *slot, sink)
        });
        SinkGuard { prev }
    }
}

impl Drop for SinkGuard {
    fn drop(&mut self) {
        CURRENT_SINK.with(|cell| {
            *cell.borrow_mut() = self.prev.take();
        });
    }
}

/// Run `f` with read-access to the currently-installed sink, if any.
/// Invoked from sub-component code via the `emit_event!` macro.
pub(crate) fn with_current_sink<F: FnOnce(&dyn ProgressSink)>(f: F) {
    CURRENT_SINK.with(|cell| {
        if let Some(sink) = cell.borrow().as_ref() {
            f(&**sink);
        }
    });
}

/// Internal emit helper for sub-component code.  Use directly when
/// the call site doesn't have access to a `&KnowledgeBase`.
#[macro_export]
#[doc(hidden)]
macro_rules! emit_event {
    ($event:expr) => {
        $crate::progress::with_current_sink(|s| s.emit(&$event));
    };
}

/// Phase events emitted by the KB.  `#[non_exhaustive]` — adding new
/// variants is non-breaking.  Match with `_ => {}` to stay
/// forward-compatible.
///
/// Variants are grouped by category; the ordering is purely
/// editorial.  Names are terse; the type's namespace
/// (`ProgressEvent::*`) carries the qualification.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ProgressEvent {
    // -- KB lifecycle --------------------------------------------------------

    /// LMDB-backed KB opened from disk.
    KbOpened          { path: PathBuf, formulas: usize, dedup_enabled: bool },

    /// Schema migration detected on open.
    SchemaMismatch    { detail: String },

    /// New session started or asserted into.
    SessionTold       { session: String, formulas: usize },

    /// Session promoted to axiomatic status.
    SessionPromoted   { session: String, promoted: usize, duplicates: usize },

    /// Session flushed (sentences removed from the KB).
    SessionFlushed    { session: String, removed: usize },

    // -- Reconcile -----------------------------------------------------------

    /// One file's reconcile pass finished.  `is_noop` is `true` iff
    /// `added == 0 && removed == 0`.
    Reconciled        { tag: String, added: usize, removed: usize, retained: usize, is_noop: bool },

    // -- Persist -------------------------------------------------------------

    /// LMDB write transaction committed.
    Committed         { added: usize, removed: usize, elapsed: Duration },

    /// One sentence's row was deleted from LMDB.
    PersistDeleted    { sid: SentenceId },

    /// A duplicate axiom was dropped during promote.
    DuplicateDropped  { sid: SentenceId },

    /// hot-path: per-symbol DB write (gated behind `cfg(debug_assertions)`).
    PersistedSymbol   { name: String, id: u64, was_present: bool },

    /// hot-path: per-formula DB write.
    PersistedFormula  { id: u64 },

    /// hot-path: per-clause hash interned.
    PersistedClause   { hash: u64, id: u64, was_present: bool },

    // -- Parse & ingest ------------------------------------------------------

    /// File ingested into the KB (parse → intern → axiomatic).
    KifLoaded         { tag: String, sentences: usize, errors: usize },

    /// hot-path: tokenizer finished one file.
    Tokenized         { tag: String, tokens: usize, errors: usize },

    /// hot-path: symbol interned.
    SymbolInterned    { name: String, id: u64 },

    /// hot-path: sentence allocated.
    SentenceAllocated { sid: SentenceId },

    /// hot-path: AST element built.
    ElementBuilt,

    /// Macro expansion (row variable).
    MacroExpanded     { input: String, output_count: usize },

    /// Sentence pruned from the store (e.g. orphaned symbol).
    SentencesPruned   { kept: usize, dropped: usize },

    // -- SInE / clausify -----------------------------------------------------

    /// SInE index rebuilt.  `axioms` is the count of SInE-eligible
    /// axioms after rebuild.
    SineRebuilt       { axioms: usize },

    /// SInE incremental update.  `delta` is the count added since
    /// the last rebuild; `total` is the running axiom count.
    SineIncremental   { delta: usize, total: usize },

    /// CNF clausification pass started for a batch.
    ClausifyStarted   { sentences: usize },

    /// CNF clausification finished.
    ClausifyFinished  { clauses: usize, elapsed: Duration },

    // -- Prover (cfg(feature = "ask")) ---------------------------------------

    /// Ask query started.  `backend` is `"subprocess"` or `"embedded"`.
    #[cfg(feature = "ask")]
    AskInvoked        { backend: &'static str, query: String },

    /// Ask query returned.
    #[cfg(feature = "ask")]
    AskReturned       { status: ProverStatus, elapsed: Duration },

    /// Vampire subprocess spawned.
    #[cfg(feature = "ask")]
    ProverSpawned     { binary: PathBuf, timeout_secs: u32 },

    // -- Diagnostics (warnings) ---------------------------------------------

    /// Domain assertion couldn't be checked (warning).
    DomainCheckFailed { detail: String },

    /// Symbol redefined (warning).
    SymbolRedefined   { name: String },

    /// Generic warning surfaced from the parse / semantic layer.
    /// `detail` carries the human-readable explanation; consumers
    /// who want strongly-typed diagnostics should inspect
    /// `SemanticError` from the operation's report instead.
    Warning           { code: &'static str, detail: String },

    /// Hard semantic error surfaced through the event stream
    /// (in addition to the `Result` path).  Carried by-clone so
    /// consumers needn't lock on the KB to read it.
    SemanticErrorEv   { error: Box<SemanticError> },

    // -- Phase timing (replaces the former Profiler) ------------------------

    /// A named phase in the KB's work has started.  Emitted by the
    /// RAII span machinery in `kb/mod.rs::profile_span!`.  Consumers
    /// who want timing capture `Instant::now()` here, then look for
    /// the matching [`Self::PhaseFinished`] (same `name`).  Phase
    /// names are compile-time constants so they can be matched cheaply.
    PhaseStarted     {
        /// Compile-time phase identifier (e.g. `"ingest.parse"`).
        name: &'static str,
    },

    /// A named phase has finished.  Pairs with [`Self::PhaseStarted`]
    /// of the same `name`.  No `elapsed` field — the consumer that
    /// cares does the subtraction; consumers that don't care
    /// (status-bar UIs etc.) can ignore this entirely.
    PhaseFinished    {
        /// Compile-time phase identifier matching the prior `PhaseStarted`.
        name: &'static str,
    },

    // -- SDK-level (emitted by sumo-sdk through this same sink) -------------

    /// SDK read a file from disk.
    FileRead          { path: PathBuf, idx: usize, total: usize, bytes: usize },

    /// SDK started a multi-source load / ingest pass.  `total_sources`
    /// is the count after dir-expansion.
    LoadStarted       { total_sources: usize },

    /// SDK ingested one source through `IngestOp` / `LoadOp`.
    SourceIngested    { tag: String, added: usize, removed: usize, retained: usize },

    /// SDK promote phase started.
    PromoteStarted    { session: String },

    /// SDK promote phase finished.
    PromoteFinished   { promoted: usize, duplicates: usize, elapsed: Duration },

    /// SDK started an ask op (mirrors `AskInvoked` from sumo-kb but
    /// fires from the SDK builder layer; both may appear in one run).
    AskStarted     { backend: &'static str },

    /// SDK ask op returned.
    #[cfg(feature = "ask")]
    AskFinished    { status: ProverStatus, elapsed: Duration },

    /// SDK test-case completed.
    TestCase { idx: usize, total: usize, tag: String, brief: &'static str },

    // -- Generic fallback ---------------------------------------------------

    /// Fallback for instrumentation that doesn't yet have a typed
    /// variant.  `target` mirrors the `log::*` `target:` field;
    /// `level` is the original severity; `message` is the formatted
    /// payload.  Consumers can route these into a `log::*` macro to
    /// reproduce the legacy behaviour:
    ///
    /// ```ignore
    /// fn forward(e: &ProgressEvent) {
    ///     if let ProgressEvent::Log { level, target, message } = e {
    ///         log::log!(target: target, *level, "{}", message);
    ///     }
    /// }
    /// ```
    ///
    /// Over time, high-value sites should be migrated to typed
    /// variants above.  This variant is the escape hatch for
    /// everything else.
    Log {
        level:   LogLevel,
        target:  &'static str,
        message: String,
    },
}

/// Severity tag for the fallback [`ProgressEvent::Log`] variant.
/// Mirrors `log::Level` but is owned by sumo-kb so consumers don't
/// have to import the `log` crate just to match on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel { Trace, Debug, Info, Warn, Error }
