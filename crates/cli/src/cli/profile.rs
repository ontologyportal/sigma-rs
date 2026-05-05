//! Per-phase timing aggregator for `--profile`.
//!
//! Listens for `ProgressEvent::PhaseStarted` and `PhaseFinished`
//! emitted by `sigmakee-rs-core`'s instrumented code paths, captures the
//! elapsed wall-clock time of each phase, and reports per-phase
//! totals.  This is purely a CLI concern — sigmakee-rs-core itself doesn't
//! know about timing, only about phase boundaries.
//!
//! The data structure mirrors what the previous `sigmakee_rs_core::Profiler`
//! exposed: a `phase: &'static str -> total_duration: Duration` map,
//! plus a call count per phase.  `report()` formats it for
//! human consumption identical to the old format.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sigmakee_rs_core::{ProgressEvent, ProgressSink};

#[derive(Default)]
struct Inner {
    /// Open phases waiting for their `PhaseFinished`.  Keyed by
    /// phase name.  Re-entrant phases (same name nested inside
    /// itself) push onto a Vec so each end matches its own start.
    starts:  HashMap<&'static str, Vec<Instant>>,
    /// Aggregate totals.
    totals:  HashMap<&'static str, Duration>,
    /// Calls per phase.
    counts:  HashMap<&'static str, usize>,
}

/// `ProgressSink` that accumulates phase timings.  Install on a
/// `KnowledgeBase` via `kb.set_progress_sink(sink.clone())`, then
/// drive the op, then call `report()`.
pub struct PhaseAggregator {
    inner: Mutex<Inner>,
}

impl PhaseAggregator {
    pub fn new() -> Self {
        Self { inner: Mutex::new(Inner::default()) }
    }

    /// Format a per-phase report.  Phases are listed in descending
    /// total-duration order so the most expensive show first.
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

    /// Drain everything.  Useful when the same aggregator is reused
    /// across multiple ops and the caller wants per-op reports.
    #[allow(dead_code)]
    pub fn reset(&self) {
        let mut inner = self.inner.lock().expect("PhaseAggregator mutex poisoned");
        inner.starts.clear();
        inner.totals.clear();
        inner.counts.clear();
    }
}

impl Default for PhaseAggregator {
    fn default() -> Self { Self::new() }
}

impl ProgressSink for PhaseAggregator {
    fn emit(&self, event: &ProgressEvent) {
        // Only PhaseStarted / PhaseFinished interest us.  Every
        // other event is a no-op here — the aggregator coexists
        // peacefully with other sinks if multiplexed via a fan-out.
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
