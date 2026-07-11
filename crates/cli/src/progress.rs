//! CLI progress reporting: the indicatif file-load bar, the live phase
//! spinner, the `--profile` phase-timing aggregator, and the consolidated
//! [`CliSink`] that fans events into all three.

use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

use sigmakee_rs_sdk::{DynSink, ProgressEvent, ProgressSink};
use sigmakee_rs_sdk::LogLevel;

/// Process-wide [`MultiProgress`] that serialises draws between the file-load
/// bar and the phase spinner so overlapping bars overwrite cleanly.
fn multi() -> &'static MultiProgress {
    static MP: OnceLock<MultiProgress> = OnceLock::new();
    MP.get_or_init(|| {
        let mp = MultiProgress::new();
        if !io::stderr().is_terminal() {
            mp.set_draw_target(ProgressDrawTarget::hidden());
        }
        mp
    })
}

/// Create a progress bar for loading `total` KIF files.
///
/// Returns `None` when `total == 0` so callers can skip the bar
/// entirely for empty file lists.
pub fn file_load_bar(total: u64) -> Option<ProgressBar> {
    if total == 0 {
        return None;
    }
    let bar = multi().add(ProgressBar::new(total));
    bar.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan} [{bar:40.cyan/blue}] {pos}/{len} {wide_msg}",
        )
        .unwrap()
        .progress_chars("=>-"),
    );
    Some(bar)
}

// -- Phase spinner -----------------------------------------------------------

/// Live spinner driven by `ProgressEvent::PhaseStarted` / `PhaseFinished`
/// events from the KB.
///
/// Shows the currently-active phase name (top of a nested-phase stack) and
/// flashes a one-line summary on selected high-signal events (file load, SInE
/// rebuild, clausify finished).
///
/// The underlying `ProgressBar` is created lazily on the first phase event and
/// torn down once the phase stack drains, so a later `println!` from the caller
/// does not land on a stale spinner line. When stderr is not a TTY, `try_new`
/// returns `None` and nothing draws.
pub struct PhaseSpinner {
    bar:   Mutex<Option<ProgressBar>>,
    stack: Mutex<Vec<&'static str>>,
}

impl PhaseSpinner {
    /// Build a spinner. Returns `None` when stderr isn't a TTY or the global
    /// `--ugly` flag is set. The `ProgressBar` is created lazily on the first
    /// phase event.
    pub fn try_new() -> Option<Arc<Self>> {
        if !io::stderr().is_terminal() {
            return None;
        }
        if crate::style::is_ugly() {
            return None;
        }
        Some(Arc::new(PhaseSpinner {
            bar:   Mutex::new(None),
            stack: Mutex::new(Vec::new()),
        }))
    }

    /// Build and register a fresh spinner bar with the shared `MultiProgress`.
    fn make_bar() -> ProgressBar {
        let bar = multi().add(ProgressBar::new_spinner());
        bar.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {wide_msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
        );
        bar.enable_steady_tick(Duration::from_millis(100));
        bar
    }

    /// Read the top of the phase stack and reconcile the bar's state.
    ///
    /// - empty → non-empty: spin up a fresh bar with the new phase name
    /// - non-empty → non-empty: update the existing bar's message
    /// - non-empty → empty: `finish_and_clear` the bar and drop it
    fn refresh_from_stack(&self) {
        let top: Option<&'static str> =
            self.stack.lock().ok().and_then(|s| s.last().copied());
        let mut bar_slot = match self.bar.lock() {
            Ok(g)  => g,
            Err(_) => return,
        };
        match (top, bar_slot.as_ref()) {
            (Some(name), Some(bar)) => {
                bar.set_message(name);
            }
            (Some(name), None) => {
                let bar = Self::make_bar();
                bar.set_message(name);
                *bar_slot = Some(bar);
            }
            (None, Some(_)) => {
                if let Some(bar) = bar_slot.take() {
                    bar.finish_and_clear();
                    multi().remove(&bar);
                }
            }
            (None, None) => {}
        }
    }

    /// Stop and clear the spinner if one is shown. Idempotent.
    pub fn finish(&self) {
        if let Ok(mut bar_slot) = self.bar.lock() {
            if let Some(bar) = bar_slot.take() {
                bar.finish_and_clear();
                multi().remove(&bar);
            }
        }
    }
}

impl Drop for PhaseSpinner {
    fn drop(&mut self) {
        self.finish();
    }
}

impl ProgressSink for PhaseSpinner {
    fn emit(&self, event: &ProgressEvent) {
        match event {
            ProgressEvent::PhaseStarted { name } => {
                if let Ok(mut s) = self.stack.lock() {
                    s.push(name);
                }
                self.refresh_from_stack();
            }
            ProgressEvent::PhaseFinished { name } => {
                if let Ok(mut s) = self.stack.lock() {
                    if let Some(pos) = s.iter().rposition(|n| n == name) {
                        s.remove(pos);
                    }
                }
                self.refresh_from_stack();
            }

            ProgressEvent::KifLoaded { tag, sentences, .. } => {
                self.set_summary_message(&format!("loaded {} ({} sentences)", tag, sentences));
            }
            ProgressEvent::SineRebuilt { axioms } => {
                self.set_summary_message(&format!("SInE index built ({} axioms)", axioms));
            }
            ProgressEvent::ClausifyFinished { clauses, .. } => {
                self.set_summary_message(&format!("clausified ({} clauses)", clauses));
            }
            #[cfg(feature = "ask")]
            ProgressEvent::AskInvoked { backend, .. } => {
                self.set_summary_message(&format!("proving (backend={})", backend));
            }

            _ => {}
        }
    }
}

