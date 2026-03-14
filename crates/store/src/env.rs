/// LMDB environment and named database handles.
use std::path::Path;

use heed::types::{Bytes, SerdeBincode, Str};
use heed::{Database, Env, EnvOpenOptions, RoTxn, RwTxn};
use log;

use crate::schema::{FormulaId, StoredFormula, StoredSymbol};
use crate::StoreError;

// ── Database names ────────────────────────────────────────────────────────────

const DB_SYMBOLS_FWD: &str = "symbols_fwd";  // name  → SymbolId (u64 BE bytes)
const DB_SYMBOLS_REV: &str = "symbols_rev";  // id BE → StoredSymbol
const DB_FORMULAS:    &str = "formulas";     // id BE → StoredFormula
const DB_PATH_INDEX:  &str = "path_index";   // 18-byte key → Vec<FormulaId>
const DB_HEAD_INDEX:  &str = "head_index";   // 8-byte pred_id → Vec<FormulaId>
const DB_SESSIONS:    &str = "sessions";     // session name → Vec<FormulaId>
const DB_SEQUENCES:   &str = "sequences";    // "sym" | "formula" | "skolem" → u64

/// The number of named databases we open.
const MAX_DBS: u32 = 8;

/// 10 GiB map size — LMDB will not actually allocate this; it is the maximum
/// the virtual address space can grow to.
const MAP_SIZE: usize = 10 * 1024 * 1024 * 1024;

// ── LmdbEnv ───────────────────────────────────────────────────────────────────

/// Wraps an LMDB `Env` together with handles to all named databases.
pub struct LmdbEnv {
    pub(crate) env:          Env,
    /// name → SymbolId encoded as 8-byte big-endian
    pub(crate) symbols_fwd:  Database<Str, Bytes>,
    /// SymbolId (8-byte BE key) → StoredSymbol
    pub(crate) symbols_rev:  Database<Bytes, SerdeBincode<StoredSymbol>>,
    /// FormulaId (8-byte BE key) → StoredFormula
    pub(crate) formulas:     Database<Bytes, SerdeBincode<StoredFormula>>,
    /// 18-byte path-index key → sorted Vec<FormulaId>
    pub(crate) path_index:   Database<Bytes, SerdeBincode<Vec<u64>>>,
    /// pred_id (8-byte BE key) → sorted Vec<FormulaId>
    pub(crate) head_index:   Database<Bytes, SerdeBincode<Vec<u64>>>,
    /// session name → Vec<FormulaId>
    pub(crate) sessions:     Database<Str, SerdeBincode<Vec<u64>>>,
    /// counter name → u64 (stored as 8-byte BE)
    pub(crate) sequences:    Database<Str, Bytes>,
}

impl LmdbEnv {
    /// Open (or create) the LMDB environment at `path`.
    ///
    /// Creates all named databases if they do not yet exist and commits the
    /// initialisation transaction.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        log::info!("Opening LMDB database at {}", path.display());
        std::fs::create_dir_all(path).map_err(|e| {
            StoreError::Other(format!("cannot create DB directory {}: {}", path.display(), e))
        })?;

        // SAFETY: We ensure only one Env is opened per path per process.
        let env = unsafe {
            EnvOpenOptions::new()
                .max_dbs(MAX_DBS)
                .map_size(MAP_SIZE)
                .open(path)
        }.map_err(|e| StoreError::Other(format!("cannot open LMDB at {}: {}", path.display(), e)))?;

        let mut wtxn = env.write_txn()?;

        let symbols_fwd = env.create_database::<Str, Bytes>(&mut wtxn, Some(DB_SYMBOLS_FWD))?;
        let symbols_rev = env.create_database::<Bytes, SerdeBincode<StoredSymbol>>(&mut wtxn, Some(DB_SYMBOLS_REV))?;
        let formulas    = env.create_database::<Bytes, SerdeBincode<StoredFormula>>(&mut wtxn, Some(DB_FORMULAS))?;
        let path_index  = env.create_database::<Bytes, SerdeBincode<Vec<u64>>>(&mut wtxn, Some(DB_PATH_INDEX))?;
        let head_index  = env.create_database::<Bytes, SerdeBincode<Vec<u64>>>(&mut wtxn, Some(DB_HEAD_INDEX))?;
        let sessions    = env.create_database::<Str, SerdeBincode<Vec<u64>>>(&mut wtxn, Some(DB_SESSIONS))?;
        let sequences   = env.create_database::<Str, Bytes>(&mut wtxn, Some(DB_SEQUENCES))?;

        wtxn.commit()?;
        log::debug!("LMDB environment opened; {} named databases initialised", MAX_DBS);

