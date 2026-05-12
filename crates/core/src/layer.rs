//! Generic stack-position trait for the layered KB architecture.
//!
//! The KnowledgeBase is built as a stack of layers: SyntacticLayer (raw
//! parse store) at the bottom, SemanticLayer (taxonomy + semantic queries)
//! in the middle, TranslationLayer (TPTP translation state) at the top.
//! Each layer owns its inner — the layer directly below — so downward
//! traversal via `inner()` is a direct field reference.
//!
//! Upward traversal via `outer()` is not wired into the layer values
//! themselves (that would create self-referential structs). Callers that
//! need an outer layer go through `KnowledgeBase` accessors
//! (`kb.semantic()`, `kb.translation()`) instead; `outer()` currently
//! returns `None`.

use std::sync::OnceLock;

use crate::cache::events::{build_schedule_indexed, CycleError, ReactorDecl, Event};
use crate::cache::router::{route_with_schedule, ReactorEntry, RouteOutcome};
use crate::cache::persistence::PersistableCache;
use crate::persist::PersistenceBackend;

/// A layer's reactor schedule, computed once and memoised.  The schedule (which
/// reactors run in which cohort) is a pure function of the static reactor graph,
/// so it is identical across every cascade — each `Layer` impl owns one
/// `OnceLock` (via [`Layer::schedule_cell`]) that holds it after the first
/// cascade.  Stored as decl-index cohorts; `Err` caches a (build-bug) cycle.
pub(crate) type ScheduleCell = OnceLock<Result<Vec<Vec<usize>>, CycleError>>;

/// Stack-position trait. Each layer announces what's directly below
/// (`Inner`) and above (`Outer`) it, and registers its cache reactors so a
/// cascade driven from any layer sees every cache from there down the stack.
#[allow(dead_code)]
pub(crate) trait Layer {
    type Inner: Layer;
    type Outer: Layer;

    /// Reference to the layer directly below `self`, or `None` if
    /// `self` is the bottom of the stack.
    fn inner(&self) -> Option<&Self::Inner>;

    /// Reference to the layer directly above `self`, or `None` if
    /// `self` is the top of the stack OR the back-pointer is not
    /// wired up (the current default — see module docs).
    fn outer(&self) -> Option<&Self::Outer>;

    /// This layer's *own* cache reactors (not the inner layers').  Override per
    /// layer to register its caches via [`crate::cache::router::bind`]; the
    /// default (no reactors) is correct for [`NoLayer`].
    fn own_reactors(&self) -> Vec<ReactorEntry<'_>>;

    /// The process-wide [`ScheduleCell`] memoising this layer type's reactor
    /// schedule.  Each impl returns a `&'static` cell backed by a method-local
    /// `static` — one per concrete layer type, which is sound because
    /// [`Self::reactors`] returns a fixed reactor list (same names, same order)
    /// for every instance of the type.
    fn schedule_cell(&self) -> &'static ScheduleCell;

    /// The shared [`CacheConfig`] for this stack — every layer returns the same
    /// `Arc`-backed config (the inner layers delegate downward to the root that
    /// owns it).  The cascade reads its parallelism knobs (`max_threads` /
    /// `parallel_floor`) to decide how to fan a cohort out across threads.
    fn cache_config(&self) -> &crate::cache::CacheConfig;

