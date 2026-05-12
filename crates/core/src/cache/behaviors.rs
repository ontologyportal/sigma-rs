//! The per-cache *behavior* traits.  Each implementer (one per cache, in its own
//! file under a layer's `caches/` module) owns its `generate`, cycle fallback,
//! and event `react` logic.  The `frontends` wrappers pair a behavior with the
//! matching `backends` store:
//!
//! ```text
//!                | lazy (compute-on-miss) | eager (maintained)
//!     keyed      |   `CacheBehavior`      |   `EagerMapBehavior`
//!     whole      |   `WholeCacheBehavior` |   `EagerBehavior`
//! ```

use std::hash::Hash;

use super::backends::{cache_key_hash, EagerIndex, EntryCache, LayerCache};

/// The per-cache behavior: what a specific cache computes, how it reacts to a
/// cycle, and how it responds to a change delta.  One implementer per cache,
/// each in its own file.
pub(crate) trait CacheBehavior: Send + Sync + Sized {
    /// The layer that owns this cache.  `generate` receives `&Parent` so it
    /// can reach sibling caches and inner layers.
    type Parent;
    /// Lookup key.
    type Key: Eq + Hash + Clone + Send + Sync;
    /// Cached value.
    type Value: Clone + Send + Sync;
    /// Non-keyed companion state — counters, sparse side indices, etc.  Reached
    /// through `&` (the reactive model only hands out shared references), so its
    /// *fields* must be interior-mutable (`EntryCache`/`DashMap`/`Atomic*`/…)
    /// for the cache to mutate it.  Use `()` when no side state is needed.
    type Side: Default + Send + Sync;
    /// A plain, serializable snapshot of [`Side`](Self::Side) for persistence.
    /// `snapshot_side`/`restore_side` convert between the (interior-mutable)
    /// live side and this form.  Use `()` when the side isn't persisted.
    type SideSnapshot: serde::Serialize + serde::de::DeserializeOwned + Default + Send + Sync;

    /// Cache name for [`CacheConfig`] enable/disable and cycle diagnostics.
    /// Conventionally the layer-prefixed constant, e.g. `semantic::is_instance`.
    const NAME: &'static str;

    /// Event kinds this cache reacts to (reactive graph; default: none).
    fn consumes(&self) -> &'static [crate::cache::events::EventKind] { &[] }
    /// Event kinds this cache may emit (reactive graph; default: none).
    fn produces(&self) -> &'static [crate::cache::events::EventKind] { &[] }
    /// Cache names this reactor's `react` READS.  The writer of each named cache
    /// (the reactor whose `NAME` equals it) is ordered strictly before this
    /// reactor.  A reactor's write is implicitly its own cache (`NAME`).
    /// Default: reads nothing.
    fn reads(&self) -> &'static [&'static str] { &[] }

    /// Whether this reactor's `react` may run on disjoint shards of its event
    /// slice concurrently (Axis-B event parallelism).  Safe to enable **iff**
    /// `react` is a commutative per-event fold over distinct keys with no
    /// whole-store operation (`clear` / `retain` / early-return-on-presence) —
    /// those would observe a different result under partition.  Default: serial.
    fn event_parallel(&self) -> bool { false }

    /// Compute the value for `key` on a miss, using `parent` to reach sibling
    /// caches and inner layers.  Must be a pure function of read-only parent
    /// data (the storage layer may call it more than once under contention —
    /// see [`EntryCache`] docs).
    fn generate(&self, parent: &Self::Parent, key: &Self::Key) -> Self::Value;

    /// Value to return when a recursive miss for the *same key* is detected on
    /// the same thread.  The default panics with an actionable message,
    /// converting what would otherwise be a stack overflow into a debuggable
    /// panic point.  Override for caches with legitimate cycles (e.g.
    /// `has_ancestor` returns `false`; memoised type inference returns
    /// `Unknown`).  The returned value is **not** cached.
    fn on_cycle(&self, _parent: &Self::Parent, key: &Self::Key) -> Self::Value {
        panic!(
            "cache '{}' detected recursive entry for the same key (hash {}); \
             override `CacheBehavior::on_cycle` if this cycle is expected",
            Self::NAME,
            cache_key_hash(key),
        );
    }

    /// React to a batch of change `events`, returning any follow-on events to
    /// dispatch.  A cache mutates its own `store`
    /// (`clear`/`evict_keys`/`retain`) and may emit events for downstream
    /// caches, or surface a [`Diagnostic`](crate::Diagnostic).  The default is
    /// inert.
    fn react(
        &self,
        _parent: &Self::Parent,
        _events: &[&crate::cache::events::Event],
        _store:  &EntryCache<Self::Key, Self::Value>,
        _side:   &Self::Side,
    ) -> Vec<crate::cache::events::Event> {
        Vec::new()
    }

    /// Snapshot the side for persistence (default: empty — `()` side).
    fn snapshot_side(&self, _side: &Self::Side) -> Self::SideSnapshot {
        Self::SideSnapshot::default()
    }
    /// Restore the side from a persisted snapshot, merging into the
    /// (interior-mutable) live `side` (default: no-op).
    fn restore_side(&self, _side: &Self::Side, _snap: Self::SideSnapshot) {}
}

