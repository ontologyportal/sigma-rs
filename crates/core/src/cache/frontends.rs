//! The cache wrapper types: each pairs a `behaviors` trait implementer with the
//! matching `backends` store and exposes the safe public API (`get` / `react` /
//! snapshot+restore).  The owning layer holds one wrapper per cache as a field.
//!
//!                    | lazy (compute-on-miss) | eager (maintained)
//!     keyed          |   `Cache<B>`           |   `EagerMap<B>`
//!     whole          |   `WholeCache<B>`      |   `Eager<B>`

use std::collections::HashMap;

use super::backends::{CacheConfig, EagerIndex, EntryCache, LayerCache};
use super::behaviors::{CacheBehavior, EagerBehavior, EagerMapBehavior, WholeCacheBehavior};

/// A single, self-contained cache: behavior + backing store.
///
/// Construct one per `CacheBehavior` and embed it as a field of `B::Parent`.
/// All public methods are safe; the parent is threaded explicitly through
/// [`get`](Self::get) / [`react`](Self::react).
pub(crate) struct Cache<B: CacheBehavior> {
    behavior: B,
    store:    EntryCache<B::Key, B::Value>,
    side:     B::Side,
}

#[allow(dead_code)] // full cache API; not every method is consumed yet
impl<B: CacheBehavior> Cache<B> {
    /// Create a cache sharing `config`, named by `B::NAME`.
    pub(crate) fn new(config: &CacheConfig, behavior: B) -> Self {
        Self { store: EntryCache::new(config, B::NAME), side: B::Side::default(), behavior }
    }

    /// The non-keyed companion state (interior-mutable).  Mirrors
    /// [`EagerMap::side`](super::frontends::EagerMap::side).
    pub(crate) fn side(&self) -> &B::Side {
        &self.side
    }

    /// A serializable snapshot of the side state (for persistence).
    pub(crate) fn snapshot_side(&self) -> B::SideSnapshot {
        self.behavior.snapshot_side(&self.side)
    }

    /// Restore the side state from a persisted snapshot.
    pub(crate) fn restore_side(&self, snap: B::SideSnapshot) {
        self.behavior.restore_side(&self.side, snap)
    }

    /// Return the cached value for `key`, computing it via `B::generate` on a
    /// miss.  `parent` is the owning layer (pass `self` from the wrapper
    /// method).  Cycle-safe: a recursive miss for the same key on the same
    /// thread returns `B::on_cycle` instead of recursing.
    pub(crate) fn get(&self, parent: &B::Parent, key: B::Key) -> B::Value {
        self.store.get_or_insert_with_cycle_safe(
            key,
            |k| self.behavior.generate(parent, k),
            |k| self.behavior.on_cycle(parent, k),
        )
    }

    /// Peek at the stored value without computing.  `None` on a miss or when
    /// the cache is disabled.
    pub(crate) fn peek(&self, key: &B::Key) -> Option<B::Value> {
        self.store.get(key)
    }

    /// Remove all entries.  No-op when disabled.
    pub(crate) fn clear(&self) {
        self.store.clear();
    }

    /// Remove entries whose keys are in `keys`.  No-op when disabled or empty.
    pub(crate) fn evict_keys(&self, keys: &[B::Key]) {
        self.store.evict_keys(keys);
    }

    /// Remove entries for which `predicate` returns `false`.  No-op when disabled.
    pub(crate) fn retain<F>(&self, predicate: F)
    where
        F: FnMut(&B::Key, &mut B::Value) -> bool,
    {
        self.store.retain(predicate);
    }

    /// Clone the entire map for serialisation (LMDB persistence).
    pub(crate) fn snapshot(&self) -> HashMap<B::Key, B::Value> {
        self.store.snapshot()
    }

    /// Replace the map from a deserialised snapshot.  No-op when disabled.
    pub(crate) fn restore(&self, map: HashMap<B::Key, B::Value>) {
        self.store.restore(map);
    }

