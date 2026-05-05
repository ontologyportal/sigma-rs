// crates/core/src/kb/progress.rs

//! Progress / instrumentation events for the knowledge base.
//!
//! `sigmakee-rs-core` does not log directly.  Every operational milestone,
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
use super::KnowledgeBase;

use crate::progress::*;

// KB implementations (Public API exposure)

impl KnowledgeBase {
    /// Install a [`ProgressSink`] on this KB.  All
    /// internal instrumentation that previously logged via `log::*`
    /// emits structured events through this sink instead.  When no
    /// sink is installed, every emit site is a single
    /// branch-on-`Option::None` and produces no output.
    ///
    /// Sinks are `Arc`-shared so the same sink can serve multiple
    /// KBs (e.g. a daemon that opens KBs across requests).
    #[allow(dead_code)]
    pub(super) fn set_progress_sink(&mut self, sink: DynSink) {
        self.progress = Some(sink);
    }

    /// The currently-installed progress sink, if any.  Used by
    /// higher layers (e.g. `sigmakee-rs-sdk`) to emit their own variants
    /// of [`ProgressEvent`] through the same
    /// channel a consumer wired up.
    pub(super) fn progress_sink(&self) -> Option<&DynSink> {
        self.progress.as_ref()
    }

    /// Internal helper — emit an event through the installed sink,
    /// or do nothing.  `#[inline(always)]` so call sites collapse to
    /// the branch-on-None when no sink is set; the event payload is
    /// only constructed at the call site (not here), so when the
    /// branch goes None the construction is dead code.
    #[inline(always)]
    pub(super) fn emit(&self, event: ProgressEvent) {
        if let Some(sink) = &self.progress {
            sink.emit(&event);
        }
    }

    /// Internal logging helper. Emit a logging event at a given level
    /// only does something if there is an active site.
    #[inline(always)]
    pub(crate) fn log(&self, level: LogLevel, message: String) {
        self.emit(ProgressEvent::Log { level, target: "sigmakee_rs_core::kb", message });
    }

    /// Internal warning logging helper
    #[inline(always)]
    pub(crate) fn warn(&self, message: String) {
        self.log(LogLevel::Warn, message);
    }

    /// Internal warning logging helper
    #[inline(always)]
    pub(crate) fn info(&self, message: String) {
        self.log(LogLevel::Info, message);
    }

    /// Internal warning logging helper
    #[inline(always)]
    pub(crate) fn debug(&self, message: String) {
        self.log(LogLevel::Debug, message);
    }

    // Phase-span macro is declared at module level (`profile_span!`)
    // because a `fn span(&self, ...)` method would borrow all of
    // `self`, making it impossible to mutate any other field while
    // the returned guard is alive.  The macro inlines direct field
    // access to `self.profiler`, giving the borrow checker enough
    // information to see that the span only borrows that one field.
}


// Macro Definitions
// General utilities for the KB

/// Span macro for KB instrumentation.
///
/// Emits a [`ProgressEvent::PhaseStarted`] at the
/// call site and a matching [`ProgressEvent::PhaseFinished`]
/// when the returned guard drops.  When no progress sink is
/// installed on `self`, the emit sites are predicted-None branches
/// — effectively free.  Phase names are compile-time `&'static str`
/// constants so consumers can match cheaply.
///
/// Usage:
///
/// ```ignore
/// let _span = profile_span!(self, "ingest.parse");
/// // ... work ...
/// // _span drops here, emitting PhaseFinished.
/// ```
///
/// Returns a [`PhaseGuard`] (RAII).  The macro does NOT borrow `self`
/// for the guard's lifetime — only at the moment of emission — so
/// surrounding code can freely mutate other fields.
///
/// Declared BEFORE the `mod prove;` / `mod export;` / `pub mod man;`
/// sub-module declarations so those submodules can use it too.
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
/// Emits a [`ProgressEvent::PhaseStarted`] before
/// the expression evaluates and a matching [`ProgressEvent::PhaseFinished`]
/// after it completes.  Unlike [`profile_span!`], the start emit fires *and*
/// the start-borrow on `self` is released before the expression runs, so the 
/// expression can take `&mut self`.
///
/// Usage:
///
/// ```ignore
/// let r = profile_call!(self, "ask.sine_select",
///     self.sine_select_for_query(query_kif, params));
/// ```
// Only consumed by `kb/prove.rs` (gated on `ask`).  Allow-unused so
// the no-ask build doesn't warn about a macro with no callers.
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

/// Final convenience macro which installs a guard for a given scope
/// equivalent to `let _guard = SinkGuard::install(self.progress.clone())`
#[macro_export]
macro_rules! with_guard {
    ($self:ident) => {
        let _guard = $crate::progress::SinkGuard::install($self.progress.clone());
    }
}