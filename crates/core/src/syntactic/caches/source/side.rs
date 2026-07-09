//! Companion state (`Side`) for the `syntactic::source` store: per-source
//! fingerprint membership, cross-source reference counts/spans, session→source
//! grouping, the inline-source registry, and the staged-removal recycle bin,
//! plus its serializable snapshot.

use std::collections::{HashMap, HashSet};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use crate::types::FileOrigin;
use crate::Span;

/// Companion state for the `syntactic::source` store.
#[derive(Debug, Default)]
pub(crate) struct SourceSide {
    /// Per-source fingerprint membership, used for the source-replacement diff
    /// (which formulas a re-ingested source added / kept / dropped).  A "source"
    /// is one file (keyed by path) or one inline `tell` (keyed by a per-call
    /// unique `__inline(N)__` id); distinct tells never reconcile against each
    /// other.
    pub(super) file_hashes: DashMap<String, HashSet<u64>>,
    /// Provenance recorded at the most recent ingest of each source key — its
    /// mtime/content-hash (`Local`) or branch/commit (`Git`) at that time.
    /// Overwritten on every re-ingest; this is a freshness *baseline*, not a
    /// history. Consulted by a later "has this changed since I loaded it"
    /// check (`sumo check`), not by ingest itself.
    pub(super) origins: DashMap<String, FileOrigin>,
    /// Every occurrence's span, keyed by fingerprint.  `len()` is the cross-source
    /// reference count.  A formula is gone from the KB exactly when its set
    /// becomes empty.
    pub(super) references: DashMap<u64, HashSet<Span>>,
    /// Session → the source keys ingested under it.  A session is an eviction
    /// group over sources; `flush_session` reconciles each of these sources to
    /// empty.
    pub(super) session_sources: DashMap<String, HashSet<String>>,
    /// Monotonic id generator for inline (`tell`) source keys.
    pub(super) inline_counter: std::sync::atomic::AtomicU64,
    /// Source keys whose origin is `Inline` (a `tell`).  Inline assertions can
    /// never be lifted to axioms, so promotion refuses any session that holds
    /// one.
    pub(super) inline_sources: dashmap::DashSet<String>,
    /// Recycle bin: per-source, the fingerprints of promoted axioms a staged
    /// re-ingest would remove.  Removal is deferred (the formula stays live)
    /// until the caller commits (`accept_kif_update`) or keeps them
    /// (`reject_kif_update`).  Only promoted axioms land here.
    pub(super) recycle: DashMap<String, HashSet<u64>>,
}

impl SourceSide {
    /// Allocate the next unique inline source key (`__inline(N)__`).
    pub(crate) fn next_inline_key(&self) -> String {
        let n = self.inline_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("__inline({n})__")
    }

    /// The source keys ingested under `session` (empty if unknown).
    pub(crate) fn sources_of_session(&self, session: &str) -> Vec<String> {
        self.session_sources.get(session).map(|s| s.iter().cloned().collect()).unwrap_or_default()
    }

    /// The fingerprints a source currently contributes (its `file_hashes` set).
    pub(crate) fn fingerprints_of(&self, source_key: &str) -> Vec<u64> {
        self.file_hashes.get(source_key).map(|s| s.iter().copied().collect()).unwrap_or_default()
    }

    /// Mark a source key as inline-origin (a `tell`).
    pub(crate) fn mark_inline(&self, source_key: &str) {
        self.inline_sources.insert(source_key.to_string());
    }

    /// Record `source_key`'s provenance as of this ingest, replacing whatever
    /// was recorded before (a baseline, not a history).
    pub(crate) fn set_origin(&self, source_key: &str, origin: FileOrigin) {
        self.origins.insert(source_key.to_string(), origin);
    }

    /// The provenance recorded at `source_key`'s most recent ingest, if any.
    pub(crate) fn origin_of(&self, source_key: &str) -> Option<FileOrigin> {
        self.origins.get(source_key).map(|o| o.clone())
    }

    /// Whether `source_key` is an inline (`tell`) source.
    pub(crate) fn is_inline_source(&self, source_key: &str) -> bool {
        self.inline_sources.contains(source_key)
    }

    /// Record a deferred (recycle-bin) removal for `source_key`.
    pub(crate) fn recycle(&self, source_key: &str, fp: u64) {
        self.recycle.entry(source_key.to_string()).or_default().insert(fp);
    }

    /// Drop a fingerprint from `source_key`'s recycle bin, called when a later
    /// reload retains the formula so it is no longer a pending removal.  Evicts
    /// an emptied bin.
    pub(crate) fn unrecycle(&self, source_key: &str, fp: u64) {
        let now_empty = match self.recycle.get_mut(source_key) {
            Some(mut set) => { set.remove(&fp); set.is_empty() }
            None => false,
        };
        if now_empty { self.recycle.remove(source_key); }
    }

    /// The fingerprints currently in `source_key`'s recycle bin.
    pub(crate) fn recycled_of(&self, source_key: &str) -> Vec<u64> {
        self.recycle.get(source_key).map(|s| s.iter().copied().collect()).unwrap_or_default()
    }

    /// Clear `source_key`'s recycle bin (the deferred formulas were either
    /// committed for removal or kept).
    pub(crate) fn clear_recycle(&self, source_key: &str) {
        self.recycle.remove(source_key);
    }

    /// Drop `source_key`'s reference to fingerprint `fp` and forget `fp` from
    /// the source's `file_hashes`.  Returns `true` if no reference remains (the
    /// formula is now gone KB-wide).
    pub(crate) fn drop_ref(&self, source_key: &str, fp: u64) -> bool {
        if let Some(mut set) = self.file_hashes.get_mut(source_key) { set.remove(&fp); }
        let now_empty = match self.references.get_mut(&fp) {
            Some(mut refs) => { refs.retain(|sp| sp.file.as_str() != source_key); refs.is_empty() }
            None => false,
        };
        if now_empty { self.references.remove(&fp); }
        now_empty
    }

    /// Forget a session's source-group bookkeeping (the sources themselves are
    /// reconciled separately).
    pub(crate) fn forget_session(&self, session: &str) {
        self.session_sources.remove(session);
    }
}

/// Serializable snapshot of [`SourceSide`] for whole-cache persistence
/// (`DashMap` doesn't serialize directly, so flatten to `HashMap`).
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct SourceSideSnapshot {
    pub(super) file_hashes: HashMap<String, HashSet<u64>>,
    pub(super) references:  HashMap<u64, HashSet<Span>>,
    /// `#[serde(default)]` so a store persisted before this field existed
    /// restores cleanly (empty — the next ingest repopulates it).
    #[serde(default)]
    pub(super) origins:     HashMap<String, FileOrigin>,
}
