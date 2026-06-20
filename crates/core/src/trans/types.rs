// crates/core/src/trans/types.rs
//
// Shared data types for the translation layer.
//
// Surfaced through the crate-level `types.rs` facade (`crate::types`).  The
// layer's larger types stay with their subsystem: `Sort` in `sort.rs`,
// `SortAnnotations`/`SymbolSortAnnotation` in `annotations.rs`, `ArithCond` in
// `arith.rs`, and the TPTP IR types in `ir/`.

use super::ir;

/// A converted formula together with the TFF preamble declarations that were
/// registered as side-effects of its conversion.
///
/// For TFF-mode entries all three `*_decls` vecs are populated.
/// For FOF-mode entries all three `*_decls` vecs are always empty —
/// FOF emits no type declarations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct CachedFormula {
    pub formula:    ir::Formula,
    pub sort_decls: Vec<ir::Sort>,
    pub fn_decls:   Vec<ir::Function>,
    pub pred_decls: Vec<ir::Predicate>,
}
