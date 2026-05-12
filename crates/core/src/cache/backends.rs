//! Storage/concurrency primitives for the cache subsystem.  These types know how
//! to memoise, shard locks, detect cycles, and honour the `CacheConfig` enable
//! flag — but nothing about what any particular cached value means.
//!
//!   EntryCache<K, V>  -- per-entry store backed by `DashMap<K, V>`.
//!   LayerCache<T>     -- whole-value lazy cache backed by `RwLock<Option<T>>`.
//!   EagerIndex<T>     -- eagerly-maintained whole value backed by `RwLock<T>`.
//!   CacheConfig       -- shared, runtime-mutable enable/disable set.
//!   Epoch             -- monotonic change counter (epoch-guarded lazy fill).

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use dashmap::DashMap;

// ---------------------------------------------------------------------------
// Cycle detection — `get_or_insert_with` / `get_or_init` re-entry guard
// ---------------------------------------------------------------------------
//
// The miss path releases its lock before invoking the closure, so a recursive
// call back into the same key during `f` re-enters as a cache miss and recurses
// until the stack overflows.  This thread-local set tracks `(cache_name,
// key_hash)` pairs currently being computed on the current thread: every cache
// miss inserts on entry and removes on exit; a recursive miss for the same key
// finds its entry already present.  A hash collision on the key would produce a
// spurious cycle.

thread_local! {
    static CACHE_IN_PROGRESS: std::cell::RefCell<HashSet<(&'static str, u64)>>
        = std::cell::RefCell::new(HashSet::new());
}

/// Hash a key down to a `u64` for the thread-local in-progress set.
pub(in crate::cache) fn cache_key_hash<K: Hash>(k: &K) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    let mut h = DefaultHasher::new();
    k.hash(&mut h);
    h.finish()
}

/// RAII guard tracking a `(name, key_hash)` pair as in-progress on the current
/// thread; removes it on drop.
struct InProgressGuard {
    name:     &'static str,
    key_hash: u64,
}

impl InProgressGuard {
    /// Marks `(name, key_hash)` in-progress, returning the guard, or `None`
    /// if it is already in-progress on this thread (cycle detected).
    fn try_acquire(name: &'static str, key_hash: u64) -> Option<Self> {
        let inserted = CACHE_IN_PROGRESS.with(|s| s.borrow_mut().insert((name, key_hash)));
        if inserted { Some(Self { name, key_hash }) } else { None }
    }
}

impl Drop for InProgressGuard {
    fn drop(&mut self) {
        CACHE_IN_PROGRESS.with(|s| {
            s.borrow_mut().remove(&(self.name, self.key_hash));
        });
    }
}

/// Shared, runtime-mutable cache configuration.
///
/// `clone()` is O(1); all clones share the same disabled-set, so `enable` /
/// `disable` take `&self` and are immediately visible to every `EntryCache` /
/// `LayerCache` that holds a clone of this config.
///
/// Cache names are `&'static str` constants defined in each layer's module,
/// conventionally prefixed with the layer name:
///   `syntactic::occurrences`, `semantic::is_instance`, etc.
#[derive(Debug, Clone)]
pub(crate) struct CacheConfig {
    /// Cache names in this set are disabled.  Empty = all caches active.
    disabled: Arc<RwLock<HashSet<&'static str>>>,
    /// Shared parallelism knobs for the reactive router (see [`ParallelCfg`]).
    parallel: Arc<ParallelCfg>,
}

/// Router parallelism settings, shared (`Arc`) across every clone of a
/// [`CacheConfig`].  Both knobs are atomics for lock-free reads on the cascade
/// hot path.
#[derive(Debug)]
struct ParallelCfg {
    /// Upper bound on the number of concurrent tasks the router will fan a
    /// single cohort / event vector into.  Defaults to the machine's available
    /// parallelism.  `1` (or the `parallel` feature being off) forces serial.
    max_threads: AtomicUsize,
    /// Minimum number of work-units (events, or reactors) one task must carry
    /// for fanning out to be worth the spawn + lock-contention cost.  A batch
    /// smaller than this runs serially regardless of `max_threads`.
    floor:       AtomicUsize,
}

/// Default minimum batch size before the router fans out.
const DEFAULT_PARALLEL_FLOOR: usize = 512;

impl Default for ParallelCfg {
    fn default() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        Self {
            max_threads: AtomicUsize::new(cores),
            floor:       AtomicUsize::new(DEFAULT_PARALLEL_FLOOR),
        }
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            disabled: Arc::new(RwLock::new(HashSet::new())),
            parallel: Arc::new(ParallelCfg::default()),
        }
    }
}

/// How many concurrent tasks to use for `units` work-items given a thread cap
/// and a per-task floor.  Pure arithmetic, no cache knowledge — shared by both
/// the cohort fan-out (Axis A) and the event-shard fan-out (Axis B).
///
/// Guarantees every task carries at least `floor` units (except possibly the
/// last), and never exceeds `max_threads`.  Returns `1` (serial) when the batch
/// is below the floor or threading is disabled.
#[cfg_attr(not(feature = "parallel"), allow(dead_code))]
pub(crate) fn plan_threads(units: usize, max_threads: usize, floor: usize) -> usize {
    if max_threads <= 1 || floor == 0 || units < floor.max(2) {
        return 1;
    }
    max_threads.min(units / floor).max(1)
}

impl CacheConfig {
    /// Returns `true` when `name` is NOT in the disabled set.
    pub(crate) fn is_enabled(&self, name: &'static str) -> bool {
        !self.disabled.read().unwrap().contains(name)
    }

