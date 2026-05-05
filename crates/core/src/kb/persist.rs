// crates/core/src/kb/mod.rs
//
// KB implementations for loading from persistent
// stored cache

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use crate::{SentenceId, SineIndex, SineParams};
use crate::persist::{LmdbEnv, load_from_db};
use crate::progress::{ProgressEvent, DynSink, SinkGuard, LogLevel};
use crate::semantics::SemanticLayer;
use crate::trans::TranslationLayer;

use super::{KnowledgeBase, KbError};

#[cfg(feature = "cnf")]
use super::Clause;
#[cfg(feature = "cnf")]
use crate::ClausifyOptions;

// KB implementations (public API)
impl KnowledgeBase {
    /// Opens the knowledge base from a persistent storage (LMDB) path.
    ///
    /// With the `cnf` feature on, the in-memory `fingerprints` dedup
    /// map is rehydrated from the `formula_hashes` LMDB table -- each
    /// key is a formula hash and each value is the owning `SentenceId`.
    /// Without `cnf`, no dedup map is built.
    pub fn open(path: &std::path::Path) -> Result<Self, KbError> {
        Self::open_with_progress(path, None)
    }

    /// Like [`Self::open`], but installs a [`super::progress::ProgressSink`]
    /// before doing the LMDB-side work, so events emitted during
    /// schema-check / rehydrate / index-replay are observable.
    /// Pass `None` for the same behaviour as `open`. Needed because
    /// no KB exists to apply to sink to
    #[cfg(feature = "persist")]
    pub fn open_with_progress(
        path: &std::path::Path,
        sink: Option<DynSink>,
    ) -> Result<Self, KbError> {
        use crate::syntactic::SyntacticLayer;

        let _sink_guard = SinkGuard::install(sink.clone());
        // Open the LMDB path
        let env = LmdbEnv::open(path)?;
        // Load the kifstore from the saved database
        let (store, session_map): (SyntacticLayer, HashMap<u64, Option<String>>) = load_from_db(&env)?;

        // Rehydrate fingerprints from DB_FORMULA_HASHES
        //
        // Only present when the `cnf` feature was on at write time.
        // Sessions for session-tagged sentences are patched in afterwards.
        #[cfg(feature = "cnf")]
        let mut fingerprints: HashMap<u64, (SentenceId, Option<String>)> = {
            let rtxn = env.read_txn()?;
            let entries = env.all_formula_hashes(&rtxn)?;
            let mut map = HashMap::with_capacity(entries.len());
            for (fh, sid) in entries {
                let session = session_map.get(&sid).cloned().flatten();
                map.insert(fh, (sid, session));
            }
            map
        };

        // Collect the set of sids that were persisted as session
        // assertions — i.e. NOT axioms.  Used below to populate
        // `Symbol.all_sentences` (axiom-only) and the SInE index.
        let session_sids: HashSet<SentenceId> = session_map.iter()
            .filter_map(|(sid, sess)| sess.as_ref().map(|_| *sid))
            .collect();

        // Silence the unused-variable warning in cnf-off builds where
        // `session_map` is not otherwise consumed.
        #[cfg(not(feature = "cnf"))]
        let _ = session_map;

        // Try to restore the semantic cache
        //
        // The cache only applies when its `kb_version` matches the
        // current counter.  On mismatch (or absence), we fall back to
        // `SemanticLayer::new`, which does the full `rebuild_taxonomy`
        // scan as before.  Either way the result is correct; this
        // just skips the scan when the cache is valid.
        let semantic = {
            let rtxn = env.read_txn()?;
            let current_version = env.kb_version(&rtxn)?;
            let cached: Option<crate::persist::CachedTaxonomy> =
                env.get_cache(&rtxn, crate::persist::CACHE_KEY_TAXONOMY)?;
            drop(rtxn);

            match cached {
                Some(tx) if tx.kb_version == current_version => {
                    crate::log!(Info, "sumo_kb::kb", format!("Phase D: restored taxonomy cache (kb_version={}, {} edges)", tx.kb_version, tx.tax_edges.len()));
                    SemanticLayer::from_cached_taxonomy(
                        store,
                        tx.tax_edges,
                    )
                }
                Some(tx) => {
                    crate::log!(Info, "sumo_kb::kb", format!("Phase D: taxonomy cache stale (cache kb_version={}, current={}); \
                         rebuilding", tx.kb_version, current_version));
                    SemanticLayer::new(store)
                }
                None => {
                    // First open or cache never written -- do the
                    // normal full-rebuild path.
                    SemanticLayer::new(store)
                }
            }
        };
        let mut layer = TranslationLayer::new(semantic);

        // Restore SortAnnotations if cached
        #[cfg(feature = "ask")]
        {
            let rtxn = env.read_txn()?;
            let current_version = env.kb_version(&rtxn)?;
            let cached: Option<crate::persist::CachedSortAnnotations> =
                env.get_cache(&rtxn, crate::persist::CACHE_KEY_SORT_ANNOT)?;
            if let Some(sa) = cached {
                if sa.kb_version == current_version {
                    layer.install_sort_annotations(sa.sorts);
                    crate::emit_event!(ProgressEvent::Log { level: LogLevel::Info, target: "sumo_kb::kb", message: format!("Phase D: restored sort_annotations cache (kb_version={})", sa.kb_version) });
                } else {
                    crate::emit_event!(ProgressEvent::Log { level: LogLevel::Info, target: "sumo_kb::kb", message: format!("Phase D: sort_annotations cache stale ({}/{}); will rebuild on first access", sa.kb_version, current_version) });
                }
            }
        }

        // -- Auto-backfill: cnf tables when cnf was off at last write -
        //
        // `env.added_features` carries the set of features that were
        // off in the persisted manifest but are on in this build.
        // When `cnf` shows up there, the `clauses`, `clause_hashes`,
        // and `formula_hashes` tables are empty for existing axioms,
        // so newly-written duplicates of existing axioms would slip
        // past the in-memory fingerprint lookup.  We'd rather fix
        // that up automatically than leave the user with a silently
        // incomplete dedup table.
        //
        // The backfill clausifies every persisted axiom, interns the
        // clauses, and populates `DB_FORMULA_HASHES` so subsequent
        // opens see a populated table and take the fast path.  The
        // manifest is re-stamped with current features; `kb_version`
        // is NOT bumped (the axiom set hasn't changed, just the cnf
        // tables), so other Phase D caches stay valid.
        #[cfg(feature = "cnf")]
        let initial_clauses: HashMap<SentenceId, Vec<Clause>> = {
            if env.added_features.iter().any(|f| *f == "cnf") {
                crate::emit_event!(ProgressEvent::Log { level: LogLevel::Info, target: "sumo_kb::kb", message: format!("Phase D: auto-backfilling cnf tables for {} axioms", layer.semantic.syntactic.roots.len()) });
                let report = crate::persist::backfill_cnf_tables(&env, &mut layer)?;
                // Backfill repopulates fingerprints too (they were
                // empty before because DB_FORMULA_HASHES was empty).
                for (sid, fh) in &report.formula_hash_by_sid {
                    fingerprints.insert(*fh, (*sid, None));
                }
                report.clauses_by_sid
            } else {
                HashMap::new()
            }
        };

        #[cfg(feature = "cnf")]
        crate::emit_event!(ProgressEvent::Log { level: LogLevel::Info, target: "sumo_kb::kb", message: format!("opened KB from {:?}: {} formulas fingerprinted", path, fingerprints.len()) });
        #[cfg(not(feature = "cnf"))]
        crate::emit_event!(ProgressEvent::Log { level: LogLevel::Info, target: "sumo_kb::kb", message: format!("opened KB from {:?} (no-dedup build)", path) });

        // Populate `Symbol.all_sentences` for every loaded axiom (every
        // root that is NOT a session assertion).  This is the live
        // generality source for SInE; maintained per-promotion afterwards.
        let axiom_sids: Vec<SentenceId> = layer.semantic.syntactic.roots.iter()
            .copied()
            .filter(|sid| !session_sids.contains(sid))
            .collect();
        for &sid in &axiom_sids {
            layer.semantic.syntactic.register_axiom_symbols(sid);
        }

        // Eagerly build the SInE index over the loaded axioms.
        let sine_index = {
            let mut idx = SineIndex::new(SineParams::default().tolerance);
            idx.add_axioms(&layer.semantic.syntactic, axiom_sids.iter().copied());
            RwLock::new(idx)
        };

        Ok(Self {
            layer,
            sessions:     HashMap::new(),
            #[cfg(feature = "cnf")] fingerprints,
            #[cfg(feature = "cnf")] clauses:  initial_clauses,
            #[cfg(feature = "cnf")] cnf_mode: true,
            #[cfg(feature = "cnf")] cnf_opts: ClausifyOptions::default(),
            db: Some(env),
            #[cfg(feature = "ask")]  axiom_cache: None,
            sine_index,
            progress: sink,
        })
    }

