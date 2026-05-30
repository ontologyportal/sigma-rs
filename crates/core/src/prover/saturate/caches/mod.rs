// crates/core/src/saturate/caches/
//
// Per-cache behaviors owned by the ProverLayer.  One file per cache,
// mirroring `semantics/caches/` and `trans/caches/`.

pub(crate) mod clause_store;
pub(crate) mod model_registry;
pub(crate) mod fingerprint;