    /// Disable a named cache.  Immediately visible to all cache instances
    /// sharing this config
    #[allow(dead_code)] // TODO: consume in KB
    pub(crate) fn disable(&self, name: &'static str) {
        self.disabled.write().unwrap().insert(name);
    }

    /// Re-enable a named cache.  Immediately visible to all cache instances
    /// sharing this config.
    #[allow(dead_code)] // TODO: consume in KB
    pub(crate) fn enable(&self, name: &'static str) {
        self.disabled.write().unwrap().remove(name);
    }

    /// Create a fresh config with every name in `names` pre-disabled.
    #[allow(dead_code)] // TODO: consume in KB
    pub(crate) fn with_disabled(names: &[&'static str]) -> Self {
        let cfg = Self::default();
        for &n in names { cfg.disable(n); }
        cfg
    }

    /// Max concurrent tasks the reactive router may fan a cohort / event vector
    /// into.  `1` forces serial dispatch.  Only read by the router's parallel
    /// dispatch (behind `feature = "parallel"`).
    #[cfg_attr(not(feature = "parallel"), allow(dead_code))]
    pub(crate) fn max_threads(&self) -> usize {
        self.parallel.max_threads.load(Ordering::Relaxed)
    }

    /// Minimum work-units per task before the router fans out (cost floor).
    /// Only read by the router's parallel dispatch.
    #[cfg_attr(not(feature = "parallel"), allow(dead_code))]
    pub(crate) fn parallel_floor(&self) -> usize {
        self.parallel.floor.load(Ordering::Relaxed)
    }

    /// Set the router's thread cap (shared with every clone of this config).
    /// `0` is clamped to `1` (serial).
    #[allow(dead_code)]
    pub(crate) fn set_max_threads(&self, n: usize) {
        self.parallel.max_threads.store(n.max(1), Ordering::Relaxed);
    }

    /// Set the router's per-task work-unit floor (shared with every clone).
    #[allow(dead_code)]
    pub(crate) fn set_parallel_floor(&self, n: usize) {
        self.parallel.floor.store(n, Ordering::Relaxed);
    }
}

// EntryCache<K, V>

/// A per-entry cache backed by `DashMap<K, V>` (sharded per-bucket locking).
///
/// Supports both lazy memoisation (`get_or_insert_with`) and
/// eagerly-maintained indices (`update`, `modify_entry`).
///
/// All write operations are no-ops when the named cache is disabled.
/// `get` and `get_or_insert_with` return `None` / always-compute when disabled.
///
/// **Concurrency model.**  DashMap shards the map across internal RwLocks;
/// threads on different keys almost always hit different shards.
///
/// **Compute-on-miss with last-write-wins.**  When two threads miss the same
/// key concurrently, both compute the value and both insert.  `DashMap::insert`
/// is atomic, and every cache closure in this crate is a pure function of
/// read-only `SyntacticLayer` data, so both threads produce identical results.
/// Wasted CPU on contended first-time misses is bounded by the number of
/// concurrent rayon workers (≤ NUM_CPUS).
#[derive(Debug)]
pub(crate) struct EntryCache<K: Eq + Hash, V> {
    map:    DashMap<K, V>,
    config: CacheConfig,
    name:   &'static str,
    /// Invalidation clock, bumped when the store is marked stale
    /// (`clear`/`evict_keys`/`retain`/`update`/`restore`), so a lazy fill that
    /// overlapped such a mutation refuses to memoise.  See [`Epoch`].
    epoch:  Epoch,
}

impl<K: Eq + Hash, V> EntryCache<K, V> {
    /// Create a new, empty cache sharing `config`, identified by `name`.
    pub(crate) fn new(config: &CacheConfig, name: &'static str) -> Self {
        Self {
            map:    DashMap::new(),
            config: config.clone(),
            name,
            epoch:  Epoch::default(),
        }
    }

    /// Returns `true` when this cache's name is enabled in the shared config.
    fn enabled(&self) -> bool {
        self.config.is_enabled(self.name)
    }
}

impl<K: Eq + Hash, V> Default for EntryCache<K, V> {
    /// Constructs a standalone, always-enabled cache with no shared config.
    /// Prefer explicit `new(config, name)` for production use so caches share
    /// a single `CacheConfig` and respond to runtime `enable`/`disable` calls.
    fn default() -> Self {
        Self::new(&CacheConfig::default(), "")
    }
}

#[allow(dead_code)]
impl<K: Eq + Hash + Clone, V: Clone> EntryCache<K, V> {
    /// Return a clone of the cached value for `key`, or `None` if absent
    /// or if the cache is disabled.
    pub(crate) fn get(&self, key: &K) -> Option<V> {
        if !self.enabled() { return None; }
        self.map.get(key).map(|r| r.value().clone())
    }

    /// True if `key` is present (and the cache is enabled).  Avoids cloning the
    /// value just to test membership.
    pub(crate) fn contains_key(&self, key: &K) -> bool {
        self.enabled() && self.map.contains_key(key)
    }