    /// Dispatch a batch of change `events` to this cache's behavior `react`,
    /// returning any follow-on events it emits.
    pub(crate) fn react(
        &self,
        parent: &B::Parent,
        events: &[&crate::cache::events::Event],
    ) -> Vec<crate::cache::events::Event> {
        self.behavior.react(parent, events, &self.store, &self.side)
    }

    /// Direct access to the backing store for snapshot/restore and tests.
    pub(crate) fn store(&self) -> &EntryCache<B::Key, B::Value> {
        &self.store
    }
}

impl<B: CacheBehavior> std::fmt::Debug for Cache<B>
where
    B::Key:   std::fmt::Debug,
    B::Value: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache")
            .field("name", &B::NAME)
            .field("store", &self.store)
            .finish()
    }
}

/// A single, self-contained whole-value cache: behavior + `LayerCache` store.
pub(crate) struct WholeCache<B: WholeCacheBehavior> {
    behavior: B,
    store:    LayerCache<B::Value>,
}

#[allow(dead_code)] // full cache API; not every method is consumed yet
impl<B: WholeCacheBehavior> WholeCache<B> {
    /// Create a cache sharing `config`, named by `B::NAME`.
    pub(crate) fn new(config: &CacheConfig, behavior: B) -> Self {
        Self { store: LayerCache::new(config, B::NAME), behavior }
    }

    /// Return the value, computing it via `B::generate` on a miss.  Cycle-safe.
    pub(crate) fn get(&self, parent: &B::Parent) -> B::Value {
        self.store.get_or_init_cycle_safe(
            || self.behavior.generate(parent),
            || self.behavior.on_cycle(parent),
        )
    }

    /// Read the value without cloning (clone-free hot path).  The closure
    /// receives `Some(&value)` when populated, `None` otherwise.
    pub(crate) fn with_ref<R>(&self, f: impl FnOnce(Option<&B::Value>) -> R) -> R {
        self.store.with_ref(f)
    }

    /// Push a precomputed value (eager priming or LMDB restore).  No-op when disabled.
    pub(crate) fn install(&self, value: B::Value) {
        self.store.install(value);
    }

    /// Clone the value for serialisation, or `None` if unpopulated.
    pub(crate) fn snapshot(&self) -> Option<B::Value> {
        self.store.snapshot()
    }

    /// Drop the value so the next `get` recomputes.
    pub(crate) fn invalidate(&self) {
        self.store.invalidate();
    }

    /// `true` when a value is currently held.
    pub(crate) fn is_populated(&self) -> bool {
        self.store.is_populated()
    }

    /// Dispatch a batch of change `events` to this cache's behavior `react`,
    /// returning any follow-on events it emits.
    pub(crate) fn react(
        &self,
        parent: &B::Parent,
        events: &[&crate::cache::events::Event],
    ) -> Vec<crate::cache::events::Event> {
        self.behavior.react(parent, events, &self.store)
    }
}

impl<B: WholeCacheBehavior> std::fmt::Debug for WholeCache<B>
where
    B::Value: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WholeCache")
            .field("name", &B::NAME)
            .field("store", &self.store)
            .finish()
    }
}

/// A single, self-contained eager index: behavior + `EagerIndex` store.
pub(crate) struct Eager<B: EagerBehavior> {
    behavior: B,
    store:    EagerIndex<B::Value>,
}

#[allow(dead_code)] // full index API; not every method is consumed yet
impl<B: EagerBehavior> Eager<B> {
    /// Create an index sharing `config`, named by `B::NAME`, seeded with
    /// `B::initial()` (stored immediately if the cache is enabled).
    pub(crate) fn new(config: &CacheConfig, behavior: B) -> Self {
        let initial = behavior.initial();
        Self { store: EagerIndex::new(config, B::NAME, initial), behavior }
    }

