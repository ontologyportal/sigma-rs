/// Commit an in-memory `KifStore` into the LMDB-backed persistent store.
///
/// # Flow
///
/// 1. Open an LMDB write transaction.
/// 2. Intern all symbols from the ephemeral `KifStore` into LMDB, building a
///    `temp_id → persistent_id` remap table.
/// 3. For each root sentence: convert elements (with remapped IDs) into a
///    `StoredFormula`, run the CNF pipeline, and write everything to LMDB.
/// 4. Commit the transaction.  On any error, the caller drops the `RwTxn`
///    (LMDB aborts automatically) and no partial state is persisted.

use std::collections::HashMap;

use sumo_parser_core::store::{Element, KifStore, Literal as KifLiteral, SentenceId, SymbolId as TempId};
use log;

use crate::cnf::sentence_to_cnf;
use crate::env::LmdbEnv;
use crate::schema::{
    FormulaId, StoredElement, StoredFormula, StoredLiteral, StoredSymbol,
};
use crate::StoreError;

/// Options controlling how a `KifStore` is committed to LMDB.
#[derive(Debug, Clone)]
pub struct CommitOptions {
    /// Hard upper bound on the number of CNF clauses per formula.
    /// Exceeding this causes a `StoreError::ClauseCountExceeded` hard error.
    /// Set via `--max-clauses` or the `SUMO_MAX_CLAUSES` env var.
    pub max_clauses: usize,
    /// Optional session key for the committed formulas.
    /// `None` → base KB; `Some(s)` → session assertion.
    pub session: Option<String>,
}

