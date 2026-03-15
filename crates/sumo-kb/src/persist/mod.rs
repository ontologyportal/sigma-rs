// crates/sumo-kb/src/persist/mod.rs
//
// Persistence module — LMDB-backed storage for KifStore formulas and symbols.
// Enabled via `--features persist`.

mod env;
mod path_index;
mod commit;
mod load;

pub(crate) use env::LmdbEnv;
pub(crate) use commit::write_axioms;
pub(crate) use load::load_from_db;
