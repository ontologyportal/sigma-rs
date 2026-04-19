// crates/sumo-kb/src/persist/commit.rs
//
// Write promoted axioms to LMDB.
//
// Key difference from old store/src/commit.rs:
// IDs are already stable -- NO remapping needed.  We write symbols and formulas
// with the IDs they already hold in `KifStore`.
//
// Phase 4 adds a clause-dedup stage: per root sentence, each CNF clause
// is interned via `LmdbEnv::intern_clause` (keyed by canonical hash).
// `StoredFormula.clause_ids` stores the deduped ids rather than the full
// clause bodies, and a formula-level hash derived from the sorted id
// list is recorded in `formula_hashes`.
#[cfg(feature = "cnf")]
use std::collections::HashMap;

use crate::error::KbError;
use crate::kif_store::KifStore;
use crate::types::{Element, Literal, SentenceId};
use super::env::{LmdbEnv, StoredElement, StoredFormula, StoredSymbol};

/// Write a batch of sentences from `store` to `env` as axioms (or session assertions).
///
/// All sentences listed in `sids` are written together in a single LMDB
/// write transaction.  On any error the transaction is aborted automatically.
///
/// `session`: `None` -> axiom; `Some(name)` -> session assertion.
/// `clauses`: pre-computed CNF clauses (only used when `cnf` feature is enabled).
pub(crate) fn write_axioms(
    env:     &LmdbEnv,
    store:   &KifStore,
    sids:    &[SentenceId],
    #[cfg(feature = "cnf")]
    clauses: &HashMap<SentenceId, Vec<crate::types::Clause>>,
    session: Option<&str>,
) -> Result<(), KbError> {
    if sids.is_empty() { return Ok(()); }
    log::info!(target: "sumo_kb::persist",
        "write_axioms: {} sentence(s), session={:?}", sids.len(), session);

    let mut wtxn = env.write_txn()?;
    log::debug!(target: "sumo_kb::persist", "write txn opened");

    // -- 1. Intern all symbols from `store` (write only new ones) -------------
    for sym in &store.symbol_data {
        if sym.name.is_empty() { continue; }
        env.put_symbol(&mut wtxn, &StoredSymbol {
            id:           store.symbols[&sym.name],
            name:         sym.name.clone(),
            is_skolem:    sym.is_skolem,
            skolem_arity: sym.skolem_arity,
        })?;
    }
    log::debug!(target: "sumo_kb::persist",
        "write_axioms: interned {} symbols", store.symbol_data.len());

    // -- 2. Write each sentence ------------------------------------------------
    for &sid in sids {
        write_sentence(env, &mut wtxn, store, sid,
            #[cfg(feature = "cnf")] clauses,
            session)?;
    }

    // -- 3. Bump kb_version so any persisted Phase D cache from before
    //       this commit is recognised as stale on the next open.
    //       Done in the same txn as the sentence writes so the counter
    //       and the axiom set move together atomically.
    let new_kb_version = env.bump_kb_version(&mut wtxn)?;
    log::debug!(target: "sumo_kb::persist",
        "write_axioms: kb_version bumped to {}", new_kb_version);

    // -- 4. Stamp the feature manifest with the current build's features.
    //       Written unconditionally so the next open can detect any drift
    //       (feature on->off or off->on) and warn the user.  Serialised
    //       via the cache table so it lives in the same bincode-blob
    //       container as the other Phase D caches.
    let manifest = super::env::FeatureManifest {
        schema:     super::env::SCHEMA_VERSION,
        kb_version: new_kb_version,
        features:   super::env::FeatureSet::current(),
    };
    env.put_cache(&mut wtxn, super::env::CACHE_KEY_FEATURE_MANIFEST, &manifest)?;

    // -- 5. Commit -------------------------------------------------------------
    wtxn.commit()?;
    log::info!(target: "sumo_kb::persist",
        "write_axioms: committed {} sentence(s), kb_version={}, features={:?}",
        sids.len(), new_kb_version, manifest.features);
    Ok(())
}

