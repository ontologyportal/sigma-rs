// crates/sumo-kb/src/persist/env.rs
//
// LMDB environment and named database handles.
// Ported from sumo-store/src/env.rs.
// Changes:
//   - StoreError -> KbError
//   - StoredSymbol/StoredFormula are defined here (no separate schema.rs)
//   - No `next_seq("sym")` / `next_seq("formula")`: stable IDs from KifStore counters
//     are written directly; we use a sequence DB for Skolem symbols (legacy)
//     and for clause IDs (Phase 4).
//
// Clause dedup layer (Phase 4):
//   - `clauses`         ClauseId BE       -> StoredClause
//   - `clause_hashes`   canonical-hash BE -> ClauseId
//   - `formula_hashes`  formula-hash BE   -> SentenceId
//   - `StoredFormula.clause_ids: Vec<ClauseId>`  (replaces the old inline
//     `clauses: Vec<Clause>`)
//
// The schema is versioned via a `"schema_version"` entry in the
// `sequences` DB (value: 8-byte LE u64 = 2).  Pre-Phase-4 DBs lack the
// entry; opening one returns `KbError::SchemaMigrationRequired`.

use std::path::Path;

use heed::types::{Bytes, SerdeBincode, Str};
use heed::{Database, Env, EnvOpenOptions, RoTxn, RwTxn};
use serde::{Deserialize, Serialize};

use crate::error::KbError;
use crate::types::{Literal, SentenceId, SymbolId};
use crate::parse::ast::OpKind;
#[cfg(feature = "cnf")]
use crate::types::{ClauseId, CnfLiteral};
#[cfg(feature = "cnf")]
use crate::semantic::Sort;

// -- Stored types --------------------------------------------------------------

/// A symbol as stored in LMDB (stable ID, no in-memory sentence lists).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredSymbol {
    pub id:           SymbolId,
    pub name:         String,
    pub is_skolem:    bool,
    pub skolem_arity: Option<usize>,
}

/// A formula element as stored in LMDB -- sub-sentences stored inline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum StoredElement {
    Symbol(SymbolId),
    Variable { id: SymbolId, name: String, is_row: bool },
    Literal(Literal),
    /// Inline sub-formula (was `Element::Sub { sid: SentenceId, .. }` in-memory).
    Sub(Box<StoredFormula>),
    Op(OpKind),
}

/// A formula as stored in LMDB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredFormula {
    /// Stable `SentenceId` assigned by `KifStore`.
    pub id:        SentenceId,
    pub elements:  Vec<StoredElement>,
    /// Pre-computed CNF clause ids (only present when `cnf` feature was
    /// enabled at commit time).  Each id resolves to a `StoredClause` in
    /// the `clauses` DB; shared clauses dedup to a single record.
    #[cfg(feature = "cnf")]
    pub clause_ids: Vec<ClauseId>,
    /// Session tag; `None` for promoted axioms.
    pub session:   Option<String>,
    /// File/session tag used to group sentences in `KifStore`.
    pub file:      String,
}

/// A canonically-addressed CNF clause as stored in LMDB.
///
/// Multiple formulas that share the same canonical clause hash point at
/// the same `StoredClause` record via `StoredFormula.clause_ids`.  The
/// on-disk shape deliberately keeps the full `CnfLiteral` contents so
/// the clause can be rehydrated without joining against the formula
/// table; `sort_meta` is reserved for a future sort-aware auxiliary
/// hash (see Risk 3 of the design plan) and is always `None` today.
#[cfg(feature = "cnf")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredClause {
    pub id:        ClauseId,
    pub literals:  Vec<CnfLiteral>,
    pub sort_meta: Option<Vec<Sort>>,
}

// =========================================================================
//  Phase D: semantic-layer / axiom-cache persistence
// =========================================================================
//
// Each persisted cache carries its own `kb_version` -- a monotonic u64
// stored under `sequences["kb_version"]` and bumped by `write_axioms`
// whenever the persisted axiom set changes.  On open, a cache blob is
// accepted only if its `kb_version` matches the current counter; on
// mismatch the cache is treated as absent and the semantic layer
// rebuilds from scratch.  This is strictly a performance optimisation
// -- correctness is preserved even if the cache is corrupted or
// missing, because the derivations are deterministic from the persisted
// sentences.

use crate::semantic::ArithCond;
#[cfg(feature = "ask")]
use crate::semantic::SortAnnotations;

/// Persisted form of `SemanticLayer`'s taxonomy state.
///
/// `tax_incoming` is NOT persisted: it's a reverse index derivable from
/// `tax_edges` in a single linear pass, so we save the ~O(edges) LMDB
/// bytes and recompute on load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CachedTaxonomy {
    pub kb_version:           u64,
    pub tax_edges:            Vec<crate::types::TaxEdge>,
    pub numeric_sort_cache:   std::collections::HashMap<SymbolId, crate::semantic::Sort>,
    pub numeric_ancestor_set: std::collections::HashSet<SymbolId>,
    pub poly_variant_symbols: std::collections::HashSet<SymbolId>,
    pub numeric_char_cache:   std::collections::HashMap<SymbolId, ArithCond>,
}

/// Persisted form of `SemanticLayer::SortAnnotations`.
#[cfg(feature = "ask")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CachedSortAnnotations {
    pub kb_version: u64,
    pub sorts:      SortAnnotations,
}

/// Snapshot of the cargo features that were active during the most
/// recent `write_axioms` commit.
///
/// Schema drift ("the on-disk bytes don't match the current struct
/// shapes") is handled separately via `SCHEMA_VERSION` and returns a
/// hard `SchemaMigrationRequired` error.  *Feature* drift ("the
/// struct shapes match, but the set of optional LMDB tables that
/// got populated is different") is softer: it's never wrong to open
/// a DB with a different feature set, but some tables may be empty
/// or stale.  This manifest captures that distinction so
/// `LmdbEnv::open` can warn the user and so downstream code can
/// decide how to degrade (e.g. if `cnf` was off at write time but on
/// now, the clause-dedup tables have no content to dedup against and
/// we'd want to log that rather than silently accept every tell as
/// fresh).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FeatureManifest {
    /// Redundant copy of `SCHEMA_VERSION` for easy inspection and as
    /// a one-step consistency check against
    /// `sequences["schema_version"]`.
    pub schema: u64,
    /// `kb_version` counter at the time this manifest was stamped.
    /// Tracks the same underlying counter as the cache blobs, so a
    /// manifest carrying version N is known to be consistent with
    /// caches also carrying version N.
    pub kb_version: u64,
    /// Which features were active at write time.
    pub features: FeatureSet,
}

