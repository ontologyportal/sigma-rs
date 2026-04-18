// Enforce at compile time: the `ask` feature requires subprocess spawning,
// which is not available on wasm32 targets.
#[cfg(all(feature = "ask", target_arch = "wasm32"))]
compile_error!(
    "The 'ask' feature is not supported on wasm32 targets. \
     Remove 'ask' from the features list for wasm builds."
);

// -- Module declarations ------------------------------------------------------

pub mod parse;
pub mod error;
pub mod types;
pub(crate) mod kif_store;
pub(crate) mod semantic;
pub(crate) mod fingerprint;

pub mod tptp;

#[cfg(feature = "cnf")]
pub(crate) mod cnf;

#[cfg(feature = "ask")]
pub mod prover;

#[cfg(feature = "vampire")]
pub(crate) mod vampire;

#[cfg(feature = "persist")]
pub(crate) mod persist;

pub(crate) mod kb;

// -- Public re-exports --------------------------------------------------------

pub use error::{
    KbError, ParseError, SemanticError, Span,
    TellResult, TellWarning,
    PromoteError, PromoteReport, DuplicateInfo, DuplicateSource,

};
pub use types::{
    SymbolId, SentenceId,
    Element, Literal, Symbol, Sentence,
    TaxRelation, TaxEdge,
    OpKind,
};
pub use tptp::{TptpOptions, TptpLang, TestCase, parse_test_content};
pub use kb::KnowledgeBase;
pub use parse::{AstNode, Pretty, Parser};

#[cfg(feature = "cnf")]
pub use kb::{ClausifyOptions, ClausifyReport};

pub use tptp::{formula_to_kif, formula_to_ast, KifProofStep, proof_steps_to_kif};

#[cfg(feature = "ask")]
pub use prover::{
    ProverRunner, ProverOpts, ProverMode, ProverResult,
    ProverStatus, Binding, VampireRunner,
};