fn write_sentence(
    env:     &LmdbEnv,
    wtxn:    &mut heed::RwTxn,
    store:   &KifStore,
    sid:     SentenceId,
    #[cfg(feature = "cnf")]
    clauses: &HashMap<SentenceId, Vec<crate::types::Clause>>,
    session: Option<&str>,
) -> Result<(), KbError> {
    let sentence  = &store.sentences[store.sent_idx(sid)];
    let elements  = build_stored_elements(store, sid)?;
    let head_id   = sentence.head_symbol();

    // -- Clause dedup stage (cnf feature only) -------------------------
    //
    // For each clause produced by the clausifier we:
    //   1. Compute its canonical hash via `canonical::canonical_clause_hash`.
    //   2. Intern via `env.intern_clause` -- returns existing id on hash
    //      match, otherwise writes a new `StoredClause` and hash mapping.
    //   3. Collect the resulting ClauseIds; they become `clause_ids` on
    //      the `StoredFormula`.
    //   4. Derive a formula-level fingerprint from the *canonical*
    //      hashes (not the ClauseIds) and record it in `formula_hashes`.
    //      This must match the hash that `kb.rs::compute_formula_hash`
    //      uses at tell() time so reopen-time dedup can look up the
    //      same key.
    //
    // In `--no-default-features` / non-cnf builds none of this runs and
    // the formula is stored without dedup state.
    #[cfg(feature = "cnf")]
    let (clause_ids, canonical_hashes): (Vec<crate::types::ClauseId>, Vec<u64>) = {
        use crate::canonical;
        let per_sid = clauses.get(&sid).cloned().unwrap_or_default();
        let mut ids    = Vec::with_capacity(per_sid.len());
        let mut hashes = Vec::with_capacity(per_sid.len());
        for clause in &per_sid {
            let h  = canonical::canonical_clause_hash(clause);
            let id = env.intern_clause(wtxn, h, clause, /* sort_meta */ None)?;
            ids.push(id);
            hashes.push(h);
        }
        (ids, hashes)
    };

    let formula = StoredFormula {
        id: sid,
        elements,
        #[cfg(feature = "cnf")]
        clause_ids: clause_ids.clone(),
        session: session.map(str::to_owned),
        file:    sentence.file.clone(),
    };

    env.put_formula(wtxn, &formula)?;

    #[cfg(feature = "cnf")]
    {
        let f_hash = crate::canonical::formula_hash_from_clauses(&canonical_hashes);
        env.put_formula_hash(wtxn, f_hash, sid)?;
    }

    if let Some(pred_id) = head_id {
        env.index_head(wtxn, pred_id, sid)?;
        // Path index: ground CNF arguments from the *in-flight* clause
        // vector (not round-tripped through the DB) to avoid a
        // read-then-decode per sentence write.
        #[cfg(feature = "cnf")]
        {
            let per_sid: &[crate::types::Clause] =
                clauses.get(&sid).map(|v| v.as_slice()).unwrap_or(&[]);
            index_cnf_paths(env, wtxn, per_sid, sid)?;
        }
    }

    if let Some(s) = session {
        env.append_session(wtxn, s, sid)?;
    }

    log::debug!(target: "sumo_kb::persist",
        "write_sentence: sid={} written", sid);
    Ok(())
}

// -- Build StoredElements ------------------------------------------------------

fn build_stored_elements(
    store: &KifStore,
    sid:   SentenceId,
) -> Result<Vec<StoredElement>, KbError> {
    let sentence = &store.sentences[store.sent_idx(sid)];
    sentence.elements.iter().map(|e| build_stored_element(store, e)).collect()
}