    /// Read the value without cloning.  `Some(&value)` when enabled+populated.
    pub(crate) fn with_ref<R>(&self, f: impl FnOnce(&B::Value) -> R) -> R {
        self.store.with_ref(f)
    }

    /// Apply a mutable update.  No-op when disabled.
    pub(crate) fn modify(&self, f: impl FnOnce(&mut B::Value)) {
        self.store.modify(f);
    }

    /// Apply a mutable update and extract a result.  `None` when disabled/unpopulated.
    pub(crate) fn update_with<R>(&self, f: impl FnOnce(&mut B::Value) -> R) -> R {
        self.store.update_with(f)
    }

    /// Replace the value wholesale (LMDB restore).  No-op when disabled.
    pub(crate) fn install(&self, value: B::Value) {
        self.store.install(value);
    }

    /// Clone the value for serialisation, or `None` when disabled/unpopulated.
    pub(crate) fn snapshot(&self) -> B::Value {
        self.store.snapshot()
    }

    /// Dispatch a batch of change `events` to this cache's behavior `react`,
    /// returning any follow-on events it emits.
    pub(crate) fn react(
        &self,
        parent: &B::Parent,
        events: &[&crate::cache::events::Event],
    ) -> Vec<crate::cache::events::Event> {
        self.behavior.react(parent, events, &self.store)
    }

    /// Prime this cache from the source of truth (no-op unless the behavior
    /// overrides `initialize`; the behavior self-guards against re-priming a
    /// populated/restored cache).
    pub(crate) fn initialize(&self, parent: &B::Parent) {
        self.behavior.initialize(parent, &self.store);
    }
}

impl<B: EagerBehavior> std::fmt::Debug for Eager<B>
where
    B::Value: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Eager")
            .field("name", &B::NAME)
            .field("store", &self.store)
            .finish()
    }
}

/// A single, self-contained eagerly-maintained keyed index: behavior + store +
/// optional non-keyed companion `side` state.
pub(crate) struct EagerMap<B: EagerMapBehavior> {
    behavior: B,
    store:    EntryCache<B::Key, B::Value>,
    side:     B::Side,
}

#[allow(dead_code)] // full index API; not every method is consumed yet
impl<B: EagerMapBehavior> EagerMap<B> {
    /// Create an index sharing `config`, named by `B::NAME`.
    pub(crate) fn new(config: &CacheConfig, behavior: B) -> Self {
        Self { store: EntryCache::new(config, B::NAME), side: B::Side::default(), behavior }
    }

    /// The keyed store (interior-mutable).  For caches that expose their own
    /// typed API (e.g. the symbol store's `intern`) over the raw map.
    pub(crate) fn entries(&self) -> &EntryCache<B::Key, B::Value> {
        &self.store
    }

    /// The non-keyed companion state (interior-mutable).
    pub(crate) fn side(&self) -> &B::Side {
        &self.side
    }

    /// A serializable snapshot of the side state (for persistence).
    pub(crate) fn snapshot_side(&self) -> B::SideSnapshot {
        self.behavior.snapshot_side(&self.side)
    }

    /// Restore the side state from a persisted snapshot.
    pub(crate) fn restore_side(&self, snap: B::SideSnapshot) {
        self.behavior.restore_side(&self.side, snap)
    }

    /// Read the entry for `key` without computing.  `None` on a miss or when disabled.
    pub(crate) fn get(&self, key: &B::Key) -> Option<B::Value> {
        self.store.get(key)
    }

    /// Insert or overwrite the entry for `key`.  No-op when disabled.
    pub(crate) fn update(&self, key: B::Key, value: B::Value) {
        self.store.update(key, value);
    }

    /// Remove entries whose keys are in `keys`.  No-op when disabled/empty.
    pub(crate) fn evict_keys(&self, keys: &[B::Key]) {
        self.store.evict_keys(keys);
    }

    /// Remove all entries.  No-op when disabled.
    pub(crate) fn clear(&self) {
        self.store.clear();
    }

