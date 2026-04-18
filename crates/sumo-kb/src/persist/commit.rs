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

    // -- 3. Commit -------------------------------------------------------------
    wtxn.commit()?;
    log::info!(target: "sumo_kb::persist",
        "write_axioms: committed {} sentence(s)", sids.len());
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