fn build_stored_element(
    store: &KifStore,
    elem:  &Element,
) -> Result<StoredElement, KbError> {
    Ok(match elem {
        Element::Symbol(id)                       => StoredElement::Symbol(*id),
        Element::Variable { id, name, is_row }    => StoredElement::Variable {
            id: *id, name: name.clone(), is_row: *is_row,
        },
        Element::Literal(Literal::Str(s))         => StoredElement::Literal(Literal::Str(s.clone())),
        Element::Literal(Literal::Number(n))      => StoredElement::Literal(Literal::Number(n.clone())),
        Element::Op(op)                           => StoredElement::Op(op.clone()),
        Element::Sub(sub_sid) => {
            let sub_elements = build_stored_elements(store, *sub_sid)?;
            let sub_sentence = &store.sentences[store.sent_idx(*sub_sid)];
            StoredElement::Sub(Box::new(StoredFormula {
                id:         *sub_sid,
                elements:   sub_elements,
                // CNF lives only at root-formula level; sub-formulas
                // carry an empty id list.  Clause dedup for a subtree
                // is already reflected in the root's `clause_ids`
                // because clausification walks the whole tree.
                #[cfg(feature = "cnf")]
                clause_ids: Vec::new(),
                session:    None,
                file:       sub_sentence.file.clone(),
            }))
        }
    })
}

// =========================================================================
//  Phase D: semantic-layer + axiom-cache persistence
// =========================================================================
//
// These helpers are called *after* `write_axioms` has committed the
// new axiom set and bumped `kb_version`.  They open a second txn to
// serialise and commit the in-memory caches alongside the current
// counter.  Splitting into two transactions keeps the sentence-write
// path simple and lets cache-persistence failures (which only degrade
// performance, not correctness) surface as warnings rather than
// aborting the whole commit.

use crate::semantic::SemanticLayer;
use super::env::{
    CACHE_KEY_TAXONOMY,
    CachedTaxonomy,
};
#[cfg(feature = "ask")]
use super::env::{
    CACHE_KEY_SORT_ANNOT,
    CACHE_KEY_AXIOM_CACHE_TFF,
    CACHE_KEY_AXIOM_CACHE_FOF,
    CachedSortAnnotations,
    CachedAxiomProblem,
};

/// Persist the taxonomy portion of the semantic layer.  Idempotent;
/// the blob is rewritten from scratch on every call.
pub(crate) fn persist_taxonomy_cache(
    env:   &LmdbEnv,
    layer: &SemanticLayer,
) -> Result<(), KbError> {
    let mut wtxn = env.write_txn()?;
    let version = env.kb_version(unsafe {
        std::mem::transmute::<&heed::RwTxn, &heed::RoTxn>(&wtxn)
    })?;
    let blob = CachedTaxonomy {
        kb_version:           version,
        tax_edges:            layer.tax_edges_snapshot(),
        numeric_sort_cache:   layer.numeric_sort_cache_snapshot(),
        numeric_ancestor_set: layer.numeric_ancestor_set_snapshot(),
        poly_variant_symbols: layer.poly_variant_symbols_snapshot(),
        numeric_char_cache:   layer.numeric_char_cache_snapshot(),
    };
    env.put_cache(&mut wtxn, CACHE_KEY_TAXONOMY, &blob)?;
    wtxn.commit()?;
    log::info!(target: "sumo_kb::persist",
        "taxonomy cache persisted: {} edges, {} numeric classes, \
         {} numeric ancestors, {} poly variants, {} numeric chars, \
         kb_version={}",
        blob.tax_edges.len(),
        blob.numeric_sort_cache.len(),
        blob.numeric_ancestor_set.len(),
        blob.poly_variant_symbols.len(),
        blob.numeric_char_cache.len(),
        version);
    Ok(())
}