        Ok(Self { env, symbols_fwd, symbols_rev, formulas, path_index, head_index, sessions, sequences })
    }

    // ── Transaction helpers ───────────────────────────────────────────────────

    pub fn read_txn(&self) -> Result<RoTxn<'_>, StoreError> {
        Ok(self.env.read_txn()?)
    }

    pub fn write_txn(&self) -> Result<RwTxn<'_>, StoreError> {
        Ok(self.env.write_txn()?)
    }

    // ── Sequence helpers ──────────────────────────────────────────────────────

    /// Atomically allocate the next value from a named sequence.
    pub fn next_seq(&self, wtxn: &mut RwTxn, name: &str) -> Result<u64, StoreError> {
        let current = match self.sequences.get(wtxn, name)? {
            Some(bytes) => u64::from_be_bytes(bytes.try_into().map_err(|_| StoreError::Other("bad sequence bytes".into()))?),
            None => 0,
        };
        let next = current + 1;
        self.sequences.put(wtxn, name, &next.to_be_bytes())?;
        Ok(current)
    }

    // ── Symbol helpers ────────────────────────────────────────────────────────

    /// Look up a symbol by name.  Returns `None` if not yet interned.
    pub fn get_symbol_id(&self, txn: &RoTxn, name: &str) -> Result<Option<u64>, StoreError> {
        match self.symbols_fwd.get(txn, name)? {
            Some(bytes) => {
                let arr: [u8; 8] = bytes.try_into()
                    .map_err(|_| StoreError::Other("bad symbol id bytes".into()))?;
                Ok(Some(u64::from_be_bytes(arr)))
            }
            None => Ok(None),
        }
    }

    /// Look up or create a symbol, returning its persistent `SymbolId`.
    pub fn intern_symbol(
        &self,
        wtxn: &mut RwTxn,
        name: &str,
        is_skolem: bool,
        skolem_arity: Option<usize>,
    ) -> Result<u64, StoreError> {
        // Try read first (within the write txn — heed allows this)
        let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(wtxn) };
        if let Some(id) = self.get_symbol_id(rtxn, name)? {
            log::trace!("intern_symbol: '{}' already exists with id {}", name, id);
            return Ok(id);
        }
        let id = self.next_seq(wtxn, "sym")?;
        log::debug!("intern_symbol: '{}' → {}", name, id);
        self.symbols_fwd.put(wtxn, name, &id.to_be_bytes())?;
        let sym = StoredSymbol { id, name: name.to_owned(), is_skolem, skolem_arity };
        self.symbols_rev.put(wtxn, &id.to_be_bytes(), &sym)?;
        Ok(id)
    }

    /// Write a `StoredFormula` to the `formulas` database.
    pub fn put_formula(
        &self,
        wtxn: &mut RwTxn,
        formula: &StoredFormula,
    ) -> Result<(), StoreError> {
        log::debug!("put_formula: storing formula {}", formula.id);
        self.formulas.put(wtxn, &formula.id.to_be_bytes(), formula)?;
        Ok(())
    }

    /// Append a `FormulaId` to the head-predicate index for `pred_id`.
    pub fn index_head(
        &self,
        wtxn: &mut RwTxn,
        pred_id: u64,
        formula_id: FormulaId,
    ) -> Result<(), StoreError> {
        let key = pred_id.to_be_bytes();
        let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(wtxn) };
        let mut ids: Vec<u64> = self.head_index.get(rtxn, &key)?.unwrap_or_default();
        ids.push(formula_id);
        self.head_index.put(wtxn, &key, &ids)?;
        Ok(())
    }

    /// Append a `FormulaId` to the path-index entry for `(pred_id, arg_pos, sym_id)`.
    pub fn index_path(
        &self,
        wtxn: &mut RwTxn,
        pred_id: u64,
        arg_pos: u16,
        sym_id:  u64,
        formula_id: FormulaId,
    ) -> Result<(), StoreError> {
        let key = crate::path_index::encode_key(pred_id, arg_pos, sym_id);
        let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(wtxn) };
        let mut ids: Vec<u64> = self.path_index.get(rtxn, &key)?.unwrap_or_default();
        ids.push(formula_id);
        self.path_index.put(wtxn, &key, &ids)?;
        Ok(())
    }

    /// Append a `FormulaId` to the session list.
    pub fn append_session(
        &self,
        wtxn: &mut RwTxn,
        session: &str,
        formula_id: FormulaId,
    ) -> Result<(), StoreError> {
        let rtxn = unsafe { std::mem::transmute::<&RwTxn, &RoTxn>(wtxn) };
        let mut ids: Vec<u64> = self.sessions.get(rtxn, session)?.unwrap_or_default();
        ids.push(formula_id);
        self.sessions.put(wtxn, session, &ids)?;
        Ok(())
    }

    // ── Read helpers ──────────────────────────────────────────────────────────

    /// Load all stored formulas in insertion order.
    pub fn all_formulas(&self, txn: &RoTxn) -> Result<Vec<StoredFormula>, StoreError> {
        let mut formulas = Vec::new();
        let iter = self.formulas.iter(txn)?;
        for result in iter {
            let (_, formula) = result?;
            formulas.push(formula);
        }
        log::debug!("all_formulas: loaded {} formula(s) from DB", formulas.len());
        Ok(formulas)
    }

    /// Load formulas belonging to a specific session.
    pub fn session_formulas(
        &self,
        txn: &RoTxn,
        session: &str,
    ) -> Result<Vec<StoredFormula>, StoreError> {
        let ids: Vec<u64> = self.sessions.get(txn, session)?.unwrap_or_default();
        let mut out = Vec::new();
        for id in ids {
            if let Some(f) = self.formulas.get(txn, &id.to_be_bytes())? {
                out.push(f);
            }
        }
        Ok(out)
    }

    /// Load all known symbol names and their persistent IDs.
    pub fn all_symbols(&self, txn: &RoTxn) -> Result<Vec<StoredSymbol>, StoreError> {
        let mut syms = Vec::new();
        let iter = self.symbols_rev.iter(txn)?;
        for result in iter {
            let (_, sym) = result?;
            syms.push(sym);
        }
        Ok(syms)
    }
}
