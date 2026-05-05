// crates/core/src/kb/ingest.rs
//
// Formula ingestion API

use thiserror::Error;
#[cfg(all(feature = "cnf", feature = "persist"))]
use std::collections::HashMap;
use std::collections::HashSet;

use crate::SentenceId;
use crate::syntactic::load_kif;
use crate::semantics::errors::SemanticError;
#[cfg(feature = "persist")]
use crate::persist::write_axioms;

use super::KnowledgeBase;
use super::error::KbError;

#[cfg(feature = "cnf")]
use crate::types::Clause;

// KB implementation (public API)
impl KnowledgeBase {
    /// Assert a single KIF string into a named session.
    ///
    /// Each sentence is semantically validated before acceptance; warnings are
    /// returned in [`TellResult::warnings`] and errors in [`TellResult::errors`].
    pub fn tell(&mut self, session: &str, kif: &str) -> TellResult {
        with_guard!(self);
        self.ingest(kif, session, session, true)
    }

    /// Load a KIF file into the KB.  If `session` is `None`, the `file` name
    /// is used as the session key.
    ///
    /// Per-sentence validation is deliberately skipped to avoid false positives
    /// from forward-references within a file or across files.  Call
    /// [`Self::validate_all`] explicitly after loading all files to get the full set
    /// of warnings with complete KB context.
    pub fn load_kif(&mut self, text: &str, file: &str, session: Option<&str>) -> TellResult {
        with_guard!(self);
        let session_key = session.unwrap_or(file);
        self.ingest(text, file, session_key, false)
    }

