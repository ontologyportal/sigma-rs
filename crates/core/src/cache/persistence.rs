//! The cache side of persistence: `PersistableCache` lets a cache freeze its
//! value to / thaw it from a [`PersistenceBackend`] under its `NAME`. The
//! backends themselves live in `crate::persist::backend`.
//!
//! This is full-snapshot persistence: every `freeze` writes the cache's whole
//! value.
//!
//! Serialization (bincode) lives behind `cfg(feature = "snapshot")`, so without
//! it `freeze`/`thaw` compile to no-ops and the whole snapshot API is inert.
//! `persist` (heed/LMDB) implies `snapshot`; `snapshot` alone gives the
//! heed-free in-memory byte snapshot used on wasm32.

use std::collections::HashMap;
use std::hash::Hash;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::persist::PersistenceBackend;
use crate::DiagResult;
#[cfg(feature = "snapshot")]
use crate::Diagnostic;

use super::{
    Cache, CacheBehavior, Eager, EagerBehavior, EagerMap, EagerMapBehavior, WholeCache,
    WholeCacheBehavior,
};

/// Serialize `value` and stage it under `key`.  No-op without `persist`.
#[cfg(feature = "snapshot")]
fn freeze_value<T: Serialize>(
    backend: &mut dyn PersistenceBackend,
    key: &'static str,
    value: &T,
) -> DiagResult<()> {
    let bytes = bincode::serialize(value)
        .map_err(|e| Diagnostic::new_error("persist", "serialize", format!("{key}: {e}")))?;
    backend.put(key, &bytes)
}

#[cfg(not(feature = "snapshot"))]
fn freeze_value<T: Serialize>(
    _backend: &mut dyn PersistenceBackend,
    _key: &'static str,
    _value: &T,
) -> DiagResult<()> {
    Ok(())
}

/// Read + deserialize the blob under `key`, or `None` if absent.  Always
/// `None` without `persist`.
#[cfg(feature = "snapshot")]
fn thaw_value<T: DeserializeOwned>(
    backend: &dyn PersistenceBackend,
    key: &'static str,
) -> DiagResult<Option<T>> {
    match backend.get(key)? {
        Some(bytes) => bincode::deserialize(&bytes).map(Some).map_err(|e| {
            Diagnostic::new_error("persist", "deserialize", format!("{key}: {e}")).into()
        }),
        None => Ok(None),
    }
}

#[cfg(not(feature = "snapshot"))]
fn thaw_value<T: DeserializeOwned>(
    _backend: &dyn PersistenceBackend,
    _key: &'static str,
) -> DiagResult<Option<T>> {
    Ok(None)
}

// -- PersistableCache --------------------------------------------------------

/// A cache that can freeze its value to / thaw it from a backend, keyed by its
/// `NAME`.  Object-safe, so a layer registers caches as `&dyn PersistableCache`.
pub(crate) trait PersistableCache {
    /// The blob key (the cache's `NAME`).
    fn cache_key(&self) -> &'static str;
    /// Write this cache's current value to `backend`.
    fn freeze(&self, backend: &mut dyn PersistenceBackend) -> DiagResult<()>;
    /// Load this cache's value from `backend` (no-op if the key is absent).
    fn thaw(&self, backend: &dyn PersistenceBackend) -> DiagResult<()>;
}

impl<B> PersistableCache for Cache<B>
where
    B: CacheBehavior,
    B::Key: Serialize + DeserializeOwned + Eq + Hash,
    B::Value: Serialize + DeserializeOwned,
{
    fn cache_key(&self) -> &'static str {
        B::NAME
    }
    fn freeze(&self, backend: &mut dyn PersistenceBackend) -> DiagResult<()> {
        freeze_value(backend, B::NAME, &(self.snapshot(), self.snapshot_side()))
    }
    fn thaw(&self, backend: &dyn PersistenceBackend) -> DiagResult<()> {
        if let Some((map, side)) =
            thaw_value::<(HashMap<B::Key, B::Value>, B::SideSnapshot)>(backend, B::NAME)?
        {
            self.restore(map);
            self.restore_side(side);
        }
        Ok(())
    }
}

impl<B> PersistableCache for EagerMap<B>
where
    B: EagerMapBehavior,
    B::Key: Serialize + DeserializeOwned + Eq + Hash,
    B::Value: Serialize + DeserializeOwned,
{
    fn cache_key(&self) -> &'static str {
        B::NAME
    }
    fn freeze(&self, backend: &mut dyn PersistenceBackend) -> DiagResult<()> {
        freeze_value(backend, B::NAME, &(self.snapshot(), self.snapshot_side()))
    }
    fn thaw(&self, backend: &dyn PersistenceBackend) -> DiagResult<()> {
        if let Some((map, side)) =
            thaw_value::<(HashMap<B::Key, B::Value>, B::SideSnapshot)>(backend, B::NAME)?
        {
            self.restore(map);
            self.restore_side(side);
        }
        Ok(())
    }
}

impl<B> PersistableCache for WholeCache<B>
where
    B: WholeCacheBehavior,
    B::Value: Serialize + DeserializeOwned,
{
    fn cache_key(&self) -> &'static str {
        B::NAME
    }
    fn freeze(&self, backend: &mut dyn PersistenceBackend) -> DiagResult<()> {
        freeze_value(backend, B::NAME, &self.snapshot())
    }
    fn thaw(&self, backend: &dyn PersistenceBackend) -> DiagResult<()> {
        if let Some(Some(value)) = thaw_value::<Option<B::Value>>(backend, B::NAME)? {
            self.install(value);
        }
        Ok(())
    }
}

impl<B> PersistableCache for Eager<B>
where
    B: EagerBehavior,
    B::Value: Serialize + DeserializeOwned,
{
    fn cache_key(&self) -> &'static str {
        B::NAME
    }
    fn freeze(&self, backend: &mut dyn PersistenceBackend) -> DiagResult<()> {
        freeze_value(backend, B::NAME, &self.snapshot())
    }
    fn thaw(&self, backend: &dyn PersistenceBackend) -> DiagResult<()> {
        if let Some(value) = thaw_value::<B::Value>(backend, B::NAME)? {
            self.install(value);
        }
        Ok(())
    }
}
