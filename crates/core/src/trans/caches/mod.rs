// crates/core/src/trans/caches/mod.rs
//
// Per-cache behavior objects for the TranslationLayer.
//
// Each file defines one cache: its `*Behavior` implementer (generate +
// react_to_delta) plus any thin accessor wrapper.  Heavy compute helpers stay
// with their subsystem (`sort.rs`, `annotations.rs`, `numeric.rs`,
// `formulas.rs`); each behavior's `generate` delegates to them.
//
// Variant mapping:
//   Cache<B>     : symbol_sort, formulas_tff, formulas_fof, relation_sorts (lazy keyed)
//   WholeCache<B>: sort_annotations, numeric_ancestor_set, poly_variant_symbols (lazy/install whole)
//   EagerMap<B>  : numeric_sorts (eager keyed, built by prime_caches)

pub(crate) mod formulas_thf;
pub(crate) mod ho_signatures;
pub(crate) mod symbol_sort;
pub(crate) mod formulas_tff;
pub(crate) mod formulas_fof;
pub(crate) mod sort_annotations;
pub(crate) mod numeric_sorts;
pub(crate) mod numeric_ancestor_set;
pub(crate) mod poly_variant_symbols;
pub(crate) mod rewrite_rules;