/// Flat bool-per-feature used by [`FeatureManifest`].  Add a new
/// field here whenever a feature becomes able to mutate LMDB
/// contents in a way that's observable across processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct FeatureSet {
    /// `cnf`: clause-level dedup + CNF storage tables are populated.
    pub cnf:               bool,
    /// `integrated-prover`: the vampire-prover C++ library is linked
    /// into the writing build.  Affects which axiom-cache modes can
    /// be produced.
    pub integrated_prover: bool,
    /// `ask`: prover-runner plumbing (subprocess or embedded) is
    /// compiled in.  Used by downstream tooling to decide whether
    /// persisted axiom caches will be exercised.
    pub ask:               bool,
}

impl FeatureSet {
    /// Snapshot of the features active in the current build.
    pub(crate) fn current() -> Self {
        Self {
            cnf:               cfg!(feature = "cnf"),
            integrated_prover: cfg!(feature = "integrated-prover"),
            ask:               cfg!(feature = "ask"),
        }
    }

    /// Features that were enabled in `prev` but are off now.  Data
    /// produced by those features may sit unused in LMDB until a
    /// future build re-enables them.
    pub(crate) fn removed_since(&self, prev: &Self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if prev.cnf               && !self.cnf               { out.push("cnf"); }
        if prev.integrated_prover && !self.integrated_prover { out.push("integrated-prover"); }
        if prev.ask               && !self.ask               { out.push("ask"); }
        out
    }

    /// Features that are enabled now but were off at last write.  The
    /// newly-available tables have no persisted content yet; they'll
    /// start filling on the next write.
    pub(crate) fn added_since(&self, prev: &Self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !prev.cnf               && self.cnf               { out.push("cnf"); }
        if !prev.integrated_prover && self.integrated_prover { out.push("integrated-prover"); }
        if !prev.ask               && self.ask               { out.push("ask"); }
        out
    }
}

/// Persisted form of a `VampireAxiomCache`.
///
/// The IR `Problem` is serialised directly via bincode (the
/// `vampire-prover/serde` feature derives `Serialize`/`Deserialize` on
/// every IR type).  An earlier TPTP-text version was ~50% slower on
/// reload than the in-memory rebuild it replaced; bincode round-trips
/// the typed tree and skips all parsing, which on benchmarks is the
/// first approach that actually undercuts `NativeConverter::add_axiom`.
#[cfg(feature = "ask")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CachedAxiomProblem {
    pub kb_version: u64,
    /// Logic mode the cache was built for.  Reject on mismatch with
    /// the caller's request (e.g. cache is TFF, ask wants FOF).
    pub mode_tff:   bool,
    /// Full IR Problem (axioms + sort / function / predicate decls).
    /// No conjecture -- that's built fresh per ask.
    pub problem:    vampire_prover::ir::Problem,
    /// Parallel to `problem.axioms()` in the rebuilt problem.
    pub sid_map:    Vec<SentenceId>,
}

// -- Database names ------------------------------------------------------------

const DB_SYMBOLS_FWD:    &str = "symbols_fwd";     // name -> id (8-byte BE)
const DB_SYMBOLS_REV:    &str = "symbols_rev";     // id BE -> StoredSymbol
const DB_FORMULAS:       &str = "formulas";        // id BE -> StoredFormula
#[cfg(feature = "cnf")]
const DB_PATH_INDEX:     &str = "path_index";      // 18-byte key -> Vec<SentenceId>
const DB_HEAD_INDEX:     &str = "head_index";      // pred_id (8-byte BE) -> Vec<SentenceId>
const DB_SESSIONS:       &str = "sessions";        // session name -> Vec<SentenceId>
const DB_SEQUENCES:      &str = "sequences";       // "skolem"|"clause_id"|"schema_version"

// Clause dedup tables (feature = "cnf").
#[cfg(feature = "cnf")]
const DB_CLAUSES:        &str = "clauses";         // ClauseId BE -> StoredClause
#[cfg(feature = "cnf")]
const DB_CLAUSE_HASHES:  &str = "clause_hashes";   // canonical-hash BE -> ClauseId
#[cfg(feature = "cnf")]
const DB_FORMULA_HASHES: &str = "formula_hashes";  // formula-hash BE  -> SentenceId

// Phase D: semantic / axiom-cache persistence.
// One kv table keyed by a short name ("taxonomy", "sort_annotations",
// "axiom_cache_tff", "axiom_cache_fof"); the value is a bincode blob
// whose first field is a `kb_version` u64 used to detect staleness.
const DB_CACHES:         &str = "caches";

// Bump must match the number of named DBs actually created below.
// Includes room for all optional tables even in feature-off builds so
// the LMDB map doesn't need resizing across feature flips.
const MAX_DBS:  u32   = 12;
const MAP_SIZE: usize = 10 * 1024 * 1024 * 1024; // 10 GiB virtual

// Current on-disk schema revision.  Bump whenever a persisted type
// changes shape in a non-backward-compatible way.
//
// 3 -- Phase D: adds the `caches` table for
//      taxonomy / SortAnnotations / axiom-cache persistence.
//      Old-schema DBs (v2) still load; the cache table is absent but
//      will be populated on the next promote.
// 2 -- Phase 4 of the clause-dedup work: `StoredFormula.clauses` is
//      replaced by `clause_ids: Vec<ClauseId>`, with side tables for
//      `clauses` / `clause_hashes` / `formula_hashes`.
// 1 -- legacy (pre-Phase-4).  Detected and rejected with
//      `SchemaMigrationRequired`.
pub(super) const SCHEMA_VERSION: u64 = 3;
const SCHEMA_KEY:                &str = "schema_version";

// Well-known keys inside `DB_CACHES`.
pub(crate) const CACHE_KEY_TAXONOMY:        &str = "taxonomy";
#[cfg(feature = "ask")]
pub(crate) const CACHE_KEY_SORT_ANNOT:      &str = "sort_annotations";

// Axiom-cache persistence keys.  Populated at ensure_axiom_cache time
// and restored on the next open (as long as kb_version matches).
#[cfg(feature = "ask")]
pub(crate) const CACHE_KEY_AXIOM_CACHE_TFF: &str = "axiom_cache_tff";
#[cfg(feature = "ask")]
pub(crate) const CACHE_KEY_AXIOM_CACHE_FOF: &str = "axiom_cache_fof";

/// Feature-manifest key in `DB_CACHES`.  Populated by every
/// `write_axioms` call with the feature set that was active at commit
/// time.  See [`FeatureManifest`] for the semantics on open.
pub(crate) const CACHE_KEY_FEATURE_MANIFEST: &str = "feature_manifest";

// Well-known key inside `DB_SEQUENCES` for the KB-version counter used
// by Phase D caches.  Bumped by `write_axioms`.
pub(crate) const SEQ_KEY_KB_VERSION: &str = "kb_version";

// -- LmdbEnv -------------------------------------------------------------------