    /// Remove entries for which `predicate` returns `false`.  No-op when disabled.
    pub(crate) fn retain<F>(&self, predicate: F)
    where
        F: FnMut(&B::Key, &mut B::Value) -> bool,
    {
        self.store.retain(predicate);
    }

    /// Iterate the cached entries.
    pub(crate) fn for_each(&self, f: impl FnMut((&B::Key, &B::Value))) {
        self.store.for_each(f);
    }

    /// Clone the entire map for serialisation.
    pub(crate) fn snapshot(&self) -> HashMap<B::Key, B::Value> {
        self.store.snapshot()
    }

    /// Replace the map from a deserialised snapshot.  No-op when disabled.
    pub(crate) fn restore(&self, map: HashMap<B::Key, B::Value>) {
        self.store.restore(map);
    }

    /// `true` if the index holds no entries.
    pub(crate) fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Number of entries.
    pub(crate) fn len(&self) -> usize {
        self.store.len()
    }

    /// Dispatch a batch of change `events` to this cache's behavior `react`,
    /// returning any follow-on events it emits.
    pub(crate) fn react(
        &self,
        parent: &B::Parent,
        events: &[&crate::cache::events::Event],
    ) -> Vec<crate::cache::events::Event> {
        self.behavior.react(parent, events, &self.store, &self.side)
    }

    /// Prime this cache from the source of truth (no-op unless the behavior
    /// overrides `initialize`; the behavior self-guards against re-priming a
    /// populated/restored cache).
    pub(crate) fn initialize(&self, parent: &B::Parent) {
        self.behavior.initialize(parent, &self.store, &self.side);
    }
}

#[allow(dead_code)]
impl<B: EagerMapBehavior> EagerMap<B>
where
    B::Value: Default,
{
    /// Call `f` on the entry for `key`, inserting `Value::default()` if absent.
    /// No-op when disabled.  Used for `Vec`-valued indices built incrementally.
    pub(crate) fn modify_entry<F>(&self, key: B::Key, f: F)
    where
        F: FnOnce(&mut B::Value),
    {
        self.store.modify_entry(key, f);
    }
}

impl<B: EagerMapBehavior> std::fmt::Debug for EagerMap<B>
where
    B::Key:   std::fmt::Debug,
    B::Value: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EagerMap")
            .field("name", &B::NAME)
            .field("store", &self.store)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// CacheLike — unify the four wrappers so the router can `bind` any of them.
// ---------------------------------------------------------------------------

impl<B: CacheBehavior> super::router::CacheLike for Cache<B> {
    type Parent = B::Parent;
    fn name(&self) -> &'static str { B::NAME }
    fn consumes(&self) -> &'static [crate::cache::events::EventKind] { self.behavior.consumes() }
    fn produces(&self) -> &'static [crate::cache::events::EventKind] { self.behavior.produces() }
    fn reads(&self) -> &'static [&'static str] { self.behavior.reads() }
    fn event_parallel(&self) -> bool { self.behavior.event_parallel() }
    fn react(
        &self,
        parent: &B::Parent,
        events: &[&crate::cache::events::Event],
    ) -> Vec<crate::cache::events::Event> {
        self.behavior.react(parent, events, &self.store, &self.side)
    }
}

impl<B: WholeCacheBehavior> super::router::CacheLike for WholeCache<B> {
    type Parent = B::Parent;
    fn name(&self) -> &'static str { B::NAME }
    fn consumes(&self) -> &'static [crate::cache::events::EventKind] { self.behavior.consumes() }
    fn produces(&self) -> &'static [crate::cache::events::EventKind] { self.behavior.produces() }
    fn reads(&self) -> &'static [&'static str] { self.behavior.reads() }
    fn event_parallel(&self) -> bool { self.behavior.event_parallel() }
    fn react(
        &self,
        parent: &B::Parent,
        events: &[&crate::cache::events::Event],
    ) -> Vec<crate::cache::events::Event> {
        self.behavior.react(parent, events, &self.store)
    }
}

