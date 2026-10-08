//! KB persistence — the unified cache-snapshot path.
//!
//! Persistence is wholesale: `open` thaws every persisted cache from the LMDB
//! blob store (`restore_caches_from`), and `persist` freezes them all back
//! (`snapshot_caches`).  The whole KB state — source AST, sentences +
//! provenance, symbols, sessions/axiom status, and the eager indices/taxonomy —
//! round-trips through the one backend-agnostic seam.

use crate::layer::TopLayer;
#[cfg(feature = "persist")]
use crate::persist::LmdbEnv;
#[cfg(any(feature = "snapshot", feature = "persist"))]
use crate::persist::PersistenceEngine;
#[cfg(feature = "persist")]
use crate::progress::{DynSink, SinkGuard};
#[cfg(any(feature = "snapshot", feature = "persist"))]
use crate::semantics::SemanticLayer;
#[cfg(any(feature = "snapshot", feature = "persist"))]
use crate::{DiagResult, Diagnostic};

use super::KnowledgeBase;

impl<L: TopLayer> KnowledgeBase<L> {
    /// A fresh stack thawed from `backend`. With a `template`, the stack
    /// carries the template's configuration -- cache config (disabled caches,
    /// thread count) and layer config such as a configured prover backend --
    /// via [`TopLayer::fresh_config_clone`]; without one, every setting is the
    /// default. Snapshot keys the layer doesn't recognize are left to rebuild,
    /// and each cache's `initialize` skips the thawed ones.
    #[cfg(feature = "snapshot")]
    fn thawed_layer(template: Option<&L>, backend: &PersistenceEngine) -> DiagResult<L> {
        use crate::syntactic::SyntacticLayer;

        let layer = match template {
            Some(t) => {
                let cfg = t.cache_config().clone();
                t.fresh_config_clone(SemanticLayer::with_config(
                    SyntacticLayer::with_config(&cfg),
                    &cfg,
                ))
            }
            None => L::from_semantic(SemanticLayer::new(SyntacticLayer::default())),
        };
        layer.restore_caches_from(backend)?;
        layer.initialize_caches();
        Ok(layer)
    }

    /// Open the LMDB store at `path` into a KB with default settings. Use
    /// [`open_like`](Self::open_like) to keep a configured layer's settings
    /// (e.g. an external prover backend).
    #[cfg(feature = "persist")]
    pub fn open(path: &std::path::Path, sink: Option<DynSink>) -> DiagResult<Self> {
        Self::open_from(None, path, sink)
    }

    /// Open the LMDB store at `path` into a new KB carrying this KB's settings
    /// -- cache config and layer config such as the prover backend -- but none
    /// of its contents. The new KB shares this KB's cache config.
    #[cfg(feature = "persist")]
    pub fn open_like(&self, path: &std::path::Path, sink: Option<DynSink>) -> DiagResult<Self> {
        Self::open_from(Some(&self.layer), path, sink)
    }

    #[cfg(feature = "persist")]
    fn open_from(
        template: Option<&L>,
        path: &std::path::Path,
        sink: Option<DynSink>,
    ) -> DiagResult<Self> {
        let _sink_guard = SinkGuard::install(sink.clone());
        let env = LmdbEnv::open(path)?;
        let layer = Self::thawed_layer(template, &PersistenceEngine::lmdb(&env))?;
        let mut kb = Self::from_layer(layer);
        kb.db = Some(env);
        kb.progress = sink;
        Ok(kb)
    }

    /// Produce an independent, fully in-memory copy of this KB with the same
    /// settings.
    ///
    /// Snapshots every persistable cache into an in-memory blob store and thaws
    /// it into a brand-new stack, so nothing touches disk and the copy is
    /// independent (this is not `Clone`). The clone is detached (`db: None`):
    /// mutating it leaves this KB untouched.
    #[cfg(feature = "snapshot")]
    pub fn snapshot_clone(&self) -> DiagResult<Self> {
        with_guard!(self);

        let mut backend = PersistenceEngine::memory();
        self.layer.snapshot_caches(&mut backend)?;
        let mut kb = Self::from_layer(Self::thawed_layer(Some(&self.layer), &backend)?);
        kb.progress = self.progress.clone();
        Ok(kb)
    }

    /// Freeze the entire KB into a self-contained byte buffer.
    ///
    /// Snapshots every persistable cache into an in-memory blob store (no disk,
    /// no heed), then bincode-encodes the blob map. The bytes round-trip through
    /// [`restore_from_bytes`](Self::restore_from_bytes) /
    /// [`restore_bytes`](Self::restore_bytes) into an independent KB.
    ///
    /// This is the browser freeze/thaw seam: a built KB can be handed to JS as a
    /// `Uint8Array`, stashed in IndexedDB, and thawed on a later visit without
    /// re-ingesting. Requires the `snapshot` feature (heed-free; compiles on
    /// wasm32, unlike [`persist`](Self::persist)).
    #[cfg(feature = "snapshot")]
    pub fn snapshot_bytes(&self) -> DiagResult<Vec<u8>> {
        with_guard!(self);

        let mut backend = PersistenceEngine::memory();
        self.layer.snapshot_caches(&mut backend)?;
        let map = match backend {
            PersistenceEngine::Memory(m) => m.into_map(),
            _ => unreachable!("memory() constructs the Memory variant"),
        };
        bincode::serialize(&map)
            .map_err(|e| Diagnostic::new_error("snapshot", "serialize", e.to_string()).into())
    }