impl PhaseSpinner {
    /// Update the bar's message only if a bar is currently shown; never spins
    /// up a new bar, so summaries arriving while the phase stack is empty are
    /// dropped.
    fn set_summary_message(&self, msg: &str) {
        if let Ok(bar_slot) = self.bar.lock() {
            if let Some(bar) = bar_slot.as_ref() {
                bar.set_message(msg.to_string());
            }
        }
    }
}

// -- The single consolidated CLI sink ----------------------------------------

/// Process-global CLI progress sink installed on the `Session`/`KnowledgeBase`,
/// fanning every event into the live phase spinner and — under `--profile` — a
/// phase-timing aggregator. Install via [`global_sink`]; report and tear down
/// via [`global`].
static GLOBAL: OnceLock<Arc<CliSink>> = OnceLock::new();

/// Build and install the process-global sink (idempotent). Call once from
/// `main` before any KB is built. The spinner is included whenever stderr is a
/// TTY and `--ugly` is off; the profiler only when `profile` is set.
pub fn init(profile: bool) {
    let spinner  = PhaseSpinner::try_new();
    let profiler = profile.then(|| Arc::new(PhaseAggregator::new()));
    let logger: Arc<LogBridgeSink> = Arc::new(LogBridgeSink);
    if spinner.is_some() || profiler.is_some() {
        let _ = GLOBAL.set(Arc::new(CliSink { spinner, profiler, logger }));
    }
}

/// The global sink as a `DynSink`, ready for `session.set_progress_sink(..)`.
/// `None` if [`init`] was never called (or installed nothing).
pub fn global_sink() -> Option<DynSink> {
    GLOBAL.get().map(|s| s.clone() as DynSink)
}

/// The global sink handle — for the end-of-run `--profile` report and spinner
/// teardown.
pub fn global() -> Option<Arc<CliSink>> {
    GLOBAL.get().cloned()
}

/// Sink that forwards `ProgressEvent::Log` variants to the `log` crate so
/// `env_logger` renders them (and `-v`, `-q`, `--config logLevel` control
/// verbosity). Structured events with their own variants are dropped.
struct LogBridgeSink;

impl ProgressSink for LogBridgeSink {
    fn emit(&self, event: &ProgressEvent) {
        if let ProgressEvent::Log { level, target, message } = event {
            let l = match level {
                LogLevel::Error => log::Level::Error,
                LogLevel::Warn  => log::Level::Warn,
                LogLevel::Info  => log::Level::Info,
                LogLevel::Debug => log::Level::Debug,
                LogLevel::Trace => log::Level::Trace,
            };
            log::log!(target: target, l, "{}", message);
        }
    }
}

/// The one CLI sink: fans each progress event into the (optional) live spinner
/// and the (optional) `--profile` aggregator.
pub struct CliSink {
    spinner:  Option<Arc<PhaseSpinner>>,
    profiler: Option<Arc<PhaseAggregator>>,
    logger:   Arc<LogBridgeSink>
}

impl ProgressSink for CliSink {
    fn emit(&self, event: &ProgressEvent) {
        if let Some(s) = &self.spinner  { s.emit(event); }
        if let Some(p) = &self.profiler { p.emit(event); }
        if matches!(event, ProgressEvent::Log { .. }) {
            self.logger.emit(event);
        }
    }
}

impl CliSink {
    /// The per-phase `--profile` report, or `None` when profiling is off.
    pub fn report(&self) -> Option<String> {
        self.profiler.as_ref().map(|p| p.report())
    }

    /// Whether the profiler recorded any phases (lets a self-reporting command
    /// suppress an empty global table).
    pub fn has_data(&self) -> bool {
        self.profiler.as_ref().is_some_and(|p| p.has_data())
    }

    /// Clear the live spinner (no-op when there isn't one).  Call before
    /// printing the final result so output doesn't land on the spinner's row.
    pub fn finish(&self) {
        if let Some(s) = &self.spinner { s.finish(); }
    }

    /// Fold a pre-measured phase duration into the `--profile` aggregator —
    /// a no-op when profiling is off.  See [`PhaseAggregator::record`].
    pub fn record(&self, name: &'static str, dur: Duration) {
        if let Some(p) = &self.profiler { p.record(name, dur); }
    }
}