impl<B: EagerBehavior> super::router::CacheLike for Eager<B> {
    type Parent = B::Parent;
    fn name(&self) -> &'static str { B::NAME }
    fn consumes(&self) -> &'static [crate::cache::events::EventKind] { self.behavior.consumes() }
    fn produces(&self) -> &'static [crate::cache::events::EventKind] { self.behavior.produces() }
    fn reads(&self) -> &'static [&'static str] { self.behavior.reads() }
    fn event_parallel(&self) -> bool { self.behavior.event_parallel() }
    fn react(
        &self,
        parent: &B::Parent,
        events: &[&crate::cache::events::Event],
    ) -> Vec<crate::cache::events::Event> {
        self.behavior.react(parent, events, &self.store)
    }
}

impl<B: EagerMapBehavior> super::router::CacheLike for EagerMap<B> {
    type Parent = B::Parent;
    fn name(&self) -> &'static str { B::NAME }
    fn consumes(&self) -> &'static [crate::cache::events::EventKind] { self.behavior.consumes() }
    fn produces(&self) -> &'static [crate::cache::events::EventKind] { self.behavior.produces() }
    fn reads(&self) -> &'static [&'static str] { self.behavior.reads() }
    fn event_parallel(&self) -> bool { self.behavior.event_parallel() }
    fn react(
        &self,
        parent: &B::Parent,
        events: &[&crate::cache::events::Event],
    ) -> Vec<crate::cache::events::Event> {
        self.behavior.react(parent, events, &self.store, &self.side)
    }
}


#[cfg(test)]
mod tests {
    use crate::cache::*;

    fn disabled_cfg(name: &'static str) -> CacheConfig {
        let cfg = CacheConfig::default();
        cfg.disable(name);
        cfg
    }

    // -- WholeCache<B> / Eager<B> behavior wrappers ---------------------------
    //
    // Demonstrate the two non-keyed behavior shapes.  A shared `DemoParent`
    // carries a call counter so the tests can prove compute-once / recompute
    // semantics — mirroring how a real `WholeCacheBehavior::generate` would
    // read the parent layer.

    use std::sync::atomic::{AtomicUsize, Ordering};

    struct DemoParent {
        generate_calls: AtomicUsize,
    }
    impl DemoParent {
        fn new() -> Self { Self { generate_calls: AtomicUsize::new(0) } }
        fn calls(&self) -> usize { self.generate_calls.load(Ordering::SeqCst) }
    }

    /// Whole-value behavior: computes a `Vec` once, recomputes after a delta.
    struct DemoNumericAncestors;
    impl WholeCacheBehavior for DemoNumericAncestors {
        type Parent = DemoParent;
        type Value  = Vec<u32>;
        const NAME: &'static str = "demo::whole";

        fn generate(&self, parent: &DemoParent) -> Vec<u32> {
            parent.generate_calls.fetch_add(1, Ordering::SeqCst);
            vec![1, 2, 3]
        }

        fn react(
            &self,
            _p: &DemoParent,
            _events: &[&crate::cache::events::Event],
            store: &LayerCache<Vec<u32>>,
        ) -> Vec<crate::cache::events::Event> {
            store.invalidate();
            Vec::new()
        }
    }

    #[test]
    fn whole_cache_computes_once_then_hits() {
        let cfg = CacheConfig::default();
        let parent = DemoParent::new();
        let cache = WholeCache::new(&cfg, DemoNumericAncestors);

        assert_eq!(cache.get(&parent), vec![1, 2, 3]);
        assert_eq!(cache.get(&parent), vec![1, 2, 3]); // hit, no recompute
        assert_eq!(parent.calls(), 1, "generate runs once; second get is opaque hit");
        assert!(cache.is_populated());
        // clone-free read sees the same value
        assert_eq!(cache.with_ref(|o| o.cloned()), Some(vec![1, 2, 3]));
    }