    /// Thaw a KB previously frozen by [`snapshot_bytes`](Self::snapshot_bytes)
    /// into a fresh, detached (`db: None`) KB with default settings. Use
    /// [`restore_bytes`](Self::restore_bytes) to keep a configured KB's
    /// settings.
    #[cfg(feature = "snapshot")]
    pub fn restore_from_bytes(bytes: &[u8]) -> DiagResult<Self> {
        let layer = Self::thawed_layer(None, &Self::bytes_backend(bytes)?)?;
        Ok(Self::from_layer(layer))
    }

    /// Replace this KB's contents with a KB frozen by
    /// [`snapshot_bytes`](Self::snapshot_bytes), keeping this KB's settings
    /// (cache config, prover backend), progress sink, and attached database.
    #[cfg(feature = "snapshot")]
    pub fn restore_bytes(&mut self, bytes: &[u8]) -> DiagResult<()> {
        self.layer = Self::thawed_layer(Some(&self.layer), &Self::bytes_backend(bytes)?)?;
        Ok(())
    }

    #[cfg(feature = "snapshot")]
    fn bytes_backend(bytes: &[u8]) -> DiagResult<PersistenceEngine<'static>> {
        use crate::persist::MemoryBackend;
        use std::collections::HashMap;

        let map: HashMap<String, Vec<u8>> = bincode::deserialize(bytes)
            .map_err(|e| Diagnostic::new_error("snapshot", "deserialize", e.to_string()))?;
        Ok(PersistenceEngine::Memory(MemoryBackend::from_map(map)))
    }

    /// Freeze the entire KB to the LMDB store (wholesale snapshot of every
    /// persistable cache).
    ///
    /// The snapshot is atomic (one backend `commit`), so all blobs are mutually
    /// consistent on disk.
    ///
    /// # Errors
    ///
    /// Fails when no database is attached (the KB was not built with
    /// [`Self::open`]): nothing would reach disk, so reporting success would
    /// be a lie.
    #[cfg(feature = "persist")]
    pub fn persist(&self) -> DiagResult<()> {
        with_guard!(self);
        let Some(env) = &self.db else {
            return Err(Box::new(Diagnostic::new_error(
                "db",
                "no-db",
                "cannot persist: no database is attached to this knowledge base",
            )));
        };
        let mut backend = PersistenceEngine::lmdb(env);
        profile_span!(self, "persist: snapshot caches to LMDB");
        self.layer.snapshot_caches(&mut backend)
    }
}

#[cfg(all(test, feature = "snapshot", feature = "native-prover"))]
mod snapshot_bytes_tests {
    use super::KnowledgeBase;
    use crate::ProverLayer;

    #[test]
    fn kb_round_trips_through_bytes() {
        // Build + promote a small theory, freeze to bytes, thaw into a fresh KB,
        // and confirm the thawed KB carries the promoted axioms and can prove.
        let mut master = KnowledgeBase::<ProverLayer>::new_native();
        let r = master.reload_kif(
            "(subclass Dog Mammal)\n(subclass Mammal Animal)\n(instance Rex Dog)",
            &std::path::PathBuf::from("s.kif"),
            "s1",
        );
        assert!(r.ok, "ingest failed: {:?}", r.diagnostics);
        master.make_session_axiomatic("s1").expect("promote");
        let roots_before = master.layer.semantic.syntactic.root_sids().len();
        assert_eq!(roots_before, 3, "three promoted roots");

        let bytes = master.snapshot_bytes().expect("snapshot_bytes");
        assert!(!bytes.is_empty(), "snapshot produced bytes");

        let thawed =
            KnowledgeBase::<ProverLayer>::restore_from_bytes(&bytes).expect("restore_from_bytes");
        assert_eq!(
            thawed.layer.semantic.syntactic.root_sids().len(),
            roots_before,
            "thawed KB carries the promoted axioms"
        );
        assert!(
            thawed.layer.semantic.syntactic.sym_id("Dog").is_some(),
            "thawed KB interns the master's symbols"
        );
    }
}

#[cfg(all(test, feature = "persist"))]
mod round_trip_tests {
    use crate::TranslationLayer;