/// Per-cache behavior for a keyless whole-value cache.  One implementer per
/// cache, each in its own file.
pub(crate) trait WholeCacheBehavior: Send + Sync + Sized {
    /// The layer that owns this cache; passed to `generate` so it can reach
    /// sibling caches and inner layers.
    type Parent;
    /// The whole cached value.
    type Value: Clone + Send + Sync;

    /// Cache name for [`CacheConfig`] and cycle diagnostics.
    const NAME: &'static str;

    /// Event kinds this cache reacts to (reactive graph; default: none).
    fn consumes(&self) -> &'static [crate::cache::events::EventKind] { &[] }
    /// Event kinds this cache may emit (reactive graph; default: none).
    fn produces(&self) -> &'static [crate::cache::events::EventKind] { &[] }
    /// Cache names this reactor's `react` READS.  The writer of each named cache
    /// (the reactor whose `NAME` equals it) is ordered strictly before this
    /// reactor.  A reactor's write is implicitly its own cache (`NAME`).
    /// Default: reads nothing.
    fn reads(&self) -> &'static [&'static str] { &[] }

    /// Whether this reactor's `react` may run on disjoint shards of its event
    /// slice concurrently (Axis-B event parallelism).  Safe to enable **iff**
    /// `react` is a commutative per-event fold over distinct keys with no
    /// whole-store operation (`clear` / `retain` / early-return-on-presence) —
    /// those would observe a different result under partition.  Default: serial.
    fn event_parallel(&self) -> bool { false }

    /// Compute the whole value on a miss, using `parent` to reach sibling
    /// caches and inner layers.
    fn generate(&self, parent: &Self::Parent) -> Self::Value;

    /// Value to return on a recursive re-entry on the same thread.  Defaults to
    /// a panic; override for caches with legitimate self-reference.
    fn on_cycle(&self, _parent: &Self::Parent) -> Self::Value {
        panic!(
            "cache '{}' detected recursive whole-value re-entry; \
             override `WholeCacheBehavior::on_cycle` if this cycle is expected",
            Self::NAME,
        );
    }

    /// React to a batch of change `events`, returning any follow-on events.
    /// Default is inert.
    fn react(
        &self,
        _parent: &Self::Parent,
        _events: &[&crate::cache::events::Event],
        _store:  &LayerCache<Self::Value>,
    ) -> Vec<crate::cache::events::Event> {
        Vec::new()
    }
}

/// Per-cache behavior for an eagerly-maintained index.  One implementer per
/// index, each in its own file.
pub(crate) trait EagerBehavior: Send + Sync + Sized {
    /// The layer that owns this index; passed to `react_to_delta`.
    type Parent;
    /// The maintained value.
    type Value: Clone + Send + Sync;

    /// Cache name for [`CacheConfig`].
    const NAME: &'static str;

    /// Event kinds this cache reacts to (reactive graph; default: none).
    fn consumes(&self) -> &'static [crate::cache::events::EventKind] { &[] }
    /// Event kinds this cache may emit (reactive graph; default: none).
    fn produces(&self) -> &'static [crate::cache::events::EventKind] { &[] }
    /// Cache names this reactor's `react` READS.  The writer of each named cache
    /// (the reactor whose `NAME` equals it) is ordered strictly before this
    /// reactor.  A reactor's write is implicitly its own cache (`NAME`).
    /// Default: reads nothing.
    fn reads(&self) -> &'static [&'static str] { &[] }

    /// Whether this reactor's `react` may run on disjoint shards of its event
    /// slice concurrently (Axis-B event parallelism).  Safe to enable **iff**
    /// `react` is a commutative per-event fold over distinct keys with no
    /// whole-store operation (`clear` / `retain` / early-return-on-presence) —
    /// those would observe a different result under partition.  Default: serial.
    fn event_parallel(&self) -> bool { false }

    /// The value the index is seeded with at construction.  (There is no
    /// compute-on-miss; the index is built up afterwards via `modify`.)
    fn initial(&self) -> Self::Value;

    /// React to a batch of change `events`, returning any follow-on events.
    /// Default is inert.
    fn react(
        &self,
        _parent: &Self::Parent,
        _events: &[&crate::cache::events::Event],
        _store:  &EagerIndex<Self::Value>,
    ) -> Vec<crate::cache::events::Event> {
        Vec::new()
    }

    /// Build this cache's contents from the source of truth (the parent layer's
    /// lower caches).  Driven by `Layer::initialize_caches` at fresh setup.
    ///
    /// MUST be idempotent / self-guarding: skip the work when the cache is
    /// already populated (e.g. just thawed by `restore_caches_from`), so that
    /// initialization is a no-op when restoring from a persistent snapshot.
    /// Default: no-op — correct for source-of-truth and event-built caches.
    fn initialize(&self, _parent: &Self::Parent, _store: &EagerIndex<Self::Value>) {}
}