/// Persist the `SortAnnotations` derived cache.
#[cfg(feature = "ask")]
pub(crate) fn persist_sort_annotations_cache(
    env:   &LmdbEnv,
    layer: &SemanticLayer,
) -> Result<(), KbError> {
    let mut wtxn = env.write_txn()?;
    let version = env.kb_version(unsafe {
        std::mem::transmute::<&heed::RwTxn, &heed::RoTxn>(&wtxn)
    })?;
    let blob = CachedSortAnnotations {
        kb_version: version,
        sorts:      layer.sort_annotations_snapshot(),
    };
    env.put_cache(&mut wtxn, CACHE_KEY_SORT_ANNOT, &blob)?;
    wtxn.commit()?;
    log::info!(target: "sumo_kb::persist",
        "sort_annotations cache persisted: {} arg-sort entries, kb_version={}",
        blob.sorts.symbol_arg_sorts.len(), version);
    Ok(())
}

/// Report returned by `backfill_cnf_tables`.
#[cfg(feature = "cnf")]
pub(crate) struct BackfillReport {
    pub axioms_processed: usize,
    pub clauses_interned: usize,
    pub formula_hashes:   usize,
    /// Parallel to `axioms_processed` but keyed by sid; lets the caller
    /// (KnowledgeBase::open) populate the in-memory `clauses` and
    /// `fingerprints` maps without re-clausifying.
    pub clauses_by_sid:        std::collections::HashMap<SentenceId, Vec<crate::types::Clause>>,
    pub formula_hash_by_sid:   Vec<(SentenceId, u64)>,
}

/// Auto-backfill: populate Phase 4 cnf tables for every existing
/// axiom when the DB was written without `cnf` but the current build
/// has it on.
///
/// For each root sentence in `layer.store.roots`:
///
/// 1. Clausify via `cnf::sentence_to_clauses` (mutates the store to
///    intern skolems).
/// 2. Compute canonical hashes per clause + formula hash over the set.
/// 3. Intern each clause via `LmdbEnv::intern_clause` (reuses the same
///    dedup logic `write_axioms` uses on new commits).
/// 4. Write `(formula_hash -> sid)` into `DB_FORMULA_HASHES`.
/// 5. After all sids are processed, re-stamp the feature manifest
///    with the current build's features.  `kb_version` is NOT bumped
///    -- the axiom set hasn't changed, only feature-specific tables
///    have been populated, so the taxonomy / sort_annotations / axiom
///    caches remain valid.
///
/// All of this happens in a single write txn so the DB moves from the
/// "cnf missing" state to the "cnf populated" state atomically.
///
/// `StoredFormula.clause_ids` entries are NOT rewritten -- they'd
/// require reading + mutating + writing every StoredFormula.  The
/// dedup path uses `DB_FORMULA_HASHES` for the "is this a duplicate"
/// check, so leaving legacy formulas with empty `clause_ids` is
/// correct: they just don't participate in cross-formula clause
/// sharing.  Future tells / promotes will write clause_ids normally.
#[cfg(feature = "cnf")]
pub(crate) fn backfill_cnf_tables(
    env:   &LmdbEnv,
    layer: &mut crate::semantic::SemanticLayer,
) -> Result<BackfillReport, KbError> {
    use crate::canonical;
    use std::collections::HashMap;

    let axiom_sids: Vec<SentenceId> = layer.store.roots.clone();

    log::info!(target: "sumo_kb::persist",
        "cnf backfill: starting for {} axioms", axiom_sids.len());
    let t0 = std::time::Instant::now();

    let mut wtxn = env.write_txn()?;
    let mut report = BackfillReport {
        axioms_processed: 0,
        clauses_interned: 0,
        formula_hashes:   0,
        clauses_by_sid:   HashMap::new(),
        formula_hash_by_sid: Vec::new(),
    };

    let mut clausify_failures = 0usize;

    for sid in axiom_sids {
        // Clausify.  Errors (e.g. Vampire internal exception on a
        // pathological formula) are logged and the sid is skipped --
        // backfill continues for the remaining axioms.
        let clauses = match crate::cnf::sentence_to_clauses(layer, sid) {
            Ok(cs) => cs,
            Err(e) => {
                log::warn!(target: "sumo_kb::persist",
                    "cnf backfill: sid={} clausify failed: {}; skipping", sid, e);
                clausify_failures += 1;
                continue;
            }
        };

        // Intern clauses.
        let canonical_hashes: Vec<u64> = clauses.iter()
            .map(canonical::canonical_clause_hash)
            .collect();
        let mut clause_ids = Vec::with_capacity(clauses.len());
        for (i, clause) in clauses.iter().enumerate() {
            let id = env.intern_clause(&mut wtxn, canonical_hashes[i], clause, None)?;
            clause_ids.push(id);
        }

        // Formula hash.
        let fh = canonical::formula_hash_from_clauses(&canonical_hashes);
        env.put_formula_hash(&mut wtxn, fh, sid)?;

        report.axioms_processed += 1;
        report.clauses_interned += clause_ids.len();
        report.formula_hashes   += 1;
        report.clauses_by_sid.insert(sid, clauses);
        report.formula_hash_by_sid.push((sid, fh));
    }

    // Re-stamp the manifest so the next open sees `cnf: true` and
    // skips backfill.  kb_version is NOT bumped -- see the method's
    // doc for the reasoning.
    env.stamp_current_feature_manifest(&mut wtxn)?;

    wtxn.commit()?;

    log::info!(target: "sumo_kb::persist",
        "cnf backfill: done in {:?} -- {} axioms, {} clauses interned, {} formula hashes ({} clausify failures)",
        t0.elapsed(),
        report.axioms_processed,
        report.clauses_interned,
        report.formula_hashes,
        clausify_failures);

    Ok(report)
}

