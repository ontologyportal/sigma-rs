// crates/sumo-kb/src/kb.rs
//
// KnowledgeBase -- the single public API type for sumo-kb.
// Assembles KifStore + SemanticLayer + sessions + fingerprints + optional persist/ask/cnf.

use std::collections::{HashMap, HashSet};

use crate::error::{
    DuplicateInfo, DuplicateSource, KbError, PromoteError, PromoteReport, SemanticError,
    TellResult, TellWarning,
};
use crate::fingerprint::{fingerprint, fingerprint_depth1};
use crate::kif_store::{load_kif, KifStore};
use crate::semantic::SemanticLayer;
use crate::tptp::{kb_to_tptp, sentence_to_tptp, TptpLang, TptpOptions};
use crate::types::SentenceId;

#[cfg(feature = "cnf")]
use crate::types::Clause;

#[cfg(feature = "persist")]
use crate::persist::{load_from_db, write_axioms, LmdbEnv};

#[cfg(feature = "ask")]
use crate::prover::{ProverMode, ProverOpts, ProverResult, ProverStatus, ProverRunner};

#[cfg(feature = "integrated-prover")]
use crate::prover::EmbeddedProverRunner;

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

pub struct KnowledgeBase {
    /// Wrapped KifStore + semantic cache.
    layer: SemanticLayer,

    /// In-memory session assertions: session name -> Vec<SentenceId>.
    /// Sentences here have NOT been promoted to axioms yet.
    sessions: HashMap<String, Vec<SentenceId>>,

    /// Deduplication table: fingerprint hash -> (SentenceId, session).
    /// session=None means promoted axiom; Some(s) means assertion in session s.
    fingerprints: HashMap<u64, (SentenceId, Option<String>)>,

    /// CNF side-car: pre-computed clauses per sentence.
    #[cfg(feature = "cnf")]
    clauses: HashMap<SentenceId, Vec<Clause>>,

    #[cfg(feature = "cnf")]
    cnf_mode: bool,

    #[cfg(feature = "cnf")]
    cnf_opts: ClausifyOptions,

    /// LMDB handle. None = purely in-memory.
    #[cfg(feature = "persist")]
    db: Option<LmdbEnv>,
}

impl KnowledgeBase {
    // -- Construction ----------------------------------------------------------

    pub fn new() -> Self {
        Self {
            layer:        SemanticLayer::new(KifStore::default()),
            sessions:     HashMap::new(),
            fingerprints: HashMap::new(),
            #[cfg(feature = "cnf")] clauses:  HashMap::new(),
            #[cfg(feature = "cnf")] cnf_mode: false,
            #[cfg(feature = "cnf")] cnf_opts: ClausifyOptions::default(),
            #[cfg(feature = "persist")] db:   None,
        }
    }

