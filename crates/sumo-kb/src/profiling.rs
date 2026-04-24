// crates/sumo-kb/src/profiling.rs
//
// Fine-grained per-phase profiling for KnowledgeBase operations.
//
// Usage (with `feature = "profiling"`):
//
//     let profiler = Arc::new(Profiler::new());
//     kb.set_profiler(Arc::clone(&profiler));
//     // ... do work ...
//     println!("{}", profiler.report());
//
// Without the feature, `Profiler::new()` returns a zero-sized stub,
// `record()` / `span()` are no-ops that compile away, and
// `report()` returns a single line indicating the feature was off at
// build time.  Call sites therefore never need to conditional-compile
// themselves — they always call `self.span(...)`, and the span guard
// type is `()` in the off build.
//
// Naming convention: phase keys use `bucket.phase` e.g. `load.parse`,
// `ingest.clausify`, `promote.sine_maintain`, `ask.tptp_build`.  The
// report groups by the dot-separated bucket prefix and prints them in
// stable bucket order when possible.

use std::time::Duration;
// Only referenced in `profiling`-feature paths; un-gate and the
// non-profiling build warns.  The `let _ = Instant::now;` in the
// span constructor is a separate no-op to keep that branch's
// closure capture consistent across features.
#[cfg(feature = "profiling")]
use std::time::Instant;

// -- Always-present public surface -------------------------------------------

/// Thread-safe accumulator for per-phase timings across `KnowledgeBase`
/// operations.  Install one on a `KnowledgeBase` via
/// `kb.set_profiler(Arc<Profiler>)` to collect fine-grained timings.
///
/// When built with `feature = "profiling"` the struct holds a
/// concurrent phase-table; without the feature it is zero-sized and
/// every method is a no-op.
///
/// The same `Profiler` can be shared across multiple `KnowledgeBase`
/// instances (e.g. by tests that want aggregate timings across
/// several short-lived KBs) — all updates go through an internal
/// `Mutex`.
#[derive(Debug, Default)]
pub struct Profiler {
    #[cfg(feature = "profiling")]
    inner: std::sync::Mutex<ProfilerInner>,
}

impl Profiler {
    /// Construct an empty profiler.
    pub fn new() -> Self { Self::default() }

    /// Start a new timed span for `phase`.  The returned guard
    /// records `now - start` onto the profiler when dropped.
    ///
    /// Use RAII-style:
    ///
    /// ```ignore
    /// {
    ///     let _span = profiler.span("ingest.parse");
    ///     parse(...);
    /// } // duration recorded here
    /// ```
    #[inline]
    pub fn span(&self, phase: &'static str) -> ProfileSpan<'_> {
        ProfileSpan::new(self, phase)
    }

    /// Record a single phase duration directly (no RAII guard).
    ///
    /// The most common consumer is `ProfileSpan::drop`, but callers
    /// that already have a `Duration` in hand (e.g. forwarding
    /// `ProverResult.timings.prover_run`) can use this directly.
    #[inline]
    pub fn record(&self, phase: &'static str, dur: Duration) {
        #[cfg(feature = "profiling")]
        {
            let mut guard = self.inner.lock().expect("profiler mutex poisoned");
            guard.record(phase, dur);
        }
        #[cfg(not(feature = "profiling"))]
        { let _ = (phase, dur); }
    }

    /// Reset all accumulated phase counters.
    pub fn reset(&self) {
        #[cfg(feature = "profiling")]
        {
            let mut guard = self.inner.lock().expect("profiler mutex poisoned");
            guard.phases.clear();
        }
    }

    /// Is this profiler collecting real data?  `false` when the
    /// `profiling` feature is off at build time — useful for the
    /// occasional caller that wants to skip output-side formatting
    /// work when there's nothing to report.
    #[inline]
    pub fn is_active(&self) -> bool {
        cfg!(feature = "profiling")
    }

    /// Formatted multi-line report, grouped by bucket, sorted
    /// descending by total time within each bucket.  Safe to call
    /// on a disabled profiler — returns a single-line "feature not
    /// enabled" marker in that case.
    pub fn report(&self) -> String {
        #[cfg(feature = "profiling")]
        {
            let guard = self.inner.lock().expect("profiler mutex poisoned");
            guard.format_report()
        }
        #[cfg(not(feature = "profiling"))]
        {
            "profiling: feature off at build time (recompile with \
             `--features profiling` to collect timings)".to_owned()
        }
    }

    /// Raw per-phase data (total, count, min, max) for every phase
    /// recorded so far.  Callers can use this to drive their own
    /// formatters — e.g. a CI benchmark that serialises the output
    /// as JSON for later comparison.  Empty vec when the feature is
    /// off.
    pub fn snapshot(&self) -> Vec<PhaseSnapshot> {
        #[cfg(feature = "profiling")]
        {
            let guard = self.inner.lock().expect("profiler mutex poisoned");
            guard.phases.iter()
                .map(|(name, stats)| PhaseSnapshot {
                    name:  (*name).to_owned(),
                    total: stats.total,
                    count: stats.count,
                    min:   stats.min,
                    max:   stats.max,
                })
                .collect()
        }
        #[cfg(not(feature = "profiling"))]
        { Vec::new() }
    }
}

/// One phase's accumulated statistics, as returned by [`Profiler::snapshot`].
#[derive(Debug, Clone)]
pub struct PhaseSnapshot {
    pub name:  String,
    pub total: Duration,
    pub count: usize,
    pub min:   Duration,
    pub max:   Duration,
}

// -- RAII span guard ---------------------------------------------------------

/// Guard returned by [`Profiler::span`].  Records `now - span_start`
/// on Drop.  When the `profiling` feature is off the guard is still
/// present (same type signature) but the Drop impl does nothing.
#[must_use = "span does nothing unless dropped at end of scope"]
pub struct ProfileSpan<'a> {
    #[cfg(feature = "profiling")]
    profiler: &'a Profiler,
    #[cfg(feature = "profiling")]
    phase:    &'static str,
    #[cfg(feature = "profiling")]
    start:    Instant,

