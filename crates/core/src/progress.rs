//! Incremental progress callback dispatch and registration.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::semantics::errors::SemanticError;
use crate::types::SentenceId;

#[cfg(feature = "ask")]
use crate::prover::ProverStatus;

/// RAII guard returned by [`profile_span!`] that emits `PhaseFinished` on drop.
pub struct PhaseGuard {
    sink: Option<DynSink>,
    name: &'static str,
}

impl PhaseGuard {
    /// Create a guard that emits `PhaseFinished { name }` through `sink` on drop.
    #[inline]
    pub fn new(sink: Option<DynSink>, name: &'static str) -> Self {
        Self { sink, name }
    }
}

impl Drop for PhaseGuard {
    #[inline]
    fn drop(&mut self) {
        if let Some(sink) = &self.sink {
            sink.emit(&ProgressEvent::PhaseFinished { name: self.name });
        }
    }
}

/// Receiver for every progress event the KB emits.
///
/// Sinks must be `Send + Sync` and should be cheap — events are delivered on
/// the operation's hot path.
pub trait ProgressSink: Send + Sync {
    /// Handle one event. The operation continues regardless of what the sink does.
    fn emit(&self, event: &ProgressEvent);
}

/// Any `Fn(&ProgressEvent) + Send + Sync` is a sink.
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

/// Cheaply-clonable carrier for the progress/log sink, passed into proving
/// code below the `KnowledgeBase`.
///
/// Exposes the same `emit` / `progress_sink` surface as the KB, so the
/// [`profile_span!`](crate::profile_span) / [`profile_call!`](crate::profile_call)
/// macros work with a `ProveCtx` in place of `self`. `Clone + Send + Sync`, so a
/// single `ProveCtx` can be shared across worker threads.
#[derive(Clone, Default)]
pub struct ProveCtx {
    sink: Option<DynSink>,
}

impl ProveCtx {
    /// A context with no sink; every emit is a no-op.
    pub fn none() -> Self {
        Self { sink: None }
    }

    /// Wrap an existing sink.
    pub fn new(sink: Option<DynSink>) -> Self {
        Self { sink }
    }

    /// Emit one event through the installed sink, or do nothing.
    #[inline(always)]
    pub fn emit(&self, event: ProgressEvent) {
        if let Some(sink) = &self.sink {
            sink.emit(&event);
        }
    }

    /// The installed sink, if any.
    #[inline(always)]
    pub fn progress_sink(&self) -> Option<&DynSink> {
        self.sink.as_ref()
    }

    /// Emit a log event at `level`.
    #[inline(always)]
    pub fn log(&self, level: LogLevel, message: String) {
        self.emit(ProgressEvent::Log { level, target: "sigmakee_rs_core::kb", message });
    }

    /// Emit an info-level log event.
    #[inline(always)]
    pub fn info(&self, message: String) {
        self.log(LogLevel::Info, message);
    }

    /// Emit a debug-level log event.
    #[inline(always)]
    pub fn debug(&self, message: String) {
        self.log(LogLevel::Debug, message);
    }
}

// -- Thread-local sink propagation -------------------------------------------

use std::cell::RefCell;

thread_local! {
    static CURRENT_SINK: RefCell<Option<DynSink>> = const { RefCell::new(None) };
}

/// RAII guard that installs a sink for the current thread and restores the
/// previous one on drop.
pub(crate) struct SinkGuard {
    prev: Option<DynSink>,
}