    #[test]
    fn whole_cache_react_to_delta_invalidates_then_recomputes() {
        let cfg = CacheConfig::default();
        let parent = DemoParent::new();
        let cache = WholeCache::new(&cfg, DemoNumericAncestors);

        cache.get(&parent);
        cache.react(&parent, &[]); // override invalidates
        assert!(!cache.is_populated(), "react dropped the value");
        cache.get(&parent); // recompute
        assert_eq!(parent.calls(), 2, "value recomputed after delta");
    }

    #[test]
    fn whole_cache_install_and_snapshot_roundtrip() {
        let cfg = CacheConfig::default();
        let parent = DemoParent::new();
        let cache = WholeCache::new(&cfg, DemoNumericAncestors);

        cache.install(vec![9, 9]); // eager prime / LMDB restore path
        assert_eq!(cache.get(&parent), vec![9, 9]);
        assert_eq!(parent.calls(), 0, "installed value short-circuits generate");
        assert_eq!(cache.snapshot(), Some(vec![9, 9]));
    }

    #[test]
    fn whole_cache_disabled_always_computes() {
        let cache = WholeCache::new(&disabled_cfg("demo::whole"), DemoNumericAncestors);
        let parent = DemoParent::new();
        cache.get(&parent);
        cache.get(&parent);
        assert_eq!(parent.calls(), 2, "disabled whole-cache recomputes every get");
        assert!(!cache.is_populated());
    }

    /// Eager behavior: seeded empty, maintained by `modify`, cleared on delta.
    struct DemoSine;
    impl EagerBehavior for DemoSine {
        type Parent = DemoParent;
        type Value  = Vec<u32>;
        const NAME: &'static str = "demo::eager";

        fn initial(&self) -> Vec<u32> { Vec::new() }

        fn react(
            &self,
            _p: &DemoParent,
            _events: &[&crate::cache::events::Event],
            store: &EagerIndex<Vec<u32>>,
        ) -> Vec<crate::cache::events::Event> {
            store.modify(|v| v.clear());
            Vec::new()
        }
    }

    #[test]
    fn eager_seeded_then_maintained_by_modify() {
        let cfg = CacheConfig::default();
        let idx = Eager::new(&cfg, DemoSine);

        // Seeded with initial() at construction; with_ref reads the live value.
        assert_eq!(idx.with_ref(|v| v.clone()), Vec::<u32>::new());
        idx.modify(|v| v.push(10));
        idx.modify(|v| v.push(20));
        assert_eq!(idx.with_ref(|v| v.clone()), vec![10, 20]);
    }

    #[test]
    fn eager_react_to_delta_mutates_in_place() {
        let cfg = CacheConfig::default();
        let parent = DemoParent::new();
        let idx = Eager::new(&cfg, DemoSine);

        idx.modify(|v| v.extend([1, 2, 3]));
        idx.react(&parent, &[]); // clears in place
        assert_eq!(idx.with_ref(|v| v.clone()), Vec::<u32>::new());
    }

    #[test]
    fn eager_install_and_snapshot_roundtrip() {
        let cfg = CacheConfig::default();
        let idx = Eager::new(&cfg, DemoSine);
        idx.install(vec![7, 8]);
        assert_eq!(idx.snapshot(), vec![7, 8]);
    }

    #[test]
    fn eager_disabled_still_initialized_and_mutable() {
        // Disabled gates persistence (`install`), not initialization or
        // in-memory mutation: the index is live and `modify` still applies.
        let idx = Eager::new(&disabled_cfg("demo::eager"), DemoSine);
        assert_eq!(idx.with_ref(|v| v.clone()), Vec::<u32>::new(),
            "disabled eager index is still seeded with initial()");
        idx.modify(|v| v.push(1));
        assert_eq!(idx.with_ref(|v| v.clone()), vec![1],
            "modify is an in-memory mutation, unaffected by the persistence gate");
    }
}
