// crates/core/src/persist/backend.rs
//
// Persistence backends + the engine that selects among them.
//
//   - `PersistenceBackend` — the common string-keyed blob-store API every
//     backend implements (`put`/`get`/`remove`/`commit`).
//   - `MemoryBackend`      — in-memory KV (tests / ephemeral).
//   - `LmdbBackend`        — heed/LMDB on disk (`cfg(feature = "persist")`).
//   - `PersistenceEngine`  — the closed enum of the above, chosen at the call
//                            site.
//
// The trait, the engine, and the in-memory / no-op backends are always
// compiled so the snapshot API exists without the `persist` feature; only the
// heed-touching `LmdbBackend` is feature-gated.

use std::collections::HashMap;

use crate::Diagnostic;

/// A string-keyed blob store.  Cache freeze/thaw is written against this trait,
/// never against a concrete backend.
pub(crate) trait PersistenceBackend {
    /// Stage a write of `bytes` under `key`.  May be buffered until `commit`.
    // Only called from `cache/persistence.rs` code that is `cfg(feature =
    // "persist")`, but the impls exist unconditionally — don't cfg out.
    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    fn put(&mut self, key: &str, bytes: &[u8]) -> Result<(), Diagnostic>;
    /// Read the committed bytes under `key`, or `None` if absent.
    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Diagnostic>;
    // TODO: implement incremental rewrites, right now, persistence is ALL OR NOTHING
    /// Stage a removal of `key`.  May be buffered until `commit`.
    #[allow(dead_code)]
    fn remove(&mut self, key: &str) -> Result<(), Diagnostic>;
    /// Flush all staged writes atomically.  No-op for backends that write
    /// eagerly (e.g. the in-memory one).
    fn commit(&mut self) -> Result<(), Diagnostic>;
}

/// In-memory blob store — tests and ephemeral use.  Writes are eager; `commit`
/// is a no-op.
#[derive(Debug, Default)]
pub(crate) struct MemoryBackend {
    map: HashMap<String, Vec<u8>>,
}

impl PersistenceBackend for MemoryBackend {
    fn put(&mut self, key: &str, bytes: &[u8]) -> Result<(), Diagnostic> {
        self.map.insert(key.to_owned(), bytes.to_vec());
        Ok(())
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Diagnostic> {
        Ok(self.map.get(key).cloned())
    }
    fn remove(&mut self, key: &str) -> Result<(), Diagnostic> {
        self.map.remove(key);
        Ok(())
    }
    fn commit(&mut self) -> Result<(), Diagnostic> {
        Ok(())
    }
}

/// LMDB-backed blob store.  Buffers writes and applies them in a single atomic
/// transaction on `commit`, against an existing [`LmdbEnv`]'s keyed-cache
/// table.
///
/// [`LmdbEnv`]: super::LmdbEnv
#[cfg(feature = "persist")]
pub(crate) struct LmdbBackend<'a> {
    env:     &'a super::LmdbEnv,
    /// Staged writes: `Some` = put these bytes, `None` = delete the key.
    pending: HashMap<String, Option<Vec<u8>>>,
}

#[cfg(feature = "persist")]
impl<'a> LmdbBackend<'a> {
    /// Creates a backend that stages writes against `env`.
    pub(crate) fn new(env: &'a super::LmdbEnv) -> Self {
        Self { env, pending: HashMap::new() }
    }
}

#[cfg(feature = "persist")]
impl PersistenceBackend for LmdbBackend<'_> {
    fn put(&mut self, key: &str, bytes: &[u8]) -> Result<(), Diagnostic> {
        self.pending.insert(key.to_owned(), Some(bytes.to_vec()));
        Ok(())
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Diagnostic> {
        let rtxn = self.env.read_txn()?;
        Ok(self.env.caches.get(&rtxn, key)?.map(|b| b.to_vec()))
    }
    fn remove(&mut self, key: &str) -> Result<(), Diagnostic> {
        self.pending.insert(key.to_owned(), None);
        Ok(())
    }
    fn commit(&mut self) -> Result<(), Diagnostic> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut wtxn = self.env.write_txn()?;
        for (key, val) in self.pending.drain() {
            match val {
                Some(bytes) => { self.env.caches.put(&mut wtxn, &key, &bytes)?; }
                None        => { self.env.caches.delete(&mut wtxn, &key)?; }
            }
        }
        wtxn.commit()?;
        Ok(())
    }
}

/// The closed set of persistence backends.  `Noop` carries the lifetime so the
/// enum is valid whether or not the `persist`-only `Lmdb` variant is compiled.
pub(crate) enum PersistenceEngine<'a> {
    /// Does nothing — the default when persistence is disabled or unwanted.
    Noop(std::marker::PhantomData<&'a ()>),
    /// In-memory KV.
    Memory(MemoryBackend),
    /// LMDB on disk.
    #[cfg(feature = "persist")]
    Lmdb(LmdbBackend<'a>),
}

#[allow(dead_code)]
impl<'a> PersistenceEngine<'a> {
    /// A no-op engine that stores nothing.
    pub(crate) fn noop() -> Self {
        Self::Noop(std::marker::PhantomData)
    }
    /// An in-memory engine.
    pub(crate) fn memory() -> Self {
        Self::Memory(MemoryBackend::default())
    }
    /// An LMDB-on-disk engine backed by `env`.
    #[cfg(feature = "persist")]
    pub(crate) fn lmdb(env: &'a super::LmdbEnv) -> Self {
        Self::Lmdb(LmdbBackend::new(env))
    }
}

impl PersistenceBackend for PersistenceEngine<'_> {
    fn put(&mut self, key: &str, bytes: &[u8]) -> Result<(), Diagnostic> {
        match self {
            Self::Noop(_)   => Ok(()),
            Self::Memory(m) => m.put(key, bytes),
            #[cfg(feature = "persist")]
            Self::Lmdb(l)   => l.put(key, bytes),
        }
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Diagnostic> {
        match self {
            Self::Noop(_)   => Ok(None),
            Self::Memory(m) => m.get(key),
            #[cfg(feature = "persist")]
            Self::Lmdb(l)   => l.get(key),
        }
    }
    fn remove(&mut self, key: &str) -> Result<(), Diagnostic> {
        match self {
            Self::Noop(_)   => Ok(()),
            Self::Memory(m) => m.remove(key),
            #[cfg(feature = "persist")]
            Self::Lmdb(l)   => l.remove(key),
        }
    }
    fn commit(&mut self) -> Result<(), Diagnostic> {
        match self {
            Self::Noop(_)   => Ok(()),
            Self::Memory(m) => m.commit(),
            #[cfg(feature = "persist")]
            Self::Lmdb(l)   => l.commit(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_backend_roundtrips() {
        let mut b = MemoryBackend::default();
        assert_eq!(b.get("k").unwrap(), None);
        b.put("k", b"hello").unwrap();
        assert_eq!(b.get("k").unwrap().as_deref(), Some(&b"hello"[..]));
        b.remove("k").unwrap();
        assert_eq!(b.get("k").unwrap(), None);
        b.commit().unwrap();
    }

    #[test]
    fn noop_engine_is_inert() {
        let mut e = PersistenceEngine::noop();
        e.put("k", b"v").unwrap();
        assert_eq!(e.get("k").unwrap(), None, "Noop stores nothing");
        e.commit().unwrap();
    }
}