    /// Core ingestion: parse `text` with file tag `file_tag`, add accepted sentences to `session`.
    ///
    /// `validate`: if `true`, run per-sentence semantic validation (used by `tell`).
    ///             if `false`, skip validation (used by `load_kif` for bulk loading).
    pub(super) fn ingest(&mut self, text: &str, file_tag: &str, session: &str, validate: bool) -> TellResult {
        // Set up the result to return
        let mut result = TellResult { ok: true, errors: vec![], warnings: vec![] };

        // Snapshot root count before loading so we only process truly new roots.
        let prev_root_count = self.layer.semantic.syntactic.file_roots
            .get(file_tag)
            .map(|v| v.len())
            .unwrap_or(0);

        // Parse into store using file_tag as the KIF "file" name.
        //
        // Error handling: record parse errors in `result.errors`
        // and mark `result.ok = false`, then continue running the
        // full pipeline on whatever *did* parse. This allows "best 
        // case" ingestion behavior desired by use cases like the LSP
        let parse_errors = {
            profile_span!(self, "ingest.load_kif");
            load_kif(&mut self.layer.semantic.syntactic, text, file_tag)
        };
        if !parse_errors.is_empty() {
            result.ok = false;
            for (_, e) in parse_errors {
                result.errors.push(e);
            }
            // Fall through -- run the pipeline on the sentences
            // that did parse so the KB stays internally consistent.
        }

        // Collect only roots added by THIS call (file_roots accumulates across calls).
        let new_roots: Vec<SentenceId> = self.layer.semantic.syntactic.file_roots
            .get(file_tag)
            .map(|v| v[prev_root_count..].to_vec())
            .unwrap_or_default();

        // Optional semantic validation (serial; cheap per sentence)
        if validate {
            profile_span!(self, "ingest.validate_sentence");
            for &sid in &new_roots {
                if let Err(e) = self.layer.semantic.validate_sentence(sid) {
                    result.warnings.push(TellWarning::Semantic(e));
                }
            }
        }

        // Dedup via clause-level formula hash (cnf feature)
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
        // SUMO).  We batch all new roots into ONE
        // `cnf::clausify_sentences_batch` call — one Vampire global-
        // mutex acquisition instead of N — and translate + hash the
        // per-sid output serially.  If Vampire throws on the batch,
        // we bisect to isolate the bad sentence(s) and keep going;
        // see `clausify_with_bisection` below.
        //
        // Output attribution: with NewCNF's naming threshold set to
        // 0 (see `cnf::clausify_sentences_batch`), every output
        // clause traces back to exactly one input sentence.  The
        // `shared` bucket should be empty.
        let mut accepted: Vec<SentenceId> = Vec::new();

        #[cfg(feature = "cnf")]
        {
            use crate::canonical::{canonical_clause_hash, formula_hash_from_clauses};
            // Batched clausification with bisection fallback.
            let batched = {
                profile_span!(self, "ingest.clausify_ir");
                self.clausify_with_bisection(&new_roots)
            };

            if !batched.shared.is_empty() {
                // With naming=0 this shouldn't fire; log as a
                // canary in case NewCNF introduces shared clauses
                // through some other path.
                self.warn(format!("ingest: {} shared clauses from batch (unattributed); discarding", batched.shared.len()));
            }

            // Serial translate + hash + dedup + side-car insert.
            // Walk `new_roots` in original order so intra-batch dedup
            // is deterministic and matches the pre-batch behaviour.
            let skipped_set: std::collections::HashSet<SentenceId> =
                batched.skipped.iter().copied().collect();
            for sid in new_roots.iter().copied() {
                let fh_clauses: Option<(u64, Vec<Clause>)> = if skipped_set.contains(&sid) {
                    // Converter refused this sentence — accept without dedup.
                    self.warn(format!("ingest: converter refused sid={}; accepting without dedup", sid));
                    None
                } else if let Some(ir_cs) = batched.by_sid.get(&sid) {
                    let clauses = {
                        profile_span!(self, "ingest.translate_and_hash");
                        crate::cnf::translate_ir_clauses(&mut self.layer.semantic.syntactic, ir_cs)
                    };
                    let hashes: Vec<u64> = clauses.iter()
                        .map(canonical_clause_hash)
                        .collect();
                    let fh = formula_hash_from_clauses(&hashes);
                    Some((fh, clauses))
                } else {
                    // Bisection left this sid as an isolated failure —
                    // accept without dedup.
                    self.warn(format!("ingest: sid={} isolated by bisection as clausify-failing; \
                         accepting without dedup", sid));
                    None
                };

                profile_span!(self, "ingest.dedup_check");
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
                    None => false,
                };
                if !duplicate {
                    accepted.push(sid);
                    self.debug(format!("tell: accepted sid={} into session '{}'", sid, session));
                } else {
                    // -q / suppress_warnings(true) silences duplicate-axiom notices
                    // the same way it silences semantic warnings.
                    self.warn(format!("tell: duplicate sid={} skipped (session '{}')", sid, session));
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

        // Incremental taxonomy extension + targeted cache
        // invalidation.  When the batch contains no taxonomy-relevant
        // sentences (the common case for most SUMO axioms), this is
        // essentially free -- no scans, no invalidations.
        {
            profile_span!(self, "ingest.taxonomy_extend");
            self.layer.semantic.extend_taxonomy_with(&accepted);
        }

        self.info(format!("tell: session='{}' accepted={} warnings={}", session, accepted.len(), result.warnings.len()));
        result
    }

    /// Collect all SentenceIds that are currently promoted axioms.
    ///
    /// "Promoted" means "not currently attached to any open session".
    /// In cnf-on builds we fast-path through the fingerprint table
    /// (value `.1 == None` marks an axiom); in cnf-off builds we
    /// reconstruct the set by subtracting every session sid from
    /// `store.roots`.
    pub(super) fn axiom_ids_set(&self) -> HashSet<SentenceId> {
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
            self.layer.semantic.syntactic.roots.iter()
                .copied()
                .filter(|sid| !session_sids.contains(sid))
                .collect()
        }
    }

    /// Mark all assertions in `session` as permanent axioms without semantic
    /// validation or LMDB writes. This is specifically used when loading from
    /// a persistent DB - each axiom is ingested then the entire thing is promoted
    ///
    /// After this call the sentences appear in [`Self::ask`]'s axiom set (TPTP role
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
        with_guard!(self);
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
            profile_span!(self, "make_session_axiomatic.fingerprint_retag");
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
            profile_span!(self, "promotemake_session_axiomatic.all_sentences_register");
            for &sid in &sids {
                self.layer.semantic.syntactic.register_axiom_symbols(sid);
            }
        }

