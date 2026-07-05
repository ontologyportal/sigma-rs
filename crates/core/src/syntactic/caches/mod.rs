// crates/core/src/syntactic/caches/mod.rs
//
// Per-cache behavior objects for the SyntacticLayer.
//
// Each file defines one cache: its `*Behavior` implementer plus any thin 1:1
// accessor wrapper.  These caches are *eagerly maintained* — there is no
// compute-on-miss — so the heavy maintenance lives with its subsystem:
// occurrence/head indexing in `index.rs` / `sentence.rs` / `remove.rs`, and
// SInE in `sine.rs`.  The cache files hold the behavior (NAME + react_to_delta)
// and the accessor that belongs 1:1 with the cache (e.g. `by_head`).
//
// `normal_implications` / `impl_sym_index` are intentionally NOT migrated yet:
// their build needs `&mut self` (it synthesises sentences) and they form a
// cross-cache relationship that warrants its own pass.  They remain raw
// `LayerCache` fields for now.

pub(crate) mod sentences;
pub(crate) mod session;
pub(crate) mod source;
pub(crate) mod symbol;
pub(crate) mod occurrences;
pub(crate) mod residue_index;
pub(crate) mod axiom_index;
pub(crate) mod sine_index;

// Toggleable compute caches — disabled by default (transparent getters).
pub(crate) mod sentence_symbols;
pub(crate) mod sentence_vars;

// Lazy compute cache, ENABLED by default (content-addressed facts can
// never go stale; reactive eviction is memory hygiene).
pub(crate) mod term_facts;
