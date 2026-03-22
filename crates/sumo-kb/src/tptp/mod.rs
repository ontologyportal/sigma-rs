// -- tptp/mod.rs ---------------------------------------------------------------
//
// TPTP (FOF / TFF) output generation for SUMO/KIF knowledge bases.
//
// This module translates a loaded `KifStore` + `SemanticLayer` into TPTP
// Problem files that can be sent to theorem provers such as Vampire or E.
//
// ## Sub-modules
//
//   options    -- `TptpOptions` configuration struct and `TptpLang` (Fof / Tff)
//                enum.  Passed by reference through every translation call.
//
//   names      -- KIF-to-TPTP identifier encoding: the `s__` symbol prefix,
//                `V__` variable prefix, `__m` mention suffix, and
//                `translate_symbol` / `translate_variable` / `translate_literal`
//                helpers.
//
//   tff        -- TFF-specific infrastructure:
//                  * `TffContext`      -- accumulates `tff(..., type, ...)` declaration
//                                       lines for the output preamble.
//                  * `ensure_declared` -- lazily emits a type declaration for a symbol
//                                       on first encounter; reads sort data from
//                                       SemanticLayer::sort_annotations().
//                  * `infer_var_types` -- infers TFF sort for each variable in a
//                                       sentence by consulting SemanticLayer::
//                                       var_type_inference() and a literal
//                                       co-occurrence pass; returns
//                                       HashMap<SymbolId, &'static str>.
//
//   translate  -- Core recursive translation:
//                  * `translate_element` / `translate_sentence` -- walk the KIF AST.
//                  * `kb_to_tptp`        -- render an entire KB as a TPTP string.
//                  * `sentence_to_tptp`  -- render a single sentence.
//
//   kif        -- Inverse direction: parse TPTP proof output back into KIF.
//                  `proof_steps_to_kif` reconstructs the proof steps that Vampire
//                  returned into KIF `ProofStep` records the CLI can display.
//
// ## Porting notes
//   Originally ported from `sumo-parser-core/src/tptp.rs`.
//   Key changes: `&KnowledgeBase` -> `&SemanticLayer`;
//   `sentences[sid as usize]` -> `sentences[store.sent_idx(sid)]`;
//   session filtering decoupled from translation.

mod options;
mod names;
mod tff;
mod translate;
pub mod kif;
pub mod test_case;

#[cfg(test)]
mod tests;

// -- Public API ----------------------------------------------------------------

pub use options::{TptpLang, TptpOptions};
pub use names::{TPTP_SYMBOL_PREFIX, TPTP_VARIABLE_PREFIX, TPTP_MENTION_SUFFIX};
pub use kif::{formula_to_ast, formula_to_kif, KifProofStep, proof_steps_to_kif};
pub use test_case::{TestCase, parse_test_content};
pub(crate) use translate::{sentence_to_tptp, kb_to_tptp};
