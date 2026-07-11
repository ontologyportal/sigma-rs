//! KB persistence — the unified cache-snapshot path.
//!
//! Persistence is wholesale: `open` thaws every persisted cache from the LMDB
//! blob store (`restore_caches_from`), and `persist` freezes them all back
//! (`snapshot_caches`).  The whole KB state — source AST, sentences +
//! provenance, symbols, sessions/axiom status, and the eager indices/taxonomy —
//! round-trips through the one backend-agnostic seam.


use crate::SentenceId;
use crate::persist::{LmdbEnv, PersistenceEngine};
use crate::progress::{DynSink, SinkGuard};
use crate::semantics::SemanticLayer;
use crate::Diagnostic;
use crate::layer::TopLayer;

use super::KnowledgeBase;

impl<L: TopLayer> KnowledgeBase<L> {
    /// Layer-generic open: build an empty stack over `L`, thaw every
    /// persisted cache, and prime the rest.  Snapshot keys a layer
    /// doesn't recognize are simply left to rebuild (layer.rs thaw
    /// semantics), so a DB written under one top layer opens cleanly
    /// under another.
    #[cfg(feature = "persist")]
    pub fn open(
        path: &std::path::Path,
        sink: Option<DynSink>,
    ) -> Result<Self, Diagnostic> {
        use crate::syntactic::SyntacticLayer;

        let _sink_guard = SinkGuard::install(sink.clone());
        let env = LmdbEnv::open(path)?;

        // Lazy query caches (domain/range/symbol_sort/…) are not snapshotted and
        // recompute on first access.
        let layer = L::from_semantic(SemanticLayer::new(SyntacticLayer::default()));
        {
            let backend = PersistenceEngine::lmdb(&env);
            layer.restore_caches_from(&backend)?;
        }
        // Each cache's `initialize` self-guards on "already populated", so the
        // thawed caches are skipped.
        layer.initialize_caches();

        let mut kb = Self::from_layer(layer);
        kb.db = Some(env);
        kb.progress = sink;
        Ok(kb)
    }

    /// Produce an independent, fully in-memory copy of this KB.
    ///
    /// Snapshots every persistable cache into an in-memory blob store, then
    /// thaws it into a brand-new, empty layer stack, routed through
    /// [`MemoryBackend`](crate::persist) instead of LMDB so nothing touches
    /// disk.  The snapshot/restore round-trip makes the copy independent (this
    /// is not `Clone`).
    ///
    /// The clone is detached (`db: None`): mutating it leaves this KB untouched.
    ///
    /// Requires the `persist` feature.
    #[cfg(feature = "persist")]
    pub fn snapshot_clone(&self) -> Result<Self, Diagnostic> {
        use crate::syntactic::SyntacticLayer;

        with_guard!(self);

        let mut backend = PersistenceEngine::memory();
        self.layer.snapshot_caches(&mut backend)?;

        // `fresh_config_clone` (not `from_semantic`) carries layer config that
        // isn't a cache — notably a configured external prover backend.
        let layer = self.layer.fresh_config_clone(SemanticLayer::new(SyntacticLayer::default()));
        layer.restore_caches_from(&backend)?;
        layer.initialize_caches();

        // Carry the KB-level fields that aren't layer caches.
        let mut kb = Self::from_layer(layer);
        kb.sessions            = self.sessions.clone();
        kb.syntax_fingerprints = self.syntax_fingerprints.clone();
        kb.progress            = self.progress.clone();
        Ok(kb)
    }

    /// Freeze the entire KB to the LMDB store (wholesale snapshot of every
    /// persistable cache).  No-op when there is no attached DB.
    ///
    /// The snapshot is atomic (one backend `commit`), so all blobs are mutually
    /// consistent on disk.
    #[cfg(feature = "persist")]
    pub fn persist(&self) -> Result<(), Diagnostic> {
        with_guard!(self);
        let Some(env) = &self.db else { return Ok(()); };
        let mut backend = PersistenceEngine::lmdb(env);
        profile_span!(self, "persist: snapshot caches to LMDB");
        self.layer.snapshot_caches(&mut backend)
    }

    /// Commit the KB, ignoring the per-file delta and re-snapshotting the whole
    /// KB via [`Self::persist`].
    #[cfg(feature = "persist")]
    pub fn persist_reconcile_diff(
        &self,
        _removed_sids: &[SentenceId],
        _added_sids:   &[SentenceId],
    ) -> Result<(), Diagnostic> {
        self.persist()
    }

}

