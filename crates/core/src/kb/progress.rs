//! Progress / instrumentation events for the knowledge base.
//!
//! Every operational milestone, diagnostic anomaly, and (in dev builds)
//! per-item internal step is emitted as a [`ProgressEvent`] through an
//! installed [`ProgressSink`].  Consumers decide what to do with them:
//! forward to `log::*`, render in a TUI status bar, publish over LSP
//! `$/progress`, capture in a metrics pipeline, or ignore entirely.
//!
//! Events are reports, not yield points: an op continues regardless of what
//! the sink does, and there is no control-flow back-pressure.
//!
//! # Example
//!
//! ```rust
//! kb.set_process_sink(Arc::new(|e: &ProgressEvent| {
//!     eprintln!("[sumo] {e:?}");
//! }));
//! ```
use super::KnowledgeBase;

use crate::progress::*;

pub use crate::progress::{ProgressEvent, ProgressSink};

impl<L: crate::layer::TopLayer> KnowledgeBase<L> {
    /// Install a [`ProgressSink`] on this KB so internal instrumentation
    /// emits structured events through it.
    ///
    /// Sinks are `Arc`-shared so the same sink can serve multiple KBs.
    #[allow(dead_code)]
    pub fn set_progress_sink(&mut self, sink: DynSink) {
        self.progress = Some(sink);
    }

    /// The currently-installed progress sink, if any.
    pub fn progress_sink(&self) -> Option<&DynSink> {
        self.progress.as_ref()
    }

    /// A [`ProveCtx`] carrying this KB's sink, for proving code that lives
    /// below the KB and cannot reach `self.progress`.
    pub(crate) fn prove_ctx(&self) -> crate::progress::ProveCtx {
        crate::progress::ProveCtx::new(self.progress.clone())
    }

    /// Emit an event through the installed sink, or do nothing.
    #[inline(always)]
    pub(super) fn emit(&self, event: ProgressEvent) {
        if let Some(sink) = &self.progress {
            sink.emit(&event);
        }
    }

    /// Emit a [`ProgressEvent::Log`] at the given level.
    #[inline(always)]
    pub(crate) fn log(&self, level: LogLevel, message: String) {
        self.emit(ProgressEvent::Log { level, target: "sigmakee_rs_core::kb", message });
    }

    /// Emit a log event at [`LogLevel::Info`].
    #[inline(always)]
    pub(crate) fn info(&self, message: String) {
        self.log(LogLevel::Info, message);
    }

    /// Emit a log event at [`LogLevel::Debug`].
    #[inline(always)]
    pub(crate) fn debug(&self, message: String) {
        self.log(LogLevel::Debug, message);
    }
}

/// Span macro for KB instrumentation.
///
/// Emits a [`ProgressEvent::PhaseStarted`] at the call site and a matching
/// [`ProgressEvent::PhaseFinished`] when the returned guard drops.  Phase
/// names are compile-time `&'static str` constants.  Returns a [`PhaseGuard`]
/// (RAII); it borrows `self` only at the moment of emission, so surrounding
/// code can freely mutate other fields.
///
/// Usage:
///
/// ```ignore
/// let _span = profile_span!(self, "ingest.parse");
/// // ... work ...
/// ```
#[macro_export]
macro_rules! profile_span {
    ($self:ident, $phase:literal) => {
        $self.emit($crate::progress::ProgressEvent::PhaseStarted { name: $phase });
        let _span = $crate::progress::PhaseGuard::new($self.progress_sink().cloned(), $phase);
    };
}

/// Companion to [`profile_span!`]: gate an expression that requires
/// `&mut self` with progress emissions.
///
/// Emits a [`ProgressEvent::PhaseStarted`] before the expression evaluates and
/// a matching [`ProgressEvent::PhaseFinished`] after it completes.  The start
/// emit fires and the start-borrow on `self` is released before the expression
/// runs, so the expression can take `&mut self`.
///
/// Usage:
///
/// ```ignore
/// let r = profile_call!(self, "ask.sine_select",
///     self.sine_select_for_query(query_kif, params));
/// ```
#[allow(unused_macros)]
#[macro_export]
macro_rules! profile_call {
    ($self:ident, $phase:literal, $e:expr) => {{
        $self.emit($crate::progress::ProgressEvent::PhaseStarted { name: $phase });
        let __r = $e;
        $self.emit($crate::progress::ProgressEvent::PhaseFinished { name: $phase });
        __r
    }};
}

/// Installs a [`SinkGuard`] for the enclosing scope, equivalent to
/// `let _guard = SinkGuard::install(self.progress.clone())`.
#[macro_export]
macro_rules! with_guard {
    ($self:ident) => {
        let _guard = $crate::progress::SinkGuard::install($self.progress.clone());
    }
}