    // Incremental file reload
    //
    // `apply_file_diff` and `compute_file_diff` are the general-purpose
    // primitives any incremental-reload workflow can use -- file
    // watchers, LSP didChange, test harness hot-reload.  They operate
    // purely on sigmakee-rs-core types (`AstNode`, `Span`, `SentenceId`) and
    // have no LSP / editor dependency.

    /// Commit a reconcile delta to the persistent LMDB.
    ///
    /// `removed_sids` are deleted from the DB (main table + head /
    /// path indexes + formula-hash map); `added_sids` are written
    /// fresh as axioms (session = None).  Runs in **two LMDB write
    /// transactions**: one for deletions, one for insertions, each
    /// committing atomically.  Splitting is deliberate — the
    /// insertion path reuses `write_axioms`, which bumps
    /// `kb_version` on commit; we want the version bump to reflect
    /// the final post-add state, so deletions go in a separate
    /// txn before it.
    ///
    /// No-op when both slices are empty.  Callers should check
    /// [`crate::ReconcileReport::is_noop`] first in the hot path to avoid
    /// opening txns unnecessarily.
    ///
    /// Requires the `persist` feature; callers without it should
    /// keep their reconcile in memory.
    #[cfg(feature = "persist")]
    pub fn persist_reconcile_diff(
        &self,
        removed_sids: &[SentenceId],
        added_sids:   &[SentenceId],
    ) -> Result<(), KbError> {
        with_guard!(self);
        let Some(env) = &self.db else {
            return Ok(());
        };
        if removed_sids.is_empty() && added_sids.is_empty() {
            return Ok(());
        }

        // Phase 1: delete removed rows
        if !removed_sids.is_empty() {
            let mut wtxn = env.write_txn()?;
            for &sid in removed_sids {
                env.delete_formula(&mut wtxn, sid)?;
            }
            wtxn.commit()?;
            self.emit(ProgressEvent::Log { level: LogLevel::Debug, target: "sumo_kb::kb", message: format!("persist_reconcile_diff: deleted {} sentence(s)", removed_sids.len()) });
        }

        // Phase 2: write added rows
        if !added_sids.is_empty() {
            #[cfg(feature = "cnf")]
            let clause_map: HashMap<SentenceId, Vec<Clause>> = {
                let mut m = HashMap::new();
                for &sid in added_sids {
                    if let Some(cs) = self.clauses.get(&sid).cloned() {
                        m.insert(sid, cs);
                    }
                }
                m
            };
            crate::persist::commit::write_axioms(
                env,
                &self.layer.semantic.syntactic,
                added_sids,
                #[cfg(feature = "cnf")] &clause_map,
                None,
            )?;
            self.emit(ProgressEvent::Log { level: LogLevel::Debug, target: "sumo_kb::kb", message: format!("persist_reconcile_diff: wrote {} sentence(s)", added_sids.len()) });
        }

        Ok(())
    }