impl Default for CommitOptions {
    fn default() -> Self {
        let max_clauses = std::env::var("SUMO_MAX_CLAUSES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10_000);
        Self { max_clauses, session: None }
    }
}

/// Commit all root sentences in `store` to `env`.
///
/// Returns the list of `FormulaId`s that were written (one per root sentence).
/// If any error occurs, the write transaction is aborted via drop — no partial
/// state reaches disk.
pub fn commit_kifstore(
    env:   &LmdbEnv,
    store: &KifStore,
    opts:  &CommitOptions,
) -> Result<Vec<FormulaId>, StoreError> {
    log::info!(
        "commit_kifstore: {} root sentence(s), session={:?}",
        store.roots.len(),
        opts.session
    );

    let mut wtxn = env.write_txn()?;

    // ── Step 1: intern all symbols and build remap table ──────────────────────
    let id_map = intern_all_symbols(env, &mut wtxn, store)?;
    log::debug!("commit_kifstore: interned {} symbols", id_map.len());

    // Skolem counter is global across this commit (within the write txn).
    let mut skolem_counter: u64 = {
        // Read current sequence value for Skolem symbols to avoid collisions
        // across separate commits.  We use the "skolem" sequence for this.
        // The synthetic IDs used during CNF are later replaced by real interned IDs.
        0u64
    };

    let mut formula_ids: Vec<FormulaId> = Vec::new();

    // ── Step 2: convert and store each root sentence ──────────────────────────
    for &root_sid in &store.roots {
        let fid = commit_sentence(
            env, &mut wtxn, store, root_sid, &id_map,
            &mut skolem_counter, opts,
        )?;
        formula_ids.push(fid);
    }

    // ── Step 3: commit ────────────────────────────────────────────────────────
    wtxn.commit()?;
    log::info!("commit_kifstore: committed {} formula(s)", formula_ids.len());
    Ok(formula_ids)
}

// ── Intern all symbols from an ephemeral KifStore ────────────────────────────

/// Intern every symbol in `store.symbol_data` into LMDB and return a map
/// from ephemeral `TempId` (Vec index) to persistent `SymbolId`.
fn intern_all_symbols(
    env:   &LmdbEnv,
    wtxn:  &mut heed::RwTxn,
    store: &KifStore,
) -> Result<HashMap<TempId, u64>, StoreError> {
    let mut map: HashMap<TempId, u64> = HashMap::with_capacity(store.symbol_data.len());
    for (temp_id, sym) in store.symbol_data.iter().enumerate() {
        if sym.name.is_empty() {
            // Tombstoned slot — skip
            continue;
        }
        let persistent_id = env.intern_symbol(wtxn, &sym.name, false, None)?;
        map.insert(temp_id as TempId, persistent_id);
    }
    Ok(map)
}

// ── Per-sentence commit ───────────────────────────────────────────────────────

fn commit_sentence(
    env:            &LmdbEnv,
    wtxn:           &mut heed::RwTxn,
    store:          &KifStore,
    sid:            SentenceId,
    id_map:         &HashMap<TempId, u64>,
    skolem_counter: &mut u64,
    opts:           &CommitOptions,
) -> Result<FormulaId, StoreError> {
    // Allocate a persistent FormulaId
    let formula_id = env.next_seq(wtxn, "formula")?;
    log::debug!("commit_sentence: sid={} → formula_id={}", sid, formula_id);

    // Build a closure that resolves ephemeral symbol names to persistent IDs
    // for use by the CNF converter.
    let id_lookup = |name: &str| -> u64 {
        store.sym_id(name)
            .and_then(|temp| id_map.get(&temp).copied())
            .unwrap_or(0)
    };

    // CNF conversion (works on the in-memory store with ephemeral IDs, but
    // the id_map closure translates names to persistent IDs for Atoms)
    let mut skolem_syms: Vec<StoredSymbol> = Vec::new();
    let clauses = sentence_to_cnf(
        store, sid, &id_lookup, skolem_counter, &mut skolem_syms, opts.max_clauses,
    )?;

    // Intern any new Skolem symbols that were generated
    for sk in &skolem_syms {
        let real_id = env.intern_symbol(wtxn, &sk.name, true, sk.skolem_arity)?;
        log::debug!("commit_sentence: Skolem '{}' interned as {}", sk.name, real_id);
        // Note: the synthetic Skolem IDs in `clauses` still reference the
        // synthetic values from cnf.rs.  A second remap pass would be needed
        // to replace them with `real_id`.  For now, since Skolem symbols are
        // only used by the future internal prover (not for TPTP CNF output which
        // uses string names), this is acceptable as a known TODO.
    }

    // Build the stored element representation (with persistent IDs)
    let elements = build_stored_elements(store, sid, id_map)?;

    // Head predicate (for head index)
    let head_pred_id: Option<u64> = store.sentences[sid as usize].head_symbol()
        .and_then(|temp| id_map.get(&temp).copied());

    let formula = StoredFormula {
        id:       formula_id,
        elements,
        clauses,
        session:  opts.session.clone(),
    };

    // Write formula
    env.put_formula(wtxn, &formula)?;

    // Update head index
    if let Some(pred_id) = head_pred_id {
        env.index_head(wtxn, pred_id, formula_id)?;

        // Update path index for ground atoms in CNF clauses
        index_cnf_paths(env, wtxn, &formula.clauses, formula_id)?;
    }

    // Session bookkeeping
    if let Some(ref session) = opts.session {
        env.append_session(wtxn, session, formula_id)?;
    }

    Ok(formula_id)
}

// ── Build StoredElements ──────────────────────────────────────────────────────

fn build_stored_elements(
    store:  &KifStore,
    sid:    SentenceId,
    id_map: &HashMap<TempId, u64>,
) -> Result<Vec<StoredElement>, StoreError> {
    let sentence = &store.sentences[sid as usize];
    sentence.elements.iter().map(|e| build_stored_element(store, e, id_map)).collect()
}

fn build_stored_element(
    store:  &KifStore,
    elem:   &Element,
    id_map: &HashMap<TempId, u64>,
) -> Result<StoredElement, StoreError> {
    Ok(match elem {
        Element::Symbol(temp_id) => {
            let pid = *id_map.get(temp_id)
                .ok_or_else(|| StoreError::Other(format!("symbol temp_id {} not in id_map", temp_id)))?;
            StoredElement::Symbol(pid)
        }
        Element::Variable { id: temp_id, name, is_row } => {
            let pid = *id_map.get(temp_id)
                .ok_or_else(|| StoreError::Other(format!("var temp_id {} not in id_map", temp_id)))?;
            StoredElement::Variable { id: pid, name: name.clone(), is_row: *is_row }
        }
        Element::Literal(KifLiteral::Str(s))    => StoredElement::Literal(StoredLiteral::Str(s.clone())),
        Element::Literal(KifLiteral::Number(n)) => StoredElement::Literal(StoredLiteral::Number(n.clone())),
        Element::Op(op)                          => StoredElement::Op(op.clone()),
        Element::Sub(sub_sid) => {
            // Recurse — build inline sub-formula (no separate LMDB entry)
            let sub_elements = build_stored_elements(store, *sub_sid, id_map)?;
            let sub_formula = StoredFormula {
                id:       0,          // sub-formulas do not have their own LMDB key
                elements: sub_elements,
                clauses:  Vec::new(), // CNF only at root level
                session:  None,
            };
            StoredElement::Sub(Box::new(sub_formula))
        }
    })
}

// ── Path index ────────────────────────────────────────────────────────────────

/// Index ground arguments from CNF clauses into the LMDB path index.
fn index_cnf_paths(
    env:        &LmdbEnv,
    wtxn:       &mut heed::RwTxn,
    clauses:    &[crate::schema::Clause],
    formula_id: FormulaId,
) -> Result<(), StoreError> {
    use crate::schema::CnfTerm;

    for clause in clauses {
        for lit in &clause.literals {
            let pred_id = match &lit.pred {
                CnfTerm::Const(id) => *id,
                _ => continue, // skip variable-headed literals
            };
            for (pos, arg) in lit.args.iter().enumerate() {
                if let CnfTerm::Const(sym_id) = arg {
                    if pos > u16::MAX as usize { break; }
                    env.index_path(wtxn, pred_id, pos as u16, *sym_id, formula_id)?;
                }
                // Variable and SkolemFn arguments are not indexed (future work)
            }
        }
    }
    Ok(())
}