    use super::*;
    use std::collections::HashSet;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "sigmakee-persist-rt-{}-{}",
            name,
            std::process::id()
        ));
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
                &std::path::PathBuf::from("rt.kif"),
                "s1",
            );
            assert!(r.ok, "ingest failed: {:?}", r.diagnostics);
            #[cfg(feature = "external-prover")]
            kb.make_session_axiomatic("s1").expect("promote");
            #[cfg(not(feature = "external-prover"))]
            kb.make_session_axiomatic("s1").expect("promote");

            let syn = &kb.layer.semantic.syntactic;
            let sub = syn.sym_id("subclass").expect("subclass interned");
            let roots: HashSet<crate::SentenceId> = syn.root_sids().into_iter().collect();
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
            let roots_after: HashSet<crate::SentenceId> = syn.root_sids().into_iter().collect();

            assert_eq!(
                roots_after, roots_before,
                "root sentences restored from LMDB"
            );
            assert_eq!(kb.sine_axiom_count(), 2, "SInE axiom count restored");
            assert_eq!(
                syn.sine.with_ref(|idx| idx.generality(sub_id)),
                gen_before,
                "SInE generality restored"
            );
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

        let origin = FileOrigin::Local(LocalProvenance {
            mtime_secs: 1_700_000_000,
            content_hash: 0xABCD1234,
        });
        {
            let mut kb = KnowledgeBase::<TranslationLayer>::open(&dir, None).expect("open new DB");
            let sf = crate::types::SourceFile {
                parser: crate::Parser::Kif { options: None },
                name: "origin.kif".to_string(),
                path: std::path::PathBuf::from("origin.kif"),
                origin: origin.clone(),
                contents: "(subclass Cat Animal)".to_string(),
                prebuilt: None,
            };
            let r = kb.load(sf, "s1");
            assert!(r.ok, "ingest failed: {:?}", r.diagnostics);
            assert_eq!(
                kb.file_origin("origin.kif"),
                Some(origin.clone()),
                "origin recorded in-memory right after ingest"
            );
            kb.persist().expect("persist to LMDB");
        }

        let kb = KnowledgeBase::<TranslationLayer>::open(&dir, None).expect("reopen DB");
        assert_eq!(
            kb.file_origin("origin.kif"),
            Some(origin),
            "origin restored from LMDB"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_clone_is_independent() {
        let dir = tmp_dir("snap-clone");
        let _ = std::fs::remove_dir_all(&dir);

        // Master: one promoted axiom.
        let mut master = KnowledgeBase::<TranslationLayer>::open(&dir, None).expect("open");
        let r = master.reload_kif(
            "(subclass Dog Animal)",
            &std::path::PathBuf::from("m.kif"),
            "s1",
        );
        assert!(r.ok, "master ingest: {:?}", r.diagnostics);
        kb_promote(&mut master, "s1");
        assert_eq!(master.layer.semantic.syntactic.root_sids().len(), 1);

        // Clone carries the master's base...
        let mut clone = master.snapshot_clone().expect("snapshot_clone");
        assert!(
            clone.layer.semantic.syntactic.sym_id("Dog").is_some(),
            "clone must carry the master's promoted base"
        );
        assert_eq!(
            clone.layer.semantic.syntactic.root_sids().len(),
            1,
            "clone starts with exactly the master's axioms"
        );

        // ...then mutate ONLY the clone.
        let r = clone.reload_kif(
            "(subclass Cat Animal)",
            &std::path::PathBuf::from("c.kif"),
            "s2",
        );
        assert!(r.ok, "clone ingest: {:?}", r.diagnostics);
        kb_promote(&mut clone, "s2");
        assert!(
            clone.layer.semantic.syntactic.sym_id("Cat").is_some(),
            "clone gained Cat"
        );
        assert_eq!(
            clone.layer.semantic.syntactic.root_sids().len(),
            2,
            "clone now has both axioms"
        );

        // Master is untouched — no leak from the clone.
        assert!(
            master.layer.semantic.syntactic.sym_id("Cat").is_none(),
            "master must NOT see the clone's Cat"
        );
        assert_eq!(
            master.layer.semantic.syntactic.root_sids().len(),
            1,
            "master root count unchanged after cloning + mutating the clone"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn kb_promote(kb: &mut KnowledgeBase<TranslationLayer>, session: &str) {
        kb.make_session_axiomatic(session).expect("promote");
    }

    #[test]
    fn open_like_keeps_the_template_s_settings() {
        let dir = tmp_dir("open-like");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let mut kb = KnowledgeBase::<TranslationLayer>::open(&dir, None).expect("open new DB");
            let r = kb.reload_kif(
                "(subclass Dog Animal)",
                &std::path::PathBuf::from("ol.kif"),
                "ol.kif",
            );
            assert!(r.ok, "ingest failed: {:?}", r.diagnostics);
            kb.make_session_axiomatic("ol.kif").expect("promote");
            kb.persist().expect("persist to LMDB");
        }

        let template = KnowledgeBase::<TranslationLayer>::new();
        template.cache_config().set_max_threads(5);
        let kb = template.open_like(&dir, None).expect("reopen");
        assert_eq!(kb.cache_config().max_threads(), 5);
        assert!(
            kb.layer.semantic.syntactic.sym_id("Dog").is_some(),
            "store contents thawed"
        );
        assert!(
            kb.db.is_some(),
            "the reopened KB stays attached to its store"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_without_a_database_is_an_error() {
        let kb = KnowledgeBase::<TranslationLayer>::new();
        let err = kb.persist().expect_err("nothing would reach disk");
        assert_eq!(err.code, "no-db");
    }
}
