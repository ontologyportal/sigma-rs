// crates/sumo-kb/src/tptp/mod.rs
//
// TPTP (FOF/TFF) output generation.
//
// Ported from sumo-parser-core/src/tptp.rs.
// Changes: `&KnowledgeBase` → `&SemanticLayer`; raw `sentences[sid as usize]`
// → `sentences[store.sent_idx(sid)]`; session filtering decoupled.

mod options;
mod names;
mod tff;
mod translate;
pub mod kif;

#[cfg(test)]
mod tests;

// ── Public API ────────────────────────────────────────────────────────────────

pub use options::{TptpLang, TptpOptions};
pub use names::{TPTP_SYMBOL_PREFIX, TPTP_VARIABLE_PREFIX, TPTP_MENTION_SUFFIX};
pub use kif::{formula_to_ast, formula_to_kif, KifProofStep, proof_steps_to_kif};
pub(crate) use translate::{sentence_to_tptp, kb_to_tptp};
