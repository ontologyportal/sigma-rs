//! Generic event → cache reaction router.
//!
//! Given a set of registered reactors and a seed batch of events, it:
//!
//!   1. builds the topological level schedule from the reactors' declared
//!      `consumes` / `produces` interfaces (rejecting cycles),
//!   2. processes one level at a time, feeding each reactor only the events it
//!      declared it `consumes` from the pool accumulated so far,
//!   3. collects each reactor's follow-on events, records diagnostics, and
//!      makes the follow-ons visible to later levels (the cascade).
//!
//! Reactors within one level have no inter-dependencies, so a level is the
//! unit of parallelism: each reactor mutates only its own (interior-mutable)
//! cache store and reads the shared layer immutably.
//!
//! Layer-agnostic: this module knows nothing about which concrete caches exist
//! or which layer owns them. A layer registers its caches by building a
//! `Vec<ReactorEntry>` (see [`bind`]); that wiring lives with the layer.

#![allow(dead_code)]


use crate::Diagnostic;

use super::backends::CacheConfig;
#[cfg(any(feature = "parallel", test))]
use super::backends::plan_threads;
use super::events::{build_schedule_indexed, CycleError, Event, EventKind, ReactorDecl};

/// A cache wrapper viewed as a reactor: its static event interface plus a
/// reaction that still needs its owning layer (`Parent`) threaded in.
///
/// Not object-safe (it has an associated `Parent`); [`bind`] erases it into a
/// parent-free [`ReactorEntry`] instead.
pub(crate) trait CacheLike {
    /// The layer that owns this cache and is passed to `react`.
    type Parent;
    /// The cache's name (its `B::NAME`).
    fn name(&self) -> &'static str;
    /// Event kinds this cache reacts to.
    fn consumes(&self) -> &'static [EventKind];
    /// Event kinds this cache may emit.
    fn produces(&self) -> &'static [EventKind];
    /// Cache names this reactor's `react` reads (data-dependency edges).
    fn reads(&self) -> &'static [&'static str];
    /// Whether `react` is safe to run on disjoint shards of its event slice
    /// concurrently.
    fn event_parallel(&self) -> bool;
    /// React to `events`, returning the follow-on events to dispatch
    /// downstream. A reactor reports a warning/error by including an
    /// [`Event::Diagnostic`] in the returned vec; the router peels those off.
    fn react(
        &self,
        parent: &Self::Parent,
        events: &[&Event],
    ) -> Vec<Event>;
}

/// A reactor registered with the router: its static interface plus a reaction
/// closure with the owning layer already bound in.  Build one with [`bind`].
pub(crate) struct ReactorEntry<'a> {
    /// Unique reactor name (used as the schedule node id).
    pub name:     &'static str,
    /// Event kinds this reactor consumes.
    pub consumes: &'static [EventKind],
    /// Event kinds this reactor may produce.
    pub produces: &'static [EventKind],
    /// Cache names this reactor's `react` reads. Each names a cache whose own
    /// reactor must run first (a data edge).
    pub reads:    &'static [&'static str],
    /// Whether `react` is safe to event-shard. When `true` and the event slice
    /// is large enough, the router splits it across threads.
    pub event_parallel: bool,
    /// The bound reaction, with the owning layer already captured. Returns
    /// follow-on events (including any [`Event::Diagnostic`]s surfaced).
    ///
    /// `Send + Sync` so the router may dispatch reactors in a cohort
    /// concurrently and call one reactor from several threads on disjoint event
    /// shards.
    pub react:    Box<dyn Fn(&[&Event]) -> Vec<Event> + Send + Sync + 'a>,
}

/// Register a cache for routing by binding it to its owning layer.
///
/// `cache` and `parent` are typically `&self.<field>` and `self` (or an inner
/// layer ref) — both shared borrows of the same layer.
pub(crate) fn bind<'a, C>(cache: &'a C, parent: &'a C::Parent) -> ReactorEntry<'a>
where
    C: CacheLike + Sync + 'a,
    C::Parent: Sync + 'a,
{
    ReactorEntry {
        name:           cache.name(),
        consumes:       cache.consumes(),
        produces:       cache.produces(),
        reads:          cache.reads(),
        event_parallel: cache.event_parallel(),
        react:          Box::new(move |events| cache.react(parent, events)),
    }
}