        // Eagerly extend the SInE index with each new axiom.  Work per
        // axiom is proportional to the number of axioms sharing a
        // symbol with it (typically dozens to low hundreds on SUMO-scale
        // KBs); in exchange downstream consumers (ask(), reconcile's
        // smart revalidate, LSP "related axioms") get O(answer-size)
        // lookups with no rebuild.
        {
            profile_span!(self, "make_session_axiomatic.sine_maintain");
            let mut idx = self.sine_index.write().expect("sine_index poisoned");
            idx.add_axioms(&self.layer.semantic.syntactic, sids.iter().copied());
        }

        self.info(format!("make_session_axiomatic: {} sentence(s) from session '{}' promoted to axioms", count, session));
        #[cfg(feature = "ask")]
        { self.axiom_cache = None; }
    }

    /// Promote all assertions in `session` to axioms WITHOUT a consistency check.
    /// Requires `persist` feature (writes to LMDB).
    #[cfg(feature = "persist")]
    pub fn promote_assertions_unchecked(
        &mut self,
        session: &str,
    ) -> Result<PromoteReport, KbError> {
        with_guard!(self);
        self.info(format!("promote_assertions_unchecked: session='{}'", session));

        let mut report = PromoteReport::default();
        let session_sids: Vec<SentenceId> = self.sessions
            .get(session)
            .cloned()
            .unwrap_or_default();

        if session_sids.is_empty() {
            self.info(format!("promote: session '{}' empty, nothing to do", session));
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

        self.debug(format!("promote: {} surviving after dedup ({} duplicates removed)", surviving.len(), report.duplicates_removed.len()));

        if surviving.is_empty() {
            self.sessions.remove(session);
            return Ok(report);
        }

        // -- Step 2: Semantic validation ---------------------------------------
        let sem_errors: Vec<(SentenceId, SemanticError)> = surviving.iter()
            .filter_map(|&sid| self.layer.semantic.validate_sentence(sid).err().map(|e| (sid, e)))
            .collect();
        if !sem_errors.is_empty() {
            let count = sem_errors.len();
            self.warn(format!("promote: {} semantic error(s) in session '{}'", count, session));
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
                &self.layer.semantic.syntactic,
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
                self.warn(format!("Phase D: taxonomy cache persist failed: {}", e));
            }
            #[cfg(feature = "ask")]
            if let Err(e) = crate::persist::persist_sort_annotations_cache(env, &self.layer) {
                self.warn(format!("Phase D: sort_annotations cache persist failed: {}", e));
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
        self.layer.semantic.syntactic.clear_file_roots(session);
        // Note: sentences remain in store.roots as promoted axioms.

        // -- Step 8: Populate axiom-occurrence index + SInE --------------------
        for &sid in &surviving {
            self.layer.semantic.syntactic.register_axiom_symbols(sid);
        }
        {
            let mut idx = self.sine_index.write().expect("sine_index poisoned");
            idx.add_axioms(&self.layer.semantic.syntactic, surviving.iter().copied());
        }

        report.promoted = surviving;
        self.info(format!("promote: {} sentence(s) promoted from session '{}'", report.promoted.len(), session));
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
        runner: &dyn crate::ProverRunner,
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

        use crate::{ProverMode, ProverOpts, ProverStatus};
        // Build TPTP: existing axioms + session assertions + $false as conjecture.
        use crate::vampire::assemble::{assemble_tptp, AssemblyOpts};
        use crate::vampire::converter::{Mode, NativeConverter};

        let mut conv = NativeConverter::new(&self.layer, Mode::Fof);
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

        self.debug(format!("promote_assertions: consistency check TPTP size={} bytes", tptp.len()));

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

    // Session management

    /// Discard all assertions in `session` (removes from store and fingerprints).
    pub fn flush_session(&mut self, session: &str) {
        with_guard!(self);
        let sids = self.sessions.remove(session).unwrap_or_default();
        if sids.is_empty() { return; }

        // Drop the in-memory fingerprint entries belonging to this
        // session.  No-op in cnf-off builds (no fingerprints table).
        #[cfg(feature = "cnf")]
        self.fingerprints.retain(|_, (_, s)| s.as_deref() != Some(session));

        // Remove sentences from SyntacticLayer.
        self.layer.semantic.syntactic.remove_file(session);
        self.layer.semantic.rebuild_taxonomy();
        self.layer.semantic.invalidate_cache();

        #[cfg(feature = "cnf")]
        for sid in &sids { self.clauses.remove(sid); }

        self.info(format!("flush_session: removed {} assertion(s) from session '{}'", sids.len(), session));
    }

    /// Discard all in-memory session assertions from all sessions.
    pub fn flush_assertions(&mut self) {
        let sessions: Vec<String> = self.sessions.keys().cloned().collect();
        for s in sessions { self.flush_session(&s); }
    }

    /// Return the SentenceIds for a given session (empty if session doesn't exist).
    pub fn session_sids(&self, session: &str) -> Vec<SentenceId> {
        self.sessions.get(session).cloned().unwrap_or_default()
    }
}

#[derive(Debug)]
pub enum TellWarning {
    /// Formula already present as an axiom in the DB.
    DuplicateAxiom {
        existing_id: SentenceId,
        /// Short human-readable preview of the formula (first ~60 chars).
        formula_preview: String,
    },
    /// Formula already present as an assertion in a session.
    DuplicateAssertion {
        existing_id: SentenceId,
        existing_session: String,
        formula_preview: String,
    },
    /// Non-fatal semantic issue (arity warning, case convention, etc.).
    Semantic(SemanticError),
}

impl std::fmt::Display for TellWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TellWarning::DuplicateAxiom { formula_preview, .. } =>
                write!(f, "duplicate axiom (skipped): {}", formula_preview),
            TellWarning::DuplicateAssertion { existing_session, formula_preview, .. } =>
                write!(f, "duplicate of assertion in session '{}' (skipped): {}",
                    existing_session, formula_preview),
            TellWarning::Semantic(e) =>
                write!(f, "semantic warning [{}]: {}", e.code(), e),
        }
    }
}

