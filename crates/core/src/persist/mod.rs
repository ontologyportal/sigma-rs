// crates/core/src/persist/mod.rs
//
// Persistence module.
//
// All persistence flows through the cache-snapshot seam: each cache freezes its
// value to / thaws it from a `PersistenceBackend` keyed by its `NAME` (see
// `crate::cache::persistence`), driven by `Layer::snapshot_caches` /
// `Layer::restore_caches_from`.  This module provides the backends.
//
// `backend` (the `PersistenceBackend` trait, the `PersistenceEngine` enum, and
// the in-memory / no-op / LMDB backends) is ALWAYS compiled so the snapshot API
// exists (as a no-op) without the `persist` feature.  The heed/LMDB-backed
// `LmdbEnv` is gated behind `--features persist`.

pub(crate) mod backend;

#[cfg(feature = "persist")]
mod env;

// -- backend abstraction (always available) ---------------------------------
pub(crate) use backend::{PersistenceBackend, PersistenceEngine};
#[cfg(test)]
pub(crate) use backend::MemoryBackend;

// -- LMDB storage (persist feature only) ------------------------------------
#[cfg(feature = "persist")]
pub(crate) use env::LmdbEnv;