    #[cfg(feature = "persist")]
    pub fn open(path: &std::path::Path) -> Result<Self, KbError> {
        let env = LmdbEnv::open(path)?;
        let (store, session_map) = load_from_db(&env)?;

        // Fingerprint every loaded sentence as an axiom (session=None).
        let mut fingerprints: HashMap<u64, (SentenceId, Option<String>)> = HashMap::new();
        for &sid in &store.roots {
            for fp in fingerprint_depth1(&store, sid) {
                fingerprints.entry(fp).or_insert((sid, None));
            }
        }

        // Track any session-tagged sentences from the DB.
        for (sid, session_opt) in session_map {
            if let Some(session) = session_opt {
                fingerprints.entry(fingerprint(&store, sid))
                    .and_modify(|entry| entry.1 = Some(session.clone()))
                    .or_insert((sid, Some(session)));
            }
        }

        let layer = SemanticLayer::new(store);
        log::info!(target: "sumo_kb::kb", "opened KB from {:?}: {} axioms fingerprinted",
            path, fingerprints.len());

        Ok(Self {
            layer,
            sessions:     HashMap::new(),
            fingerprints,
            #[cfg(feature = "cnf")] clauses:  HashMap::new(),
            #[cfg(feature = "cnf")] cnf_mode: false,
            #[cfg(feature = "cnf")] cnf_opts: ClausifyOptions::default(),
            db: Some(env),
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
        let mut result = TellResult { ok: true, errors: vec![], warnings: vec![] };

        // Snapshot root count before loading so we only process truly new roots.
        let prev_root_count = self.layer.store.file_roots
            .get(file_tag)
            .map(|v| v.len())
            .unwrap_or(0);

        // Parse into store using file_tag as the KIF "file" name.
        self.layer.invalidate_cache();
        let parse_errors = load_kif(&mut self.layer.store, text, file_tag);

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

            // Fingerprint check for deduplication.
            let fps = fingerprint_depth1(&self.layer.store, sid);
            let mut duplicate = false;
            for fp in &fps {
                if let Some((existing_id, existing_session)) = self.fingerprints.get(fp) {
                    let preview = self.formula_preview(*existing_id);
                    match existing_session {
                        None => {
                            result.warnings.push(TellWarning::DuplicateAxiom {
                                existing_id: *existing_id,
                                formula_preview: preview,
                            });
                        }
                        Some(s) => {
                            result.warnings.push(TellWarning::DuplicateAssertion {
                                existing_id: *existing_id,
                                existing_session: s.clone(),
                                formula_preview: preview,
                            });
                        }
                    }
                    duplicate = true;
                    break;
                }
            }

            if !duplicate {
                // Accept: add fingerprint + add to session.
                let root_fp = fingerprint(&self.layer.store, sid);
                self.fingerprints.insert(root_fp, (sid, Some(session.to_owned())));
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
        for &sid in &sids {
            let fp = fingerprint(&self.layer.store, sid);
            self.fingerprints.insert(fp, (sid, None));
        }
        log::info!(target: "sumo_kb::kb",
            "make_session_axiomatic: {} sentence(s) from session '{}' promoted to axioms",
            count, session);
    }

    // -- Session management ----------------------------------------------------

    /// Discard all assertions in `session` (removes from store and fingerprints).
    pub fn flush_session(&mut self, session: &str) {
        let sids = self.sessions.remove(session).unwrap_or_default();
        if sids.is_empty() { return; }

        // Remove fingerprint entries for this session.
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

        // -- Step 1: Cross-session dedup ---------------------------------------
        let mut surviving: Vec<SentenceId> = Vec::new();
        for &sid in &session_sids {
            let fp = fingerprint(&self.layer.store, sid);
            // Check if this fp exists in fingerprints with session=None (already an axiom)
            // or with a DIFFERENT session (duplicate assertion).
            let is_dup = self.fingerprints.get(&fp).map(|(_, s)| {
                match s {
                    None                        => true,  // already an axiom
                    Some(s) if s != session     => true,  // in another session
                    _                           => false, // same session -> OK
                }
            }).unwrap_or(false);

            if is_dup {
                if let Some((dup_of, dup_session)) = self.fingerprints.get(&fp) {
                    let preview = self.formula_preview(sid);
                    report.duplicates_removed.push(DuplicateInfo {
                        sentence_id:     sid,
                        duplicate_of:    *dup_of,
                        source:          match dup_session {
                            None    => DuplicateSource::Axiom,
                            Some(s) => DuplicateSource::Session(s.clone()),
                        },
                        formula_preview: preview,
                    });
                }
            } else {
                surviving.push(sid);
            }
        }
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

        // -- Step 3: Clausify [cnf feature] -----------------------------------
        #[cfg(feature = "cnf")]
        let clause_map: HashMap<SentenceId, Vec<Clause>> = {
            if self.cnf_mode {
                let mut map = HashMap::new();
                for &sid in &surviving {
                    let mut skolem_counter = 0u64;
                    let mut skolem_syms: Vec<crate::types::Symbol> = Vec::new();
                    let max_clauses = self.cnf_opts.max_clauses_per_formula;
                    match crate::cnf::sentence_to_cnf(
                        &self.layer.store, sid, &mut skolem_counter, &mut skolem_syms, max_clauses,
                    ) {
                        Ok(clauses) => { map.insert(sid, clauses); }
                        Err(e) => log::warn!(target: "sumo_kb::kb",
                            "clausify: sid={} failed: {}", sid, e),
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
        for &sid in &surviving {
            let fp = fingerprint(&self.layer.store, sid);
            self.fingerprints.insert(fp, (sid, None));
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

        // Build TPTP: existing axioms as axioms + session assertions as hypotheses + $false as conjecture.
        let axiom_ids = self.axiom_ids_set();
        let assertion_ids: HashSet<SentenceId> = session_sids.iter().copied().collect();

        let kb_opts = TptpOptions { lang: TptpLang::Fof, hide_numbers: true, ..TptpOptions::default() };
        let mut tptp = kb_to_tptp(&self.layer, "kb", &kb_opts, &axiom_ids, &assertion_ids);
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
    pub fn to_tptp(&self, opts: &TptpOptions, session: Option<&str>) -> String {
        let axiom_ids = self.axiom_ids_set();
        let assertion_ids: HashSet<SentenceId> = session
            .and_then(|s| self.sessions.get(s))
            .map(|v| v.iter().copied().collect())
            .unwrap_or_default();
        kb_to_tptp(&self.layer, "kb", opts, &axiom_ids, &assertion_ids)
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
    #[cfg(feature = "cnf")]
    pub fn clausify(&mut self) -> Result<ClausifyReport, KbError> {
        let mut report = ClausifyReport::default();
        let max_clauses = self.cnf_opts.max_clauses_per_formula;

        // Collect all SIDs to clausify (axioms + all session assertions).
        let axiom_ids = self.axiom_ids_set();
        let mut all_sids: Vec<SentenceId> = axiom_ids.into_iter().collect();
        for sids in self.sessions.values() { all_sids.extend(sids.iter().copied()); }

        let mut skolem_counter = 0u64;
        let mut skolem_syms: Vec<crate::types::Symbol> = Vec::new();

        for sid in all_sids {
            match crate::cnf::sentence_to_cnf(
                &self.layer.store, sid, &mut skolem_counter, &mut skolem_syms, max_clauses,
            ) {
                Ok(clauses) => {
                    self.clauses.insert(sid, clauses);
                    report.clausified += 1;
                }
                Err(_) => {
                    report.exceeded_limit.push(sid);
                    report.skipped += 1;
                }
            }
        }

        // Add Skolem symbols to store.
        for sym in skolem_syms {
            self.layer.store.intern_skolem(&sym.name, sym.skolem_arity);
        }

        log::info!(target: "sumo_kb::kb",
            "clausify: {} clausified, {} exceeded limit", report.clausified, report.skipped);
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

        self.layer.invalidate_cache();
        let parse_errors: Vec<(Span, KbError)> = load_kif(&mut self.layer.store, query_kif, query_tag);
        if !parse_errors.is_empty() {

            self.layer.store.remove_file(query_tag);
            self.layer.rebuild_taxonomy();
            self.layer.invalidate_cache();
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: parse_errors.iter()
                    .map(|(_, e): &(Span, KbError)| e.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
            };
        }

        let query_sids: Vec<SentenceId> = self.layer.store.file_roots
            .get(query_tag)
            .map(|v| v[prev_count..].to_vec())
            .unwrap_or_default();

        if query_sids.is_empty() {
            self.layer.store.remove_file(query_tag);
            self.layer.rebuild_taxonomy();
            self.layer.invalidate_cache();
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: "No query sentence parsed".into(),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
            };
        }

        // Build TPTP: axioms + optional session assertions.
        let axiom_ids    = self.axiom_ids_set();
        let assertion_ids: HashSet<SentenceId> = session
            .and_then(|s| self.sessions.get(s))
            .map(|v| v.iter().copied().collect())
            .unwrap_or_default();

        let kb_opts = TptpOptions { lang, hide_numbers: true, ..TptpOptions::default() };
        let mut tptp = kb_to_tptp(&self.layer, "kb", &kb_opts, &axiom_ids, &assertion_ids);

        // Append conjecture(s).
        let q_opts = TptpOptions { lang, query: true, hide_numbers: true, ..TptpOptions::default() };
        for (i, &qsid) in query_sids.iter().enumerate() {
            let conj = sentence_to_tptp(qsid, &self.layer, &q_opts);
            tptp.push_str(&format!("\n{}(query_{}, conjecture, ({})).\n", lang.as_str(), i, conj));
        }

        log::debug!(target: "sumo_kb::kb", "ask: TPTP size={} bytes", tptp.len());

        // Remove query sentences from the store (they were added directly,
        // not via a session, so flush_session would not clean them up).
        self.layer.store.remove_file(query_tag);
        self.layer.rebuild_taxonomy();
        self.layer.invalidate_cache();

        let prover_opts = ProverOpts { timeout_secs: 30, mode: ProverMode::Prove };
        runner.prove(&tptp, &prover_opts)
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

        self.layer.invalidate_cache();
        let parse_errors = load_kif(&mut self.layer.store, query_kif, query_tag);
        if !parse_errors.is_empty() {
            self.layer.store.remove_file(query_tag);
            self.layer.rebuild_taxonomy();
            self.layer.invalidate_cache();
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: parse_errors.iter()
                    .map(|(_, e)| e.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
            };
        }

        let query_sids: Vec<SentenceId> = self.layer.store.file_roots
            .get(query_tag)
            .map(|v| v[prev_count..].to_vec())
            .unwrap_or_default();

        if query_sids.is_empty() {
            self.layer.store.remove_file(query_tag);
            self.layer.rebuild_taxonomy();
            self.layer.invalidate_cache();
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: "No query sentence parsed".into(),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
            };
        }

        let axiom_sids: Vec<SentenceId> = self.axiom_ids_set().into_iter().collect();
        let assertion_sids: Vec<SentenceId> = session
            .and_then(|s| self.sessions.get(s))
            .map(|v| v.clone())
            .unwrap_or_default();

        let runner = EmbeddedProverRunner;
        let prover_opts = ProverOpts { timeout_secs, mode: ProverMode::Prove };
        let result = runner.run(
            &self.layer.store,
            &axiom_sids,
            &assertion_sids,
            &query_sids,
            &prover_opts,
        );

        self.layer.store.remove_file(query_tag);
        self.layer.rebuild_taxonomy();
        self.layer.invalidate_cache();

        result
    }

    // -- Internal helpers ------------------------------------------------------

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
    pub fn format_sentence_tptp(&self, sid: SentenceId, opts: &TptpOptions) -> String {
        sentence_to_tptp(sid, &self.layer, opts)
    }

    /// Render a single sentence back to KIF notation (plain text, no ANSI).
    pub fn sentence_kif_str(&self, sid: SentenceId) -> String {
        crate::kif_store::sentence_to_plain_kif(sid, &self.layer.store)
    }

    /// Collect all SentenceIds that are currently promoted axioms.
    fn axiom_ids_set(&self) -> HashSet<SentenceId> {
        self.fingerprints.values()
            .filter(|(_, s)| s.is_none())
            .map(|(sid, _)| *sid)
            .collect()
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
        if display.len() > 60 { format!("{}...", &display[..62]) } else { display }
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
        CnfTerm::SkolemFn { id, args } => {
            let name = format!("s__{}", store.sym_name(*id));
            let arg_strs: Vec<String> = args.iter().map(|a| format_cnf_term(store, a)).collect();
            format!("{}({})", name, arg_strs.join(","))
        }
        CnfTerm::Num(s) => s.clone(),
        CnfTerm::Str(s) => s.clone(),
    }
}
