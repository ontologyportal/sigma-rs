// crates/core/src/syntactic/pattern/mod.rs
//
// Typed structural pattern matching for `SyntacticLayer` sentences.
//
//   types   -- MatchKey / PatternElement / SentencePattern / Bindings + helpers
//   matcher -- PatternMatcher: match patterns against stored sentences
//   build   -- construct a SentencePattern from a KIF string
//
// Consumed by the rewrite pass (`trans/rewrite.rs`) and the KB store layer.

mod types;
mod matcher;
mod build;
#[cfg(test)]
mod tests;

// Externally-used pattern items (rewrite pass, KB store).  `instantiate_pattern`,
// `elements_eq_*`, and `PatternMatcher` are internal to this module (and its
// tests), reached via the submodule paths directly.  `Bindings` is re-exported
// because scope-aware callers (semantic type inference) name it in the return
// type of their `find_by_pattern_sub*` wrappers.
pub(crate) use types::{Bindings, MatchKey, PatternElement, SentencePattern};
pub(crate) use build::PatternFromKifError;