    /// Return the cached value for `key`, computing and storing it via `f`
    /// on a miss.  If disabled, always calls `f` and returns the result
    /// without storing it.
    ///
    /// **Cycle detection (panic on re-entry).**  If a recursive call into
    /// this method for the same key on the same thread happens, the
    /// thread-local cycle guard panics with an actionable message rather than
    /// deadlocking on the shard lock.
    ///
    /// If your closure can legitimately call back into this cache for the
    /// same key (e.g. via mutually-recursive memoised traversals over a
    /// graph that may contain cycles), use
    /// [`get_or_insert_with_cycle_safe`](Self::get_or_insert_with_cycle_safe)
    /// and provide a fallback to return on the cycle.
    pub(crate) fn get_or_insert_with(&self, key: K, f: impl FnOnce(&K) -> V) -> V {
        // Not epoch-gated: this authoritative interning path runs inside
        // cascades and must always persist.
        self.get_or_insert_with_impl(key, f, /* epoch_gated */ false, |k| {
            panic!(
                "cache '{}' detected recursive `get_or_insert_with` entry for the same key \
                 (key hash {}); use `get_or_insert_with_cycle_safe(key, compute, on_cycle)` \
                 if this cycle is expected",
                self.name, cache_key_hash(k),
            );
        })
    }

    /// Cycle-safe variant of [`get_or_insert_with`].  Identical fast path,
    /// but if the closure recursively calls back into this cache for the
    /// same key on the same thread, `on_cycle(&key)` is returned instead of
    /// recursing further.
    ///
    /// `on_cycle` is invoked synchronously with no lock held; it should
    /// return a domain-appropriate sentinel ("no information yet").  The
    /// result of `on_cycle` is not cached — the outer call's eventual real
    /// value is the one that gets stored.
    pub(crate) fn get_or_insert_with_cycle_safe<F, G>(
        &self,
        key:      K,
        f:        F,
        on_cycle: G,
    ) -> V
    where
        F: FnOnce(&K) -> V,
        G: FnOnce(&K) -> V,
    {
        self.get_or_insert_with_impl(key, f, /* epoch_gated */ true, on_cycle)
    }

    /// Shared implementation of the cycle-checked miss path.  Callers differ
    /// only in what `on_cycle` does (panic vs. return a fallback).
    fn get_or_insert_with_impl<F, G>(&self, key: K, f: F, epoch_gated: bool, on_cycle: G) -> V
    where
        F: FnOnce(&K) -> V,
        G: FnOnce(&K) -> V,
    {
        if !self.enabled() {
            return f(&key);
        }
        if let Some(r) = self.map.get(&key) {
            return r.value().clone();
        }
        // If this key is already being computed on the current thread, surface
        // `on_cycle` instead of recursing.
        let key_hash = cache_key_hash(&key);
        let _guard = match InProgressGuard::try_acquire(self.name, key_hash) {
            Some(g) => g,
            None    => return on_cycle(&key),
        };
        // Epoch-guarded (see [`Epoch`]): if a mutation batch overlapped the
        // compute the value is still returned but NOT stored, so the next miss
        // recomputes against the settled state.
        let entry_epoch = self.epoch.now();
        let v = f(&key);
        if !epoch_gated || self.epoch.now() == entry_epoch {
            self.map.insert(key.clone(), v.clone());
        }
        v
    }

    /// Insert or overwrite the entry for `key`.  No-op when disabled.
    pub(crate) fn update(&self, key: K, value: V) {
        if self.enabled() {
            self.map.insert(key, value);
            self.epoch.bump();
        }
    }

    /// Remove entries whose keys are in `keys`.  No-op when disabled or empty.
    pub(crate) fn evict_keys(&self, keys: &[K]) {
        if self.enabled() && !keys.is_empty() {
            for k in keys {
                self.map.remove(k);
            }
            self.epoch.bump();
        }
    }

    /// Remove all entries.  No-op when disabled.
    pub(crate) fn clear(&self) {
        if self.enabled() {
            self.map.clear();
            self.epoch.bump();
        }
    }

    /// Remove entries for which `predicate` returns `false`.
    /// No-op when disabled.
    pub(crate) fn retain<F>(&self, mut predicate: F)
    where
        F: FnMut(&K, &mut V) -> bool,
    {
        if self.enabled() {
            self.map.retain(|k, v| predicate(k, v));
            self.epoch.bump();
        }
    }

    // -- Persistence helpers ---------------------------------------------------

    /// Clone the entire map for serialisation.
    pub(crate) fn snapshot(&self) -> HashMap<K, V> {
        self.map.iter().map(|r| (r.key().clone(), r.value().clone())).collect()
    }

    /// Replace the entire map from a deserialised snapshot.
    /// No-op when disabled.
    pub(crate) fn restore(&self, map: HashMap<K, V>) {
        if self.enabled() {
            self.map.clear();
            for (k, v) in map { self.map.insert(k, v); }
            self.epoch.bump();
        }
    }

    // -- Utilities -------------------------------------------------------------

    /// Iterate over the cached entries.  Concurrent reads are safe;
    /// writers may block briefly per shard.  Iterates over whatever is
    /// currently stored even when the cache is disabled.
    pub(crate) fn for_each(&self, mut f: impl FnMut((&K, &V))) {
        for r in self.map.iter() {
            f((r.key(), r.value()));
        }
    }

    /// Return a clone of the first `(key, value)` for which `f` returns `true`,
    /// or `None` if no entry matches.
    pub(crate) fn find(&self, mut f: impl FnMut((&K, &V)) -> bool) -> Option<(K, V)> {
        for r in self.map.iter() {
            if f((r.key(), r.value())) {
                return Some((r.key().clone(), r.value().clone()))
            }
        }
        return None
    }

    /// Return clones of all `(key, value)` pairs for which `f` returns `true`.
    pub(crate) fn filter(&self, mut f: impl FnMut((&K, &V)) -> bool) -> Vec<(K, V)> {
        self.map.iter().filter_map(|r| {
            if f((r.key(), r.value())) {
                return Some((r.key().clone(), r.value().clone()))
            }
            return None
        }).collect()
    }

    /// Returns `true` if the map contains no entries.
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Returns the number of entries currently in the map.
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }
}