    /// Drop every root sentence tagged with `file`.  Orphaned
    /// symbols (those no longer referenced by any remaining
    /// sentence) are pruned from the intern table.  The
    /// occurrence index, head-index, and file-hash side table
    /// all update in lockstep via the underlying
    /// `SyntacticLayer::remove_file` primitive.
    ///
    /// Non-LSP uses: `sumo watch` file-watchers that want to
    /// drop a deleted file from the in-memory KB, test harness
    /// hot-reloads, any external-driver that wants a clean
    /// per-file tear-down without invoking the full diff path.
    ///
    /// The persistent LMDB store (when `persist` is enabled) is
    /// not touched -- `remove_file` operates purely on the
    /// in-memory view.  Use `flush_session` / `flush_assertions`
    /// for LMDB-affecting mutations.
    pub fn remove_file(&mut self, file: &str) {
        // Snapshot the removed-sentence set before the store mutation
        // so we can drop only those fingerprint entries.  Clone the
        // Vec so we don't hold a borrow across the mutation.
        #[cfg(feature = "cnf")]
        let removed_sids: std::collections::HashSet<SentenceId> =
            self.layer.semantic.syntactic.file_roots.get(file)
                .map(|v| v.iter().copied().collect())
                .unwrap_or_default();

        self.layer.semantic.syntactic.remove_file(file);

        #[cfg(feature = "cnf")]
        {
            self.clauses.retain(|sid, _| !removed_sids.contains(sid));
            self.fingerprints.retain(|_, (sid, _)| !removed_sids.contains(sid));
        }

        // The session-assertion map may also reference these sids
        // (e.g. a file loaded as a session assertion rather than an
        // axiom).  Prune.
        for sids in self.sessions.values_mut() {
            sids.retain(|s| {
                #[cfg(feature = "cnf")]
                { !removed_sids.contains(s) }
                #[cfg(not(feature = "cnf"))]
                { self.layer.semantic.syntactic.has_sentence(*s) }
            });
        }
    }

}