#[cfg(all(test, feature = "persist"))]
mod round_trip_tests {
    use crate::TranslationLayer;

use super::*;
    use std::collections::HashSet;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("sigmakee-persist-rt-{}-{}", name, std::process::id()));
        p
    }

    #[test]
    fn kb_round_trips_through_lmdb() {
        let dir = tmp_dir("kb-rt");
        let _ = std::fs::remove_dir_all(&dir);

        // --- Build, promote, persist to disk ---
        let (sub_id, roots_before, gen_before) = {
            let mut kb = KnowledgeBase::<TranslationLayer>::open(&dir, None).expect("open new DB");
            // File-style ingest: inline `tell`s are transient super-hypotheses
            // and cannot be promoted — promotable content must arrive as a
            // source file.
            let r = kb.reload_kif(
                "(subclass Dog Animal)\n(instance Fido Dog)",
                &std::path::PathBuf::from("rt.kif"), "s1");
            assert!(r.ok, "ingest failed: {:?}", r.diagnostics);
            #[cfg(feature = "ask")]
            kb.make_session_axiomatic("s1").expect("promote");
            #[cfg(not(feature = "ask"))]
            kb.make_session_axiomatic("s1").expect("promote");

            let syn = &kb.layer.semantic.syntactic;
            let sub = syn.sym_id("subclass").expect("subclass interned");
            let roots: HashSet<crate::SentenceId> =
                syn.root_sids().into_iter().collect();
            let generality = syn.sine.with_ref(|idx| idx.generality(sub));
            assert_eq!(roots.len(), 2, "two roots before persist");
            assert!(generality > 0, "SInE populated before persist");

            kb.persist().expect("persist to LMDB");
            (sub, roots, generality)
        };

        // --- Reopen from disk; assert every cache restored ---
        {
            let kb = KnowledgeBase::<TranslationLayer>::open(&dir, None).expect("reopen DB");
            let syn = &kb.layer.semantic.syntactic;
            let roots_after: HashSet<crate::SentenceId> =
                syn.root_sids().into_iter().collect();

            assert_eq!(roots_after, roots_before, "root sentences restored from LMDB");
            assert_eq!(kb.sine_axiom_count(), 2, "SInE axiom count restored");
            assert_eq!(syn.sine.with_ref(|idx| idx.generality(sub_id)), gen_before,
                "SInE generality restored");
            for sid in &roots_before {
                assert!(syn.sentence(*sid).is_some(), "sentence body restored");
                assert!(syn.is_axiom(*sid), "axiom status (promoted set) restored");
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_origin_round_trips_through_lmdb() {
        use crate::types::{FileOrigin, LocalProvenance};

        let dir = tmp_dir("file-origin-rt");
        let _ = std::fs::remove_dir_all(&dir);

        let origin = FileOrigin::Local(LocalProvenance { mtime_secs: 1_700_000_000, content_hash: 0xABCD1234 });
        {
            let mut kb = KnowledgeBase::<TranslationLayer>::open(&dir, None).expect("open new DB");
            let sf = crate::types::SourceFile {
                parser:   crate::Parser::Kif,
                name:     "origin.kif".to_string(),
                path:     std::path::PathBuf::from("origin.kif"),
                origin:   origin.clone(),
                contents: "(subclass Cat Animal)".to_string(),
                prebuilt: None,
            };
            let r = kb.load(sf, "s1");
            assert!(r.ok, "ingest failed: {:?}", r.diagnostics);
            assert_eq!(kb.file_origin("origin.kif"), Some(origin.clone()),
                "origin recorded in-memory right after ingest");
            kb.persist().expect("persist to LMDB");
        }

        let kb = KnowledgeBase::<TranslationLayer>::open(&dir, None).expect("reopen DB");
        assert_eq!(kb.file_origin("origin.kif"), Some(origin), "origin restored from LMDB");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_clone_is_independent() {
        let dir = tmp_dir("snap-clone");
        let _ = std::fs::remove_dir_all(&dir);

        // Master: one promoted axiom.
        let mut master = KnowledgeBase::<TranslationLayer>::open(&dir, None).expect("open");
        let r = master.reload_kif(
            "(subclass Dog Animal)", &std::path::PathBuf::from("m.kif"), "s1");
        assert!(r.ok, "master ingest: {:?}", r.diagnostics);
        kb_promote(&mut master, "s1");
        assert_eq!(master.layer.semantic.syntactic.root_sids().len(), 1);

        // Clone carries the master's base...
        let mut clone = master.snapshot_clone().expect("snapshot_clone");
        assert!(clone.layer.semantic.syntactic.sym_id("Dog").is_some(),
            "clone must carry the master's promoted base");
        assert_eq!(clone.layer.semantic.syntactic.root_sids().len(), 1,
            "clone starts with exactly the master's axioms");

        // ...then mutate ONLY the clone.
        let r = clone.reload_kif(
            "(subclass Cat Animal)", &std::path::PathBuf::from("c.kif"), "s2");
        assert!(r.ok, "clone ingest: {:?}", r.diagnostics);
        kb_promote(&mut clone, "s2");
        assert!(clone.layer.semantic.syntactic.sym_id("Cat").is_some(),
            "clone gained Cat");
        assert_eq!(clone.layer.semantic.syntactic.root_sids().len(), 2,
            "clone now has both axioms");

        // Master is untouched — no leak from the clone.
        assert!(master.layer.semantic.syntactic.sym_id("Cat").is_none(),
            "master must NOT see the clone's Cat");
        assert_eq!(master.layer.semantic.syntactic.root_sids().len(), 1,
            "master root count unchanged after cloning + mutating the clone");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn kb_promote(kb: &mut KnowledgeBase<TranslationLayer>, session: &str) {
        kb.make_session_axiomatic(session).expect("promote");
    }
}