impl<K: Eq + Hash + Clone, V: Default> EntryCache<K, V> {
    /// Call `f` on the entry for `key`, inserting a `V::default()` if
    /// absent.  Useful for `Vec`-valued maps where individual items are
    /// pushed incrementally.  No-op when disabled.
    ///
    /// Holds the per-shard write lock for the duration of `f`, so `f`
    /// should be short (typically a single push or extend).
    pub(crate) fn modify_entry<F>(&self, key: K, f: F)
    where
        F: FnOnce(&mut V),
    {
        if self.enabled() {
            let mut entry = self.map.entry(key).or_default();
            f(entry.value_mut());
        }
    }
}

/// Per-cache monotonic invalidation clock for the epoch-guarded lazy fill.
///
/// Bumped on every store mutation that marks the cache stale — `clear` /
/// `evict_keys` / `retain` / `update` / `restore`.  A lazy fill records the
/// epoch before computing and memoises its result only if the epoch is
/// unchanged at insert; otherwise the value is returned to the caller but not
/// stored, closing the window where an in-flight fill straddling a delta commit
/// would persist a stale value.
///
/// Cross-cache dependencies are handled through the event graph rather than by
/// inspecting other caches' epochs: when an upstream cache changes, the cascade
/// clears the dependent cache, and that clear bumps the dependent's own epoch,
/// so its in-flight fill (computed off the stale upstream) is dropped.
#[derive(Debug, Default)]
pub(crate) struct Epoch(std::sync::atomic::AtomicU64);