/// The result of a cascade: what it produced and what went wrong.
///
/// Errors are **non-fatal and accumulated** — a reactor's `Err` for one event
/// kills only that event's line (no follow-on is produced from it, so its
/// downstream chain never forms) while every other event, reactor, and level
/// keeps processing.  The caller decides what to do with `errors` (log them,
/// surface as warnings, abort the enclosing operation, …).
#[derive(Debug, Default)]
pub(crate) struct RouteOutcome {
    /// Every follow-on event produced across the whole cascade.
    pub emitted: Vec<Event>,
    /// Every per-event reaction failure, in the order encountered.  Empty on a
    /// fully successful cascade.
    pub errors:  Vec<Diagnostic>,
}

/// Drive the reactive cascade.
///
/// Dispatches `seed` (and every follow-on it triggers) across `entries` in
/// topological level order, and **always runs to completion**: a reactor error
/// on one event is recorded in [`RouteOutcome::errors`] and that event's line
/// stops (it yields no follow-on), but processing of all other events continues.
///
/// Each reactor runs **once**, in a level after all of its producers, and sees
/// every event it consumes from the pool accumulated by earlier levels — so the
/// acyclic schedule guarantees it never misses a relevant event nor double-runs.
///
/// A cyclic reactor graph is the one genuinely-fatal case (nothing can run): it
/// short-circuits to an empty `emitted` with the cycle reported in `errors`.
pub(crate) fn route(
    entries: &[ReactorEntry<'_>],
    seed:    Vec<Event>,
) -> RouteOutcome {
    let decls: Vec<ReactorDecl> = entries
        .iter()
        .map(|e| ReactorDecl { name: e.name, consumes: e.consumes, produces: e.produces, reads: e.reads })
        .collect();
    route_with_schedule(entries, &build_schedule_indexed(&decls), &CacheConfig::default(), seed)
}

/// Dispatch `seed` through `entries` using a precomputed level schedule
/// (`cohorts`, as decl-index groups — see [`build_schedule_indexed`]).
///
/// Does the per-cascade dispatch only: no topological sort, no name→index map.
pub(crate) fn route_with_schedule(
    entries: &[ReactorEntry<'_>],
    cohorts: &Result<Vec<Vec<usize>>, CycleError>,
    config:  &CacheConfig,
    seed:    Vec<Event>,
) -> RouteOutcome {
    let levels = match cohorts {
        Ok(levels) => levels,
        Err(cycle) => {
            return RouteOutcome {
                emitted: Vec::new(),
                errors:  vec![Diagnostic::new_error(
                    "reactive-cache",
                    "reactor-cycle",
                    format!("reactor dependency cycle: {}", cycle.names.join(" -> ")),
                )],
            };
        }
    };
    // The schedule indexes into `entries`; a mismatch means the reactor list
    // drifted from the graph the schedule was built for.
    debug_assert!(
        levels.iter().flatten().all(|&i| i < entries.len()),
        "cached reactor schedule index out of range for {} entries", entries.len(),
    );

    // `pool` = every event visible so far (seed + follow-ons from earlier
    // levels); `emitted` = every follow-on produced across the whole cascade.
    let mut pool: Vec<Event>     = seed;
    let mut emitted: Vec<Event>  = Vec::new();
    let mut errors: Vec<Diagnostic> = Vec::new();

    for level in levels {
        // Gather each reactor's relevant events as borrows into `pool` (no
        // payload clone) and run the cohort. Scoped in a block so the `pool`
        // borrow held by `jobs` ends before the pool is mutated below. Reactors
        // with no relevant events are dropped here. Results come back in
        // schedule order, so the cascade is deterministic regardless of thread
        // timing.
        let outs: Vec<Vec<Event>> = {
            let jobs: Vec<(usize, Vec<&Event>)> = level
                .iter()
                .filter_map(|&i| {
                    let relevant: Vec<&Event> = pool
                        .iter()
                        .filter(|ev| entries[i].consumes.contains(&ev.kind()))
                        .collect();
                    (!relevant.is_empty()).then_some((i, relevant))
                })
                .collect();
            run_cohort(entries, &jobs, config)
        };

        // Follow-ons are visible only to later levels, so collect separately
        // before folding into the pool.
        let mut level_out: Vec<Event> = Vec::new();
        for out in outs {
            for ev in out {
                match ev {
                    // A diagnostic is collected, never dispatched.
                    Event::Diagnostic(diag) => errors.push(diag),
                    other                   => level_out.push(other),
                }
            }
        }
        pool.extend(level_out.iter().cloned());
        emitted.extend(level_out);
    }

    RouteOutcome { emitted, errors }
}

/// Run every reactor in one cohort and return their follow-on events in
/// schedule order (`jobs` order), computing each via [`run_reactor`].
///
/// When the cohort has more than one reactor and the total event volume clears
/// the config floor, the reactors run concurrently; each writes only its own
/// store, so this is race-free. Otherwise serial. Order is preserved either way.
fn run_cohort(
    entries: &[ReactorEntry<'_>],
    jobs:    &[(usize, Vec<&Event>)],
    config:  &CacheConfig,
) -> Vec<Vec<Event>> {
    #[cfg(not(feature = "parallel"))]
    let _ = config; // only the parallel path consults the thread cap / floor
    #[cfg(feature = "parallel")]
    {
        let total: usize = jobs.iter().map(|(_, r)| r.len()).sum();
        if jobs.len() > 1
            && plan_threads(total, config.max_threads(), config.parallel_floor()) > 1
        {
            use rayon::prelude::*;
            return jobs
                .par_iter()
                .map(|(i, relevant)| run_reactor(&entries[*i], relevant, config))
                .collect();
        }
    }
    jobs.iter()
        .map(|(i, relevant)| run_reactor(&entries[*i], relevant, config))
        .collect()
}

/// Run a single reactor over its `relevant` events, returning its follow-ons.
///
/// When the reactor opts in (`event_parallel`) and its event slice clears the
/// floor, the slice is split into disjoint shards run concurrently against the
/// reactor's own store. Sound only for reactors whose `react` is a commutative
/// per-event fold — hence the opt-in. Shard order is preserved, so follow-ons
/// stay deterministic.
fn run_reactor(
    entry:    &ReactorEntry<'_>,
    relevant: &[&Event],
    config:   &CacheConfig,
) -> Vec<Event> {
    // Opt-in per-reactor timing (`SIGMA_REACTOR_PROFILE=<min-ms>`): prints one
    // line per dispatch that takes ≥ the given threshold (ms), attributing
    // cascade cost to a cache.  Any unparsable value defaults to 1 ms.
    static PROFILE: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
    if let Some(min_ms) = *PROFILE.get_or_init(|| {
        std::env::var("SIGMA_REACTOR_PROFILE").ok().map(|v| v.parse().unwrap_or(1.0))
    }) {
        let t0  = crate::clock::Instant::now();
        let out = run_reactor_inner(entry, relevant, config);
        let ms  = t0.elapsed().as_secs_f64() * 1e3;
        if ms >= min_ms {
            eprintln!("[reactor] {:<40} {:>9.1} ms  ({} event(s))", entry.name, ms, relevant.len());
        }
        return out;
    }
    run_reactor_inner(entry, relevant, config)
}

/// The untimed dispatch body of [`run_reactor`].
fn run_reactor_inner(
    entry:    &ReactorEntry<'_>,
    relevant: &[&Event],
    config:   &CacheConfig,
) -> Vec<Event> {
    #[cfg(feature = "parallel")]
    if entry.event_parallel {
        let threads = plan_threads(relevant.len(), config.max_threads(), config.parallel_floor());
        if threads > 1 {
            use rayon::prelude::*;
            let chunk = relevant.len().div_ceil(threads);
            return relevant
                .par_chunks(chunk)
                .flat_map(|shard| (entry.react)(shard))
                .collect();
        }
    }
    let _ = config;
    (entry.react)(relevant)
}

#[cfg(test)]
mod tests {
    use super::*;
    // `react` is `Send + Sync`, so the test log uses a `Mutex` (not `RefCell`,
    // which is `!Sync`).
    use std::sync::Mutex;

    // A trivial reactor: records the events it saw and optionally emits one
    // follow-on per consumed event.
    fn entry<'a>(
        name:     &'static str,
        consumes: &'static [EventKind],
        produces: &'static [EventKind],
        log:      &'a Mutex<Vec<&'static str>>,
        emit:     Option<Event>,
    ) -> ReactorEntry<'a> {
        ReactorEntry {
            name,
            consumes,
            produces,
            reads: &[],
            event_parallel: false,
            react: Box::new(move |events| {
                log.lock().unwrap().push(name);
                events
                    .iter()
                    .filter_map(|_| emit.clone())
                    .collect()
            }),
        }
    }

    #[test]
    fn empty_seed_runs_nothing() {
        let log = Mutex::new(Vec::new());
        let entries = [entry("a", &[EventKind::RootAdded], &[], &log, None)];
        let out = route(&entries, Vec::new());
        assert!(out.emitted.is_empty());
        assert!(out.errors.is_empty());
        assert!(log.lock().unwrap().is_empty(), "no relevant events -> reactor not run");
    }

    #[test]
    fn dispatches_only_to_consumers() {
        let log = Mutex::new(Vec::new());
        let entries = [
            entry("wants_added",   &[EventKind::RootAdded],   &[], &log, None),
            entry("wants_removed", &[EventKind::RootRemoved], &[], &log, None),
        ];
        route(&entries, vec![Event::RootAdded { sid: 1 }]);
        assert_eq!(*log.lock().unwrap(), vec!["wants_added"]);
    }

    #[test]
    fn follow_on_reaches_downstream_level() {
        let log = Mutex::new(Vec::new());
        // producer consumes RootAdded, emits TaxonomyChanged; consumer consumes
        // TaxonomyChanged — so it must run in a later level off the follow-on.
        let entries = [
            entry("consumer", &[EventKind::TaxonomyChanged], &[], &log, None),
            entry("producer", &[EventKind::RootAdded], &[EventKind::TaxonomyChanged],
                  &log, Some(Event::TaxonomyChanged { syms: vec![7] })),
        ];
        let out = route(&entries, vec![Event::RootAdded { sid: 1 }]);
        assert_eq!(*log.lock().unwrap(), vec!["producer", "consumer"], "producer runs first, then consumer");
        assert_eq!(out.emitted.len(), 1, "the TaxonomyChanged follow-on was emitted");
        assert!(out.errors.is_empty());
    }

    #[test]
    fn reactor_error_is_collected_not_fatal() {
        let log = Mutex::new(Vec::new());
        // `boom` errors on the RootAdded; `survivor` also consumes RootAdded and
        // must still run — the error kills only `boom`'s line.
        let entries = [
            ReactorEntry {
                name: "boom",
                consumes: &[EventKind::RootAdded],
                produces: &[],
                reads: &[],
                event_parallel: false,
                react: Box::new(|_| vec![Event::Diagnostic(Diagnostic::new_error("t", "boom", "kaboom"))]),
            },
            entry("survivor", &[EventKind::RootAdded], &[], &log, None),
        ];
        let out = route(&entries, vec![Event::RootAdded { sid: 1 }]);
        assert_eq!(out.errors.len(), 1, "the failure is recorded, not propagated");
        assert_eq!(out.errors[0].code, "boom");
        assert_eq!(*log.lock().unwrap(), vec!["survivor"], "other reactors still process the event");
    }

    #[test]
    fn plan_threads_respects_cap_and_floor() {
        // Below the floor → serial.
        assert_eq!(plan_threads(100, 8, 512), 1);
        // At/above floor but < 2×floor → still 1 (a single task carries it).
        assert_eq!(plan_threads(900, 8, 512), 1);
        // 2×floor → 2 tasks; each carries ≥ floor.
        assert_eq!(plan_threads(1024, 8, 512), 2);
        // Capped by max_threads even with huge volume.
        assert_eq!(plan_threads(1_000_000, 4, 512), 4);
        // Threading disabled / degenerate inputs → serial.
        assert_eq!(plan_threads(10_000, 1, 512), 1);
        assert_eq!(plan_threads(10_000, 8, 0), 1);
    }

    // A reactor that records each consumed event's sid into a shared concurrent
    // set and emits one `TaxonomyChanged` follow-on per event.  Opts into
    // event-sharding so the router may split its event slice across threads.
    #[cfg(feature = "parallel")]
    fn collector<'a>(store: &'a dashmap::DashSet<u64>) -> ReactorEntry<'a> {
        ReactorEntry {
            name: "collector",
            consumes: &[EventKind::RootAdded],
            produces: &[EventKind::TaxonomyChanged],
            reads: &[],
            event_parallel: true,
            react: Box::new(move |events| {
                events
                    .iter()
                    .filter_map(|e| match e {
                        Event::RootAdded { sid } => {
                            store.insert(*sid);
                            Some(Event::TaxonomyChanged { syms: vec![*sid] })
                        }
                        _ => None,
                    })
                    .collect()
            }),
        }
    }

    /// Event-sharding (Axis B) must be observationally identical to serial: the
    /// same final store, and — because the parallel `collect` preserves shard
    /// order — the same `emitted` sequence.
    #[cfg(feature = "parallel")]
    #[test]
    fn event_sharding_matches_serial() {
        let sched: Result<Vec<Vec<usize>>, CycleError> = Ok(vec![vec![0]]);
        let seed: Vec<Event> = (0..2_000).map(|sid| Event::RootAdded { sid }).collect();

        // Serial: cap threads at 1.
        let serial_store = dashmap::DashSet::new();
        let serial_entries = [collector(&serial_store)];
        let serial_cfg = CacheConfig::default();
        serial_cfg.set_max_threads(1);
        let serial = route_with_schedule(&serial_entries, &sched, &serial_cfg, seed.clone());

        // Parallel: 8 threads, low floor so the 2 000-event slice fans out.
        let par_store = dashmap::DashSet::new();
        let par_entries = [collector(&par_store)];
        let par_cfg = CacheConfig::default();
        par_cfg.set_max_threads(8);
        par_cfg.set_parallel_floor(64);
        let parallel = route_with_schedule(&par_entries, &sched, &par_cfg, seed);

        // Same final store (no lost / duplicated updates).
        assert_eq!(serial_store.len(), 2_000);
        assert_eq!(par_store.len(), 2_000);
        let mut a: Vec<u64> = serial_store.iter().map(|r| *r).collect();
        let mut b: Vec<u64> = par_store.iter().map(|r| *r).collect();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b);

        // Deterministic emitted order despite the fan-out (`Event` isn't `Eq`,
        // so compare the projected sid sequence).
        let proj = |out: &[Event]| -> Vec<u64> {
            out.iter()
                .filter_map(|e| match e {
                    Event::TaxonomyChanged { syms } => syms.first().copied(),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(proj(&serial.emitted), proj(&parallel.emitted));
        assert_eq!(serial.emitted.len(), 2_000);
    }

    /// Reactor-parallel (Axis A): a cohort of independent reactors each writing
    /// its own store runs to the same result whether dispatched serially or
    /// concurrently — no cross-store contamination.
    #[cfg(feature = "parallel")]
    #[test]
    fn cohort_fan_out_matches_serial() {
        // Three independent reactors, all consuming RootAdded, each into its own
        // set — a single cohort (no inter-dependencies).
        let sched: Result<Vec<Vec<usize>>, CycleError> = Ok(vec![vec![0, 1, 2]]);
        let seed: Vec<Event> = (0..3_000).map(|sid| Event::RootAdded { sid }).collect();

        let run = |threads: usize| -> Vec<usize> {
            let stores = [dashmap::DashSet::new(), dashmap::DashSet::new(), dashmap::DashSet::new()];
            let entries = [collector(&stores[0]), collector(&stores[1]), collector(&stores[2])];
            let cfg = CacheConfig::default();
            cfg.set_max_threads(threads);
            cfg.set_parallel_floor(64);
            let _ = route_with_schedule(&entries, &sched, &cfg, seed.clone());
            stores.iter().map(|s| s.len()).collect()
        };

        assert_eq!(run(1), vec![3_000, 3_000, 3_000]);
        assert_eq!(run(8), vec![3_000, 3_000, 3_000]);
    }
}
