//! TPTP-related utilities.
//!
//! Historically this module hosted a custom TPTP emitter (`kb_to_tptp`,
//! `sentence_to_tptp`, the `tff` preamble builder, and KIF-to-TPTP name
//! mangling).  After the vampire-prover IR migration those emitters were
//! replaced by `vampire::converter::NativeConverter` +
//! `vampire::assemble::assemble_tptp` and deleted.
//!
//! What remains here is:
//!
//! * `options`   -- the [`TptpOptions`] / [`TptpLang`] config types that
//!                  the `KnowledgeBase::to_tptp` / `ask` API still takes.
//! * `kif`       -- inverse direction: parse Vampire proof-step output
//!                  back into KIF for pretty-printed proofs.
//! * `test_case` -- `.kif.tq` test-file parser consumed by
//!                  `sumo test`.

mod options;
pub mod kif;
pub mod test_case;

pub use options::{TptpLang, TptpOptions};
pub use kif::{formula_to_ast, formula_to_kif, KifProofStep, proof_steps_to_kif};
pub use test_case::{TestCase, parse_test_content};
