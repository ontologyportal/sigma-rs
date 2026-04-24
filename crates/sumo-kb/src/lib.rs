// Enforce at compile time: the `ask` feature requires subprocess spawning,
// which is not available on wasm32 targets.
#[cfg(all(feature = "ask", target_arch = "wasm32"))]
compile_error!(
    "The 'ask' feature is not supported on wasm32 targets. \
     Remove 'ask' from the features list for wasm builds."
);

// `parallel` uses rayon, whose default thread-pool story on wasm32
// requires cross-origin isolation and a non-trivial runtime setup
// (wasm-bindgen-rayon).  Rather than smuggle that in through the
// back door, refuse to compile the feature on wasm32 and make
// downstream consumers make an explicit choice.
#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
compile_error!(
    "The 'parallel' feature is not supported on wasm32 targets. \
     Remove 'parallel' from the features list for wasm builds, \
     or enable it only on non-wasm targets via target-conditional \
     dependency declarations."
);

// -- Module declarations ------------------------------------------------------

pub mod parse;
pub mod error;
pub mod diagnostic;
pub mod types;
pub(crate) mod kif_store;
pub(crate) mod lookup;
pub(crate) mod semantic;

pub mod tptp;

// Vampire-backed clausifier.  `cnf` implies `integrated-prover`, so
// this module is always backed by the linked Vampire library.
#[cfg(feature = "cnf")]
pub(crate) mod cnf;

// Canonical hashing of CNF clauses -- foundation for clause-level
// dedup.  Works on the crate-local `Clause` / `CnfTerm` types alone.
#[cfg(feature = "cnf")]
pub(crate) mod canonical;

#[cfg(feature = "ask")]
pub mod prover;

pub(crate) mod vampire;

#[cfg(feature = "persist")]
pub(crate) mod persist;

pub(crate) mod kb;

// SInE axiom selection — a general "relevance index" over symbols.
//
// Originally gated behind `ask` because the only consumer was the
// prover input filter.  Un-gated now: the same index is useful for
// smart revalidation in `kb::reconcile`, LSP "related axioms"
// features, and any consumer that wants "which axioms share symbols
// with X?" — none of which require a prover to be built.  Eager
// maintenance runs on every promote and reconcile regardless of
// feature set; the cost is microseconds per edit and bounded by
// shared-symbol counts.
pub mod sine;

// General per-phase profiling hooks.  Always compiled (so call sites
// don't need `cfg` gates on every `span()` invocation) but the
// recording path is feature-gated: when `feature = "profiling"` is
// off, all operations are no-ops and `Profiler` is zero-sized.
pub mod profiling;

// Reserved session / file tags for internal KB plumbing.  One place
// for every `"__query__"` / `"__reconcile_add__"` / `"__load__"`
// literal so typos in one call site can't silently create a phantom
// session.
pub mod session_tags;

// Natural-language rendering of SUO-KIF formulas via SUMO's `format`
// / `termFormat` relations.  Gated on `ask` since its only consumer
// is the proof-printing path in the CLI — editor tooling that builds
// without `ask` has no need for it.
#[cfg(feature = "ask")]
pub mod natural_lang;

#[cfg(feature = "ask")]
pub use natural_lang::RenderReport;

// Map proof-step formulas back to the source axioms that produced
// them.  Uses the alpha-equivalent `canonical_sentence_fingerprint`
// to bridge Vampire's variable renaming (?HUMAN → ?X0).
#[cfg(feature = "ask")]
pub mod axiom_source;

#[cfg(feature = "ask")]
pub use axiom_source::{AxiomSource, AxiomSourceIndex};

// File-level reconcile report produced by `KnowledgeBase::reconcile_file`.
// Un-gated along with `sine` — reconcile's only `ask` dependency was
// the SInE-based smart revalidator.
pub use kb::reconcile::ReconcileReport;

// -- Public re-exports --------------------------------------------------------

pub use error::{
    KbError, ParseError, SemanticError, Span,
    TellResult, TellWarning,
    PromoteError, PromoteReport, DuplicateInfo, DuplicateSource,

};
pub use diagnostic::{Diagnostic, RelatedInfo, Severity, ToDiagnostic};
pub use types::{
    SymbolId, SentenceId,
    Element, Literal, Symbol, Sentence,
    Occurrence, OccurrenceKind,
    TaxRelation, TaxEdge,
    OpKind,
};

#[cfg(feature = "cnf")]
pub use types::ClauseId;
pub use tptp::{TptpOptions, TptpLang, TestCase, parse_test_content};
pub use kb::KnowledgeBase;
pub use kb::{FileDiff, compute_file_diff};
pub use kb::man::{DocEntry, ManKind, ManPage, ParentEdge, SortSig};
pub use lookup::ElementHit;
pub use parse::{AstNode, Pretty, Parser, ParsedDocument, parse_document, sentence_fingerprint};
pub use parse::kif::{Token, TokenKind};

#[cfg(feature = "cnf")]
pub use kb::{ClausifyOptions, ClausifyReport};

pub use tptp::{formula_to_kif, formula_to_ast, KifProofStep, proof_steps_to_kif};

#[cfg(feature = "ask")]
pub use prover::{
    ProverRunner, ProverOpts, ProverMode, ProverResult,
    ProverStatus, Binding, VampireRunner,
};

// SInE types are now available unconditionally.
pub use sine::{SineIndex, SineParams};

pub use profiling::{Profiler, ProfileSpan, PhaseSnapshot};

