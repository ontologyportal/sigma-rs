// crates/core/src/persist/env.rs
//
// LMDB environment for the unified cache-snapshot persistence.
//
// Persistence is a single backend-agnostic blob store: every cache freezes its
// value under its `NAME` key and thaws it back (see `cache::persistence` and
// `persist::backend`).  LMDB backs that with ONE keyed table (`caches`); a tiny
// `meta` table holds a format-version stamp.
//
// The legacy per-sentence `Stored*` tables + `write_axioms`/`load_from_db` are
// gone — the wholesale cache snapshot IS the on-disk format.  Because every
// cache is snapshotted together, the blobs are mutually consistent by
// construction, so there is no per-blob `kb_version` gating: a single
// `FORMAT_VERSION` guards the serialized shape, and a mismatch just clears the
// table for a cold rebuild.

use std::path::Path;

use heed::types::{Bytes, Str};
use heed::{Database, Env, EnvOpenOptions, RoTxn, RwTxn};

use crate::Diagnostic;

/// LMDB/heed errors surface as `db`-kind diagnostics so `?` works throughout
/// the persistence path.
impl From<heed::Error> for Diagnostic {
    fn from(e: heed::Error) -> Self {
        Diagnostic::new_error("db", "error", format!("LMDB error: {e}"))
    }
}

const MAX_DBS:  u32   = 4;
const MAP_SIZE: usize = 10 * 1024 * 1024 * 1024; // 10 GiB virtual

/// On-disk format revision for the cache-snapshot store.  Bump on an
/// incompatible change to any persisted cache's serialized shape; a mismatched
/// stamp means "ignore the persisted blobs and rebuild from a cold load".
const FORMAT_VERSION: u64 = 1;

const DB_CACHES: &str = "caches"; // cache NAME -> bincode blob
const DB_META:   &str = "meta";   // "format_version" -> 8-byte LE u64

const META_KEY_FORMAT: &str = "format_version";

pub(crate) struct LmdbEnv {
    pub env:    Env,
    /// The single keyed cache table.  Every persistable cache freezes/thaws its
    /// blob here under its `NAME` (see [`crate::persist::backend::LmdbBackend`]).
    pub caches: Database<Str, Bytes>,
    #[allow(dead_code)]
    /// Tiny side table for the format-version stamp.
    pub meta:   Database<Str, Bytes>,
}

impl LmdbEnv {
    /// Open an LMDB environment or create a new one if it does not already
    /// exist
    pub(crate) fn open(path: &Path) -> Result<Self, Diagnostic> {
        crate::log!(Info, "sigmakee_rs_core::persist",
            format!("opening LMDB at {}", path.display()));
        std::fs::create_dir_all(path).map_err(|e| {
            Diagnostic::new_error("db", "error",
                format!("cannot create DB directory {}: {}", path.display(), e))
        })?;

        // SAFETY: callers open at most one Env per path per process.
        let env = unsafe {
            EnvOpenOptions::new()
                .max_dbs(MAX_DBS)
                .map_size(MAP_SIZE)
                .open(path)
        }.map_err(|e| Diagnostic::new_error("db", "error",
            format!("cannot open LMDB at {}: {}", path.display(), e)))?;

        let mut wtxn = env.write_txn()?;
        let caches: Database<Str, Bytes> = env.create_database(&mut wtxn, Some(DB_CACHES))?;
        let meta:   Database<Str, Bytes> = env.create_database(&mut wtxn, Some(DB_META))?;

        // Format-version gate.  Fresh DB → stamp it.  Stale stamp → wipe the
        // cache table so the caller cold-loads (the blobs are an incompatible
        // shape).  Matching stamp → fast path.
        match meta.get(&wtxn, META_KEY_FORMAT)?.and_then(read_u64) {
            Some(v) if v == FORMAT_VERSION => {}
            Some(_) => {
                crate::log!(Warn, "sigmakee_rs_core::persist",
                    "persisted cache format is stale; clearing for a cold rebuild".to_string());
                caches.clear(&mut wtxn)?;
                meta.put(&mut wtxn, META_KEY_FORMAT, &FORMAT_VERSION.to_le_bytes())?;
            }
            None => {
                meta.put(&mut wtxn, META_KEY_FORMAT, &FORMAT_VERSION.to_le_bytes())?;
            }
        }
        wtxn.commit()?;

        Ok(Self { env, caches, meta })
    }

    pub(crate) fn read_txn(&self) -> Result<RoTxn<'_>, Diagnostic> {
        Ok(self.env.read_txn()?)
    }

    pub(crate) fn write_txn(&self) -> Result<RwTxn<'_>, Diagnostic> {
        Ok(self.env.write_txn()?)
    }
}

fn read_u64(bytes: &[u8]) -> Option<u64> {
    bytes.try_into().ok().map(u64::from_le_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_and_stamps_format_version() {
        let dir = std::env::temp_dir().join(format!("sigma_env_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let env = LmdbEnv::open(&dir).expect("open");
        let rtxn = env.read_txn().expect("rtxn");
        let stamp = env.meta.get(&rtxn, META_KEY_FORMAT).unwrap().and_then(read_u64);
        assert_eq!(stamp, Some(FORMAT_VERSION));
        drop(rtxn);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