// -- tell() result types -------------------------------------------------------

/// Result returned by `KnowledgeBase::tell()` and `load_kif()`.
#[derive(Debug, Default)]
pub struct TellResult {
    /// True if the call succeeded (parse + semantic checks passed).
    /// Duplicate-skipped formulas do NOT make this false.
    pub ok: bool,
    /// Hard errors (parse failures, fatal semantic errors).
    pub errors: Vec<KbError>,
    /// Non-fatal notices (semantic warnings, duplicates skipped).
    pub warnings: Vec<TellWarning>,
}

// -- promote_assertions() result types ----------------------------------------
//
// `PromoteReport`, `DuplicateInfo`, `DuplicateSource`, and `PromoteError`
// are the public return types for `KnowledgeBase::promote_assertions*()`.
// They aren't currently constructed by any wired-up code path, but the
// `Error`-derived `PromoteError` is part of the public API surface so
// downstream callers can `match` on it.  `#[allow(dead_code)]` keeps
// warning-clean until promotion plumbing is reattached.

/// Successful result from `KnowledgeBase::promote_assertions*()`.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct PromoteReport {
    /// SentenceIds successfully promoted to axioms.
    pub promoted: Vec<SentenceId>,
    /// Formulas removed from the session as duplicates before promotion.
    pub duplicates_removed: Vec<DuplicateInfo>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct DuplicateInfo {
    pub sentence_id: SentenceId,
    pub duplicate_of: SentenceId,
    pub source: DuplicateSource,
    /// Short human-readable preview of the formula.
    pub formula_preview: String,
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum DuplicateSource {
    /// Duplicate of an existing axiom in the DB.
    Axiom,
    /// Duplicate of an assertion in another in-memory session.
    Session(String),
}

/// Error returned by `KnowledgeBase::promote_assertions()`.
#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum PromoteError {
    /// The prover showed the session assertions make the KB inconsistent.
    #[error("promotion rejected: session '{session}' makes the KB inconsistent")]
    Inconsistent {
        session: String,
        /// Raw prover output explaining the inconsistency.
        explanation: String,
        /// Assertion SentenceIds implicated (best-effort extraction).
        conflicting: Vec<SentenceId>,
    },

    /// The prover could not determine consistency (timeout or unknown result).
    /// Promotion is conservatively rejected.
    #[error("promotion rejected: prover could not determine consistency ({reason})")]
    ProverUncertain { reason: String },

    /// Hard semantic errors in the session prevented promotion.
    #[error("promotion rejected: {count} semantic error(s) in session")]
    Semantic {
        count: usize,
        errors: Vec<(SentenceId, SemanticError)>,
    },

    #[cfg(feature = "persist")]
    #[error("database write failed: {0}")]
    Db(KbError),
}