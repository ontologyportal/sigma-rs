// crates/sumo-kb/src/kb/mod.rs
//
// KnowledgeBase -- the single public API type for sumo-kb.
// Assembles KifStore + SemanticLayer + sessions + (optionally) the
// clause-level dedup map + optional persist/ask/cnf.
//
// Deduplication model (Phase 5 of the clause-dedup work):
//   * With `cnf` on (default):
//       - tell/load_kif clausifies each candidate root sentence via
//         `cnf::sentence_to_clauses`, derives a formula hash via
//         `canonical::formula_hash_from_clauses` over the canonical
//         clause hashes, and checks against the in-memory
//         `fingerprints` map.
//       - On reopen, `fingerprints` is populated from
//         `DB_FORMULA_HASHES`.
//   * With `cnf` off:
//       - No dedup.  Duplicate axioms are accepted silently.  The
//         in-memory map does not exist.

use std::collections::{HashMap, HashSet};

use crate::error::{
    DuplicateInfo, DuplicateSource, KbError, PromoteError, PromoteReport, SemanticError,
    Span, TellResult, TellWarning,
};
use crate::kif_store::{load_kif, KifStore};
use crate::parse::ast::AstNode;
use crate::semantic::SemanticLayer;
use crate::types::{SentenceId, SymbolId};

#[cfg(feature = "ask")]
use std::sync::RwLock;
#[cfg(feature = "ask")]
use crate::sine::{collect_conjecture_symbols, SineIndex, SineParams};

#[cfg(feature = "cnf")]
use crate::types::Clause;

#[cfg(feature = "persist")]
use crate::persist::{load_from_db, write_axioms, LmdbEnv};

#[cfg(feature = "ask")]
use crate::prover::{ProverMode, ProverOpts, ProverRunner, ProverStatus};

// Sub-modules: prove/export/man methods broken out for file-size
// hygiene.  All four files share the same `KnowledgeBase` and see
// each other's private items because they live in the same module
// tree.
/// Timed-span macro for KB instrumentation.
///
/// Expands to `self.profiler.as_ref().map(|p| p.span(phase))` at the
/// call site.  Using a macro (instead of a `fn span(&self, ...)`
/// method) preserves Rust's field-level borrow disjointness: the
/// returned guard borrows only `self.profiler`, not all of `self`,
/// so surrounding code can still mutate other fields.
///
/// Usage:
///
/// ```ignore
/// let _span = profile_span!(self, "ingest.parse");
/// // ... work ...
/// // _span drops here, recording duration onto the profiler.
/// ```
///
/// When no profiler is installed, `.as_ref()` returns `None` and
/// `.map` yields `None`, so the `_span = None` binding compiles to
/// nothing measurable.  When the `profiling` feature is off at
/// build time, the span guard itself is a zero-sized no-op.
///
/// Declared BEFORE the `mod prove;` / `mod export;` / `pub mod man;`
/// sub-module declarations so those submodules can use it too.
macro_rules! profile_span {
    ($self:ident, $phase:literal) => {
        $self.profiler.as_ref().map(|p| p.span($phase))
    };
}

/// Companion to `profile_span!`: time an expression that requires
/// `&mut self`.
///
/// `profile_span!` holds an immutable borrow on `self.profiler` for
/// the whole span's lifetime and so is incompatible with calling
/// `&mut self` methods inside the measured scope.  `profile_call!`
/// instead captures the start time, evaluates the expression (which
/// is free to take a mutable borrow), and records the elapsed time
/// against the profiler only after the expression completes — by
/// which point the mutable borrow has already been released.
///
/// Usage:
///
/// ```ignore
/// let r = profile_call!(self, "ask.sine_select",
///     self.sine_select_for_query(query_kif, params));
/// ```
macro_rules! profile_call {
    ($self:ident, $phase:literal, $e:expr) => {{
        let __t = std::time::Instant::now();
        let __r = $e;
        if let Some(p) = $self.profiler.as_ref() {
            p.record($phase, __t.elapsed());
        }
        __r
    }};
}

#[cfg(feature = "ask")]
mod prove;
mod export;
pub mod man;

// -- Feature-gated KB config types --------------------------------------------

#[cfg(feature = "cnf")]
pub struct ClausifyOptions {
    pub max_clauses_per_formula: usize,
}

#[cfg(feature = "cnf")]
impl Default for ClausifyOptions {
    fn default() -> Self { Self { max_clauses_per_formula: 1000 } }
}

#[cfg(feature = "cnf")]
#[derive(Debug, Default)]
pub struct ClausifyReport {
    pub clausified:      usize,
    pub skipped:         usize,
    pub exceeded_limit:  Vec<SentenceId>,
}

// -- FileDiff -----------------------------------------------------------------

/// Incremental-reload input for a single file.
///
/// Describes the delta between the KB's current view of `file` and
/// the new source text: which existing sentences survive (with their
/// updated spans), which are gone, and which fresh AST nodes need to
/// be built into new root sentences.
///
/// Produced by `compute_file_diff` (or directly by any consumer that
/// already tracks per-file fingerprints).  Consumed by
/// [`KnowledgeBase::apply_file_diff`].
///
/// Entirely general-purpose: used by LSP didChange handling, file
/// watcher CLIs, and hot-reload test harnesses with no type
/// differences.
#[derive(Debug, Clone, Default)]
pub struct FileDiff {
    /// The `Sentence.file` tag this diff applies to.
    pub file:     String,
    /// Sentence ids whose body is unchanged; only the span moves.
    pub retained: Vec<(SentenceId, Span)>,
    /// Sentence ids that no longer exist in the new source.
    pub removed:  Vec<SentenceId>,
    /// Fresh AST nodes to intern as new root sentences.  Positionally
    /// aligned with `added_hashes` / `added_spans` when produced by
    /// `compute_file_diff`; the `apply_file_diff` path doesn't require
    /// the auxiliary vectors.
    pub added:    Vec<AstNode>,
}

/// Compute a sentence-level diff for `file` given its new
/// per-root-sentence fingerprint list + AST nodes + spans.
///
/// Uses a positional-greedy match: walks `new_hashes` in source order
/// and, for each hash, pops a matching old sid off a per-hash bucket
/// if one exists.  Duplicate sentences (same hash) preserve their
/// ids in source-order pairing; the first new duplicate pairs with
/// the first old duplicate, second with second, etc.  Anything left
/// over on the old side becomes `removed`; anything left over on the
/// new side becomes `added`.
///
/// Callers that don't need AST preservation (e.g. consumers that
/// plan to rebuild from scratch anyway) can pass `new_ast = &[]`
/// and ignore the `added` field.
pub fn compute_file_diff(
    file:        &str,
    old_sids:    &[SentenceId],
    old_hashes:  &[u64],
    new_hashes:  &[u64],
    new_ast:     &[AstNode],
    new_spans:   &[Span],
) -> FileDiff {
    debug_assert_eq!(old_sids.len(),    old_hashes.len(),
                     "old_sids and old_hashes must be positionally aligned");
    debug_assert_eq!(new_hashes.len(),  new_spans.len(),
                     "new_hashes and new_spans must be positionally aligned");
    debug_assert!(new_ast.is_empty() || new_ast.len() == new_hashes.len(),
                  "new_ast, when provided, must be positionally aligned with new_hashes");

    // Bucket old sids by hash, preserving source order for duplicates.
    let mut buckets: HashMap<u64, std::collections::VecDeque<SentenceId>> = HashMap::new();
    for (sid, &h) in old_sids.iter().zip(old_hashes) {
        buckets.entry(h).or_default().push_back(*sid);
    }

    let mut retained = Vec::with_capacity(new_hashes.len().min(old_sids.len()));
    let mut added: Vec<AstNode> = Vec::new();

    for (i, &h) in new_hashes.iter().enumerate() {
        match buckets.get_mut(&h).and_then(|b| b.pop_front()) {
            Some(sid) => {
                retained.push((sid, new_spans[i].clone()));
            }
            None => {
                if !new_ast.is_empty() {
                    added.push(new_ast[i].clone());
                }
            }
        }
    }

    // Anything still in the buckets is gone.
    let mut removed: Vec<SentenceId> = buckets.into_values().flatten().collect();
    removed.sort_unstable();  // deterministic for testing

    FileDiff { file: file.to_owned(), retained, removed, added }
}

// -- KnowledgeBase -------------------------------------------------------------
/// The base structure defining a knowledge base
pub struct KnowledgeBase {
    /// Wrapped KifStore + semantic cache.
    layer: SemanticLayer,

    /// In-memory session assertions: session name -> Vec<SentenceId>.
    /// Sentences here have NOT been promoted to axioms yet.
    sessions: HashMap<String, Vec<SentenceId>>,