pub(crate) struct LmdbEnv {
    pub env:            Env,
    pub symbols_fwd:    Database<Str, Bytes>,
    pub symbols_rev:    Database<Bytes, SerdeBincode<StoredSymbol>>,
    pub formulas:       Database<Bytes, SerdeBincode<StoredFormula>>,
    #[cfg(feature = "cnf")]
    pub path_index:     Database<Bytes, SerdeBincode<Vec<u64>>>,
    pub head_index:     Database<Bytes, SerdeBincode<Vec<u64>>>,
    pub sessions:       Database<Str, SerdeBincode<Vec<u64>>>,
    pub sequences:      Database<Str, Bytes>,
    #[cfg(feature = "cnf")]
    pub clauses:        Database<Bytes, SerdeBincode<StoredClause>>,
    #[cfg(feature = "cnf")]
    pub clause_hashes:  Database<Bytes, Bytes>,  // canonical-hash BE -> ClauseId BE
    #[cfg(feature = "cnf")]
    pub formula_hashes: Database<Bytes, Bytes>,  // formula-hash BE -> SentenceId BE

    /// Phase D: keyed cache table.  See `CACHE_KEY_*` constants above
    /// for the valid keys.  Values are bincode blobs whose leading
    /// field is a `kb_version` u64 for staleness detection.
    pub caches:         Database<Str, Bytes>,

    /// Features that were off at the last `write_axioms` commit but
    /// are enabled in the current build.  Populated during
    /// [`LmdbEnv::open`] from the feature manifest comparison; read
    /// by higher-level code (e.g. [`KnowledgeBase::open`]) to decide
    /// whether to auto-backfill the now-relevant tables.
    ///
    /// Empty when the manifest matches (fast path) or when the DB is
    /// fresh (nothing to diff against).  Read only by the cnf-gated
    /// backfill decision in `kb::open`; allow dead_code for non-cnf
    /// builds.
    #[allow(dead_code)]
    pub added_features: Vec<&'static str>,
}

