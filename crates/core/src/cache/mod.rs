// crates/core/src/cache.rs
//
// Reactive cache + event subsystem.  This module root wires together four
// submodules and re-exports their public surface so callers keep using
// `crate::cache::{EntryCache, Cache, CacheBehavior, …}` unchanged:
//
//   - `backends`  — storage/concurrency primitives (`EntryCache`, `LayerCache`,
//                   `EagerIndex`, `CacheConfig`, `Epoch`).  Knows how to
//                   memoise, shard locks, detect cycles, and honour the
//                   `CacheConfig` enable flag — nothing about value meaning.
//   - `behaviors` — the per-cache behavior traits (`CacheBehavior`,
//                   `WholeCacheBehavior`, `EagerBehavior`, `EagerMapBehavior`).
//   - `frontends` — the cache wrapper types (`Cache`, `WholeCache`, `Eager`,
//                   `EagerMap`) pairing a behavior with its backing store.
//   - `events`    — the cross-layer change-event model (`Event`, `EventKind`)
//                   and the reactor schedule (`build_schedule`).
//
// CacheConfig: every cache holds a clone of an `Arc<RwLock<…>>`-backed config,
// so `enable`/`disable` are O(1), shared across clones, and immediately visible.
// When a cache's name is disabled: reads miss (compute-on-miss always recomputes
// without storing) and writes are no-ops.

mod backends;
mod behaviors;
mod frontends;
pub(crate) mod events;
pub(crate) mod router;
pub(crate) mod persistence;

// `Epoch` lives in `backends` (defined for a later phase, not yet consumed); it
// is reachable as `backends::Epoch` and re-exported here once a caller needs it.
pub(crate) use backends::{CacheConfig, EagerIndex, EntryCache, LayerCache};
pub(crate) use behaviors::{CacheBehavior, EagerBehavior, EagerMapBehavior, WholeCacheBehavior};
pub(crate) use frontends::{Cache, Eager, EagerMap, WholeCache};
// `persistence::PersistableCache` (the cache-side freeze/thaw; the backends
// live in `crate::persist::backend`) and
// `router::{route, bind, ReactorEntry, CacheLike}` are reached via the module
// path; re-export them here once a layer wires the cascade through `route`.