impl SinkGuard {
    /// Install `sink` for the current thread, returning a guard that restores
    /// the previous sink on drop.
    pub(super) fn install(sink: Option<DynSink>) -> Self {
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
pub fn with_current_sink<F: FnOnce(&dyn ProgressSink)>(f: F) {
    CURRENT_SINK.with(|cell| {
        if let Some(sink) = cell.borrow().as_ref() {
            f(&**sink);
        }
    });
}

/// Emit an event through the current thread-local sink.
///
/// For sub-component code that has no access to a `&KnowledgeBase`.
#[macro_export]
#[doc(hidden)]
macro_rules! emit_event {
    ($event:expr) => {
        $crate::progress::with_current_sink(|s| s.emit(&$event));
    };
}

/// Progress events emitted by the KB.
///
/// `#[non_exhaustive]`: match with `_ => {}` to handle unknown variants.
/// Variants are grouped by category.
#[allow(dead_code)]
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

    /// Per-symbol DB write (gated behind `cfg(debug_assertions)`).
    PersistedSymbol   { name: String, id: u64, was_present: bool },

    /// Per-formula DB write.
    PersistedFormula  { id: u64 },

    /// Per-clause hash interned.
    PersistedClause   { hash: u64, id: u64, was_present: bool },

    // -- Parse & ingest ------------------------------------------------------

    /// File ingested into the KB (parse → intern → axiomatic).
    KifLoaded         { tag: String, sentences: usize, errors: usize },

    /// Tokenizer finished one file.
    Tokenized         { tag: String, tokens: usize, errors: usize },

    /// Symbol interned.
    SymbolInterned    { name: String, id: u64 },

    /// Sentence allocated.
    SentenceAllocated { sid: SentenceId },

    /// AST element built.
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

    // -- Phase timing --------------------------------------------------------

    /// A named phase in the KB's work has started. Consumers that want timing
    /// capture `Instant::now()` here and pair with the matching
    /// [`Self::PhaseFinished`] (same `name`).
    PhaseStarted     {
        /// Compile-time phase identifier (e.g. `"ingest.parse"`).
        name: &'static str,
    },

    /// A named phase has finished. Pairs with [`Self::PhaseStarted`] of the
    /// same `name`. Consumers do their own elapsed-time subtraction.
    PhaseFinished    {
        /// Compile-time phase identifier matching the prior `PhaseStarted`.
        name: &'static str,
    },

    // -- SDK-level -----------------------------------------------------------

    /// SDK read a file from disk.
    FileRead          { path: PathBuf, idx: usize, total: usize, bytes: usize },

    /// SDK started a multi-source load / ingest pass. `total_sources` is the
    /// count after directory expansion.
    LoadStarted       { total_sources: usize },

    /// SDK ingested one source.
    SourceIngested    { tag: String, added: usize, removed: usize, retained: usize },

    /// SDK promote phase started.
    PromoteStarted    { session: String },

    /// SDK promote phase finished.
    PromoteFinished   { promoted: usize, duplicates: usize, elapsed: Duration },

    /// SDK started an ask op.
    AskStarted     { backend: &'static str },

    /// SDK ask op returned.
    #[cfg(feature = "ask")]
    AskFinished    { status: ProverStatus, elapsed: Duration },

    /// SDK test-case completed.
    TestCase { idx: usize, total: usize, tag: String, brief: &'static str },

    // -- Generic fallback ---------------------------------------------------

    /// Fallback for instrumentation that has no typed variant. `target`
    /// mirrors the `log::*` `target:` field; `level` is the severity;
    /// `message` is the formatted payload. Consumers can route these into a
    /// `log::*` macro:
    ///
    /// ```ignore
    /// fn forward(e: &ProgressEvent) {
    ///     if let ProgressEvent::Log { level, target, message } = e {
    ///         log::log!(target: target, *level, "{}", message);
    ///     }
    /// }
    /// ```
    Log {
        level:   LogLevel,
        target:  &'static str,
        message: String,
    },
}

/// Severity tag for the fallback [`ProgressEvent::Log`] variant.
///
/// Mirrors `log::Level` but is owned by this crate so consumers needn't import
/// the `log` crate to match on it.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel { Trace, Debug, Info, Warn, Error }

/// Emit a [`ProgressEvent::Log`] through the current thread-local sink.
#[macro_export]
macro_rules! log {
    ($level:ident, $target:literal, $message:expr) => {
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::$level, target: $target, message: $message });
    };
}