/// Fold a `ProverResult::phase_profile` slice (the native prover's
/// per-mechanism saturation-loop timers — resimplify/factor/eq_resolve/
/// paramodulate/resolve, summed across every given-clause step) into the
/// global `--profile` aggregator, so they show up alongside the coarser
/// `ask.*` phases with the same total/count/average shape.
///
/// `ProverResult` carries names as owned `String`s (it derives
/// `Deserialize`, which rules out a `&'static str` field), but the native
/// prover only ever emits this fixed, known set — mapped back to `'static`
/// here rather than leaking a string per call.
pub fn record_mechanism_profile(sink: &CliSink, profile: &[(String, Duration)]) {
    for (name, dur) in profile {
        let static_name = match name.as_str() {
            "saturate.select"       => "saturate.select",
            "saturate.resimplify"   => "saturate.resimplify",
            "saturate.factor"       => "saturate.factor",
            "saturate.eq_resolve"   => "saturate.eq_resolve",
            "saturate.paramodulate" => "saturate.paramodulate",
            "saturate.resolve"      => "saturate.resolve",
            "saturate.activate"     => "saturate.activate",
            _                       => "saturate.other",
        };
        sink.record(static_name, *dur);
    }
}

// -- Phase-timing aggregator (the `--profile` half of `CliSink`) --------------

#[derive(Default)]
struct Inner {
    /// Open phases waiting for their `PhaseFinished`, keyed by phase name. A
    /// re-entrant phase pushes onto its Vec so each end matches its own start.
    starts:  HashMap<&'static str, Vec<Instant>>,
    /// Aggregate durations per phase.
    totals:  HashMap<&'static str, Duration>,
    /// Call count per phase.
    counts:  HashMap<&'static str, usize>,
}

/// `ProgressSink` that accumulates phase timings. Drive an op with it
/// installed, then call [`report`](PhaseAggregator::report).
pub struct PhaseAggregator {
    inner: Mutex<Inner>,
}

impl PhaseAggregator {
    /// Create an empty aggregator.
    pub fn new() -> Self {
        Self { inner: Mutex::new(Inner::default()) }
    }

    /// Format a per-phase report, descending by total duration.
    pub fn report(&self) -> String {
        let inner = self.inner.lock().expect("PhaseAggregator mutex poisoned");
        if inner.totals.is_empty() {
            return "(no phase events recorded — sink installed too late?)".into();
        }
        let mut entries: Vec<(&'static str, Duration, usize)> = inner.totals
            .iter()
            .map(|(name, dur)| (*name, *dur, *inner.counts.get(name).unwrap_or(&0)))
            .collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1));

        let max_name_len = entries.iter().map(|(n, _, _)| n.len()).max().unwrap_or(0);
        let mut out = String::new();
        for (name, total, count) in &entries {
            let avg = if *count > 0 { total.as_secs_f64() * 1000.0 / *count as f64 } else { 0.0 };
            out.push_str(&format!(
                "  {:<width$}  {:>10.3} ms   ({} call(s), avg {:.3} ms)\n",
                name, total.as_secs_f64() * 1000.0, count, avg,
                width = max_name_len,
            ));
        }
        out
    }

    /// `true` if any phase events were recorded.
    pub fn has_data(&self) -> bool {
        !self.inner.lock().expect("PhaseAggregator mutex poisoned").totals.is_empty()
    }

    /// Drain everything (for reuse across multiple ops).
    #[allow(dead_code)]
    pub fn reset(&self) {
        let mut inner = self.inner.lock().expect("PhaseAggregator mutex poisoned");
        inner.starts.clear();
        inner.totals.clear();
        inner.counts.clear();
    }

    /// Fold a pre-measured duration directly into a phase's total/count,
    /// bypassing the `PhaseStarted`/`PhaseFinished` pair — for callers that
    /// already hold an accumulated [`Duration`] rather than discrete
    /// start/end events (e.g. the native prover's per-mechanism
    /// saturation-loop timers, summed internally across every given-clause
    /// step and only available once the call returns).  One `record` call
    /// counts as one call for averaging purposes, same as one
    /// `PhaseStarted`/`PhaseFinished` pair — so a phase fed this way reads
    /// exactly like any other row: total, call count, average per call.
    pub fn record(&self, name: &'static str, dur: Duration) {
        let mut inner = self.inner.lock().expect("PhaseAggregator mutex poisoned");
        *inner.totals.entry(name).or_default() += dur;
        *inner.counts.entry(name).or_insert(0) += 1;
    }
}

impl Default for PhaseAggregator {
    fn default() -> Self { Self::new() }
}

impl ProgressSink for PhaseAggregator {
    fn emit(&self, event: &ProgressEvent) {
        match event {
            ProgressEvent::PhaseStarted { name } => {
                let mut inner = self.inner.lock().expect("PhaseAggregator mutex poisoned");
                inner.starts.entry(name).or_default().push(Instant::now());
            }
            ProgressEvent::PhaseFinished { name } => {
                let mut inner = self.inner.lock().expect("PhaseAggregator mutex poisoned");
                if let Some(stack) = inner.starts.get_mut(name) {
                    if let Some(start) = stack.pop() {
                        let elapsed = start.elapsed();
                        *inner.totals.entry(name).or_default() += elapsed;
                        *inner.counts.entry(name).or_insert(0) += 1;
                    }
                }
            }
            _ => {}
        }
    }
}