    #[cfg(not(feature = "profiling"))]
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> ProfileSpan<'a> {
    #[inline]
    fn new(profiler: &'a Profiler, phase: &'static str) -> Self {
        #[cfg(feature = "profiling")]
        {
            Self { profiler, phase, start: Instant::now() }
        }
        #[cfg(not(feature = "profiling"))]
        {
            let _ = (profiler, phase);
            Self { _marker: std::marker::PhantomData }
        }
    }
}

impl Drop for ProfileSpan<'_> {
    #[inline]
    fn drop(&mut self) {
        #[cfg(feature = "profiling")]
        self.profiler.record(self.phase, self.start.elapsed());
    }
}

// -- Internals (only compiled with feature) ----------------------------------

#[cfg(feature = "profiling")]
#[derive(Debug, Default)]
struct ProfilerInner {
    /// Ordered by first-seen insertion.  Name is `&'static str` since
    /// all phase keys are string literals at call sites; that avoids
    /// an allocation per record() call.
    phases: Vec<(&'static str, PhaseStats)>,
}

#[cfg(feature = "profiling")]
#[derive(Debug, Default, Clone, Copy)]
struct PhaseStats {
    total: Duration,
    count: usize,
    min:   Duration,
    max:   Duration,
}

#[cfg(feature = "profiling")]
impl ProfilerInner {
    fn record(&mut self, phase: &'static str, dur: Duration) {
        if let Some((_, stats)) = self.phases.iter_mut().find(|(n, _)| *n == phase) {
            stats.total += dur;
            stats.count += 1;
            if dur < stats.min || stats.count == 1 { stats.min = dur; }
            if dur > stats.max                     { stats.max = dur; }
        } else {
            self.phases.push((phase, PhaseStats {
                total: dur, count: 1, min: dur, max: dur,
            }));
        }
    }

    fn format_report(&self) -> String {
        use std::fmt::Write as _;
        if self.phases.is_empty() {
            return "profiling: no phases recorded".to_owned();
        }

        // Group by bucket (prefix before first '.').
        let mut buckets: Vec<(&'static str, Vec<(&'static str, PhaseStats)>)> = Vec::new();
        for (name, stats) in &self.phases {
            let bucket = name.split_once('.').map(|(b, _)| b).unwrap_or("");
            if let Some(entry) = buckets.iter_mut().find(|(b, _)| *b == bucket) {
                entry.1.push((*name, *stats));
            } else {
                buckets.push((bucket, vec![(*name, *stats)]));
            }
        }
        // Sort phases within each bucket by total duration descending.
        for (_, phases) in buckets.iter_mut() {
            phases.sort_by(|a, b| b.1.total.cmp(&a.1.total));
        }
        // Sort buckets by the canonical lifecycle order; unknown
        // buckets (including empty-string for un-prefixed phases)
        // sink to the bottom.
        let order = ["load", "ingest", "promote", "ask"];
        buckets.sort_by_key(|(b, _)| {
            order.iter().position(|x| x == b).unwrap_or(usize::MAX)
        });

        let mut out = String::new();
        let bucket_totals: Vec<(&'static str, Duration, usize)> = buckets.iter().map(|(b, ps)| {
            let total: Duration = ps.iter().map(|(_, s)| s.total).sum();
            let count: usize    = ps.iter().map(|(_, s)| s.count).sum();
            (*b, total, count)
        }).collect();
        let grand_total: Duration = bucket_totals.iter().map(|(_, t, _)| *t).sum();

