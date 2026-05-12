//! Per-cache behavior objects for the SemanticLayer.
//!
//! Each file defines one cache: its `*Behavior` implementer plus the thin
//! `impl SemanticLayer` accessor that delegates to it. `SemanticLayer` holds
//! the corresponding `Cache<B>` / `WholeCache<B>` / `Eager<B>` fields.

// Taxonomy structure
pub(crate) mod tax_edges;

// Validation cache
pub(crate) mod validate;

// IS-A queries
pub(crate) mod is_instance;
pub(crate) mod is_class;
pub(crate) mod is_relation;
pub(crate) mod is_predicate;
pub(crate) mod is_function;
pub(crate) mod has_ancestor;

// Relation metadata
pub(crate) mod arity;
pub(crate) mod subrel_lattice;
pub(crate) mod trans_reach;
pub(crate) mod domain;
pub(crate) mod range;

// Documentation
pub(crate) mod documentation;

// Type inference
pub(crate) mod inferred_class;

#[cfg(test)]
pub(crate) mod test_support;
