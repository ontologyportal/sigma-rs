// crates/sumo-kb/src/persist/env.rs
//
// LMDB environment and named database handles.
// Ported from sumo-store/src/env.rs.
// Changes:
//   - StoreError → KbError
//   - StoredSymbol/StoredFormula are defined here (no separate schema.rs)
//   - No `next_seq("sym")` / `next_seq("formula")`: stable IDs from KifStore counters
//     are written directly; we only use a sequence for Skolem symbols.

use std::path::Path;

use heed::types::{Bytes, SerdeBincode, Str};
use heed::{Database, Env, EnvOpenOptions, RoTxn, RwTxn};
use serde::{Deserialize, Serialize};

use crate::error::KbError;
use crate::types::{Literal, SentenceId, SymbolId};
use crate::tokenizer::OpKind;

// ── Stored types ──────────────────────────────────────────────────────────────

/// A symbol as stored in LMDB (stable ID, no in-memory sentence lists).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredSymbol {
    pub id:           SymbolId,
    pub name:         String,
    pub is_skolem:    bool,
    pub skolem_arity: Option<usize>,
}

/// A formula element as stored in LMDB — sub-sentences stored inline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum StoredElement {
    Symbol(SymbolId),
    Variable { id: SymbolId, name: String, is_row: bool },
    Literal(Literal),
    /// Inline sub-formula (was `Element::Sub(SentenceId)` in-memory).
    Sub(Box<StoredFormula>),
    Op(OpKind),
}

/// A formula as stored in LMDB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredFormula {
    /// Stable `SentenceId` assigned by `KifStore`.
    pub id:       SentenceId,
    pub elements: Vec<StoredElement>,
    /// Pre-computed CNF clauses (only present when `cnf` feature was enabled at commit time).
    #[cfg(feature = "cnf")]
    pub clauses:  Vec<crate::types::Clause>,
    /// Session tag; `None` for promoted axioms.
    pub session:  Option<String>,
    /// File/session tag used to group sentences in `KifStore`.
    pub file:     String,
}

// ── Database names ────────────────────────────────────────────────────────────

const DB_SYMBOLS_FWD: &str = "symbols_fwd";  // name → id (8-byte BE)
const DB_SYMBOLS_REV: &str = "symbols_rev";  // id BE → StoredSymbol
const DB_FORMULAS:    &str = "formulas";     // id BE → StoredFormula
const DB_PATH_INDEX:  &str = "path_index";   // 18-byte key → Vec<SentenceId>
const DB_HEAD_INDEX:  &str = "head_index";   // pred_id (8-byte BE) → Vec<SentenceId>
const DB_SESSIONS:    &str = "sessions";     // session name → Vec<SentenceId>
const DB_SEQUENCES:   &str = "sequences";    // "skolem" → u64 (Skolem counter only)

const MAX_DBS:  u32   = 8;
const MAP_SIZE: usize = 10 * 1024 * 1024 * 1024; // 10 GiB virtual

// ── LmdbEnv ───────────────────────────────────────────────────────────────────

pub(crate) struct LmdbEnv {
    pub env:         Env,
    pub symbols_fwd: Database<Str, Bytes>,
    pub symbols_rev: Database<Bytes, SerdeBincode<StoredSymbol>>,
    pub formulas:    Database<Bytes, SerdeBincode<StoredFormula>>,
    pub path_index:  Database<Bytes, SerdeBincode<Vec<u64>>>,
    pub head_index:  Database<Bytes, SerdeBincode<Vec<u64>>>,
    pub sessions:    Database<Str, SerdeBincode<Vec<u64>>>,
}

impl LmdbEnv {
    pub(crate) fn open(path: &Path) -> Result<Self, KbError> {
        log::info!(target: "sumo_kb::persist", "opening LMDB at {}", path.display());
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

        let mut wtxn = env.write_txn()?;
        let symbols_fwd = env.create_database::<Str, Bytes>(&mut wtxn, Some(DB_SYMBOLS_FWD))?;
        let symbols_rev = env.create_database::<Bytes, SerdeBincode<StoredSymbol>>(&mut wtxn, Some(DB_SYMBOLS_REV))?;
        let formulas    = env.create_database::<Bytes, SerdeBincode<StoredFormula>>(&mut wtxn, Some(DB_FORMULAS))?;
        let path_index  = env.create_database::<Bytes, SerdeBincode<Vec<u64>>>(&mut wtxn, Some(DB_PATH_INDEX))?;
        let head_index  = env.create_database::<Bytes, SerdeBincode<Vec<u64>>>(&mut wtxn, Some(DB_HEAD_INDEX))?;
        let sessions    = env.create_database::<Str, SerdeBincode<Vec<u64>>>(&mut wtxn, Some(DB_SESSIONS))?;
        let _sequences  = env.create_database::<Str, Bytes>(&mut wtxn, Some(DB_SEQUENCES))?;
        wtxn.commit()?;
        log::debug!(target: "sumo_kb::persist", "LMDB opened; {} databases initialised", MAX_DBS);

        Ok(Self { env, symbols_fwd, symbols_rev, formulas, path_index, head_index, sessions })
    }

    pub(crate) fn read_txn(&self) -> Result<RoTxn<'_>, KbError> {
        Ok(self.env.read_txn()?)
    }

    pub(crate) fn write_txn(&self) -> Result<RwTxn<'_>, KbError> {
        Ok(self.env.write_txn()?)
    }

    // ── Symbol helpers ────────────────────────────────────────────────────────

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
            log::trace!(target: "sumo_kb::persist",
                "put_symbol: '{}' already in DB", sym.name);
            return Ok(());
        }
        log::trace!(target: "sumo_kb::persist",
            "put_symbol: '{}' id={}", sym.name, sym.id);
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
        log::trace!(target: "sumo_kb::persist",
            "put_formula: id={}", formula.id);
        self.formulas.put(wtxn, &formula.id.to_be_bytes(), formula)?;
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

    // ── Read helpers ──────────────────────────────────────────────────────────

    pub(crate) fn all_formulas(&self, txn: &RoTxn) -> Result<Vec<StoredFormula>, KbError> {
        let mut out = Vec::new();
        for result in self.formulas.iter(txn)? {
            let (_, formula) = result?;
            out.push(formula);
        }
        log::debug!(target: "sumo_kb::persist",
            "all_formulas: {} formula(s) loaded", out.len());
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

}