/// Per-cache behavior for an eagerly-maintained keyed index.  No `generate`:
/// entries are produced by the owning layer's maintenance methods, not on miss.
pub(crate) trait EagerMapBehavior: Send + Sync + Sized {
    /// The layer that owns this index; passed to `react_to_delta`.
    type Parent;
    /// Lookup key.
    type Key: Eq + Hash + Clone + Send + Sync;
    /// Stored value.
    type Value: Clone + Send + Sync;
    /// Non-keyed companion state — counters, sparse side indices, etc.  Reached
    /// through `&` (the reactive model only hands out shared references), so its
    /// *fields* must be interior-mutable (`EntryCache`/`DashMap`/`Atomic*`/…)
    /// for the cache to mutate it.  Use `()` when no side state is needed.
    type Side: Default + Send + Sync;
    /// A plain, serializable snapshot of [`Side`](Self::Side) for persistence.
    /// `snapshot_side`/`restore_side` convert between the (interior-mutable)
    /// live side and this form.  Use `()` when the side isn't persisted.
    type SideSnapshot: serde::Serialize + serde::de::DeserializeOwned + Default + Send + Sync;

    /// Cache name for [`CacheConfig`].
    const NAME: &'static str;

    /// Event kinds this cache reacts to (reactive graph; default: none).
    fn consumes(&self) -> &'static [crate::cache::events::EventKind] { &[] }
    /// Event kinds this cache may emit (reactive graph; default: none).
    fn produces(&self) -> &'static [crate::cache::events::EventKind] { &[] }
    /// Cache names this reactor's `react` READS.  The writer of each named cache
    /// (the reactor whose `NAME` equals it) is ordered strictly before this
    /// reactor.  A reactor's write is implicitly its own cache (`NAME`).
    /// Default: reads nothing.
    fn reads(&self) -> &'static [&'static str] { &[] }

    /// Whether this reactor's `react` may run on disjoint shards of its event
    /// slice concurrently (Axis-B event parallelism).  Safe to enable **iff**
    /// `react` is a commutative per-event fold over distinct keys with no
    /// whole-store operation (`clear` / `retain` / early-return-on-presence) —
    /// those would observe a different result under partition.  Default: serial.
    fn event_parallel(&self) -> bool { false }

    /// React to a batch of change `events`, returning any follow-on events.
    /// `store` is the keyed map; `side` is the companion state.  Default inert.
    fn react(
        &self,
        _parent: &Self::Parent,
        _events: &[&crate::cache::events::Event],
        _store:  &EntryCache<Self::Key, Self::Value>,
        _side:   &Self::Side,
    ) -> Vec<crate::cache::events::Event> {
        Vec::new()
    }

    /// Snapshot the side for persistence (default: empty — `()` side).
    fn snapshot_side(&self, _side: &Self::Side) -> Self::SideSnapshot {
        Self::SideSnapshot::default()
    }
    /// Restore the side from a persisted snapshot, merging into the
    /// (interior-mutable) live `side` (default: no-op).
    fn restore_side(&self, _side: &Self::Side, _snap: Self::SideSnapshot) {}

    /// Build this cache's contents from the source of truth (the parent layer's
    /// lower caches).  Driven by `Layer::initialize_caches` at fresh setup.
    ///
    /// MUST be idempotent / self-guarding: skip the work when the cache is
    /// already populated (e.g. just thawed by `restore_caches_from`), so that
    /// initialization is a no-op when restoring from a persistent snapshot.
    /// Default: no-op — correct for source-of-truth and event-built caches.
    fn initialize(
        &self,
        _parent: &Self::Parent,
        _store:  &EntryCache<Self::Key, Self::Value>,
        _side:   &Self::Side,
    ) {}
}

