// crates/sumo-kb/src/persist/mod.rs
//
// Persistence module -- LMDB-backed storage for KifStore formulas and symbols.
// Enabled via `--features persist`.

mod env;
mod path_index;
mod commit;
mod load;

pub(crate) use env::LmdbEnv;
pub(crate) use env::{
    CachedTaxonomy,
    CACHE_KEY_TAXONOMY,
};
#[cfg(feature = "ask")]
pub(crate) use env::{
    CachedSortAnnotations,
    CACHE_KEY_SORT_ANNOT,
};
pub(crate) use commit::{
    write_axioms,
    persist_taxonomy_cache,
};
#[cfg(feature = "ask")]
pub(crate) use commit::persist_sort_annotations_cache;
pub(crate) use load::load_from_db;