        let _ = writeln!(out, "Profile (grand total: {}, across {} recorded event{})",
                         fmt_dur(grand_total),
                         bucket_totals.iter().map(|(_, _, c)| c).sum::<usize>(),
                         if bucket_totals.iter().map(|(_, _, c)| c).sum::<usize>() == 1 { "" } else { "s" });

        for ((bucket, phases), (_, b_total, b_count)) in buckets.iter().zip(bucket_totals.iter()) {
            let bucket_label = if bucket.is_empty() { "<other>" } else { *bucket };
            let _ = writeln!(out,
                "\n[{}]  total {}  ({} event{})",
                bucket_label, fmt_dur(*b_total), b_count,
                if *b_count == 1 { "" } else { "s" });
            let _ = writeln!(out,
                "  {:<38} {:>12} {:>7} {:>12} {:>12}",
                "phase", "total", "count", "avg", "max");
            for (name, stats) in phases {
                let avg = if stats.count > 0 {
                    Duration::from_nanos((stats.total.as_nanos() / stats.count as u128) as u64)
                } else {
                    Duration::ZERO
                };
                let _ = writeln!(out,
                    "  {:<38} {:>12} {:>7} {:>12} {:>12}",
                    name,
                    fmt_dur(stats.total),
                    stats.count,
                    fmt_dur(avg),
                    fmt_dur(stats.max));
            }
        }
        out
    }
}

#[cfg(feature = "profiling")]
fn fmt_dur(d: Duration) -> String {
    let ns = d.as_nanos();
    if ns >= 1_000_000_000 { format!("{:.3} s",  d.as_secs_f64()) }
    else if ns >= 1_000_000 { format!("{:.3} ms", (ns as f64) / 1e6) }
    else if ns >= 1_000     { format!("{:.3} µs", (ns as f64) / 1e3) }
    else                    { format!("{} ns", ns) }
}

// -- Tests -------------------------------------------------------------------

#[cfg(all(test, feature = "profiling"))]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn span_records_on_drop() {
        let p = Profiler::new();
        {
            let _s = p.span("test.work");
            thread::sleep(Duration::from_millis(1));
        }
        let snap = p.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].name, "test.work");
        assert_eq!(snap[0].count, 1);
        assert!(snap[0].total >= Duration::from_millis(1));
    }

    #[test]
    fn multiple_spans_accumulate() {
        let p = Profiler::new();
        for _ in 0..3 {
            let _s = p.span("test.work");
            thread::sleep(Duration::from_micros(500));
        }
        let snap = p.snapshot();
        assert_eq!(snap[0].count, 3);
    }

    #[test]
    fn reset_clears_state() {
        let p = Profiler::new();
        { let _s = p.span("test.work"); }
        assert_eq!(p.snapshot().len(), 1);
        p.reset();
        assert!(p.snapshot().is_empty());
    }

    #[test]
    fn report_groups_by_bucket_and_orders_lifecycle() {
        let p = Profiler::new();
        { let _s = p.span("ask.tptp_build"); thread::sleep(Duration::from_micros(100)); }
        { let _s = p.span("load.parse");     thread::sleep(Duration::from_micros(100)); }
        { let _s = p.span("ingest.clausify"); thread::sleep(Duration::from_micros(100)); }
        let r = p.report();
        // load before ingest before ask
        let load_pos   = r.find("[load]").expect("load bucket");
        let ingest_pos = r.find("[ingest]").expect("ingest bucket");
        let ask_pos    = r.find("[ask]").expect("ask bucket");
        assert!(load_pos < ingest_pos);
        assert!(ingest_pos < ask_pos);
    }

    #[test]
    fn empty_profiler_report_is_safe() {
        let p = Profiler::new();
        let r = p.report();
        assert!(r.contains("no phases recorded"));
    }

    #[test]
    fn shared_profiler_is_thread_safe() {
        use std::sync::Arc;
        let p = Arc::new(Profiler::new());
        let handles: Vec<_> = (0..4).map(|_| {
            let p = Arc::clone(&p);
            thread::spawn(move || {
                for _ in 0..10 {
                    let _s = p.span("parallel.work");
                }
            })
        }).collect();
        for h in handles { h.join().unwrap(); }
        let snap = p.snapshot();
        assert_eq!(snap[0].count, 40);
    }
}

#[cfg(all(test, not(feature = "profiling")))]
mod tests_feature_off {
    use super::*;

    #[test]
    fn span_is_a_noop_when_feature_off() {
        let p = Profiler::new();
        { let _s = p.span("test.work"); }
        // No phases are recorded.
        assert!(p.snapshot().is_empty());
        // Report renders the feature-off placeholder.
        assert!(p.report().contains("feature off"));
    }

    #[test]
    fn profiler_is_zero_sized_when_feature_off() {
        assert_eq!(std::mem::size_of::<Profiler>(), 0);
    }
}