    /// Deduplication table: formula-hash -> (SentenceId, session).
    /// session=None means promoted axiom; Some(s) means assertion in session s.
    ///
    /// Populated from `DB_FORMULA_HASHES` on open and from fresh
    /// clausifications in `tell` / `load_kif` / `promote_*`.  Gated on
    /// `cnf`: without that feature no clausifier is linked, so there
    /// is no way to compute the canonical hash and duplicate axioms are
    /// accepted silently.
    #[cfg(feature = "cnf")]
    fingerprints: HashMap<u64, (SentenceId, Option<String>)>,

    /// CNF side-car: pre-computed clauses per sentence.  Populated by
    /// the ingestion path and drained at promote time into the LMDB
    /// clause-dedup tables.
    #[cfg(feature = "cnf")]
    clauses: HashMap<SentenceId, Vec<Clause>>,

    #[cfg(feature = "cnf")]
    cnf_mode: bool,

    #[cfg(feature = "cnf")]
    cnf_opts: ClausifyOptions,

    /// LMDB handle. None = purely in-memory.
    #[cfg(feature = "persist")]
    db: Option<LmdbEnv>,

    /// Pre-built TFF TPTP for the current axiom set; None when invalidated.
    /// Rebuilt lazily on the first `ask_embedded()` call after the axiom
    /// set changes.  Not reused by `ask()` itself, which SInE-filters
    /// per-conjecture and thus cannot benefit from a whole-KB cache.
    #[cfg(feature = "ask")]
    axiom_cache: Option<crate::vampire::VampireAxiomCache>,

    /// Eagerly-maintained SInE axiom-selection index.
    ///
    /// Every axiom promotion incrementally updates this index (adding
    /// the new axiom and recomputing triggers for axioms that share a
    /// symbol with it).  `ask()` reads the index on every query to
    /// SInE-filter the axiom set it ships to the prover; no rebuild
    /// is done at query time (except on tolerance switch, which
    /// rebuilds just the D-relation in place).
    ///
    /// Gated on `ask` because SInE is only useful to consumers that
    /// actually call the prover.  Editor builds (no `ask` feature)
    /// pay no maintenance cost.
    #[cfg(feature = "ask")]
    sine_index: RwLock<SineIndex>,

    /// Optional per-phase profiler for ingest/promote/ask work.
    /// `None` unless the caller has explicitly installed one via
    /// [`set_profiler`].  When a profiler is installed, hot paths
    /// create RAII `ProfileSpan`s inside each instrumented block;
    /// when `feature = "profiling"` is off at build time those
    /// spans are zero-sized no-ops.
    ///
    /// The field itself is an `Option<Arc<...>>` so that the same
    /// profiler can be shared across multiple KB instances (useful
    /// for test harnesses that open and close KBs within one run).
    profiler: Option<std::sync::Arc<crate::profiling::Profiler>>,
}

impl KnowledgeBase {
    // -- Construction ----------------------------------------------------------
    /// Constructs a new KnowledgeBase
    pub fn new() -> Self {
        Self {
            layer:        SemanticLayer::new(KifStore::default()),
            sessions:     HashMap::new(),
            #[cfg(feature = "cnf")] fingerprints: HashMap::new(),
            #[cfg(feature = "cnf")] clauses:  HashMap::new(),
            #[cfg(feature = "cnf")] cnf_mode: true,
            #[cfg(feature = "cnf")] cnf_opts: ClausifyOptions::default(),
            #[cfg(feature = "persist")] db:   None,
            #[cfg(feature = "ask")]  axiom_cache: None,
            #[cfg(feature = "ask")]
            sine_index: RwLock::new(
                SineIndex::new(SineParams::default().tolerance)
            ),
            profiler: None,
        }
    }

    // -- Profiler hook --------------------------------------------------------

    /// Install a per-phase profiler.  Every subsequent instrumented
    /// call site on this `KnowledgeBase` (ingest, promote, ask, load)
    /// records its duration onto the given profiler.  Call
    /// [`Profiler::report`] on the handle to get a formatted
    /// breakdown.
    ///
    /// Pass `None`-valued setters aren't exposed — to detach,
    /// construct a fresh `KnowledgeBase` or create a new profiler
    /// and drop the old handle.
    ///
    /// No-op when built without `feature = "profiling"` (the spans
    /// don't record), but still safe to call — the profiler handle
    /// is kept for uniform API.
    pub fn set_profiler(&mut self, profiler: std::sync::Arc<crate::profiling::Profiler>) {
        self.profiler = Some(profiler);
    }

    /// The currently-installed profiler, if any.
    pub fn profiler(&self) -> Option<&std::sync::Arc<crate::profiling::Profiler>> {
        self.profiler.as_ref()
    }

    // Phase-span macro is declared at module level (`profile_span!`)
    // because a `fn span(&self, ...)` method would borrow all of
    // `self`, making it impossible to mutate any other field while
    // the returned guard is alive.  The macro inlines direct field
    // access to `self.profiler`, giving the borrow checker enough
    // information to see that the span only borrows that one field.