/// Persist an axiom cache (IR `Problem` + sid_map) for the given mode.
///
/// The IR tree is bincoded directly -- no TPTP round-trip -- thanks
/// to the `vampire-prover/serde` feature that derives
/// Serialize/Deserialize on every IR type.  See `docs/phase-d-notes.md`
/// for the benchmark justifying bincode over TPTP.
#[cfg(feature = "ask")]
pub(crate) fn persist_axiom_cache(
    env:      &LmdbEnv,
    mode_tff: bool,
    problem:  &vampire_prover::ir::Problem,
    sid_map:  &[SentenceId],
) -> Result<(), KbError> {
    let mut wtxn = env.write_txn()?;
    let version = env.kb_version(unsafe {
        std::mem::transmute::<&heed::RwTxn, &heed::RoTxn>(&wtxn)
    })?;
    let blob = CachedAxiomProblem {
        kb_version: version,
        mode_tff,
        problem:    problem.clone(),
        sid_map:    sid_map.to_vec(),
    };
    let key = if mode_tff { CACHE_KEY_AXIOM_CACHE_TFF } else { CACHE_KEY_AXIOM_CACHE_FOF };
    env.put_cache(&mut wtxn, key, &blob)?;
    wtxn.commit()?;
    log::info!(target: "sumo_kb::persist",
        "axiom cache persisted ({}): {} axioms bincoded, kb_version={}",
        if mode_tff { "TFF" } else { "FOF" },
        sid_map.len(), version);
    Ok(())
}

// -- Path index from CNF clauses -----------------------------------------------

#[cfg(feature = "cnf")]
fn index_cnf_paths(
    env:        &LmdbEnv,
    wtxn:       &mut heed::RwTxn,
    clauses:    &[crate::types::Clause],
    formula_id: SentenceId,
) -> Result<(), KbError> {
    use crate::types::CnfTerm;

    for clause in clauses {
        for lit in &clause.literals {
            let pred_id = match &lit.pred {
                CnfTerm::Const(id) => *id,
                _                  => continue,
            };
            for (pos, arg) in lit.args.iter().enumerate() {
                if let CnfTerm::Const(sym_id) = arg {
                    if pos > u16::MAX as usize { break; }
                    env.index_path(wtxn, pred_id, pos as u16, *sym_id, formula_id)?;
                }
            }
        }
    }
    Ok(())
}
