/// sumo-store — LMDB-backed persistent storage for the SUMO knowledge base.
///
/// This crate sits between `sumo-parser-core` (in-memory parse/validate) and
/// `sumo-native` (CLI).  Its responsibilities are:
///
/// 1. **Symbol interning** — assign and persist stable 64-bit `SymbolId`s via
///    LMDB so that IDs survive process restarts.
/// 2. **Formula storage** — persist formulas in two forms: the original element
///    representation (for KifStore reconstruction / semantic validation) and
///    pre-computed CNF clauses (for theorem-prover queries).
/// 3. **Path indexing** — index each ground literal at each argument position
///    for fast predicate-argument lookups during future unification.
/// 4. **Transactional tell()** — LMDB write transactions provide atomic
///    commit/rollback for incremental KB updates.

pub mod schema;
pub mod cnf;
pub mod env;
pub mod path_index;
pub mod commit;
pub mod load;
pub mod tptp_cnf;
pub mod display;

pub use env::LmdbEnv;
pub use schema::{
    FormulaId, StoredFormula, StoredElement, StoredSymbol, Clause, CnfLiteral, CnfTerm,
};
pub use commit::{CommitOptions, commit_kifstore};
pub use load::load_kifstore_from_db;
pub use tptp_cnf::db_to_tptp_cnf;
pub use display::{clause_to_kif, clauses_to_kif};

/// Error type for store operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("LMDB error: {0}")]
    Lmdb(#[from] heed::Error),

    #[error("serialisation error: {0}")]
    Serialise(String),

    #[error("CNF clause count exceeded limit ({limit}) for formula — add more CNF headroom \
             via --max-clauses or SUMO_MAX_CLAUSES")]
    ClauseCountExceeded { limit: usize },

    #[error("database not found at path '{path}' — run `sumo validate -f <files>` first")]
    DatabaseNotFound { path: String },

    #[error("{0}")]
    Other(String),
}