    #[cfg(feature = "persist")]
    /// Opens the knowledge base from a persistent storage (LMDB) path.
    ///
    /// With the `cnf` feature on, the in-memory `fingerprints` dedup
    /// map is rehydrated from the `formula_hashes` LMDB table -- each
    /// key is a formula hash and each value is the owning `SentenceId`.
    /// Without `cnf`, no dedup map is built.
    pub fn open(path: &std::path::Path) -> Result<Self, KbError> {
        // Open the LMDB path
        let env = LmdbEnv::open(path)?;
        // Load the kifstore from the saved database
        let (store, session_map) = load_from_db(&env)?;

        // -- Rehydrate fingerprints from DB_FORMULA_HASHES ---------------
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

        // -- Phase D: try to restore the taxonomy cache ---------------
        //
        // The cache only applies when its `kb_version` matches the
        // current counter.  On mismatch (or absence), we fall back to
        // `SemanticLayer::new`, which does the full `rebuild_taxonomy`
        // scan as before.  Either way the result is correct; Phase D
        // just skips the scan when the cache is valid.
        let mut layer = {
            let rtxn = env.read_txn()?;
            let current_version = env.kb_version(&rtxn)?;
            let cached: Option<crate::persist::CachedTaxonomy> =
                env.get_cache(&rtxn, crate::persist::CACHE_KEY_TAXONOMY)?;
            drop(rtxn);

            match cached {
                Some(tx) if tx.kb_version == current_version => {
                    log::info!(target: "sumo_kb::kb",
                        "Phase D: restored taxonomy cache (kb_version={}, {} edges)",
                        tx.kb_version, tx.tax_edges.len());
                    SemanticLayer::from_cached_taxonomy(
                        store,
                        tx.tax_edges,
                        tx.numeric_sort_cache,
                        tx.numeric_ancestor_set,
                        tx.poly_variant_symbols,
                        tx.numeric_char_cache,
                    )
                }
                Some(tx) => {
                    log::info!(target: "sumo_kb::kb",
                        "Phase D: taxonomy cache stale (cache kb_version={}, current={}); \
                         rebuilding", tx.kb_version, current_version);
                    SemanticLayer::new(store)
                }
                None => {
                    // First open or cache never written -- do the
                    // normal full-rebuild path.
                    SemanticLayer::new(store)
                }
            }
        };

        // -- Phase D: restore SortAnnotations if cached ---------------
        #[cfg(feature = "ask")]
        {
            let rtxn = env.read_txn()?;
            let current_version = env.kb_version(&rtxn)?;
            let cached: Option<crate::persist::CachedSortAnnotations> =
                env.get_cache(&rtxn, crate::persist::CACHE_KEY_SORT_ANNOT)?;
            if let Some(sa) = cached {
                if sa.kb_version == current_version {
                    layer.install_sort_annotations(sa.sorts);
                    log::info!(target: "sumo_kb::kb",
                        "Phase D: restored sort_annotations cache (kb_version={})",
                        sa.kb_version);
                } else {
                    log::info!(target: "sumo_kb::kb",
                        "Phase D: sort_annotations cache stale ({}/{}); will rebuild on first access",
                        sa.kb_version, current_version);
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
                log::info!(target: "sumo_kb::kb",
                    "Phase D: auto-backfilling cnf tables for {} axioms",
                    layer.store.roots.len());
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
        log::info!(target: "sumo_kb::kb", "opened KB from {:?}: {} formulas fingerprinted",
            path, fingerprints.len());
        #[cfg(not(feature = "cnf"))]
        log::info!(target: "sumo_kb::kb", "opened KB from {:?} (no-dedup build)", path);

        // Populate `Symbol.all_sentences` for every loaded axiom (every
        // root that is NOT a session assertion).  This is the live
        // generality source for SInE; maintained per-promotion afterwards.
        let axiom_sids: Vec<SentenceId> = layer.store.roots.iter()
            .copied()
            .filter(|sid| !session_sids.contains(sid))
            .collect();
        for &sid in &axiom_sids {
            layer.store.register_axiom_symbols(sid);
        }

        // Eagerly build the SInE index over the loaded axioms.
        #[cfg(feature = "ask")]
        let sine_index = {
            let mut idx = SineIndex::new(SineParams::default().tolerance);
            idx.add_axioms(&layer.store, axiom_sids.iter().copied());
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
            #[cfg(feature = "ask")]  sine_index,
            profiler: None,
        })
    }

    // -- Ingestion -------------------------------------------------------------

    /// Assert a single KIF string into a named session.
    ///
    /// Each sentence is semantically validated before acceptance; warnings are
    /// returned in [`TellResult::warnings`] and errors in [`TellResult::errors`].
    pub fn tell(&mut self, session: &str, kif: &str) -> TellResult {
        self.ingest(kif, session, session, true)
    }

    /// Load a KIF file into the KB.  If `session` is `None`, the `file` name
    /// is used as the session key.
    ///
    /// Per-sentence validation is deliberately skipped to avoid false positives
    /// from forward-references within a file or across files.  Call
    /// [`validate_all`] explicitly after loading all files to get the full set
    /// of warnings with complete KB context.
    pub fn load_kif(&mut self, text: &str, file: &str, session: Option<&str>) -> TellResult {
        let session_key = session.unwrap_or(file);
        self.ingest(text, file, session_key, false)
    }

    /// Core ingestion: parse `text` with file tag `file_tag`, add accepted sentences to `session`.
    ///
    /// `validate`: if `true`, run per-sentence semantic validation (used by `tell`).
    ///             if `false`, skip validation (used by `load_kif` for bulk loading).
    fn ingest(&mut self, text: &str, file_tag: &str, session: &str, validate: bool) -> TellResult {
        // No top-level `ingest.total` span here: it would hold an
        // immutable borrow on `self.profiler` across the many
        // `&mut self` mutations below.  Per-phase spans cover
        // everything of interest; the profiler's grand-total line
        // aggregates sibling phases within the [ingest] bucket.
        // Set up the result to return
        let mut result = TellResult { ok: true, errors: vec![], warnings: vec![] };

        // Snapshot root count before loading so we only process truly new roots.
        let prev_root_count = self.layer.store.file_roots
            .get(file_tag)
            .map(|v| v.len())
            .unwrap_or(0);

        // Phase B: we NO LONGER preemptively invalidate caches before
        // parsing.  Parsing only adds sentences to the store; none of
        // the existing cache entries become stale *because* of a parse.
        // Whether the cache is actually affected depends on which
        // sentences survived validation + dedup and made it into the
        // store as accepted axioms -- we handle that below via
        // `extend_taxonomy_with(&accepted)`.
        //
        // Parse into store using file_tag as the KIF "file" name.
        let parse_errors = {
            let _span = profile_span!(self, "ingest.parse_and_store");
            load_kif(&mut self.layer.store, text, file_tag)
        };

        // Failed to ingest due to parse errors
        if !parse_errors.is_empty() {
            result.ok = false;
            for (_, e) in parse_errors {
                result.errors.push(e);
            }
            return result;
        }

        // Collect only roots added by THIS call (file_roots accumulates across calls).
        let new_roots: Vec<SentenceId> = self.layer.store.file_roots
            .get(file_tag)
            .map(|v| v[prev_root_count..].to_vec())
            .unwrap_or_default();

        // -- Optional semantic validation (serial; cheap per sentence) -----
        if validate {
            let _span = profile_span!(self, "ingest.semantic_validate");
            for &sid in &new_roots {
                if let Err(e) = self.layer.validate_sentence(sid) {
                    result.warnings.push(TellWarning::Semantic(e));
                }
            }
        }

        // -- Dedup via clause-level formula hash (cnf feature) -------------
        //
        // In the cnf-on build we clausify each candidate sentence,
        // derive a canonical-hash-set-based formula fingerprint, and
        // probe the in-memory `fingerprints` table.  The clauses stay
        // in the side-car so `promote_*` doesn't have to re-clausify.
        //
        // In cnf-off builds no dedup runs; every syntactically-fresh
        // root sentence is accepted.
        //
        // Clausification is the single most expensive phase during
        // bootstrap (~47 µs per sentence × 15k axioms ≈ 750 ms on
        // SUMO).  When `parallel` is on, the pure IR-building step
        // `clausify_ir(&SemanticLayer, sid)` runs across sids with
        // `rayon::par_iter`.  The follow-up translation +
        // canonical-hash + fingerprint-insert steps mutate the store
        // and must run serially, so we split the work into:
        //   1. parallel clausify → Vec<(sid, Result<Vec<ir::Clause>>)>
        //   2. serial merge: translate, hash, dedup, insert
        //
        // Preserving `new_roots` order through `par_iter().map().collect()`
        // keeps intra-batch dedup deterministic.
        let mut accepted: Vec<SentenceId> = Vec::new();

        #[cfg(feature = "cnf")]
        {
            use crate::canonical::{canonical_clause_hash, formula_hash_from_clauses};

            // Phase 1: clausify each sid into `Vec<ir::Clause>`.
            //
            // Note on parallelism.  `clausify_ir` takes `&SemanticLayer`
            // and doesn't touch the store's interning tables, so the
            // Rust-side work is in principle thread-safe.  However
            // `vampire-prover` protects every Vampire C++ call with
            // a single global `Mutex` (see `vampire/src/lock.rs`),
            // and `Problem::clausify` calls into that serialized
            // section.  Empirically, running this with
            // `rayon::par_iter` on SUMO-scale inputs is ~30% slower
            // than the serial version: the rayon scheduler + mutex
            // contention overhead dominates any parallelism benefit,
            // since the C++ clausifier is the hot inner loop and
            // that's exactly the part that must serialize.
            //
            // We keep the two-phase structure (clausify → translate)
            // because it's cleaner than the interleaved original,
            // but we always iterate serially.  Re-enable parallelism
            // here only if vampire-prover's global mutex is split
            // into finer-grained locks (or if the hot Vampire call
            // moves out of the critical section).
            //
            // `KbError` contains a `Box<dyn ParseError>` which isn't
            // `Send`; we still convert to `String` here so a future
            // switch back to `par_iter` needs no further refactor.
            let ir_results: Vec<(SentenceId, Result<Vec<vampire_prover::ir::Clause>, String>)> = {
                let _span = profile_span!(self, "ingest.clausify_ir");
                new_roots.iter().map(|&sid| {
                    (sid, crate::cnf::clausify_ir(&self.layer, sid).map_err(|e| e.to_string()))
                }).collect()
            };

            // Phase 2: serial translate + hash + dedup + side-car insert.
            for (sid, ir_result) in ir_results {
                let fh_clauses: Option<(u64, Vec<Clause>)> = match ir_result {
                    Ok(ir_cs) => {
                        let clauses = {
                            let _span = profile_span!(self, "ingest.translate_and_hash");
                            crate::cnf::translate_ir_clauses(&mut self.layer.store, &ir_cs)
                        };
                        let hashes: Vec<u64> = clauses.iter()
                            .map(canonical_clause_hash)
                            .collect();
                        let fh = formula_hash_from_clauses(&hashes);
                        Some((fh, clauses))
                    }
                    Err(e) => {
                        log::warn!(target: "sumo_kb::kb",
                            "ingest: clausify failed for sid={}: {}", sid, e);
                        None
                    }
                };

                let _span_dedup = profile_span!(self, "ingest.dedup_check");
                let duplicate = match fh_clauses {
                    Some((fh, clauses)) => {
                        if let Some((existing_id, existing_session)) =
                            self.fingerprints.get(&fh).cloned()
                        {
                            let preview = self.formula_preview(existing_id);
                            match existing_session {
                                None => {
                                    result.warnings.push(TellWarning::DuplicateAxiom {
                                        existing_id,
                                        formula_preview: preview,
                                    });
                                }
                                Some(s) => {
                                    result.warnings.push(TellWarning::DuplicateAssertion {
                                        existing_id,
                                        existing_session: s,
                                        formula_preview: preview,
                                    });
                                }
                            }
                            true
                        } else {
                            // Accept and register.
                            self.fingerprints.insert(fh, (sid, Some(session.to_owned())));
                            self.clauses.insert(sid, clauses);
                            false
                        }
                    }
                    None => {
                        // Clausification failed; accept without dedup so
                        // the sentence isn't lost.
                        false
                    }
                };
                if !duplicate {
                    accepted.push(sid);
                    log::debug!(target: "sumo_kb::kb",
                        "tell: accepted sid={} into session '{}'", sid, session);
                } else {
                    log::warn!(target: "sumo_kb::kb",
                        "tell: duplicate sid={} skipped (session '{}')", sid, session);
                }
            }
        }

        #[cfg(not(feature = "cnf"))]
        {
            let _ = session;
            // No dedup without cnf: every parsed root is accepted.
            accepted.extend(new_roots.iter().copied());
        }

        self.sessions.entry(session.to_owned()).or_default().extend(&accepted);

        // Phase B + C: incremental taxonomy extension + targeted cache
        // invalidation.  When the batch contains no taxonomy-relevant
        // sentences (the common case for most SUMO axioms), this is
        // essentially free -- no scans, no invalidations.
        {
            let _span = profile_span!(self, "ingest.taxonomy_extend");
            self.layer.extend_taxonomy_with(&accepted);
        }

        log::info!(target: "sumo_kb::kb",
            "tell: session='{}' accepted={} warnings={}", session, accepted.len(), result.warnings.len());
        result
    }

    /// Clausify `sid`, derive the canonical formula hash, and return the
    /// hash alongside the (cached) clause list.  Returns `None` if
    /// clausification failed -- the caller should treat that as "skip
    /// dedup, accept the sentence".
    ///
    /// The clause list is `Vec<Clause>` so callers that accept the
    /// sentence can stash it in `self.clauses` without recomputing it
    /// later at promote time.
    #[cfg(feature = "cnf")]
    fn compute_formula_hash(&mut self, sid: SentenceId) -> Option<(u64, Vec<Clause>)> {
        use crate::canonical::{canonical_clause_hash, formula_hash_from_clauses};

        // `cnf::sentence_to_clauses` borrows the semantic layer mutably to
        // intern new skolem/wrapper symbols into the KifStore.
        let clauses = match crate::cnf::sentence_to_clauses(&mut self.layer, sid) {
            Ok(cs)  => cs,
            Err(e)  => {
                log::warn!(target: "sumo_kb::kb",
                    "compute_formula_hash: sid={} clausify failed: {}", sid, e);
                return None;
            }
        };
        let canonical: Vec<u64> = clauses
            .iter()
            .map(canonical_clause_hash)
            .collect();
        let fh = formula_hash_from_clauses(&canonical);
        Some((fh, clauses))
    }

    /// Mark all assertions in `session` as permanent axioms without semantic
    /// validation or LMDB writes.
    ///
    /// After this call the sentences appear in [`ask`]'s axiom set (TPTP role
    /// `axiom`).  This is the right operation for in-memory KBs where the full
    /// KB content should be available to the prover without a prior
    /// `promote_assertions_unchecked` round-trip through LMDB.
    ///
    /// ## Why we don't clausify here
    ///
    /// A tempting refactor is: "let `tell()` just store sentences
    /// without clausifying, and batch-clausify them all here under the
    /// assumption that batching Vampire's clausifier is faster than
    /// 15k per-sentence calls."  Don't.  Vampire's `clausify()` takes
    /// a whole problem and returns a **flat, interleaved** list of
    /// clauses with no per-input-unit attribution.  We need per-sid
    /// clauses because `StoredFormula.clause_ids` must map each
    /// formula to its own ClauseIds and the formula-level hash is
    /// `xxh64(sorted(canonical_hashes_of_that_sid's_clauses))`.
    /// Batching would require either a custom C-shim that
    /// sid-tags each input unit, or post-hoc clause partitioning
    /// based on a synthetic predicate injected into every sentence.
    /// Both are ~200-line shim extensions for a ~300 ms one-time
    /// bulk-load win and would defer duplicate-detection feedback
    /// from tell-time to promote-time -- a real DX regression.
    ///
    /// So: clausify per-tell (fast feedback, simple code), and in
    /// this function just retag the fingerprint entries from
    /// `Some(session)` to `None`.  That makes this call O(|fingerprints|)
    /// and avoids any clausification work at all.
    pub fn make_session_axiomatic(&mut self, session: &str) {
        // Per-phase spans below; no outer `promote.total` since it
        // would conflict with the inner `&mut self` accesses.
        let sids  = self.sessions.remove(session).unwrap_or_default();
        let count = sids.len();

        // Flip each sentence's fingerprint entry from session-tagged
        // to axiom (session=None).  The `tell()` path already
        // clausified these sentences and registered
        // (formula_hash -> (sid, Some(session))) in `self.fingerprints`;
        // all we need to do here is retag those entries.  Earlier
        // revisions re-clausified every sid in this loop, paying the
        // full dedup cost a second time; on a 15k-axiom KB that was a
        // ~1.7 s tax per `make_session_axiomatic` call.
        //
        // Walk `self.fingerprints` once and retag in place.
        #[cfg(feature = "cnf")]
        {
            let _span = profile_span!(self, "promote.fingerprint_retag");
            use std::collections::HashSet;
            let sid_set: HashSet<SentenceId> = sids.iter().copied().collect();
            for (_, (sid, s)) in self.fingerprints.iter_mut() {
                if sid_set.contains(sid) && s.as_deref() == Some(session) {
                    *s = None;
                }
            }
        }

        // Populate the axiom-occurrence index for each newly-promoted
        // axiom.  Must happen before the SInE update so both indexes
        // reflect the same axiom set.
        {
            let _span = profile_span!(self, "promote.all_sentences_register");
            for &sid in &sids {
                self.layer.store.register_axiom_symbols(sid);
            }
        }

        // Eagerly extend the SInE index with each new axiom.  Work per
        // axiom is proportional to the number of axioms sharing a
        // symbol with it (typically dozens to low hundreds on SUMO-scale
        // KBs); in exchange the query-time `ask()` path does zero
        // rebuild work.
        #[cfg(feature = "ask")]
        {
            let _span = profile_span!(self, "promote.sine_maintain");
            let mut idx = self.sine_index.write().expect("sine_index poisoned");
            idx.add_axioms(&self.layer.store, sids.iter().copied());
        }

        log::info!(target: "sumo_kb::kb",
            "make_session_axiomatic: {} sentence(s) from session '{}' promoted to axioms",
            count, session);
        #[cfg(feature = "ask")]
        { self.axiom_cache = None; }
    }

    // -- Session management ----------------------------------------------------

    /// Discard all assertions in `session` (removes from store and fingerprints).
    pub fn flush_session(&mut self, session: &str) {
        let sids = self.sessions.remove(session).unwrap_or_default();
        if sids.is_empty() { return; }

        // Drop the in-memory fingerprint entries belonging to this
        // session.  No-op in cnf-off builds (no fingerprints table).
        #[cfg(feature = "cnf")]
        self.fingerprints.retain(|_, (_, s)| s.as_deref() != Some(session));

        // Remove sentences from KifStore.
        self.layer.store.remove_file(session);
        self.layer.rebuild_taxonomy();
        self.layer.invalidate_cache();

        #[cfg(feature = "cnf")]
        for sid in &sids { self.clauses.remove(sid); }

        log::info!(target: "sumo_kb::kb",
            "flush_session: removed {} assertion(s) from session '{}'", sids.len(), session);
    }

    /// Discard all in-memory session assertions.
    pub fn flush_assertions(&mut self) {
        let sessions: Vec<String> = self.sessions.keys().cloned().collect();
        for s in sessions { self.flush_session(&s); }
    }

    // -- Promotion -------------------------------------------------------------

    /// Promote all assertions in `session` to axioms WITHOUT a consistency check.
    /// Requires `persist` feature (writes to LMDB).
    #[cfg(feature = "persist")]
    pub fn promote_assertions_unchecked(
        &mut self,
        session: &str,
    ) -> Result<PromoteReport, KbError> {
        log::info!(target: "sumo_kb::kb",
            "promote_assertions_unchecked: session='{}'", session);

        let mut report = PromoteReport::default();
        let session_sids: Vec<SentenceId> = self.sessions
            .get(session)
            .cloned()
            .unwrap_or_default();

        if session_sids.is_empty() {
            log::info!(target: "sumo_kb::kb", "promote: session '{}' empty, nothing to do", session);
            return Ok(report);
        }

        // -- Step 1: Cross-session dedup (cnf feature) -------------------
        //
        // With `cnf` on, we consult `fingerprints` -- populated at
        // tell/load_kif time and at open() rehydration -- and skip any
        // sentence whose formula hash is already attached to an axiom
        // or a different session.  With `cnf` off there is no dedup
        // map, so every session sentence proceeds to promotion.
        let mut surviving: Vec<SentenceId> = Vec::new();
        #[cfg(feature = "cnf")]
        {
            for &sid in &session_sids {
                // The tell/load_kif path has already populated the
                // formula hash; look it up in the session's clause cache.
                // If the sentence was added without going through tell
                // (unusual), recompute on the fly.
                let fh_opt = match self.clauses.get(&sid) {
                    Some(cs) => {
                        let canonical: Vec<u64> = cs.iter()
                            .map(crate::canonical::canonical_clause_hash)
                            .collect();
                        Some(crate::canonical::formula_hash_from_clauses(&canonical))
                    }
                    None => self.compute_formula_hash(sid).map(|(h, cs)| {
                        self.clauses.insert(sid, cs);
                        h
                    }),
                };
                let Some(fh) = fh_opt else {
                    // Clausification failed; accept without dedup.
                    surviving.push(sid);
                    continue;
                };

                let entry = self.fingerprints.get(&fh).cloned();
                let is_dup = match &entry {
                    Some((_, None))                       => true,  // existing axiom
                    Some((_, Some(s))) if s != session    => true,  // other session
                    _                                      => false, // same session or absent
                };

                if is_dup {
                    if let Some((dup_of, dup_session)) = entry {
                        let preview = self.formula_preview(sid);
                        report.duplicates_removed.push(DuplicateInfo {
                            sentence_id:     sid,
                            duplicate_of:    dup_of,
                            source:          match dup_session {
                                None    => DuplicateSource::Axiom,
                                Some(s) => DuplicateSource::Session(s),
                            },
                            formula_preview: preview,
                        });
                    }
                } else {
                    surviving.push(sid);
                }
            }
        }
        #[cfg(not(feature = "cnf"))]
        { surviving.extend(&session_sids); }

        log::debug!(target: "sumo_kb::kb",
            "promote: {} surviving after dedup ({} duplicates removed)",
            surviving.len(), report.duplicates_removed.len());

        if surviving.is_empty() {
            self.sessions.remove(session);
            return Ok(report);
        }

        // -- Step 2: Semantic validation ---------------------------------------
        let sem_errors: Vec<(SentenceId, SemanticError)> = surviving.iter()
            .filter_map(|&sid| self.layer.validate_sentence(sid).err().map(|e| (sid, e)))
            .collect();
        if !sem_errors.is_empty() {
            let count = sem_errors.len();
            log::warn!(target: "sumo_kb::kb",
                "promote: {} semantic error(s) in session '{}'", count, session);
            return Err(KbError::Semantic(sem_errors.into_iter().next().unwrap().1));
        }

        // -- Step 3: Clausify (cnf feature) -----------------------------------
        //
        // Reuse the side-car clauses populated at tell time; fall back
        // to clausifying on demand for anything that wasn't cached.
        #[cfg(feature = "cnf")]
        let clause_map: HashMap<SentenceId, Vec<Clause>> = {
            if self.cnf_mode {
                let mut map = HashMap::new();
                for &sid in &surviving {
                    if let Some(cs) = self.clauses.get(&sid).cloned() {
                        map.insert(sid, cs);
                    } else if let Some((_h, cs)) = self.compute_formula_hash(sid) {
                        map.insert(sid, cs);
                    }
                }
                map
            } else {
                HashMap::new()
            }
        };

        // -- Step 4: Write to LMDB ---------------------------------------------
        // Promoted sentences become axioms (session=None) in the DB.
        if let Some(env) = &self.db {
            write_axioms(
                env,
                &self.layer.store,
                &surviving,
                #[cfg(feature = "cnf")] &clause_map,
                None,
            )?;

            // Phase D: persist the derived semantic caches alongside
            // the axiom set.  `write_axioms` already bumped
            // `kb_version`, so the blobs we write next carry the
            // current counter and will be accepted by the next open.
            //
            // Each helper opens its own txn.  Failures are logged but
            // not propagated -- the caches are a performance hint;
            // losing them just means the next cold open rebuilds.
            if let Err(e) = crate::persist::persist_taxonomy_cache(env, &self.layer) {
                log::warn!(target: "sumo_kb::kb",
                    "Phase D: taxonomy cache persist failed: {}", e);
            }
            #[cfg(feature = "ask")]
            if let Err(e) = crate::persist::persist_sort_annotations_cache(env, &self.layer) {
                log::warn!(target: "sumo_kb::kb",
                    "Phase D: sort_annotations cache persist failed: {}", e);
            }
        }

        // -- Step 5: Update fingerprints to axiom (session=None) ---------------
        #[cfg(feature = "cnf")]
        {
            for &sid in &surviving {
                if let Some(cs) = clause_map.get(&sid) {
                    let canonical: Vec<u64> = cs.iter()
                        .map(crate::canonical::canonical_clause_hash)
                        .collect();
                    let fh = crate::canonical::formula_hash_from_clauses(&canonical);
                    self.fingerprints.insert(fh, (sid, None));
                }
            }
        }

        // -- Step 6: Store CNF clauses -----------------------------------------
        #[cfg(feature = "cnf")]
        self.clauses.extend(clause_map);

        // -- Step 7: Detach from session ---------------------------------------
        self.sessions.remove(session);
        self.layer.store.clear_file_roots(session);
        // Note: sentences remain in store.roots as promoted axioms.

        // -- Step 8: Populate axiom-occurrence index + SInE --------------------
        for &sid in &surviving {
            self.layer.store.register_axiom_symbols(sid);
        }
        #[cfg(feature = "ask")]
        {
            let mut idx = self.sine_index.write().expect("sine_index poisoned");
            idx.add_axioms(&self.layer.store, surviving.iter().copied());
        }

        report.promoted = surviving;
        log::info!(target: "sumo_kb::kb",
            "promote: {} sentence(s) promoted from session '{}'",
            report.promoted.len(), session);
        #[cfg(feature = "ask")]
        { self.axiom_cache = None; }
        Ok(report)
    }

    /// Promote assertions WITH a consistency check via the theorem prover.
    /// Requires both `persist` and `ask` features.
    #[cfg(all(feature = "persist", feature = "ask"))]
    pub fn promote_assertions(
        &mut self,
        session: &str,
        runner: &dyn ProverRunner,
    ) -> Result<PromoteReport, PromoteError> {
        // First run the unchecked flow to get surviving sentences.
        // We need a staging approach: collect survivors, check consistency, then commit.
        let session_sids: Vec<SentenceId> = self.sessions
            .get(session)
            .cloned()
            .unwrap_or_default();

        if session_sids.is_empty() {
            return Ok(PromoteReport::default());
        }

        // Build TPTP: existing axioms + session assertions + $false as conjecture.
        use crate::vampire::assemble::{assemble_tptp, AssemblyOpts};
        use crate::vampire::converter::{Mode, NativeConverter};

        let mut conv = NativeConverter::new(&self.layer.store, &self.layer, Mode::Fof);
        let mut axioms_sorted: Vec<SentenceId> =
            self.axiom_ids_set().into_iter().collect();
        axioms_sorted.sort_unstable();
        for sid in axioms_sorted {
            conv.add_axiom(sid);
        }
        for &sid in &session_sids {
            conv.add_axiom(sid);
        }
        let (problem, sid_map) = conv.finish();
        let mut tptp = assemble_tptp(&problem, &sid_map, &AssemblyOpts::default());
        tptp.push_str("\nfof(check_consistency, conjecture, ($false)).\n");

        log::debug!(target: "sumo_kb::kb",
            "promote_assertions: consistency check TPTP size={} bytes", tptp.len());

        let prover_opts = ProverOpts {
            timeout_secs: 30,
            mode: ProverMode::CheckConsistency,
        };
        let prover_result = runner.prove(&tptp, &prover_opts);

        match prover_result.status {
            ProverStatus::Inconsistent => {
                return Err(PromoteError::Inconsistent {
                    session:     session.to_owned(),
                    explanation: prover_result.raw_output,
                    conflicting: session_sids,
                });
            }
            ProverStatus::Timeout | ProverStatus::Unknown => {
                return Err(PromoteError::ProverUncertain {
                    reason: format!("{:?}", std::mem::discriminant(&prover_result.status)),
                });
            }
            _ => {} // Consistent or other -> proceed
        }

        self.promote_assertions_unchecked(session)
            .map_err(PromoteError::Db)
    }

    // -- Semantic queries ------------------------------------------------------

    pub fn is_instance(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.is_instance(sym)
    }

    pub fn is_class(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.is_class(sym)
    }

    pub fn is_relation(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.is_relation(sym)
    }

    pub fn is_function(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.is_function(sym)
    }

    pub fn is_predicate(&self, sym: crate::types::SymbolId) -> bool {
        self.layer.is_predicate(sym)
    }

    pub fn has_ancestor(&self, sym: crate::types::SymbolId, ancestor: &str) -> bool {
        self.layer.has_ancestor_by_name(sym, ancestor)
    }

    pub fn symbol_id(&self, name: &str) -> Option<crate::types::SymbolId> {
        self.layer.store.sym_id(name)
    }

    /// Inverse of [`symbol_id`]: resolve a SymbolId to its interned
    /// name.  Returns an owned `String` to keep the lifetime simple.
    /// Ids that aren't in the store return `None`.
    pub fn sym_name(&self, id: crate::types::SymbolId) -> Option<String> {
        if self.layer.store.has_symbol(id) {
            Some(self.layer.store.sym_name(id).to_owned())
        } else {
            None
        }
    }

    /// Fetch a root or sub-sentence by id.  Returns `None` when
    /// `sid` isn't a known sentence (e.g. after `remove_sentence`
    /// the id is valid but the body is empty -- see [`has_sentence`]).
    pub fn sentence(&self, sid: SentenceId) -> Option<&crate::types::Sentence> {
        if !self.layer.store.has_sentence(sid) { return None; }
        Some(&self.layer.store.sentences[self.layer.store.sent_idx(sid)])
    }

    /// Find the innermost element at byte `offset` in `file`.
    ///
    /// Walks the file's root sentences and descends through sub-
    /// sentences; returns the deepest non-synthetic element whose
    /// span covers the offset.  Useful for hover, goto-definition,
    /// rename, and any other cursor-driven query.
    ///
    /// Returns `None` when `file` isn't loaded or when `offset`
    /// falls outside every root sentence's `(...)` range.
    pub fn element_at_offset(&self, file: &str, offset: usize) -> Option<crate::lookup::ElementHit> {
        crate::lookup::element_at_offset(&self.layer.store, file, offset)
    }

    /// Name of the symbol at `offset`, if the element there is a
    /// [`Element::Symbol`](crate::types::Element::Symbol).  Thin
    /// wrapper over [`element_at_offset`](Self::element_at_offset)
    /// + a type check.
    pub fn symbol_at_offset(&self, file: &str, offset: usize) -> Option<String> {
        crate::lookup::symbol_at_offset(&self.layer.store, file, offset)
    }

    /// Interned id for whatever symbol-like element is at `offset`,
    /// **including** `Element::Variable`.  For ordinary symbols the
    /// id is the intern-table entry; for variables it's the
    /// scope-qualified id (distinct `?X` instances in different
    /// quantifier bodies get distinct ids).
    ///
    /// Powers references / rename for variables: looking up the
    /// occurrence index by this id automatically gives back every
    /// co-bound occurrence inside the same scope and excludes
    /// same-named variables in other scopes.
    ///
    /// Returns `(id, display_name)` -- the display name is `"?X"` or
    /// `"@Row"` for variables, the plain interned name for symbols.
    pub fn id_at_offset(
        &self, file: &str, offset: usize,
    ) -> Option<(crate::types::SymbolId, String)> {
        let hit = self.element_at_offset(file, offset)?;
        let sent = self.sentence(hit.sid)?;
        match sent.elements.get(hit.idx)? {
            crate::types::Element::Symbol { id, .. } => {
                let name = self.layer.store.sym_name(*id).to_owned();
                Some((*id, name))
            }
            crate::types::Element::Variable { id, name, is_row, .. } => {
                let display = if *is_row { format!("@{}", name) } else { format!("?{}", name) };
                Some((*id, display))
            }
            _ => None,
        }
    }

    /// Every occurrence of `symbol` across every loaded file.
    ///
    /// Returned in insertion order (root sentences first by their
    /// load order, then sub-sentences within each).  Non-LSP
    /// consumers: a CLI "find references" command, coverage
    /// reporting, programmatic walks.  Returns an empty slice when
    /// the symbol is unknown or has no non-synthetic occurrences.
    pub fn occurrences(&self, symbol: &str) -> &[crate::types::Occurrence] {
        self.symbol_id(symbol)
            .map(|id| self.occurrences_of(id))
            .unwrap_or(&[])
    }

    /// Occurrences by raw `SymbolId`.  Useful when the caller has
    /// already done name lookup (variables' scope-qualified ids,
    /// cursor-driven queries that already produced a
    /// `KnowledgeBase::element_at_offset` hit).
    pub fn occurrences_of(&self, id: crate::types::SymbolId) -> &[crate::types::Occurrence] {
        self.layer.store.occurrences.get(&id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Iterate every interned symbol as `(SymbolId, name)` pairs.
    /// Powers workspace-symbol search (fuzzy "jump to any symbol
    /// in the KB"), dump utilities, and any consumer that needs
    /// the full symbol set.  Skolem symbols are included --
    /// callers that want to hide them can filter via
    /// [`symbol_is_skolem`](Self::symbol_is_skolem).
    ///
    /// Iteration order matches the intern table's hash-map order
    /// (i.e. arbitrary but stable within one KB instance).
    pub fn iter_symbols(&self) -> impl Iterator<Item = (crate::types::SymbolId, &str)> + '_ {
        self.layer.store.symbols.iter().map(|(name, &id)| (id, name.as_str()))
    }

    /// Iterate every distinct head-predicate name currently indexed
    /// in the store.  These are the relations / predicates /
    /// functions that *actually appear as sentence heads*, which is
    /// almost always what a completion menu at sentence-head
    /// position wants to suggest (anything declared but never used
    /// as a head isn't a useful completion target).
    ///
    /// Non-LSP uses: any tool presenting a menu of the KB's
    /// relation vocabulary -- CLI REPL completions, doc generators,
    /// summary reports.
    pub fn head_names(&self) -> impl Iterator<Item = &str> + '_ {
        self.layer.store.head_index.keys().map(|s| s.as_str())
    }

    /// Expected domain class for argument `arg_idx` (1-based) of
    /// relation `head`, or `None` when the relation has no explicit
    /// `(domain head arg_idx class)` axiom for this position.
    ///
    /// Completion and validation both want this: context-aware
    /// completion filters the candidate list by the expected class;
    /// arity / domain checks use the same data from a different
    /// angle.  The return is the declared class name (instance-of
    /// or subclass-of flag folded away) -- callers that care about
    /// the distinction (e.g. TFF sort derivation) use the lower-level
    /// `SemanticLayer::domain` path.
    pub fn expected_arg_class(&self, head: &str, arg_idx: usize) -> Option<String> {
        let head_id   = self.symbol_id(head)?;
        let domains   = self.layer.domain(head_id);
        // `arg_idx` is 1-based (element-index convention); `domains`
        // is 0-based.
        if arg_idx == 0 || arg_idx > domains.len() { return None; }
        let rd = &domains[arg_idx - 1];
        let class_id = rd.id();
        // Sentinel `u64::MAX` means "no explicit domain for this arg".
        if class_id == u64::MAX { return None; }
        self.sym_name(class_id)
    }

    /// True when `symbol` is a Skolem function introduced by the
    /// CNF clausifier.  Exposed so workspace-symbol search can
    /// filter these out by default.  O(1) -- name -> id -> Symbol
    /// via the intern table + `sym_idx`.
    pub fn symbol_is_skolem(&self, symbol: &str) -> bool {
        self.symbol_id(symbol)
            .and_then(|id| self.layer.store.symbol_of(id))
            .map(|s| s.is_skolem)
            .unwrap_or(false)
    }

    /// Defining sentence for `symbol`, by heuristic: the first
    /// `(subclass sym _)`, `(instance sym _)`, `(subrelation sym _)`,
    /// `(subAttribute sym _)`, or `(documentation sym _ _)`
    /// root sentence, in that priority order.  Returns the
    /// `(SentenceId, Span)` of that sentence so the caller can
    /// resolve the source location (e.g. LSP goto-definition).
    ///
    /// Falls back to any root where `symbol` appears as the head,
    /// then to any root where it appears at all.  `None` when the
    /// symbol has no declarations anywhere.
    pub fn defining_sentence(&self, symbol: &str) -> Option<(SentenceId, crate::error::Span)> {
        let sym_id  = self.symbol_id(symbol)?;
        let store   = &self.layer.store;

        // Priority 1: canonical declarations -- subclass / instance /
        // subrelation / subAttribute with this symbol as arg 1.
        const DECLARATIONS: &[&str] = &[
            "subclass", "instance", "subrelation", "subAttribute",
            "documentation",
        ];
        for &head in DECLARATIONS {
            for &sid in store.by_head(head) {
                let sent = &store.sentences[store.sent_idx(sid)];
                if matches!(
                    sent.elements.get(1),
                    Some(crate::types::Element::Symbol { id, .. }) if *id == sym_id
                ) {
                    if !sent.span.is_synthetic() {
                        return Some((sid, sent.span.clone()));
                    }
                }
            }
        }

        // Priority 2: any root where symbol is the head.  O(1)
        // id -> &Symbol via `symbol_of`.
        let sym_vec = store.symbol_of(sym_id)?;
        for &sid in &sym_vec.head_sentences {
            let sent = &store.sentences[store.sent_idx(sid)];
            if !sent.span.is_synthetic() {
                return Some((sid, sent.span.clone()));
            }
        }
        None
    }

    // -- Validation ------------------------------------------------------------

    pub fn validate_sentence(&self, sid: SentenceId) -> Result<(), SemanticError> {
        self.layer.validate_sentence(sid)
    }

    pub fn validate_all(&self) -> Vec<(SentenceId, SemanticError)> {
        self.layer.validate_all()
    }

    /// Validate only the sentences belonging to `session`.
    ///
    /// Use this after [`load_kif`] to perform end-of-load validation without
    /// re-validating the entire base KB.
    pub fn validate_session(&self, session: &str) -> Vec<(SentenceId, SemanticError)> {
        let sids = self.sessions.get(session).cloned().unwrap_or_default();
        sids.iter()
            .filter_map(|&sid| self.layer.validate_sentence(sid).err().map(|e| (sid, e)))
            .collect()
    }

    // -- TPTP output -----------------------------------------------------------
    //
    // `to_tptp`, `to_tptp_cnf`, `format_sentence_tptp`, and their helpers
    // live in `export.rs`.

    // -- CNF control -----------------------------------------------------------

    #[cfg(feature = "cnf")]
    pub fn enable_cnf(&mut self, opts: ClausifyOptions) {
        self.cnf_mode = true;
        self.cnf_opts = opts;
        log::debug!(target: "sumo_kb::kb", "CNF mode enabled");
    }

    #[cfg(feature = "cnf")]
    pub fn disable_cnf(&mut self) {
        self.cnf_mode = false;
        log::debug!(target: "sumo_kb::kb", "CNF mode disabled");
    }

    /// Clausify all current axioms and session assertions into the clauses side-car.
    ///
    /// In the Phase-5 pipeline the side-car is populated opportunistically
    /// by `tell` / `load_kif`, so this method is mostly idempotent --
    /// it forces re-clausification of any sentence that isn't already
    /// cached and reports the count.  Skolem symbols discovered by the
    /// Vampire clausifier are interned directly into the `KifStore` by
    /// `cnf::sentence_to_clauses`, so the method no longer needs an
    /// out-parameter for new symbols.
    #[cfg(feature = "cnf")]
    pub fn clausify(&mut self) -> Result<ClausifyReport, KbError> {
        let mut report = ClausifyReport::default();

        // Collect all SIDs to clausify (axioms + all session assertions).
        let axiom_ids = self.axiom_ids_set();
        let mut all_sids: Vec<SentenceId> = axiom_ids.into_iter().collect();
        for sids in self.sessions.values() { all_sids.extend(sids.iter().copied()); }

        for sid in all_sids {
            if self.clauses.contains_key(&sid) {
                report.clausified += 1;
                continue;
            }
            match crate::cnf::sentence_to_clauses(&mut self.layer, sid) {
                Ok(clauses) => {
                    self.clauses.insert(sid, clauses);
                    report.clausified += 1;
                }
                Err(e) => {
                    log::warn!(target: "sumo_kb::kb",
                        "clausify: sid={} failed: {}", sid, e);
                    report.exceeded_limit.push(sid);
                    report.skipped += 1;
                }
            }
        }

        log::info!(target: "sumo_kb::kb",
            "clausify: {} clausified, {} skipped",
            report.clausified, report.skipped);
        Ok(report)
    }

    // -- Theorem proving -------------------------------------------------------
    //
    // `ask`, `ask_embedded`, and their helpers (`query_affects_taxonomy`,
    // `ensure_axiom_cache`) live in `prove.rs`.

    // -- Additional helpers for embeddings (wasm, etc.) ------------------------

    /// Pattern-based sentence lookup (delegates to KifStore::lookup).
    pub fn lookup(&self, pattern: &str) -> Vec<SentenceId> {
        self.layer.store.lookup(pattern)
    }

    /// Return the SentenceIds for a given session (empty if session doesn't exist).
    pub fn session_sids(&self, session: &str) -> Vec<SentenceId> {
        self.sessions.get(session).cloned().unwrap_or_default()
    }

    /// Render a single sentence as a KIF string (for display).
    pub fn sentence_to_string(&self, sid: SentenceId) -> String {
        use crate::types::Element;
        if !self.layer.store.has_sentence(sid) { return format!("<sid:{}>", sid); }
        let sentence = &self.layer.store.sentences[self.layer.store.sent_idx(sid)];
        let parts: Vec<String> = sentence.elements.iter().map(|e| match e {
            Element::Symbol { id, .. }                                       => self.layer.store.sym_name(*id).to_owned(),
            Element::Variable { name, .. }                                   => name.clone(),
            Element::Literal { lit: crate::types::Literal::Str(s), .. }      => s.clone(),
            Element::Literal { lit: crate::types::Literal::Number(n), .. }   => n.clone(),
            Element::Op { op, .. }                                           => op.name().to_owned(),
            Element::Sub { sid: sub_id, .. }                                 => format!("({})", self.sentence_to_string(*sub_id)),
        }).collect();
        format!("({})", parts.join(" "))
    }

    /// Render a single sentence back to KIF notation (plain text, no ANSI).
    pub fn sentence_kif_str(&self, sid: SentenceId) -> String {
        crate::kif_store::sentence_to_plain_kif(sid, &self.layer.store)
    }

    // -- Incremental file reload ----------------------------------------------
    //
    // `apply_file_diff` and `compute_file_diff` are the general-purpose
    // primitives any incremental-reload workflow can use -- file
    // watchers, LSP didChange, test harness hot-reload.  They operate
    // purely on sumo-kb types (`AstNode`, `Span`, `SentenceId`) and
    // have no LSP / editor dependency.

    /// Read-only view of the per-file fingerprint vector.  The
    /// returned slice is positionally aligned with
    /// [`file_roots`](Self::file_roots) for the same `file`.
    pub fn file_hashes(&self, file: &str) -> &[u64] {
        self.layer.store.file_hashes.get(file).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Read-only view of the per-file root-sentence ids, in source order.
    pub fn file_roots(&self, file: &str) -> &[SentenceId] {
        self.layer.store.file_roots.get(file).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Iterate every file tag currently loaded in the KB, in
    /// HashMap-iteration (arbitrary but stable-within-run) order.
    ///
    /// Non-LSP uses: CLI dump utilities ("which files are live in
    /// this KB?"), file watchers that want to diff against disk,
    /// any consumer reconciling an external file list against
    /// the in-memory state.
    pub fn iter_files(&self) -> impl Iterator<Item = &str> + '_ {
        self.layer.store.file_roots.keys().map(|s| s.as_str())
    }

    /// Drop every root sentence tagged with `file`.  Orphaned
    /// symbols (those no longer referenced by any remaining
    /// sentence) are pruned from the intern table.  The
    /// occurrence index, head-index, and file-hash side table
    /// all update in lockstep via the underlying
    /// `KifStore::remove_file` primitive.
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
            self.layer.store.file_roots.get(file)
                .map(|v| v.iter().copied().collect())
                .unwrap_or_default();

        self.layer.store.remove_file(file);

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
                { self.layer.store.has_sentence(*s) }
            });
        }
    }

    /// Apply an incremental reload diff to the knowledge base.
    ///
    /// General-purpose primitive for any consumer that wants to
    /// re-sync an in-memory KB with a changed source file without
    /// paying the full `remove_file` + `load_kif` cost.  LSP
    /// didChange is the motivating caller, but a file-watcher CLI
    /// or hot-reload test harness uses the same entry point.
    ///
    /// * `retained` — sentence ids whose body is unchanged; only the
    ///   span is updated to the new source position.
    /// * `removed` — sentence ids that no longer exist in the new
    ///   source.  Bodies are cleared and indices updated; the stable
    ///   `SentenceId` position is left in place to preserve dangling
    ///   references.
    /// * `added` — fresh AST nodes to build into new root sentences,
    ///   tagged with `diff.file`.
    ///
    /// Orphan pruning + cache invalidation run once at the end: the
    /// union of symbol sets from removed + added sentences is
    /// collected and handed to
    /// [`SemanticLayer::invalidate_symbols`](crate::semantic::SemanticLayer)
    /// for targeted eviction.  Retained sentences trigger no cache
    /// churn.
    pub fn apply_file_diff(&mut self, diff: FileDiff) -> TellResult {
        let mut result = TellResult { ok: true, errors: Vec::new(), warnings: Vec::new() };
        let mut affected_syms: HashSet<SymbolId> = HashSet::new();

        // 1. Retained: update spans only.
        for (sid, new_span) in &diff.retained {
            let ok = self.layer.store.update_sentence_span(*sid, new_span.clone());
            if !ok {
                log::warn!(target: "sumo_kb::kb",
                    "apply_file_diff: retained sid={} missing in store", sid);
            }
        }

        // 2. Removed: collect symbols first (they'll need invalidation),
        //    then drop each sentence.
        for &sid in &diff.removed {
            for sym in self.layer.store.sentence_symbols(sid) {
                affected_syms.insert(sym);
            }
            self.layer.store.remove_sentence(sid);
        }

        // 3. Added: append as root sentences.
        let mut parse_errs: Vec<(Span, KbError)> = Vec::new();
        for node in &diff.added {
            if let Some(sid) = self.layer.store.append_root_sentence(node, &diff.file, &mut parse_errs) {
                for sym in self.layer.store.sentence_symbols(sid) {
                    affected_syms.insert(sym);
                }
            }
        }
        for (_, e) in parse_errs {
            result.ok = false;
            result.errors.push(e);
        }

        // 4. Prune orphaned symbols + invalidate affected cache entries.
        if !diff.removed.is_empty() {
            self.layer.store.prune_orphaned_symbols_now();
        }
        self.layer.invalidate_symbols(&affected_syms);
        // `SortAnnotations` depends on domain/range edges -- easier to
        // rebuild wholesale than track per-sentence.  Only flush when
        // the diff actually mutated the KB.
        if !diff.removed.is_empty() || !diff.added.is_empty() {
            self.layer.invalidate_sort_annotations();
        }

        log::debug!(target: "sumo_kb::kb",
            "apply_file_diff file='{}': {} retained, {} removed, {} added, {} affected syms",
            diff.file, diff.retained.len(), diff.removed.len(), diff.added.len(),
            affected_syms.len());

        result
    }

    /// Collect all SentenceIds that are currently promoted axioms.
    ///
    /// "Promoted" means "not currently attached to any open session".
    /// In cnf-on builds we fast-path through the fingerprint table
    /// (value `.1 == None` marks an axiom); in cnf-off builds we
    /// reconstruct the set by subtracting every session sid from
    /// `store.roots`.
    fn axiom_ids_set(&self) -> HashSet<SentenceId> {
        #[cfg(feature = "cnf")]
        {
            self.fingerprints.values()
                .filter(|(_, s)| s.is_none())
                .map(|(sid, _)| *sid)
                .collect()
        }
        #[cfg(not(feature = "cnf"))]
        {
            let session_sids: HashSet<SentenceId> = self.sessions.values()
                .flat_map(|v| v.iter().copied())
                .collect();
            self.layer.store.roots.iter()
                .copied()
                .filter(|sid| !session_sids.contains(sid))
                .collect()
        }
    }

    // -- SInE axiom selection (feature = "ask") -------------------------------
    //
    // The SInE index is maintained eagerly by `make_session_axiomatic`,
    // `promote_assertions_unchecked`, and `open`: every axiom promotion
    // incrementally updates the D-relation.  Query-path methods below
    // are pure reads (plus a parse-and-roll-back to extract conjecture
    // symbols) and pay zero rebuild cost at query time.

    /// Number of axioms currently tracked by the SInE index.
    #[cfg(feature = "ask")]
    pub fn sine_axiom_count(&self) -> usize {
        self.sine_index.read().expect("sine_index poisoned").axiom_count()
    }

    /// The tolerance at which the SInE D-relation is currently computed.
    #[cfg(feature = "ask")]
    pub fn sine_tolerance(&self) -> f32 {
        self.sine_index.read().expect("sine_index poisoned").tolerance()
    }

    /// Rebuild the SInE index from scratch over the current axiom set.
    /// Normally not needed — the index is maintained eagerly — but
    /// useful as an escape hatch after non-standard axiom mutations.
    #[cfg(feature = "ask")]
    pub fn rebuild_sine_index(&mut self) {
        let axiom_ids = self.axiom_ids_set();
        let tolerance = self.sine_index.read().expect("sine_index poisoned").tolerance();
        let mut idx = SineIndex::new(tolerance);
        idx.add_axioms(&self.layer.store, axiom_ids.into_iter());
        *self.sine_index.write().expect("sine_index poisoned") = idx;
    }

    /// Extract the symbols of a KIF conjecture string without mutating
    /// the KB's logical state.
    ///
    /// Parses `query_kif` into the store under a temporary file tag,
    /// walks every resulting sentence to collect its symbol ids, then
    /// rolls the parse back — leaving no orphan sentences, taxonomy
    /// edges, or semantic-cache entries.
    ///
    /// Returns [`KbError`] on parse failure.  On success the returned
    /// set may be empty if the conjecture references only variables
    /// and literals.
    ///
    /// The returned SymbolIds are a single-use seed: pass them
    /// straight into [`sine_select_for_query`] or similar — they are
    /// not stable across multiple calls because the name→id interning
    /// resets under roll-back.
    #[cfg(feature = "ask")]
    pub fn query_symbols(&mut self, query_kif: &str) -> Result<HashSet<SymbolId>, KbError> {
        let query_tag = "__sine_query__";
        let prev_count = self.layer.store.file_roots
            .get(query_tag).map(|v| v.len()).unwrap_or(0);

        let parse_errors = load_kif(&mut self.layer.store, query_kif, query_tag);
        if !parse_errors.is_empty() {
            self.layer.store.remove_file(query_tag);
            self.layer.rebuild_taxonomy();
            self.layer.invalidate_cache();
            let (_, e) = parse_errors.into_iter().next().unwrap();
            return Err(e);
        }

        let query_sids: Vec<SentenceId> = self.layer.store.file_roots
            .get(query_tag)
            .map(|v| v[prev_count..].to_vec())
            .unwrap_or_default();

        let mut syms: HashSet<SymbolId> = HashSet::new();
        for &sid in &query_sids {
            collect_conjecture_symbols(&self.layer.store, sid, &mut syms);
        }

        // Roll back the temporary parse.  The SInE index is unaffected —
        // we only mutated file-tag-scoped state that `remove_file` fully
        // undoes.
        self.layer.store.remove_file(query_tag);
        self.layer.rebuild_taxonomy();
        self.layer.invalidate_cache();

        log::debug!(target: "sumo_kb::kb",
            "query_symbols: extracted {} syms from {} query sentence(s)",
            syms.len(), query_sids.len());
        Ok(syms)
    }

    /// Return the SentenceIds of promoted axioms that SInE identifies
    /// as relevant to `query_kif` at the given parameters.
    ///
    /// Session assertions are **not** included — SInE operates over
    /// the stable promoted axiom base only.  Callers wiring this into
    /// a prover call are responsible for unioning in any session
    /// assertions they want kept as hypotheses.
    ///
    /// The conjecture's parse is rolled back before this method
    /// returns, so repeated calls with different queries do not
    /// accumulate state.
    ///
    /// Tolerance handling: the eager index caches the D-relation at
    /// a single tolerance.  If `params.tolerance` differs, this
    /// method rebuilds the D-relation in place (preserving the
    /// tolerance-independent per-axiom symbol sets and generality
    /// counts).  In the common case — all queries at the same
    /// tolerance — this rebuild never fires.
    #[cfg(feature = "ask")]
    pub fn sine_select_for_query(
        &mut self,
        query_kif: &str,
        params: SineParams,
    ) -> Result<HashSet<SentenceId>, KbError> {
        let seed = self.query_symbols(query_kif)?;

        // Ensure the cached D-relation matches the requested tolerance.
        {
            let current = self.sine_index
                .read().expect("sine_index poisoned").tolerance();
            if (current - params.tolerance.max(1.0)).abs() > f32::EPSILON {
                self.sine_index
                    .write().expect("sine_index poisoned")
                    .set_tolerance(params.tolerance);
            }
        }

        let idx = self.sine_index.read().expect("sine_index poisoned");
        let selected = idx.select(&seed, params.depth_limit);
        log::info!(target: "sumo_kb::kb",
            "sine_select_for_query: {} seed syms -> {} relevant axioms (of {} total) \
             at tolerance {}",
            seed.len(), selected.len(), idx.axiom_count(), idx.tolerance(),
        );
        Ok(selected)
    }

    /// Print a SemanticError with formula context to the log.
    pub fn pretty_print_error(&self, e: &SemanticError, level: log::Level) {
        e.pretty_print(&self.layer.store, level);
    }

    /// Produce a short human-readable preview of a sentence.
    fn formula_preview(&self, sid: SentenceId) -> String {
        let store = &self.layer.store;
        if !store.has_sentence(sid) { return format!("<sid:{}>", sid); }
        let sentence = &store.sentences[store.sent_idx(sid)];
        let display = format!("{:?}", sentence.elements);
        if display.chars().count() > 60 {
            let truncated: String = display.chars().take(60).collect();
            format!("{}...", truncated)
        } else {
            display
        }
    }
}

impl Default for KnowledgeBase {
    fn default() -> Self { Self::new() }
}