impl LmdbEnv {
    pub(crate) fn open(path: &Path) -> Result<Self, KbError> {
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Info, target: "sumo_kb::persist", message: format!("opening LMDB at {}", path.display()) });
        std::fs::create_dir_all(path).map_err(|e| {
            KbError::Db(format!("cannot create DB directory {}: {}", path.display(), e))
        })?;

        // SAFETY: We ensure only one Env is opened per path per process.
        let env = unsafe {
            EnvOpenOptions::new()
                .max_dbs(MAX_DBS)
                .map_size(MAP_SIZE)
                .open(path)
        }.map_err(|e| KbError::Db(format!("cannot open LMDB at {}: {}", path.display(), e)))?;

        // -- Schema-drift probe (pre-creation) -------------------------
        //
        // If the `formulas` table already has entries but `sequences`
        // is missing the `schema_version` key, we're looking at a
        // pre-Phase-4 database.  Bail out before `create_database` in
        // the write txn silently creates the new DBs on top of the
        // legacy layout.
        {
            let rtxn = env.read_txn()?;
            let formulas_opt =
                env.open_database::<Bytes, SerdeBincode<StoredFormula>>(&rtxn, Some(DB_FORMULAS))?;
            let has_formulas = match formulas_opt {
                Some(db) => db.iter(&rtxn)?.next().is_some(),
                None     => false,
            };
            let sequences_opt =
                env.open_database::<Str, Bytes>(&rtxn, Some(DB_SEQUENCES))?;
            let schema_marker = match sequences_opt {
                Some(db) => db.get(&rtxn, SCHEMA_KEY)?.map(|b| b.to_vec()),
                None     => None,
            };
            if has_formulas && schema_marker.is_none() {
                return Err(KbError::SchemaMigrationRequired(format!(
                    "LMDB at {} was created by an older build of sumo-kb \
                     (no `{}` key in `sequences`).  Delete the directory \
                     and re-import, or downgrade to a pre-Phase-4 build.",
                    path.display(), SCHEMA_KEY,
                )));
            }
            if let Some(bytes) = schema_marker {
                if bytes.len() != 8 {
                    return Err(KbError::Db(format!(
                        "malformed schema_version entry ({} bytes; expected 8)",
                        bytes.len(),
                    )));
                }
                let arr: [u8; 8] = bytes.as_slice().try_into().unwrap();
                let v = u64::from_le_bytes(arr);
                // Accept v2 as a forward-compatible read: Phase D adds
                // the `caches` table on top of the v2 layout, and the
                // v2 side is unchanged.  On the first promote we'll
                // stamp v3 and start populating caches.
                if v != SCHEMA_VERSION && v != 2 {
                    return Err(KbError::SchemaMigrationRequired(format!(
                        "LMDB at {} is at schema version {}; this build \
                         expects version {} (or the forward-compatible 2).",
                        path.display(), v, SCHEMA_VERSION,
                    )));
                }
            }
        }

        // -- DB creation --------------------------------------------------
        let mut wtxn = env.write_txn()?;
        let symbols_fwd    = env.create_database::<Str, Bytes>(&mut wtxn, Some(DB_SYMBOLS_FWD))?;
        let symbols_rev    = env.create_database::<Bytes, SerdeBincode<StoredSymbol>>(&mut wtxn, Some(DB_SYMBOLS_REV))?;
        let formulas       = env.create_database::<Bytes, SerdeBincode<StoredFormula>>(&mut wtxn, Some(DB_FORMULAS))?;
        #[cfg(feature = "cnf")]
        let path_index     = env.create_database::<Bytes, SerdeBincode<Vec<u64>>>(&mut wtxn, Some(DB_PATH_INDEX))?;
        let head_index     = env.create_database::<Bytes, SerdeBincode<Vec<u64>>>(&mut wtxn, Some(DB_HEAD_INDEX))?;
        let sessions       = env.create_database::<Str, SerdeBincode<Vec<u64>>>(&mut wtxn, Some(DB_SESSIONS))?;
        let sequences      = env.create_database::<Str, Bytes>(&mut wtxn, Some(DB_SEQUENCES))?;
        #[cfg(feature = "cnf")]
        let clauses        = env.create_database::<Bytes, SerdeBincode<StoredClause>>(&mut wtxn, Some(DB_CLAUSES))?;
        #[cfg(feature = "cnf")]
        let clause_hashes  = env.create_database::<Bytes, Bytes>(&mut wtxn, Some(DB_CLAUSE_HASHES))?;
        #[cfg(feature = "cnf")]
        let formula_hashes = env.create_database::<Bytes, Bytes>(&mut wtxn, Some(DB_FORMULA_HASHES))?;
        let caches         = env.create_database::<Str, Bytes>(&mut wtxn, Some(DB_CACHES))?;

        // Stamp (or upgrade) the schema version for fresh + v2 DBs.
        // v2 DBs become v3 at first open: the `caches` table is
        // already created empty above, so the upgrade is a pure
        // metadata bump with no data migration.
        let current_schema = sequences.get(&wtxn, SCHEMA_KEY)?
            .and_then(|b| <[u8; 8]>::try_from(b).ok())
            .map(u64::from_le_bytes);
        if current_schema != Some(SCHEMA_VERSION) {
            sequences.put(&mut wtxn, SCHEMA_KEY, &SCHEMA_VERSION.to_le_bytes())?;
        }

        wtxn.commit()?;
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sumo_kb::persist", message: format!("LMDB opened; schema v{}, max_dbs={}", SCHEMA_VERSION, MAX_DBS) });

        // -- Feature-manifest comparison ------------------------------
        //
        // After the write txn commits the (possibly freshly-created)
        // caches table, read the manifest back and diff against the
        // current build's features.  This catches the case where the
        // DB was written by a different feature-set configuration
        // (e.g. `cnf` was off at write time, on now).  Correctness
        // isn't at risk -- schema mismatches already refused to open
        // above -- but the user deserves a clear signal about which
        // tables may be stale, empty, or ignored.
        //
        // Populate `added_features` so higher-level callers can
        // trigger an automatic backfill of the newly-enabled
        // feature's tables.
        let added_features: Vec<&'static str> = {
            let rtxn = env.read_txn()?;
            let current = FeatureSet::current();
            let manifest: Option<FeatureManifest> = caches
                .get(&rtxn, CACHE_KEY_FEATURE_MANIFEST)?
                .and_then(|b| bincode::deserialize(b).ok());

            match manifest {
                None => {
                    // Fresh DB, or a pre-Phase-D write that never
                    // stamped a manifest.  Nothing to diff against;
                    // the manifest will be written on the next
                    // `write_axioms` call.
                    crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sumo_kb::persist", message: format!("no feature manifest present; will stamp on next commit (features={:?})", current) });
                    Vec::new()
                }
                Some(m) if m.features == current => {
                    crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sumo_kb::persist", message: format!("feature manifest matches current build: {:?}", current) });
                    Vec::new()
                }
                Some(m) => {
                    let removed = current.removed_since(&m.features);
                    let added   = current.added_since(&m.features);
                    if !removed.is_empty() {
                        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Warn, target: "sumo_kb::persist", message: format!("feature drift: DB was written with {:?} but this build \
                             lacks those features.  The corresponding LMDB tables \
                             will sit unused; data is preserved across reopens.", removed) });
                    }
                    if !added.is_empty() {
                        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Info, target: "sumo_kb::persist", message: format!("feature drift: DB was written WITHOUT {:?} but this build \
                             enables them.  An automatic backfill will populate the \
                             corresponding LMDB tables from the existing axioms.", added) });
                    }
                    added
                }
            }
        };

        Ok(Self {
            env,
            symbols_fwd, symbols_rev, formulas,
            #[cfg(feature = "cnf")]
            path_index,
            head_index, sessions, sequences,
            #[cfg(feature = "cnf")]
            clauses,
            #[cfg(feature = "cnf")]
            clause_hashes,
            #[cfg(feature = "cnf")]
            formula_hashes,
            caches,
            added_features,
        })
    }

    pub(crate) fn read_txn(&self) -> Result<RoTxn<'_>, KbError> {
        Ok(self.env.read_txn()?)
    }

    pub(crate) fn write_txn(&self) -> Result<RwTxn<'_>, KbError> {
        Ok(self.env.write_txn()?)
    }

    // -- Symbol helpers --------------------------------------------------------

    /// Look up a symbol id by name (read within a write txn).
    pub(crate) fn get_symbol_id(&self, txn: &RoTxn, name: &str) -> Result<Option<SymbolId>, KbError> {
        match self.symbols_fwd.get(txn, name)? {
            Some(b) => {
                let arr: [u8; 8] = b.try_into()
                    .map_err(|_| KbError::Db("bad symbol id bytes".into()))?;
                Ok(Some(u64::from_be_bytes(arr)))
            }
            None => Ok(None),
        }
    }

    /// Write a symbol to both `symbols_fwd` and `symbols_rev`.
    /// No-ops if the name already exists with the same ID.
    pub(crate) fn put_symbol(
        &self,
        wtxn:  &mut RwTxn,
        sym:   &StoredSymbol,
    ) -> Result<(), KbError> {
        let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(wtxn) };
        if self.get_symbol_id(rtxn, &sym.name)?.is_some() {
            #[cfg(debug_assertions)]
            crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Trace, target: "sumo_kb::persist", message: format!("put_symbol: '{}' already in DB", sym.name) });
            return Ok(());
        }
        #[cfg(debug_assertions)]
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Trace, target: "sumo_kb::persist", message: format!("put_symbol: '{}' id={}", sym.name, sym.id) });
        self.symbols_fwd.put(wtxn, &sym.name, &sym.id.to_be_bytes())?;
        self.symbols_rev.put(wtxn, &sym.id.to_be_bytes(), sym)?;
        Ok(())
    }

    /// Write a formula to `formulas` DB.
    pub(crate) fn put_formula(
        &self,
        wtxn:    &mut RwTxn,
        formula: &StoredFormula,
    ) -> Result<(), KbError> {
        #[cfg(debug_assertions)]
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Trace, target: "sumo_kb::persist", message: format!("put_formula: id={}", formula.id) });
        self.formulas.put(wtxn, &formula.id.to_be_bytes(), formula)?;
        Ok(())
    }

    /// Delete a formula from `formulas` DB and scrub every secondary
    /// index entry that referenced its `SentenceId`.
    ///
    /// Idempotent — deleting an absent sid is fine.  Indexes that
    /// would be left empty after scrubbing are removed entirely
    /// (consistent with `index_head` / `index_path`, which lazily
    /// create them on first insert).
    ///
    /// Used by the `sumo load` per-file reconcile path to commit
    /// `removed` deltas without a full DB rewrite.  Does **not**
    /// touch clause interning (clauses are deduped globally; a
    /// stale clause blob costs at most a few KB until the next
    /// rewrite) or the `sessions` table (session assertions aren't
    /// persisted beyond the process).
    pub(crate) fn delete_formula(
        &self,
        wtxn: &mut RwTxn,
        sid:  SentenceId,
    ) -> Result<(), KbError> {
        // 1. Load the formula so we know which head-predicate and
        //    path-index buckets to scrub.  Absent → nothing to do.
        let key = sid.to_be_bytes();
        let stored: Option<StoredFormula> = {
            let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(&*wtxn) };
            self.formulas.get(rtxn, &key)?
        };
        let Some(stored) = stored else {
            return Ok(());
        };

        // 2. Delete from formulas table.
        self.formulas.delete(wtxn, &key)?;

        // 3. Scrub head_index.  The stored `elements` preserve the
        //    exact head bytes that `write_sentence` used when
        //    inserting, so we can recover the pred_id without a
        //    symbols-rev lookup.
        if let Some(StoredElement::Symbol(pred_id)) = stored.elements.first() {
            let pkey = pred_id.to_be_bytes();
            let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(&*wtxn) };
            if let Some(mut ids) = self.head_index.get(rtxn, &pkey)? {
                ids.retain(|&id| id != sid);
                if ids.is_empty() {
                    self.head_index.delete(wtxn, &pkey)?;
                } else {
                    self.head_index.put(wtxn, &pkey, &ids)?;
                }
            }
        }

        // 4. Scrub path_index (cnf feature only).  Each indexed path
        //    key is derived from (pred_id, arg_pos, sym_id); we walk
        //    the stored elements to reconstruct the same keys rather
        //    than opening a cursor over the whole path table.
        #[cfg(feature = "cnf")]
        {
            if let Some(StoredElement::Symbol(pred_id)) = stored.elements.first() {
                for (i, elem) in stored.elements.iter().enumerate().skip(1) {
                    if let StoredElement::Symbol(sym_id) = elem {
                        let arg_pos = (i - 1) as u16;
                        let k = super::path_index::encode_key(*pred_id, arg_pos, *sym_id);
                        let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(&*wtxn) };
                        if let Some(mut ids) = self.path_index.get(rtxn, &k)? {
                            ids.retain(|&id| id != sid);
                            if ids.is_empty() {
                                self.path_index.delete(wtxn, &k)?;
                            } else {
                                self.path_index.put(wtxn, &k, &ids)?;
                            }
                        }
                    }
                }
            }
        }

        // 5. Scrub formula_hashes by value.  `put_formula_hash`
        //    stores `hash -> sid`; to find the hash for this sid we
        //    could either (a) walk the table, or (b) require the
        //    caller to supply the hash.  (a) is O(|formulas|); (b)
        //    is O(1) but complicates the caller.  We go with (a)
        //    since `delete_formula` is off the hot path and the
        //    simpler API is worth the scan.
        #[cfg(feature = "cnf")]
        {
            // `formula_hashes` is declared `Database<Bytes, Bytes>`
            // (see field decl near the top of this file), with
            // `put_formula_hash` writing both key and value as 8-byte
            // big-endian encodings.  Iter yields `(&[u8], &[u8])`, so
            // we decode the 8-byte value before comparing to `sid`,
            // and copy the key bytes into an owned `[u8; 8]` before
            // the borrow ends.
            let mut to_delete: Vec<[u8; 8]> = Vec::new();
            {
                let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(&*wtxn) };
                for result in self.formula_hashes.iter(rtxn)? {
                    let (k, v) = result?;
                    let arr: [u8; 8] = v.try_into().map_err(|_| {
                        KbError::Db("bad formula_hashes value length".into())
                    })?;
                    if SentenceId::from_be_bytes(arr) == sid {
                        let key_arr: [u8; 8] = k.try_into().map_err(|_| {
                            KbError::Db("bad formula_hashes key length".into())
                        })?;
                        to_delete.push(key_arr);
                    }
                }
            }
            for k in to_delete {
                self.formula_hashes.delete(wtxn, &k)?;
            }
        }

        #[cfg(debug_assertions)]
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Trace, target: "sumo_kb::persist", message: format!("delete_formula: sid={} removed", sid) });
        Ok(())
    }

    /// Append a `SentenceId` to the head-predicate index.
    pub(crate) fn index_head(
        &self,
        wtxn:       &mut RwTxn,
        pred_id:    u64,
        formula_id: SentenceId,
    ) -> Result<(), KbError> {
        let key = pred_id.to_be_bytes();
        let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(wtxn) };
        let mut ids: Vec<u64> = self.head_index.get(rtxn, &key)?.unwrap_or_default();
        ids.push(formula_id);
        self.head_index.put(wtxn, &key, &ids)?;
        Ok(())
    }

    #[cfg(feature = "cnf")]
    /// Append to the path index for `(pred_id, arg_pos, sym_id)`.
    pub(crate) fn index_path(
        &self,
        wtxn:       &mut RwTxn,
        pred_id:    u64,
        arg_pos:    u16,
        sym_id:     u64,
        formula_id: SentenceId,
    ) -> Result<(), KbError> {
        let key = super::path_index::encode_key(pred_id, arg_pos, sym_id);
        let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(wtxn) };
        let mut ids: Vec<u64> = self.path_index.get(rtxn, &key)?.unwrap_or_default();
        ids.push(formula_id);
        self.path_index.put(wtxn, &key, &ids)?;
        Ok(())
    }

    /// Append a `SentenceId` to the session list.
    pub(crate) fn append_session(
        &self,
        wtxn:       &mut RwTxn,
        session:    &str,
        formula_id: SentenceId,
    ) -> Result<(), KbError> {
        let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(wtxn) };
        let mut ids: Vec<u64> = self.sessions.get(rtxn, session)?.unwrap_or_default();
        ids.push(formula_id);
        self.sessions.put(wtxn, session, &ids)?;
        Ok(())
    }

    // -- Clause dedup helpers (feature = "cnf") -------------------------------

    /// Intern a clause by its canonical hash, returning its `ClauseId`.
    ///
    /// If a record with the same canonical hash already exists, returns
    /// the existing `ClauseId` without writing.  Otherwise allocates a
    /// fresh id via the `clause_id` sequence, writes the `StoredClause`
    /// record, and records the hash-to-id mapping.
    ///
    /// `sort_meta` is currently always `None`; the parameter is kept on
    /// the signature so the future sort-aware auxiliary hash (Risk 3 in
    /// the design plan) has a place to flow through.
    #[cfg(feature = "cnf")]
    pub(crate) fn intern_clause(
        &self,
        wtxn:      &mut RwTxn,
        canonical: u64,
        clause:    &crate::types::Clause,
        sort_meta: Option<Vec<Sort>>,
    ) -> Result<ClauseId, KbError> {
        let hash_key = canonical.to_be_bytes();

        // Existing?
        let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(wtxn) };
        if let Some(bytes) = self.clause_hashes.get(rtxn, &hash_key)? {
            let arr: [u8; 8] = bytes.try_into()
                .map_err(|_| KbError::Db("bad clause_hashes value length".into()))?;
            let id = u64::from_be_bytes(arr);
            #[cfg(debug_assertions)]
            crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Trace, target: "sumo_kb::persist", message: format!("intern_clause: hash={:016x} -> existing id={}", canonical, id) });
            return Ok(id);
        }

        // Fresh id.
        let id = self.next_clause_id(wtxn)?;
        let record = StoredClause {
            id,
            literals: clause.literals.clone(),
            sort_meta,
        };
        self.clauses.put(wtxn, &id.to_be_bytes(), &record)?;
        self.clause_hashes.put(wtxn, &hash_key, &id.to_be_bytes())?;
        #[cfg(debug_assertions)]
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Trace, target: "sumo_kb::persist", message: format!("intern_clause: hash={:016x} -> new id={}", canonical, id) });
        Ok(id)
    }

    /// Record a formula-level fingerprint → `SentenceId` mapping.
    ///
    /// Idempotent by hash; later writes overwrite earlier ones, which is
    /// fine because an identical clause set always maps to the same
    /// SentenceId (or, in the dedup-rejection path, the earliest id
    /// already recorded — the caller checks existence before calling).
    #[cfg(feature = "cnf")]
    pub(crate) fn put_formula_hash(
        &self,
        wtxn:         &mut RwTxn,
        formula_hash: u64,
        sid:          SentenceId,
    ) -> Result<(), KbError> {
        self.formula_hashes.put(
            wtxn,
            &formula_hash.to_be_bytes(),
            &sid.to_be_bytes(),
        )?;
        Ok(())
    }

    /// Look up a formula-level fingerprint in `formula_hashes`.
    ///
    /// Test-only helper: production code populates the in-memory
    /// `fingerprints` map via `all_formula_hashes` at open time; no
    /// runtime path does point lookups.
    #[cfg(all(feature = "cnf", test))]
    pub(crate) fn get_formula_hash(
        &self,
        txn:          &RoTxn,
        formula_hash: u64,
    ) -> Result<Option<SentenceId>, KbError> {
        let key = formula_hash.to_be_bytes();
        match self.formula_hashes.get(txn, &key)? {
            Some(bytes) => {
                let arr: [u8; 8] = bytes.try_into()
                    .map_err(|_| KbError::Db("bad formula_hashes value length".into()))?;
                Ok(Some(u64::from_be_bytes(arr)))
            }
            None => Ok(None),
        }
    }

    /// Allocate the next `ClauseId` from the `clause_id` sequence.
    #[cfg(feature = "cnf")]
    fn next_clause_id(&self, wtxn: &mut RwTxn) -> Result<ClauseId, KbError> {
        const KEY: &str = "clause_id";
        let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(wtxn) };
        let current: u64 = match self.sequences.get(rtxn, KEY)? {
            Some(bytes) => {
                let arr: [u8; 8] = bytes.try_into()
                    .map_err(|_| KbError::Db("bad clause_id seq length".into()))?;
                u64::from_le_bytes(arr)
            }
            None => 0,
        };
        let next = current + 1;
        self.sequences.put(wtxn, KEY, &next.to_le_bytes())?;
        Ok(current)
    }

    // -- Phase D: semantic / axiom-cache helpers --------------------------

    /// Read the current `kb_version` counter (0 if absent).
    pub(crate) fn kb_version(&self, txn: &RoTxn) -> Result<u64, KbError> {
        Ok(self.sequences.get(txn, SEQ_KEY_KB_VERSION)?
            .and_then(|b| <[u8; 8]>::try_from(b).ok())
            .map(u64::from_le_bytes)
            .unwrap_or(0))
    }

    /// Re-stamp the feature manifest with the current build's feature
    /// set at the current `kb_version`.
    ///
    /// Used by the auto-backfill path, which populates tables made
    /// newly-relevant by a feature flip (e.g. `cnf` off -> on).  The
    /// underlying axiom set is unchanged so `kb_version` is NOT
    /// bumped; only the manifest's `features` field moves.  This
    /// keeps the taxonomy / sort_annotations / axiom caches valid
    /// (their stored `kb_version` still matches the counter) while
    /// recording that the cnf tables are now populated.
    #[cfg(feature = "cnf")]
    pub(super) fn stamp_current_feature_manifest(
        &self,
        wtxn: &mut RwTxn,
    ) -> Result<(), KbError> {
        let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(wtxn) };
        let kb_version = self.kb_version(rtxn)?;
        let manifest = FeatureManifest {
            schema: SCHEMA_VERSION,
            kb_version,
            features: FeatureSet::current(),
        };
        self.put_cache(wtxn, CACHE_KEY_FEATURE_MANIFEST, &manifest)?;
        Ok(())
    }

    /// Bump the `kb_version` counter (read-modify-write).
    ///
    /// # Invariant: `write_axioms` is the sole authorised caller
    ///
    /// Every Phase D cache (`taxonomy`, `sort_annotations`,
    /// `axiom_cache_*`, `feature_manifest`) is validated by comparing
    /// its stored `kb_version` with the value this method returns.
    /// If any mutation path touches persisted axioms **without**
    /// routing through `write_axioms`, the counter stays the same
    /// while the underlying data changes, and next-open cache
    /// restoration would return stale state without warning.
    ///
    /// To enforce the invariant at the type level, this method is
    /// `pub(super)` — visible only inside `crate::persist`.  Code
    /// outside the persist module cannot call it directly; any new
    /// writer that mutates the axiom set must live in `commit.rs`
    /// (next to `write_axioms`) and bump the counter there.
    ///
    /// If you find yourself needing to call this from elsewhere in
    /// the tree, the right move is to move your mutation function
    /// into `persist/commit.rs` instead of widening the visibility.
    pub(super) fn bump_kb_version(&self, wtxn: &mut RwTxn) -> Result<u64, KbError> {
        let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(wtxn) };
        let current = self.kb_version(rtxn)?;
        let next = current + 1;
        self.sequences.put(wtxn, SEQ_KEY_KB_VERSION, &next.to_le_bytes())?;
        Ok(next)
    }

    /// Write a cache blob under `key` (bincode-serialised).
    ///
    /// The caller is responsible for packing the current `kb_version`
    /// into the blob; this helper only handles the bytes.
    pub(crate) fn put_cache<T: Serialize>(
        &self,
        wtxn:  &mut RwTxn,
        key:   &str,
        value: &T,
    ) -> Result<(), KbError> {
        let bytes = bincode::serialize(value)
            .map_err(|e| KbError::Db(format!("cache serialize for '{key}': {e}")))?;
        self.caches.put(wtxn, key, &bytes)?;
        Ok(())
    }

    /// Read a cache blob under `key`.  Returns `Ok(None)` if the key
    /// is absent or the blob fails to deserialize (treated as absent
    /// so a stale/garbled cache degrades to a rebuild rather than an
    /// open failure).
    pub(crate) fn get_cache<T: for<'de> Deserialize<'de>>(
        &self,
        txn:  &RoTxn,
        key:  &str,
    ) -> Result<Option<T>, KbError> {
        let Some(bytes) = self.caches.get(txn, key)? else { return Ok(None) };
        match bincode::deserialize::<T>(bytes) {
            Ok(v)  => Ok(Some(v)),
            Err(e) => {
                crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Warn, target: "sumo_kb::persist", message: format!("cache '{}' deserialize failed ({}); treating as absent", key, e) });
                Ok(None)
            }
        }
    }

    // -- Read helpers ----------------------------------------------------------

    pub(crate) fn all_formulas(&self, txn: &RoTxn) -> Result<Vec<StoredFormula>, KbError> {
        let mut out = Vec::new();
        for result in self.formulas.iter(txn)? {
            let (_, formula) = result?;
            out.push(formula);
        }
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sumo_kb::persist", message: format!("all_formulas: {} formula(s) loaded", out.len()) });
        Ok(out)
    }

    pub(crate) fn all_symbols(&self, txn: &RoTxn) -> Result<Vec<StoredSymbol>, KbError> {
        let mut out = Vec::new();
        for result in self.symbols_rev.iter(txn)? {
            let (_, sym) = result?;
            out.push(sym);
        }
        Ok(out)
    }

    /// Iterate every `StoredClause` in the `clauses` table.
    ///
    /// Test-only helper: lets tests assert the exact dedup shape of
    /// the `clauses` table.  Production rehydration reads via
    /// `all_formula_hashes` (formula-level map) rather than
    /// reconstructing clauses.
    #[cfg(all(feature = "cnf", test))]
    pub(crate) fn all_clauses(&self, txn: &RoTxn) -> Result<Vec<StoredClause>, KbError> {
        let mut out = Vec::new();
        for result in self.clauses.iter(txn)? {
            let (_, clause) = result?;
            out.push(clause);
        }
        Ok(out)
    }

    /// Iterate every `(formula_hash, SentenceId)` pair in the
    /// `formula_hashes` table.  Used at open time to rehydrate the
    /// in-memory dedup map.
    #[cfg(feature = "cnf")]
    pub(crate) fn all_formula_hashes(
        &self,
        txn: &RoTxn,
    ) -> Result<Vec<(u64, SentenceId)>, KbError> {
        let mut out = Vec::new();
        for result in self.formula_hashes.iter(txn)? {
            let (k, v) = result?;
            let k_arr: [u8; 8] = k.try_into()
                .map_err(|_| KbError::Db("bad formula_hashes key length".into()))?;
            let v_arr: [u8; 8] = v.try_into()
                .map_err(|_| KbError::Db("bad formula_hashes value length".into()))?;
            out.push((u64::from_be_bytes(k_arr), u64::from_be_bytes(v_arr)));
        }
        Ok(out)
    }

}