impl Epoch {
    /// Current epoch value.
    pub(crate) fn now(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Advance the clock — call on every invalidating store mutation.
    pub(crate) fn bump(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// A whole-value cache backed by `RwLock<Option<T>>`.
///
/// Either fully populated or absent.  Used for derived structures that are
/// computed as a unit (e.g. `SortAnnotations`, taxonomy edge list).
///
/// When the named cache is disabled, `get_or_init` always calls `f` and
/// returns the result without storing it.  `install` and `invalidate` are
/// no-ops when disabled (though `invalidate` always clears — see docs).
#[derive(Debug)]
pub(crate) struct LayerCache<T> {
    inner:  RwLock<Option<T>>,
    config: CacheConfig,
    name:   &'static str,
    /// Invalidation clock (see [`Epoch`]); bumped on `install` / `invalidate` /
    /// `modify`.
    epoch:  Epoch,
}

#[allow(dead_code)]
impl<T: Clone> LayerCache<T> {
    /// Create a new, empty cache sharing `config`, identified by `name`.
    pub(crate) fn new(config: &CacheConfig, name: &'static str) -> Self {
        Self { inner: RwLock::new(None), config: config.clone(), name, epoch: Epoch::default() }
    }

    /// Returns `true` when this cache's name is enabled in the shared config.
    fn enabled(&self) -> bool {
        self.config.is_enabled(self.name)
    }

    // -- Reads -----------------------------------------------------------------

    /// Return the cached value, computing and storing it via `f` on a miss.
    /// If disabled, always calls `f` and returns the result without storing.
    ///
    /// **Cycle detection (panic on re-entry).**  The lock is released before
    /// `f` runs, so a recursive call back into `get_or_init` on this same
    /// cache from within `f` would re-enter as a miss and recurse forever.
    /// This method panics with an actionable message when re-entry is
    /// detected.
    ///
    /// For closures that may legitimately re-enter, use
    /// [`get_or_init_cycle_safe`](Self::get_or_init_cycle_safe).
    pub(crate) fn get_or_init(&self, f: impl FnOnce() -> T) -> T {
        self.get_or_init_impl(f, /* epoch_gated */ false, || {
            panic!(
                "cache '{}' detected recursive `get_or_init` entry; \
                 use `get_or_init_cycle_safe(compute, on_cycle)` \
                 if this cycle is expected",
                self.name,
            );
        })
    }

    /// Cycle-safe variant of [`get_or_init`].  If the compute closure
    /// recursively re-enters this same cache on the same thread,
    /// `on_cycle()` is returned instead of recursing.  The cycle value
    /// is not stored; the outer call's eventual real value is.
    #[allow(dead_code)]
    pub(crate) fn get_or_init_cycle_safe<F, G>(&self, f: F, on_cycle: G) -> T
    where
        F: FnOnce() -> T,
        G: FnOnce() -> T,
    {
        self.get_or_init_impl(f, /* epoch_gated */ true, on_cycle)
    }

    /// Shared implementation of the cycle-checked miss path.
    fn get_or_init_impl<F, G>(&self, f: F, epoch_gated: bool, on_cycle: G) -> T
    where
        F: FnOnce() -> T,
        G: FnOnce() -> T,
    {
        if self.enabled() {
            if let Some(v) = self.inner.read().unwrap().as_ref() {
                return v.clone();
            }
        }
        // LayerCache has no key; the `(name, 0)` pair disambiguates re-entry
        // for a given instance.
        let _guard = match InProgressGuard::try_acquire(self.name, 0) {
            Some(g) => g,
            None    => return on_cycle(),
        };
        // Epoch-guarded memoisation (see [`Epoch`]): persist only if no mutation
        // batch overlapped the compute; otherwise return the value un-cached.
        let entry_epoch = self.epoch.now();
        let v = f();
        if self.enabled() && (!epoch_gated || self.epoch.now() == entry_epoch) {
            *self.inner.write().unwrap() = Some(v.clone());
        }
        v
    }

    // -- Writes ----------------------------------------------------------------

    /// Install a precomputed value, replacing whatever is currently cached,
    /// without calling the compute closure.  No-op when disabled.
    pub(crate) fn install(&self, value: T) {
        if self.enabled() {
            *self.inner.write().unwrap() = Some(value);
            self.epoch.bump();
        }
    }

    /// Clear the cached value so the next `get_or_init` call recomputes it.
    /// Always clears regardless of `enabled` so that a cache that was
    /// enabled, populated, then disabled can still be cleared on invalidation.
    pub(crate) fn invalidate(&self) {
        *self.inner.write().unwrap() = None;
        self.epoch.bump();
    }

    /// Apply a mutable transformation to the stored value.
    /// No-op if the cache is not populated or is disabled.
    pub(crate) fn modify<F>(&self, f: F)
    where
        F: FnOnce(&mut T),
    {
        if self.enabled() {
            if let Some(v) = self.inner.write().unwrap().as_mut() {
                f(v);
                self.epoch.bump();
            }
        }
    }

    // -- Persistence helpers ---------------------------------------------------

    /// Clone the current value for serialisation.  Returns `None` if the
    /// cache is not populated.
    pub(crate) fn snapshot(&self) -> Option<T> {
        self.inner.read().unwrap().clone()
    }

    // -- Utilities -------------------------------------------------------------

    /// Returns `true` if the cache currently holds a value.
    pub(crate) fn is_populated(&self) -> bool {
        self.inner.read().unwrap().is_some()
    }

    /// Access the cached value via a read-only closure without cloning it.
    ///
    /// The closure receives `Some(&T)` if the cache is populated, `None`
    /// otherwise (including when disabled).  Returns the closure's result.
    ///
    /// Use this for hot-path key lookups into `LayerCache<HashMap<K,V>>`
    /// or membership tests on `LayerCache<HashSet<T>>` where copying the
    /// entire container for each access would be too expensive.
    pub(crate) fn with_ref<R>(&self, f: impl FnOnce(Option<&T>) -> R) -> R {
        let guard = self.inner.read().unwrap();
        f(guard.as_ref())
    }
}

impl<T: Clone> Default for LayerCache<T> {
    /// Constructs a standalone, always-enabled cache with no shared config.
    /// Prefer explicit `new(config, name)` for production use.
    fn default() -> Self {
        Self::new(&CacheConfig::default(), "")
    }
}

// -- Specialisations for Vec-valued caches ------------------------------------

#[allow(dead_code)]
impl<T: Clone> LayerCache<Vec<T>> {
    /// Push `item` to the stored `Vec`.  No-op if not populated or disabled.
    pub(crate) fn push(&self, item: T) {
        if self.enabled() {
            if let Some(v) = self.inner.write().unwrap().as_mut() {
                v.push(item);
            }
        }
    }

    /// Retain only elements matching the predicate.
    /// No-op if not populated or disabled.
    pub(crate) fn retain<F>(&self, mut f: F)
    where
        F: FnMut(&T) -> bool,
    {
        if self.enabled() {
            if let Some(v) = self.inner.write().unwrap().as_mut() {
                v.retain(|x| f(x));
            }
        }
    }

    /// Number of elements in the stored `Vec`, or 0 if not populated.
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.with_ref(|opt| opt.map(Vec::len).unwrap_or(0))
    }

    /// Returns `true` if the stored `Vec` is empty or not yet populated.
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.with_ref(|opt| opt.map(Vec::is_empty).unwrap_or(true))
    }
}

// EagerIndex<T>

/// A [`CacheConfig`]-aware wrapper for an *eagerly-maintained* data structure.
///
/// Unlike [`LayerCache`], there is no lazy-compute / "miss" path.  The value
/// is always live when the cache is enabled; the caller is responsible for
/// keeping it up-to-date via [`EagerIndex::modify`].  The [`CacheConfig`]
/// gate lets tests opt out of all maintenance by disabling the named cache,
/// which makes every write a no-op and every read return `None`.
///
/// ## Lifecycle
///
/// | State             | `inner`          | Description                              |
/// |-------------------|------------------|------------------------------------------|
/// | Enabled (initial) | `Some(initial)`  | Live; all `modify` calls take effect     |
/// | Disabled          | `None`           | All writes and reads are no-ops          |
/// | After `install`   | `Some(restored)` | Value replaced from an LMDB restore      |
///
/// ## Contrast with [`LayerCache`]
///
/// | Property             | `LayerCache<T>`              | `EagerIndex<T>`             |
/// |----------------------|------------------------------|-----------------------------|
/// | Initial state        | `None` (unpopulated)         | `Some(initial)` (live)      |
/// | Compute-on-miss      | Yes — via `get_or_init`      | No                          |
/// | Incremental updates  | No (whole-value replace)     | Yes — via `modify`          |
/// | Invalidation         | `invalidate()` → recompute   | No invalidation concept     |
#[derive(Debug)]
pub(crate) struct EagerIndex<T> {
    inner:  RwLock<T>,
    config: CacheConfig,
    name:   &'static str,
}

#[allow(dead_code)]
impl<T> EagerIndex<T> {
    /// Create a new index, always seeded with `initial`.  The value is live
    /// regardless of the `enabled` flag — `enabled` gates only the persistence
    /// path (`install`), not initialization or in-memory mutation.
    pub(crate) fn new(config: &CacheConfig, name: &'static str, initial: T) -> Self {
        let value = initial;
        Self { inner: RwLock::new(value), config: config.clone(), name }
    }

    fn enabled(&self) -> bool {
        self.config.is_enabled(self.name)
    }
}

#[allow(dead_code)]
impl<T: Clone> EagerIndex<T> {
    /// Access the current value via a read-only closure without cloning it.
    pub(crate) fn with_ref<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        let guard = self.inner.read().unwrap();
        f(&guard)
    }

