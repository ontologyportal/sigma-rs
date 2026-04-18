// crates/sumo-kb/src/kb.rs
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
use std::time::Instant;

use crate::error::{
    DuplicateInfo, DuplicateSource, KbError, PromoteError, PromoteReport, SemanticError,
    TellResult, TellWarning,
};
use crate::kif_store::{load_kif, KifStore};
use crate::semantic::SemanticLayer;
use crate::tptp::{TptpLang, TptpOptions};
use crate::types::SentenceId;

#[cfg(feature = "cnf")]
use crate::types::Clause;

#[cfg(feature = "persist")]
use crate::persist::{load_from_db, write_axioms, LmdbEnv};

#[cfg(feature = "ask")]
use crate::prover::{Binding, ProverMode, ProverOpts, ProverResult, ProverStatus, ProverRunner, ProverTimings};

// EmbeddedProverRunner used only in the FOF embedded path (not the TFF native path)

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
    /// Rebuilt lazily on the first `ask()` or `ask_embedded()` call after the
    /// axiom set changes.
    #[cfg(feature = "ask")]
    axiom_cache: Option<crate::vampire::VampireAxiomCache>,
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
        }
    }

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

        // Silence the unused-variable warning in cnf-off builds.
        #[cfg(not(feature = "cnf"))]
        let _ = session_map;

        // Generate the Semantic Layer from the KIF Symbol Store
        let layer = SemanticLayer::new(store);

        #[cfg(feature = "cnf")]
        log::info!(target: "sumo_kb::kb", "opened KB from {:?}: {} formulas fingerprinted",
            path, fingerprints.len());
        #[cfg(not(feature = "cnf"))]
        log::info!(target: "sumo_kb::kb", "opened KB from {:?} (no-dedup build)", path);

        // Silence mut-not-needed in cnf-on builds (we don't add new
        // entries here; tell/promote will do it).
        #[cfg(feature = "cnf")]
        { let _ = &mut fingerprints; }

        Ok(Self {
            layer,
            sessions:     HashMap::new(),
            #[cfg(feature = "cnf")] fingerprints,
            #[cfg(feature = "cnf")] clauses:  HashMap::new(),
            #[cfg(feature = "cnf")] cnf_mode: true,
            #[cfg(feature = "cnf")] cnf_opts: ClausifyOptions::default(),
            db: Some(env),
            #[cfg(feature = "ask")]  axiom_cache: None,
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
        // Set up the result to return
        let mut result = TellResult { ok: true, errors: vec![], warnings: vec![] };

        // Snapshot root count before loading so we only process truly new roots.
        let prev_root_count = self.layer.store.file_roots
            .get(file_tag)
            .map(|v| v.len())
            .unwrap_or(0);

        // We have to invalidate the cache layer as ingestion may introduce 
        // new axioms which invalidates the kb semantics
        // TODO: Fix this so it regenerates the semantic layer intelligently
        self.layer.invalidate_cache();
        
        // Parse into store using file_tag as the KIF "file" name.
        let parse_errors = load_kif(&mut self.layer.store, text, file_tag);

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

        let mut accepted: Vec<SentenceId> = Vec::new();

        for sid in new_roots {
            // Semantic validation -- only for interactive tell(), not bulk load_kif().
            if validate {
                if let Err(e) = self.layer.validate_sentence(sid) {
                    result.warnings.push(TellWarning::Semantic(e));
                }
            }

            // -- Dedup via clause-level formula hash (cnf feature) -----
            //
            // In the cnf-on build we clausify the candidate sentence,
            // derive a canonical-hash-set-based formula fingerprint,
            // and probe the in-memory `fingerprints` table.  The
            // clauses stay in the side-car so `promote_*` doesn't have
            // to re-clausify.
            //
            // In cnf-off builds no dedup runs; every syntactically-
            // fresh root sentence is accepted.
            #[cfg(feature = "cnf")]
            let duplicate = {
                match self.compute_formula_hash(sid) {
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
                        // the sentence isn't lost.  Error is logged
                        // upstream in `compute_formula_hash`.
                        false
                    }
                }
            };

            #[cfg(not(feature = "cnf"))]
            let duplicate = false;

            if !duplicate {
                accepted.push(sid);
                log::debug!(target: "sumo_kb::kb",
                    "tell: accepted sid={} into session '{}'", sid, session);
            } else {
                log::warn!(target: "sumo_kb::kb",
                    "tell: duplicate sid={} skipped (session '{}')", sid, session);
            }
        }

        self.sessions.entry(session.to_owned()).or_default().extend(&accepted);
        self.layer.extend_taxonomy();
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
    pub fn make_session_axiomatic(&mut self, session: &str) {
        let sids = self.sessions.remove(session).unwrap_or_default();
        let count = sids.len();

        // Flip each sentence's fingerprint entry from session-tagged to
        // axiom (session=None).  Without cnf, no dedup map exists.
        #[cfg(feature = "cnf")]
        {
            for &sid in &sids {
                if let Some((fh, _clauses)) = self.compute_formula_hash(sid) {
                    self.fingerprints.insert(fh, (sid, None));
                }
            }
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

    /// Generate TPTP for the KB.
    ///
    /// - Axioms = all promoted/loaded sentences (fingerprint session=None).
    /// - Assertions = sentences in `session` (if Some) rendered as `hypothesis`.
    /// - Pass `session=None` to omit assertions.
    ///
    /// Routes through the `NativeConverter` + `assemble_tptp` IR pipeline:
    /// SID-based axiom names (`kb_<sid>`), per-axiom KIF comments when
    /// `opts.show_kif_comment` is set, `excluded` predicate filter
    /// applied before conversion.
    pub fn to_tptp(&self, opts: &TptpOptions, session: Option<&str>) -> String {
        use crate::vampire::assemble::{assemble_tptp, AssemblyOpts};
        use crate::vampire::converter::{Mode, NativeConverter};

        let mode = match opts.lang {
            TptpLang::Tff => Mode::Tff,
            TptpLang::Fof => Mode::Fof,
        };

        let mut conv = NativeConverter::new(&self.layer.store, &self.layer, mode)
            .with_hide_numbers(opts.hide_numbers);

        let axiom_ids = self.axiom_ids_set();
        let mut axioms_sorted: Vec<SentenceId> = axiom_ids.into_iter().collect();
        axioms_sorted.sort_unstable();
        for sid in axioms_sorted {
            if self.sentence_excluded(sid, &opts.excluded) { continue; }
            conv.add_axiom(sid);
        }

        if let Some(name) = session {
            if let Some(sids) = self.sessions.get(name) {
                for &sid in sids {
                    if self.sentence_excluded(sid, &opts.excluded) { continue; }
                    conv.add_axiom(sid);
                }
            }
        }

        let (problem, sid_map) = conv.finish();
        assemble_tptp(&problem, &sid_map, &AssemblyOpts {
            show_kif: opts.show_kif_comment,
            layer:    Some(&self.layer),
            ..AssemblyOpts::default()
        })
    }

    /// Return the head predicate name of a sentence, if it has one.
    /// Returns `None` for operator-rooted sentences (e.g. `(and ...)`) or
    /// for sentences whose first element is not a plain symbol.
    fn sentence_head_name(&self, sid: SentenceId) -> Option<String> {
        use crate::types::Element;
        let store = &self.layer.store;
        if !store.has_sentence(sid) { return None; }
        let sentence = &store.sentences[store.sent_idx(sid)];
        match sentence.elements.first()? {
            Element::Symbol(id) => Some(store.sym_name(*id).to_owned()),
            _ => None,
        }
    }

    /// `true` if the sentence's head predicate matches an `excluded` entry.
    fn sentence_excluded(&self, sid: SentenceId, excluded: &HashSet<String>) -> bool {
        if excluded.is_empty() { return false; }
        self.sentence_head_name(sid)
            .map(|n| excluded.contains(&n))
            .unwrap_or(false)
    }

    /// Generate TPTP CNF from pre-computed clauses.
    /// Returns an error if `clausify()` has not been called (or cnf_mode=false).
    #[cfg(feature = "cnf")]
    pub fn to_tptp_cnf(&self, session: Option<&str>) -> Result<String, KbError> {
        use std::fmt::Write as _;

        if self.clauses.is_empty() {
            return Err(KbError::Other(
                "to_tptp_cnf: no clauses available; call clausify() first".into()
            ));
        }

        let sid_set: Option<HashSet<SentenceId>> = session
            .and_then(|s| self.sessions.get(s))
            .map(|v| v.iter().copied().collect());

        let store = &self.layer.store;
        let mut out = String::new();
        let mut idx = 0usize;
        for (&sid, clauses) in &self.clauses {
            if let Some(ref filter) = sid_set {
                if !filter.contains(&sid) { continue; }
            }
            let role = if self.axiom_ids_set().contains(&sid) { "axiom" } else { "hypothesis" };
            for clause in clauses {
                let lit_strs: Vec<String> = clause.literals.iter()
                    .map(|lit| format_cnf_literal(store, lit))
                    .collect();
                let body = if lit_strs.len() == 1 {
                    lit_strs[0].clone()
                } else {
                    format!("({})", lit_strs.join(" | "))
                };
                let _ = writeln!(out, "cnf(c_{}, {}, {}).", idx, role, body);
                idx += 1;
            }
        }
        Ok(out)
    }

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

    /// Ask the theorem prover whether `query_kif` is entailed by the KB.
    /// `session` = optional in-memory session whose assertions are included as hypotheses.
    /// `lang` controls the TPTP language used for the generated problem file.
    #[cfg(feature = "ask")]
    pub fn ask(
        &mut self,
        query_kif: &str,
        session:   Option<&str>,
        runner:    &dyn ProverRunner,
        lang:      TptpLang,
    ) -> ProverResult {
        use crate::Span;

        log::debug!(target: "sumo_kb::kb", "ask: query={}", query_kif);

        // Parse the query directly into the store, bypassing fingerprint
        // deduplication.  The query is a conjecture -- it must be translated
        // even if the same formula already exists as an axiom in the KB.
        let query_tag = "__query__";
        let prev_count = self.layer.store.file_roots
            .get(query_tag).map(|v| v.len()).unwrap_or(0);

        // Phase A: we do NOT preemptively invalidate the cache here.
        //   - Nothing has changed in the KB since the previous operation
        //     that correctly maintained cache validity.
        //   - If the query itself turns out to affect a taxonomy-cache
        //     invariant (only when its head is a taxonomy relation like
        //     `subclass`/`instance`/`subrelation`/`subAttribute`), we do
        //     a targeted invalidation on the way out, not a preemptive
        //     full-cache nuke on the way in.
        let parse_errors: Vec<(Span, KbError)> = load_kif(&mut self.layer.store, query_kif, query_tag);
        if !parse_errors.is_empty() {
            // Parse failed -- no sentences were added, so nothing to
            // clean up beyond the (possibly partial) file_roots entry.
            self.layer.store.remove_file(query_tag);
            // No taxonomy or cache invariants were touched: the query
            // never made it into the store as a well-formed sentence.
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: parse_errors.iter()
                    .map(|(_, e): &(Span, KbError)| e.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
                timings:    ProverTimings::default(),
            };
        }

        let query_sids: Vec<SentenceId> = self.layer.store.file_roots
            .get(query_tag)
            .map(|v| v[prev_count..].to_vec())
            .unwrap_or_default();

        if query_sids.is_empty() {
            // No sentences parsed from the query text -- nothing got
            // into the store or the taxonomy, so no cleanup beyond
            // remove_file is needed (Phase A).
            self.layer.store.remove_file(query_tag);
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: "No query sentence parsed".into(),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
                timings:    ProverTimings::default(),
            };
        }

        // Collect assertion SentenceIds for the requested session.
        let assertion_ids: HashSet<SentenceId> = session
            .and_then(|s| self.sessions.get(s))
            .map(|v| v.iter().copied().collect())
            .unwrap_or_default();

        // Unified FOF + TFF path: build the Problem through NativeConverter,
        // serialise through assemble_tptp, hand off to the runner.  TFF
        // reuses the cached axiom problem (rebuilt lazily); FOF rebuilds
        // fresh each call (no cache for FOF mode today).
        use crate::vampire::assemble::{assemble_tptp, AssemblyOpts};
        use crate::vampire::converter::{Mode, NativeConverter};

        let mode = match lang {
            TptpLang::Tff => Mode::Tff,
            TptpLang::Fof => Mode::Fof,
        };
        let t_input = Instant::now();

        let (problem, sid_map) = if mode == Mode::Tff {
            self.ensure_axiom_cache();
            let (seed_problem, seed_sid_map) = {
                let cache = self.axiom_cache.as_ref().unwrap();
                (cache.problem.clone(), cache.sid_map.clone())
            };
            let mut conv = NativeConverter::from_parts(
                &self.layer.store, &self.layer, seed_problem, seed_sid_map, Mode::Tff,
            );
            for &sid in &assertion_ids { conv.add_axiom(sid); }
            for &qsid in &query_sids {
                if conv.set_conjecture(qsid).is_some() { break; }
            }
            conv.finish()
        } else {
            let mut conv = NativeConverter::new(&self.layer.store, &self.layer, Mode::Fof);
            let mut axioms_sorted: Vec<SentenceId> =
                self.axiom_ids_set().into_iter().collect();
            axioms_sorted.sort_unstable();
            for sid in axioms_sorted { conv.add_axiom(sid); }
            for &sid in &assertion_ids { conv.add_axiom(sid); }
            for &qsid in &query_sids {
                if conv.set_conjecture(qsid).is_some() { break; }
            }
            conv.finish()
        };

        let tptp = assemble_tptp(&problem, &sid_map, &AssemblyOpts {
            conjecture_name: "query_0",
            ..AssemblyOpts::default()
        });
        let input_gen = t_input.elapsed();
        log::debug!(target: "sumo_kb::kb",
            "ask({:?}): TPTP size={} bytes", mode, tptp.len());

        // Remove query sentences from the store (they were added directly,
        // not via a session, so flush_session would not clean them up).
        //
        // Phase A: only rebuild the taxonomy / invalidate caches if the
        // query actually affected taxonomy-relevant state.  The common
        // case -- a non-taxonomy conjecture like
        // `(attribute Alice Tall)` -- touches neither `tax_edges` nor
        // the `sort_annotations` / `var_type_inference` caches, so we
        // skip both a full scan of all KB sentences and a wipe of the
        // derived tables.
        let needs_rebuild = self.query_affects_taxonomy(&query_sids);
        self.layer.store.remove_file(query_tag);
        if needs_rebuild {
            self.layer.rebuild_taxonomy();
            self.layer.invalidate_cache();
        }

        let prover_opts = ProverOpts { timeout_secs: runner.timeout_secs(), mode: ProverMode::Prove };
        let mut result = runner.prove(&tptp, &prover_opts);
        result.timings.input_gen = input_gen;
        result
    }

    // -- Embedded theorem proving ----------------------------------------------

    /// Ask the embedded Vampire prover whether `query_kif` is entailed by the KB.
    ///
    /// Unlike [`ask`], this bypasses TPTP generation and calls Vampire in-process
    /// via the programmatic API.  Binding extraction is not yet supported.
    ///
    /// `session` = optional in-memory session whose assertions are included as hypotheses.
    #[cfg(feature = "integrated-prover")]
    pub fn ask_embedded(
        &mut self,
        query_kif: &str,
        session:   Option<&str>,
        timeout_secs: u32,
    ) -> ProverResult {
        let query_tag = "__query_embedded__";
        let prev_count = self.layer.store.file_roots
            .get(query_tag).map(|v| v.len()).unwrap_or(0);

        // Phase A: no preemptive invalidation.  See the comment in
        // `ask()` for the reasoning.
        let parse_errors = load_kif(&mut self.layer.store, query_kif, query_tag);
        if !parse_errors.is_empty() {
            self.layer.store.remove_file(query_tag);
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: parse_errors.iter()
                    .map(|(_, e)| e.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
                timings:    ProverTimings::default(),
            };
        }

        let query_sids: Vec<SentenceId> = self.layer.store.file_roots
            .get(query_tag)
            .map(|v| v[prev_count..].to_vec())
            .unwrap_or_default();

        if query_sids.is_empty() {
            // No sentences parsed from the query text -- nothing got
            // into the store or the taxonomy, so no cleanup beyond
            // remove_file is needed (Phase A).
            self.layer.store.remove_file(query_tag);
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: "No query sentence parsed".into(),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
                timings:    ProverTimings::default(),
            };
        }

        let assertion_sids: Vec<SentenceId> = session
            .and_then(|s| self.sessions.get(s))
            .cloned()
            .unwrap_or_default();

        // Ensure the IR axiom cache is built.
        self.ensure_axiom_cache();

        // Build the IR problem: clone the cached axiom set, extend with
        // session assertions and the conjecture.
        use crate::vampire::converter::{Mode, NativeConverter};
        let (seed_problem, seed_sid_map) = {
            let cache = self.axiom_cache.as_ref().unwrap();
            (cache.problem.clone(), cache.sid_map.clone())
        };
        let mut conv = NativeConverter::from_parts(
            &self.layer.store, &self.layer, seed_problem, seed_sid_map, Mode::Tff,
        );
        for &sid in &assertion_sids {
            conv.add_axiom(sid);
        }
        let mut query_var_map: Option<crate::vampire::converter::QueryVarMap> = None;
        for &sid in &query_sids {
            if let Some(qvm) = conv.set_conjecture(sid) {
                query_var_map = Some(qvm);
                break;
            }
        }
        let (ir_problem, _sid_map) = conv.finish();

        // Lower to the FFI problem, set options, and solve.
        let mut opts = vampire_prover::Options::new();
        if timeout_secs > 0 {
            opts.timeout(std::time::Duration::from_secs(timeout_secs as u64));
        }
        opts.set_option("mode", "casc");
        let mut problem = vampire_prover::lower_problem(&ir_problem, opts);

        let (res, proof) = problem.solve_and_prove();
        log::debug!(target: "sumo_kb::embedded_prover", "TFF embedded result: {:?}", res);

        let status = match res {
            vampire_prover::ProofRes::Proved     => ProverStatus::Proved,
            vampire_prover::ProofRes::Unprovable => ProverStatus::Disproved,
            vampire_prover::ProofRes::Unknown(_) => ProverStatus::Unknown,
        };

        // Extract variable bindings from the native proof when one is
        // available. Empty result is non-fatal (prover may not produce a
        // proof, or the extractor may not recognise the encoding).
        let bindings: Vec<Binding> = if matches!(status, ProverStatus::Proved) {
            log::debug!(target: "sumo_kb::embedded_prover",
                "bindings eligibility: proof={}, qvm={}",
                proof.is_some(), query_var_map.is_some());
            match (proof, query_var_map) {
                (Some(p), Some(qvm)) => crate::vampire::bindings::extract_bindings(&p, &qvm)
                    .into_iter()
                    .map(|b| Binding { variable: b.variable, value: b.value })
                    .collect(),
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };

        // Phase A: skip the full taxonomy rebuild unless the query
        // actually added a taxonomy edge.  See the comment in `ask()`.
        let needs_rebuild = self.query_affects_taxonomy(&query_sids);
        self.layer.store.remove_file(query_tag);
        if needs_rebuild {
            self.layer.rebuild_taxonomy();
            self.layer.invalidate_cache();
        }

        ProverResult {
            status,
            raw_output: format!("{:?}", res),
            bindings,
            proof_kif:  Vec::new(),
            timings:    ProverTimings::default(), // profiling TODO
        }
    }

    // -- Internal helpers ------------------------------------------------------

    /// `true` if any sentence in `sids` has a taxonomy-relation head
    /// (`subclass`, `instance`, `subrelation`, or `subAttribute`).
    ///
    /// Used by `ask()` / `ask_embedded()` to decide whether the
    /// post-proof cleanup needs a `rebuild_taxonomy` + `invalidate_cache`
    /// cycle.  For the overwhelming majority of conjectures (which are
    /// not taxonomy relations), both sides are no-ops and can be
    /// skipped -- saving an O(total KB) rebuild per ask.
    ///
    /// This check is intentionally conservative: it only looks at the
    /// head of each root sentence, not sub-sentences.  A negated
    /// taxonomy-head query (`(not (subclass X Y))`) returns `false`
    /// here because its head is `not`, not `subclass`; we'd miss the
    /// rebuild in that case.  In practice, negated taxonomy queries
    /// don't add taxonomy edges because `extract_tax_edge_for` only
    /// acts on positive top-level taxonomy sentences, so this
    /// conservativeness is safe.
    #[cfg(feature = "ask")]
    fn query_affects_taxonomy(&self, sids: &[SentenceId]) -> bool {
        use crate::types::TaxRelation;
        sids.iter().any(|&sid| {
            let sentence = &self.layer.store.sentences[self.layer.store.sent_idx(sid)];
            match sentence.head_symbol() {
                Some(head_id) => {
                    let name = self.layer.store.sym_name(head_id);
                    TaxRelation::from_str(name).is_some()
                }
                None => false,
            }
        })
    }

    /// Ensure the TFF IR axiom cache is populated; build it if needed.
    /// After this call `self.axiom_cache` is guaranteed to be `Some`.
    #[cfg(feature = "ask")]
    fn ensure_axiom_cache(&mut self) {
        if self.axiom_cache.is_none() {
            let axiom_ids = self.axiom_ids_set();
            self.axiom_cache = Some(crate::vampire::VampireAxiomCache::build(
                &self.layer,
                &axiom_ids,
                crate::vampire::converter::Mode::Tff,
            ));
        }
    }

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
            Element::Symbol(id)                   => self.layer.store.sym_name(*id).to_owned(),
            Element::Variable { name, .. }        => name.clone(),
            Element::Literal(crate::types::Literal::Str(s))    => s.clone(),
            Element::Literal(crate::types::Literal::Number(n)) => n.clone(),
            Element::Op(op)                       => op.name().to_owned(),
            Element::Sub(sub_id)                  => format!("({})", self.sentence_to_string(*sub_id)),
        }).collect();
        format!("({})", parts.join(" "))
    }

    /// Render a single sentence as TPTP.
    ///
    /// Returns the formula body only (no `tff(...)` / `fof(...)` wrapper);
    /// callers add their own `<kw>(name, role, ...)` framing.  Respects
    /// `opts.query` (existential wrap for conjectures vs universal wrap
    /// for axioms), `opts.lang`, and `opts.hide_numbers`.
    pub fn format_sentence_tptp(&self, sid: SentenceId, opts: &TptpOptions) -> String {
        use crate::vampire::converter::{Mode, NativeConverter};

        let mode = match opts.lang {
            TptpLang::Tff => Mode::Tff,
            TptpLang::Fof => Mode::Fof,
        };
        let mut conv = NativeConverter::new(&self.layer.store, &self.layer, mode)
            .with_hide_numbers(opts.hide_numbers);

        if opts.query {
            conv.set_conjecture(sid);
            let (problem, _) = conv.finish();
            return problem
                .conjecture_ref()
                .map(|f| f.to_tptp())
                .unwrap_or_default();
        }
        conv.add_axiom(sid);
        let (problem, _) = conv.finish();
        problem
            .axioms()
            .first()
            .map(|f| f.to_tptp())
            .unwrap_or_default()
    }

    /// Render a single sentence back to KIF notation (plain text, no ANSI).
    pub fn sentence_kif_str(&self, sid: SentenceId) -> String {
        crate::kif_store::sentence_to_plain_kif(sid, &self.layer.store)
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

// -- CNF clause formatting -----------------------------------------------------

#[cfg(feature = "cnf")]
fn format_cnf_literal(store: &KifStore, lit: &crate::types::CnfLiteral) -> String {
    let pred = format_cnf_term(store, &lit.pred);
    let args: Vec<String> = lit.args.iter().map(|t| format_cnf_term(store, t)).collect();
    let atom = if args.is_empty() {
        pred
    } else {
        format!("{}({})", pred, args.join(","))
    };
    if lit.positive { atom } else { format!("~{}", atom) }
}

#[cfg(feature = "cnf")]
fn format_cnf_term(store: &KifStore, term: &crate::types::CnfTerm) -> String {
    use crate::types::CnfTerm;
    match term {
        CnfTerm::Const(id)  => format!("s__{}", store.sym_name(*id)),
        CnfTerm::Var(id)    => format!("V__{}", store.sym_name(*id).replace('@', "_")),
        CnfTerm::Fn { id, args } => {
            let name = format!("s__{}", store.sym_name(*id));
            let arg_strs: Vec<String> = args.iter().map(|a| format_cnf_term(store, a)).collect();
            format!("{}({})", name, arg_strs.join(","))
        }
        CnfTerm::SkolemFn { id, args } => {
            let name = format!("s__{}", store.sym_name(*id));
            let arg_strs: Vec<String> = args.iter().map(|a| format_cnf_term(store, a)).collect();
            format!("{}({})", name, arg_strs.join(","))
        }
        CnfTerm::Num(s) => s.clone(),
        CnfTerm::Str(s) => s.clone(),
    }
}