// =========================================================================
//  Tests
// =========================================================================

#[cfg(all(test, feature = "cnf"))]
mod tests {
    use super::*;
    use crate::types::{Clause, CnfLiteral, CnfTerm};

    /// Make a unique temp dir path per test invocation.  Uses PID + a
    /// monotonic counter so concurrent runs don't collide.
    fn tmp_dir(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!("sumo-kb-phase4-{}-{}-{}",
            name, std::process::id(), n));
        p
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_dir_all(path);
    }

    fn clause_p(pred_id: SymbolId) -> Clause {
        Clause {
            literals: vec![CnfLiteral {
                positive: true,
                pred:     CnfTerm::Const(pred_id),
                args:     vec![CnfTerm::Const(42)],
            }],
        }
    }

    /// Fresh DB gets stamped with the current schema version and
    /// reopens cleanly.
    #[test]
    fn fresh_db_stamps_schema_and_reopens() {
        let dir = tmp_dir("schema-stamp");
        cleanup(&dir);

        {
            let env = LmdbEnv::open(&dir).expect("fresh open");
            // Version stamped?
            let rtxn = env.read_txn().unwrap();
            let bytes = env.sequences.get(&rtxn, SCHEMA_KEY).unwrap()
                .expect("schema_version key missing after fresh open");
            assert_eq!(bytes.len(), 8);
            let v = u64::from_le_bytes(bytes.try_into().unwrap());
            assert_eq!(v, SCHEMA_VERSION);
        }

        // Second open on the same path should succeed.
        let _env2 = LmdbEnv::open(&dir).expect("reopen");

        cleanup(&dir);
    }

    /// `intern_clause` is idempotent: two calls with the same canonical
    /// hash return the same ClauseId and leave the `clauses` DB with a
    /// single entry.
    #[test]
    fn intern_clause_dedups_by_hash() {
        let dir = tmp_dir("intern-dedup");
        cleanup(&dir);

        let env = LmdbEnv::open(&dir).expect("open");
        let c1 = clause_p(100);
        let c2 = clause_p(100); // structurally identical
        let c3 = clause_p(200); // different pred

        let (id1, id2, id3) = {
            let mut wtxn = env.write_txn().unwrap();
            let id1 = env.intern_clause(&mut wtxn, 0xAAAA, &c1, None).unwrap();
            let id2 = env.intern_clause(&mut wtxn, 0xAAAA, &c2, None).unwrap();
            let id3 = env.intern_clause(&mut wtxn, 0xBBBB, &c3, None).unwrap();
            wtxn.commit().unwrap();
            (id1, id2, id3)
        };

        assert_eq!(id1, id2, "same hash must return same ClauseId");
        assert_ne!(id1, id3, "different hash must allocate fresh id");

        // `clauses` DB should have exactly two records.
        let rtxn = env.read_txn().unwrap();
        let all = env.all_clauses(&rtxn).unwrap();
        assert_eq!(all.len(), 2);

        cleanup(&dir);
    }

    /// `put_formula_hash` / `get_formula_hash` round-trip.
    #[test]
    fn formula_hash_roundtrip() {
        let dir = tmp_dir("formula-hash");
        cleanup(&dir);

        let env = LmdbEnv::open(&dir).expect("open");
        {
            let mut wtxn = env.write_txn().unwrap();
            env.put_formula_hash(&mut wtxn, 0x1234_5678, 42).unwrap();
            env.put_formula_hash(&mut wtxn, 0xDEAD_BEEF, 99).unwrap();
            wtxn.commit().unwrap();
        }
        let rtxn = env.read_txn().unwrap();
        assert_eq!(env.get_formula_hash(&rtxn, 0x1234_5678).unwrap(), Some(42));
        assert_eq!(env.get_formula_hash(&rtxn, 0xDEAD_BEEF).unwrap(), Some(99));
        assert_eq!(env.get_formula_hash(&rtxn, 0x0000_0000).unwrap(), None);

        let all = env.all_formula_hashes(&rtxn).unwrap();
        assert_eq!(all.len(), 2);

        cleanup(&dir);
    }

    /// `next_clause_id` is monotonic and persists across reopens.
    #[test]
    fn clause_id_sequence_is_monotonic_and_persisted() {
        let dir = tmp_dir("seq-persist");
        cleanup(&dir);

        let first_batch = {
            let env = LmdbEnv::open(&dir).unwrap();
            let mut wtxn = env.write_txn().unwrap();
            let a = env.intern_clause(&mut wtxn, 1, &clause_p(1), None).unwrap();
            let b = env.intern_clause(&mut wtxn, 2, &clause_p(2), None).unwrap();
            wtxn.commit().unwrap();
            (a, b)
        };
        assert!(first_batch.0 < first_batch.1);

        // Reopen: new id should start past the existing max.
        let env2 = LmdbEnv::open(&dir).unwrap();
        let mut wtxn = env2.write_txn().unwrap();
        let c = env2.intern_clause(&mut wtxn, 3, &clause_p(3), None).unwrap();
        wtxn.commit().unwrap();
        assert!(c > first_batch.1, "seq reset across reopen: {} <= {}", c, first_batch.1);

        cleanup(&dir);
    }

    /// Reject a DB that has formulas but no schema marker (pre-Phase-4
    /// layout).  We simulate this by hand-building the legacy shape
    /// before calling `open()`.
    #[test]
    fn legacy_db_is_rejected() {
        let dir = tmp_dir("legacy-reject");
        cleanup(&dir);

        // -- Stage 1: create a DB with the legacy shape ---------------
        //
        // We can't open via `LmdbEnv::open` because that would stamp
        // the schema version.  Go directly through heed with a
        // minimal write that only populates `formulas` + `sequences`
        // (without the `schema_version` key).
        std::fs::create_dir_all(&dir).unwrap();
        let env = unsafe {
            EnvOpenOptions::new()
                .max_dbs(MAX_DBS)
                .map_size(MAP_SIZE)
                .open(&dir)
                .unwrap()
        };
        {
            let mut wtxn = env.write_txn().unwrap();
            let formulas = env.create_database::<Bytes, SerdeBincode<StoredFormula>>(
                &mut wtxn, Some(DB_FORMULAS)).unwrap();
            let _sequences = env.create_database::<Str, Bytes>(
                &mut wtxn, Some(DB_SEQUENCES)).unwrap();

            // Pre-Phase-4 StoredFormula had `clauses: Vec<Clause>` rather
            // than `clause_ids: Vec<ClauseId>`.  We can't construct that
            // shape directly anymore, but the detector only cares
            // whether `formulas` has any entry at all — any byte payload
            // keyed under an 8-byte BE id will do.  Use the `Bytes`
            // codec via a second open handle so the raw bytes bypass
            // the strict SerdeBincode decoder.
            let raw_formulas = env.open_database::<Bytes, Bytes>(
                &wtxn, Some(DB_FORMULAS)).unwrap().unwrap();
            raw_formulas.put(&mut wtxn, &1u64.to_be_bytes(), b"legacy-bytes").unwrap();

            wtxn.commit().unwrap();
            // Deliberately do NOT write `schema_version` into sequences.
            let _ = formulas;
        }
        drop(env);

        // -- Stage 2: opening must fail with SchemaMigrationRequired --
        match LmdbEnv::open(&dir) {
            Ok(_) => panic!("opening legacy-shape DB must fail"),
            Err(KbError::SchemaMigrationRequired(msg)) => {
                assert!(msg.contains(SCHEMA_KEY) || msg.contains("schema"),
                    "unexpected message: {msg}");
            }
            Err(other) => panic!("expected SchemaMigrationRequired, got {:?}", other),
        }

        cleanup(&dir);
    }

    // =====================================================================
    //  Feature manifest tests
    // =====================================================================

    /// `FeatureSet::current()` reports the exact features the test
    /// binary was compiled with.  The sumo-kb test suite runs with
    /// `cnf integrated-prover persist ask`, so all flags must be on.
    #[test]
    fn feature_set_current_reflects_build() {
        let fs = FeatureSet::current();
        // If this assertion fails, either the Cargo.toml features
        // stopped matching what the test suite needs, or the test
        // configuration changed -- investigate either way.
        assert!(fs.cnf,               "cnf feature expected on in test build");
        assert!(fs.integrated_prover, "integrated-prover expected on in test build");
        assert!(fs.ask,               "ask expected on in test build");
    }

    /// `removed_since` / `added_since` report directional drift.
    #[test]
    fn feature_set_drift_detection() {
        let all  = FeatureSet { cnf: true,  integrated_prover: true,  ask: true  };
        let none = FeatureSet { cnf: false, integrated_prover: false, ask: false };
        let cnf_only = FeatureSet { cnf: true, integrated_prover: false, ask: false };

        // all -> none: everything removed.
        assert_eq!(none.removed_since(&all),
            vec!["cnf", "integrated-prover", "ask"]);
        assert_eq!(none.added_since(&all), Vec::<&str>::new());

        // none -> all: everything added.
        assert_eq!(all.added_since(&none),
            vec!["cnf", "integrated-prover", "ask"]);
        assert_eq!(all.removed_since(&none), Vec::<&str>::new());

        // cnf_only -> all: integrated_prover + ask added.
        assert_eq!(all.added_since(&cnf_only),
            vec!["integrated-prover", "ask"]);
        // all -> cnf_only: integrated_prover + ask removed.
        assert_eq!(cnf_only.removed_since(&all),
            vec!["integrated-prover", "ask"]);
    }

    /// Manifest is stamped by `write_axioms` and survives a close +
    /// reopen.  The round-tripped value matches the in-process
    /// `FeatureSet::current()` exactly.
    #[test]
    fn manifest_roundtrips_across_reopen() {
        use crate::kif_store::{load_kif, KifStore};
        use crate::persist::write_axioms;
        use std::collections::HashMap;

        let dir = tmp_dir("manifest-roundtrip");
        cleanup(&dir);

        // Populate one axiom so `write_axioms` actually runs.
        let kb_version_after = {
            let env = LmdbEnv::open(&dir).expect("open");
            let mut store = KifStore::default();
            load_kif(&mut store, "(subclass Dog Animal)", "t");
            let sid = *store.roots.last().unwrap();
            let clauses = HashMap::new();
            write_axioms(&env, &store, &[sid], &clauses, None).unwrap();
            let rtxn = env.read_txn().unwrap();
            env.kb_version(&rtxn).unwrap()
        };
        assert!(kb_version_after >= 1, "kb_version should be bumped");

        // Reopen and inspect the manifest.
        let env = LmdbEnv::open(&dir).expect("reopen");
        let rtxn = env.read_txn().unwrap();
        let manifest: FeatureManifest = bincode::deserialize(
            env.caches.get(&rtxn, CACHE_KEY_FEATURE_MANIFEST).unwrap().unwrap()
        ).unwrap();

        assert_eq!(manifest.schema,     SCHEMA_VERSION);
        assert_eq!(manifest.kb_version, kb_version_after);
        assert_eq!(manifest.features,   FeatureSet::current());

        cleanup(&dir);
    }

    /// A fresh LMDB with no prior commit does not have a manifest
    /// yet.  Opening must succeed (no hard error on missing manifest)
    /// but the caches table has no `feature_manifest` key.
    #[test]
    fn fresh_db_has_no_manifest_but_opens() {
        let dir = tmp_dir("no-manifest");
        cleanup(&dir);

        let env = LmdbEnv::open(&dir).expect("fresh open");
        let rtxn = env.read_txn().unwrap();
        let present = env.caches.get(&rtxn, CACHE_KEY_FEATURE_MANIFEST).unwrap();
        assert!(present.is_none(),
            "fresh DB should not have a manifest yet (will be stamped on first write_axioms)");

        cleanup(&dir);
    }
}