    /// Apply a mutable update.
    pub(crate) fn modify(&self, f: impl FnOnce(&mut T)) {
        let mut v = self.inner.write().unwrap();
        f(&mut v);
    }

    /// Apply a mutable update and extract a return value.
    ///
    /// Use for methods that both mutate the index and return a result.
    pub(crate) fn update_with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut v = self.inner.write().unwrap();
        f(&mut v)
    }

    /// Replace the current value wholesale.  No-op when disabled.
    pub(crate) fn install(&self, value: T) {
        if self.enabled() {
            *self.inner.write().unwrap() = value;
        }
    }

    /// Clone the current value for serialisation.
    pub(crate) fn snapshot(&self) -> T {
        self.inner.read().unwrap().clone()
    }
}

impl<T: Clone + Default> Default for EagerIndex<T> {
    /// Constructs a standalone, always-enabled index with a default initial
    /// value.  Prefer `new(config, name, initial)` for production use so all
    /// indices in a layer share a single [`CacheConfig`].
    fn default() -> Self {
        Self::new(&CacheConfig::default(), "", T::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: a cache config with a single cache disabled
    fn disabled_cfg(name: &'static str) -> CacheConfig {
        let cfg = CacheConfig::default();
        cfg.disable(name);
        cfg
    }

    // -- EntryCache -----------------------------------------------------------

    #[test]
    fn entry_cache_miss_then_hit() {
        let cfg = CacheConfig::default();
        let cache: EntryCache<u32, u32> = EntryCache::new(&cfg, "test");
        let mut calls = 0u32;
        let v1 = cache.get_or_insert_with(1, |_| { calls += 1; 42 });
        let v2 = cache.get_or_insert_with(1, |_| { calls += 1; 99 }); // should hit cache
        assert_eq!(v1, 42);
        assert_eq!(v2, 42);
        assert_eq!(calls, 1, "compute fn should be called exactly once");
    }

    #[test]
    fn epoch_gates_derived_memoisation_but_not_interning() {
        let cfg = CacheConfig::default();
        let cache: EntryCache<u64, u64> = EntryCache::new(&cfg, "test");

        // No invalidation during the compute: a derived fill memoises.
        assert_eq!(cache.get_or_insert_with_cycle_safe(1, |_| 10, |_| 0), 10);
        assert_eq!(cache.get(&1), Some(10), "settled fill is memoised");

        // A derived fill whose compute is interrupted by an invalidation of this
        // cache (here `clear()` bumps its epoch — modelling a concurrent cascade
        // clearing it mid-fill) is RETURNED but NOT memoised.
        let v = cache.get_or_insert_with_cycle_safe(2, |_| { cache.clear(); 20 }, |_| 0);
        assert_eq!(v, 20, "the freshly-computed value is still returned");
        assert_eq!(cache.get(&2), None,
            "a derived fill invalidated mid-compute must not be memoised");

        // The authoritative interning variant ignores the epoch — it always
        // persists, even if a mutation lands during its compute.
        cache.get_or_insert_with(3, |_| { cache.clear(); 30 });
        assert_eq!(cache.get(&3), Some(30), "authoritative inserts persist regardless");

        // With no mid-compute invalidation, derived fills memoise again.
        assert_eq!(cache.get_or_insert_with_cycle_safe(4, |_| 40, |_| 0), 40);
        assert_eq!(cache.get(&4), Some(40), "settled fill memoises once the cache is quiescent");
    }

    #[test]
    fn entry_cache_disabled_always_computes() {
        let cache: EntryCache<u32, u32> = EntryCache::new(&disabled_cfg("test"), "test");
        let mut calls = 0u32;
        cache.get_or_insert_with(1, |_| { calls += 1; 42 });
        cache.get_or_insert_with(1, |_| { calls += 1; 42 });
        assert_eq!(calls, 2, "disabled cache must always call compute fn");
        assert!(cache.is_empty(), "disabled cache must store nothing");
    }

    #[test]
    fn entry_cache_disabled_get_returns_none() {
        let cache: EntryCache<u32, u32> = EntryCache::new(&disabled_cfg("test"), "test");
        cache.map.insert(1, 42); // bypass enabled check
        // get should still return None when disabled
        assert_eq!(cache.get(&1), None);
    }

    #[test]
    fn entry_cache_evict_keys() {
        let cfg = CacheConfig::default();
        let cache: EntryCache<u32, u32> = EntryCache::new(&cfg, "test");
        cache.update(1, 10);
        cache.update(2, 20);
        cache.update(3, 30);
        cache.evict_keys(&[1, 3]);
        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.get(&2), Some(20));
        assert_eq!(cache.get(&3), None);
    }

    #[test]
    fn entry_cache_clear() {
        let cfg = CacheConfig::default();
        let cache: EntryCache<u32, u32> = EntryCache::new(&cfg, "test");
        cache.update(1, 1);
        cache.update(2, 2);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn entry_cache_retain() {
        let cfg = CacheConfig::default();
        let cache: EntryCache<u32, u32> = EntryCache::new(&cfg, "test");
        cache.update(1, 10);
        cache.update(2, 20);
        cache.update(3, 30);
        cache.retain(|k, _| k % 2 == 0); // keep only even keys
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(&2), Some(20));
    }

    #[test]
    fn entry_cache_snapshot_and_restore() {
        let cfg = CacheConfig::default();
        let cache: EntryCache<u32, u32> = EntryCache::new(&cfg, "test");
        cache.update(1, 100);
        cache.update(2, 200);
        let snap = cache.snapshot();
        cache.clear();
        assert!(cache.is_empty());
        cache.restore(snap);
        assert_eq!(cache.get(&1), Some(100));
        assert_eq!(cache.get(&2), Some(200));
    }

    #[test]
    fn entry_cache_restore_no_op_when_disabled() {
        let cache: EntryCache<u32, u32> = EntryCache::new(&disabled_cfg("test"), "test");
        let mut map = std::collections::HashMap::new();
        map.insert(1u32, 42u32);
        cache.restore(map);
        // Disabled: restore should not populate the map
        assert!(cache.map.is_empty());
    }

    #[test]
    fn entry_cache_modify_entry() {
        let cfg = CacheConfig::default();
        let cache: EntryCache<u32, Vec<u32>> = EntryCache::new(&cfg, "test");
        cache.modify_entry(1, |v| v.push(10));
        cache.modify_entry(1, |v| v.push(20));
        cache.modify_entry(2, |v| v.push(30));
        assert_eq!(cache.get(&1), Some(vec![10, 20]));
        assert_eq!(cache.get(&2), Some(vec![30]));
    }

    #[test]
    fn entry_cache_modify_entry_no_op_when_disabled() {
        let cache: EntryCache<u32, Vec<u32>> = EntryCache::new(&disabled_cfg("test"), "test");
        cache.modify_entry(1, |v| v.push(10));
        assert!(cache.is_empty());
    }

    // -- LayerCache -----------------------------------------------------------

    #[test]
    fn layer_cache_miss_then_hit() {
        let cfg = CacheConfig::default();
        let cache: LayerCache<u32> = LayerCache::new(&cfg, "test");
        let mut calls = 0u32;
        let v1 = cache.get_or_init(|| { calls += 1; 42 });
        let v2 = cache.get_or_init(|| { calls += 1; 99 }); // should hit
        assert_eq!(v1, 42);
        assert_eq!(v2, 42);
        assert_eq!(calls, 1);
    }

    #[test]
    fn layer_cache_disabled_always_computes() {
        let cache: LayerCache<u32> = LayerCache::new(&disabled_cfg("test"), "test");
        let mut calls = 0u32;
        cache.get_or_init(|| { calls += 1; 42 });
        cache.get_or_init(|| { calls += 1; 42 });
        assert_eq!(calls, 2);
        assert!(!cache.is_populated());
    }

    #[test]
    fn layer_cache_install_and_populated() {
        let cfg = CacheConfig::default();
        let cache: LayerCache<u32> = LayerCache::new(&cfg, "test");
        assert!(!cache.is_populated());
        cache.install(99);
        assert!(cache.is_populated());
        assert_eq!(cache.get_or_init(|| 0), 99); // should return installed value
    }

    #[test]
    fn layer_cache_install_no_op_when_disabled() {
        let cache: LayerCache<u32> = LayerCache::new(&disabled_cfg("test"), "test");
        cache.install(99);
        assert!(!cache.is_populated());
    }

    #[test]
    fn layer_cache_invalidate() {
        let cfg = CacheConfig::default();
        let cache: LayerCache<u32> = LayerCache::new(&cfg, "test");
        cache.install(42);
        assert!(cache.is_populated());
        cache.invalidate();
        assert!(!cache.is_populated());
    }

    #[test]
    fn layer_cache_invalidate_works_even_when_disabled() {
        // invalidate() always clears — documented in the method's doc comment.
        let cache: LayerCache<u32> = LayerCache::new(&disabled_cfg("test"), "test");
        // Manually populate to test invalidate bypasses enabled check
        *cache.inner.write().unwrap() = Some(42);
        cache.invalidate();
        assert!(!cache.is_populated());
    }

    #[test]
    fn layer_cache_snapshot_none_when_unpopulated() {
        let cfg = CacheConfig::default();
        let cache: LayerCache<u32> = LayerCache::new(&cfg, "test");
        assert_eq!(cache.snapshot(), None);
    }

    #[test]
    fn layer_cache_snapshot_some_when_populated() {
        let cfg = CacheConfig::default();
        let cache: LayerCache<u32> = LayerCache::new(&cfg, "test");
        cache.install(77);
        assert_eq!(cache.snapshot(), Some(77));
    }

    #[test]
    fn layer_cache_vec_push_len_is_empty() {
        let cfg = CacheConfig::default();
        let cache: LayerCache<Vec<u32>> = LayerCache::new(&cfg, "test");
        cache.install(vec![]);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        cache.push(1);
        cache.push(2);
        assert!(!cache.is_empty());
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn layer_cache_vec_retain() {
        let cfg = CacheConfig::default();
        let cache: LayerCache<Vec<u32>> = LayerCache::new(&cfg, "test");
        cache.install(vec![1, 2, 3, 4]);
        cache.retain(|&x| x % 2 == 0);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn layer_cache_modify() {
        let cfg = CacheConfig::default();
        let cache: LayerCache<u32> = LayerCache::new(&cfg, "test");
        // No-op when not populated
        cache.modify(|v| *v = 99);
        assert!(!cache.is_populated());
        // Modifies when populated
        cache.install(10);
        cache.modify(|v| *v += 5);
        assert_eq!(cache.get_or_init(|| 0), 15);
    }

    // -- CacheConfig ----------------------------------------------------------

    #[test]
    fn cache_config_default_enables_all() {
        let cfg = CacheConfig::default();
        assert!(cfg.is_enabled("anything"));
        assert!(cfg.is_enabled("syntactic::occurrences"));
        assert!(cfg.is_enabled("semantic::taxonomy"));
    }

    #[test]
    fn cache_config_disable_specific() {
        let cfg = CacheConfig::default();
        cfg.disable("semantic::is_instance");
        assert!(!cfg.is_enabled("semantic::is_instance"));
        assert!(cfg.is_enabled("semantic::is_class")); // unaffected
    }

    #[test]
    fn cache_config_enable_restores() {
        let cfg = CacheConfig::default();
        cfg.disable("semantic::is_instance");
        assert!(!cfg.is_enabled("semantic::is_instance"));
        cfg.enable("semantic::is_instance");
        assert!(cfg.is_enabled("semantic::is_instance"));
    }

    #[test]
    fn cache_config_with_disabled_list() {
        let cfg = CacheConfig::with_disabled(&[
            "semantic::is_instance",
            "semantic::is_class",
        ]);
        assert!(!cfg.is_enabled("semantic::is_instance"));
        assert!(!cfg.is_enabled("semantic::is_class"));
        assert!(cfg.is_enabled("semantic::domain"));
    }

    #[test]
    fn cache_config_shared_across_clones() {
        let cfg = CacheConfig::default();
        let cfg2 = cfg.clone();
        // Disabling via the clone is visible through the original
        cfg2.disable("semantic::is_instance");
        assert!(!cfg.is_enabled("semantic::is_instance"),
            "shared Arc: disable on clone must be visible on original");
        cfg.enable("semantic::is_instance");
        assert!(cfg2.is_enabled("semantic::is_instance"),
            "enable on original must be visible on clone");
    }

    // -- EagerIndex -----------------------------------------------------------
    //
    // EagerIndex is ALWAYS initialized; `enabled` gates only the persistence
    // path (`install`).  In-memory `modify` / `update_with` / `with_ref` /
    // `snapshot` always operate on the live value (no `Option`, no
    // `is_populated`).

    #[test]
    fn eager_index_starts_initialized_when_enabled() {
        let cfg = CacheConfig::default();
        let idx: EagerIndex<u32> = EagerIndex::new(&cfg, "test", 0);
        assert_eq!(idx.with_ref(|v| *v), 0);
    }

    #[test]
    fn eager_index_starts_initialized_even_when_disabled() {
        // Disabled gates persistence, not initialization: the value is live.
        let cfg = CacheConfig::default();
        cfg.disable("test");
        let idx: EagerIndex<u32> = EagerIndex::new(&cfg, "test", 99);
        assert_eq!(idx.with_ref(|v| *v), 99);
    }

    #[test]
    fn eager_index_modify_updates_value() {
        let cfg = CacheConfig::default();
        let idx: EagerIndex<u32> = EagerIndex::new(&cfg, "test", 10);
        idx.modify(|v| *v += 5);
        assert_eq!(idx.with_ref(|v| *v), 15);
    }

    #[test]
    fn eager_index_modify_applies_even_when_disabled() {
        // `modify` is an in-memory mutation, unaffected by the persistence gate.
        let cfg = CacheConfig::default();
        cfg.disable("test");
        let idx: EagerIndex<u32> = EagerIndex::new(&cfg, "test", 10);
        idx.modify(|v| *v += 5);
        assert_eq!(idx.with_ref(|v| *v), 15);
    }

    #[test]
    fn eager_index_update_with_returns_value() {
        let cfg = CacheConfig::default();
        let idx: EagerIndex<u32> = EagerIndex::new(&cfg, "test", 7);
        let result = idx.update_with(|v| {
            let old = *v;
            *v = 0;
            old
        });
        assert_eq!(result, 7);
        assert_eq!(idx.with_ref(|v| *v), 0);
    }

    #[test]
    fn eager_index_update_with_applies_even_when_disabled() {
        let cfg = CacheConfig::default();
        cfg.disable("test");
        let idx: EagerIndex<u32> = EagerIndex::new(&cfg, "test", 7);
        let result = idx.update_with(|v| { *v += 1; *v });
        assert_eq!(result, 8);
        assert_eq!(idx.with_ref(|v| *v), 8);
    }

    #[test]
    fn eager_index_install_replaces_value() {
        let cfg = CacheConfig::default();
        let idx: EagerIndex<u32> = EagerIndex::new(&cfg, "test", 1);
        idx.install(42);
        assert_eq!(idx.with_ref(|v| *v), 42);
    }

    #[test]
    fn eager_index_install_noop_when_disabled() {
        // `install` is the restore/persistence path; disabled ⇒ value untouched.
        let cfg = CacheConfig::default();
        cfg.disable("test");
        let idx: EagerIndex<u32> = EagerIndex::new(&cfg, "test", 1);
        idx.install(42);
        assert_eq!(idx.with_ref(|v| *v), 1,
            "install is gated by `enabled`; the value stays at its initial");
    }

    #[test]
    fn eager_index_snapshot_returns_current_value() {
        let cfg = CacheConfig::default();
        let idx: EagerIndex<u32> = EagerIndex::new(&cfg, "test", 5);
        idx.modify(|v| *v = 99);
        assert_eq!(idx.snapshot(), 99);
    }

    #[test]
    fn eager_index_snapshot_returns_value_even_when_disabled() {
        let cfg = CacheConfig::default();
        cfg.disable("test");
        let idx: EagerIndex<u32> = EagerIndex::new(&cfg, "test", 5);
        assert_eq!(idx.snapshot(), 5);
    }

    #[test]
    fn eager_index_default_is_initialized_with_default_value() {
        let idx: EagerIndex<u32> = EagerIndex::default();
        assert_eq!(idx.with_ref(|v| *v), 0);
    }
}