    /// Every reactor from this layer *down* the stack — this layer's own plus,
    /// recursively, each inner layer's.  Driving a cascade from the top layer
    /// therefore sees every cache in the KB, bound to its owning layer.
    fn reactors(&self) -> Vec<ReactorEntry<'_>> {
        let mut rs = self.own_reactors();
        if let Some(inner) = self.inner() {
            rs.extend(inner.reactors());
        }
        rs
    }

    /// Drive the reactive cascade for `seed` across this layer's stack via the
    /// generic [`route`], returning the emitted follow-ons plus any per-event
    /// errors (non-fatal — see [`RouteOutcome`]).  The layer-specific producer
    /// (classification, `&mut self`) and structural steps (`prime_caches`, …)
    /// bracket this call; this is just the fan-out.
    /// The `retain` argument controls which intermediate event types are collected
    /// and returned (useful for collecting intermediate results from the event 
    /// responses)
    fn cascade(&self, seed: Vec<Event>) -> RouteOutcome {
        let entries = self.reactors();
        // The schedule is a pure function of the (static) reactor graph, so
        // compute it once and reuse it across every cascade — only the
        // `entries` (whose `react` closures borrow `&self`) are rebuilt per call.
        let schedule = self.schedule_cell().get_or_init(|| {
            let decls: Vec<ReactorDecl> = entries
                .iter()
                .map(|e| ReactorDecl { name: e.name, consumes: e.consumes, produces: e.produces, reads: e.reads })
                .collect();
            build_schedule_indexed(&decls)
        });
        route_with_schedule(&entries, schedule, self.cache_config(), seed)
    }

    /// This layer's *own* persistable caches (not the inner layers').  Override
    /// per layer to register the caches whose values should be snapshotted;
    /// the default (none) is correct for [`NoLayer`] and for layers whose state
    /// is entirely rebuildable (derived caches can be regenerated on restore).
    fn own_persistable(&self) -> Vec<&dyn PersistableCache> {
        Vec::new()
    }

    /// Every persistable cache from this layer *down* the stack — this layer's
    /// own plus, recursively, each inner layer's.  Snapshotting from the top
    /// layer therefore freezes every registered cache in the KB.
    fn persistable(&self) -> Vec<&dyn PersistableCache> {
        let mut cs = self.own_persistable();
        if let Some(inner) = self.inner() {
            cs.extend(inner.persistable());
        }
        cs
    }

    /// Freeze the current state of every registered cache (this layer down) to
    /// `backend`, then commit atomically.  A no-op when the `persist` feature
    /// is off (each `freeze` compiles to nothing) or the backend is `Noop`.
    fn snapshot_caches(
        &self,
        backend: &mut dyn PersistenceBackend,
    ) -> Result<(), crate::Diagnostic> {
        for cache in self.persistable() {
            cache.freeze(backend)?;
        }
        backend.commit()
    }

    /// Thaw every registered cache from this layer *down*, inner layers first
    /// (mirroring [`Self::initialize_caches`]).  Recursing — rather than thawing
    /// the flattened [`Self::persistable`] list — lets a layer override this to
    /// control the order of its *own* caches' thaw (e.g. `SyntacticLayer` thaws
    /// the symbol table before the sentence store so the latter can resolve
    /// symbol ids back to shared `Arc`s; see its override).  Caches whose key is
    /// absent are left untouched (to be rebuilt via `initialize_caches`).
    fn restore_caches_from(
        &self,
        backend: &dyn PersistenceBackend,
    ) -> Result<(), crate::Diagnostic> {
        if let Some(inner) = self.inner() {
            inner.restore_caches_from(backend)?;
        }
        for cache in self.own_persistable() {
            cache.thaw(backend)?;
        }
        Ok(())
    }

    /// Prime *this* layer's eager caches from the source of truth (its lower
    /// caches).  Default: nothing — a layer overrides this to call
    /// `self.<eager_cache>.initialize(self)` for each cache that builds from the
    /// store (e.g. `tax_edges` from the sentence store).  Override of
    /// [`Self::own_reactors`]'s sibling for the build-once path.
    fn initialize_own_caches(&self) {}

    /// Prime every eager cache from this layer *down*, inner layers first (so a
    /// higher layer's `initialize` sees its lower layers already built).
    ///
    /// The complement of [`Self::restore_caches_from`]: call this at fresh
    /// setup.  Each cache's `initialize` self-guards on "already populated", so
    /// calling this *after* a restore is a no-op for the thawed caches and fills
    /// only the ones the snapshot didn't carry — i.e. initialization runs only
    /// where the cache was **not** restored from persistent storage.
    fn initialize_caches(&self) {
        if let Some(inner) = self.inner() {
            inner.initialize_caches();
        }
        self.initialize_own_caches();
    }
}

/// The interchangeable TOP of the stack: any layer sitting directly on
/// the semantic layer (`TranslationLayer` for the TPTP/Vampire pipeline,
/// `ProverLayer` for the native saturation prover).  [`KnowledgeBase`]
/// is generic over this, defaulting to `TranslationLayer`, so the two
/// tops are swappable without touching the layers below.
///
/// [`KnowledgeBase`]: crate::kb::KnowledgeBase
/// `Sync` is a supertrait: layers are shared across threads by the
/// cascade's parallel cohorts and by `parallel`-feature query paths
/// (`validate`'s rayon fan-out borrows the layer through `&self`).
/// The bound is `Layer` (not `Layer<Inner = SemanticLayer>`): a top layer need not
/// sit *directly* on the semantic layer, only reach it. `semantic()` is the single
/// sanctioned reach — a layer that wraps another top layer (e.g. a future
/// `ExternalProverLayer` over `TranslationLayer`) delegates it one hop down. This
/// lets the proving stack nest while keeping the same accessor contract.
pub trait TopLayer: Sync + Layer {
    /// Build this layer over a fresh (or just-restored) semantic stack.
    fn from_semantic(semantic: crate::semantics::SemanticLayer) -> Self;
    /// Build a fresh, empty layer over `semantic` that carries **this** layer's
    /// configuration (e.g. a configured external prover backend, cache config)
    /// but none of its cache state — the construction half of
    /// [`KnowledgeBase::snapshot_clone`](crate::kb::KnowledgeBase).
    /// [`from_semantic`](Self::from_semantic) resets such config to defaults,
    /// which would silently drop a configured prover on a clone; layers with
    /// config worth preserving override this.  Default: `from_semantic`.
    fn fresh_config_clone(&self, semantic: crate::semantics::SemanticLayer) -> Self
    where
        Self: Sized,
    {
        Self::from_semantic(semantic)
    }
    /// The owned (or transitively-owned) semantic layer.
    fn semantic(&self) -> &crate::semantics::SemanticLayer;
    /// Mutable access to the (transitively-owned) semantic layer.
    fn semantic_mut(&mut self) -> &mut crate::semantics::SemanticLayer;
}

/// Marker terminating the stack at either end. Has no inhabitants.
#[allow(dead_code)]
pub(crate) enum NoLayer {}

impl Layer for NoLayer {
    type Inner = NoLayer;
    type Outer = NoLayer;
    fn inner(&self) -> Option<&Self::Inner> { None }
    fn outer(&self) -> Option<&Self::Outer> { None }
    // `NoLayer` is uninhabited, so these are never called.
    fn own_reactors(&self) -> Vec<ReactorEntry<'_>> { match *self {} }
    fn schedule_cell(&self) -> &'static ScheduleCell { match *self {} }
    fn cache_config(&self) -> &crate::cache::CacheConfig { match *self {} }
}
// Persistence is now the unified cache-snapshot seam: `Layer::snapshot_caches`
// / `Layer::restore_caches_from` (above) freeze/thaw every `own_persistable()`
// cache through a `PersistenceBackend`.  The old per-layer `PersistLayer`
// (bespoke LMDB blob wiring + `kb_version` gating) was removed.
