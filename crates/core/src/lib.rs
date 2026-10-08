// `KnowledgeBase<L = TranslationLayer>` is `pub` but its top-layer bound
// `L: TopLayer` is `pub(crate)` (sealed-layer design), so `private_bounds` is
// silenced crate-wide.
#![allow(private_bounds)]

#[cfg(all(feature = "ask", target_arch = "wasm32"))]
compile_error!(
    "The 'ask' feature is not supported on wasm32 targets. \
     Remove 'ask' from the features list for wasm builds."
);

#[cfg(all(
    feature = "parallel",
    target_arch = "wasm32",
    not(target_feature = "atomics")
))]
compile_error!(
    "The 'parallel' feature requires a threads-enabled wasm build (atomics \
     and bulk-memory target features via -Zbuild-std, as used by \
     wasm-bindgen-rayon). Remove 'parallel' from the features list for \
     plain wasm32 builds, or enable it only on non-wasm targets via \
     target-conditional dependency declarations."
);

// -- Module declarations ------------------------------------------------------

pub(crate) mod cache;
pub(crate) mod clock;
pub(crate) mod diagnostic;
pub(crate) mod gf64;
pub(crate) mod layer;
pub(crate) mod numeric;
pub(crate) mod parse;
pub mod progress;
pub(crate) mod semantics;
pub(crate) mod syntactic;
pub mod types;

pub(crate) mod trans;

#[cfg(any(feature = "external-prover", feature = "native-prover"))]
pub mod prover;

// Crate-internal alias so `crate::saturate::…` paths resolve.
#[cfg(feature = "native-prover")]
pub(crate) use prover::saturate;

// Backend-agnostic persistence abstraction; the heed/LMDB internals inside it
// stay `cfg(feature = "persist")`.
pub(crate) mod persist;

pub(crate) mod kb;
pub(crate) use kb::progress::{profile_span, with_guard};

/// Offline WordNet 3.0 lexicon with SUMO anchors -- pure in-memory parsing,
/// no filesystem access.  See [`lexicon`] for the module-level docs.
#[cfg(feature = "lexicon")]
pub mod lexicon;

#[doc(hidden)]
pub use crate::trans::TranslationLayer;

/// A generic trait used to control the active KB layer.
pub use crate::layer::TopLayer;

/// Monotonic-clock shim (`std::time::Instant` off wasm32, a `Date.now()`
/// work-alike on it, since `Instant::now()` panics there). Any caller that
/// needs to measure elapsed time or compute a deadline and might run on
/// wasm32 -- rather than only `cfg(not(target_arch = "wasm32"))` -- should
/// use this instead of `std::time::Instant` directly.
pub use crate::clock::Instant;

pub use crate::trans::HasTranslation;

#[cfg(feature = "native-prover")]
#[doc(hidden)]
pub use crate::prover::saturate::ProverLayer;

#[cfg(feature = "external-prover")]
#[doc(hidden)]
pub use crate::prover::ExternalProverLayer;

/// External-prover options (selection, session, budget, TPTP mode).
#[cfg(feature = "external-prover")]
pub use crate::prover::ExternalOpts;

/// Native-prover options (budget, step caps, `Strategy`).
#[cfg(feature = "native-prover")]
pub use crate::prover::saturate::prover::NativeOpts;
/// One portfolio lane's worth of search-shaping knobs. Serializable, so sweep /
/// portfolio specs can live in JSON.
#[cfg(feature = "native-prover")]
pub use crate::prover::saturate::strategy::Strategy;

// -- Public re-exports --------------------------------------------------------

#[cfg(any(feature = "external-prover", feature = "native-prover"))]
pub use semantics::render::{RenderReport, RenderStyle};

pub use diagnostic::{
    DiagResult, Diagnostic, DiagnosticSource, RelatedInfo, Severity, ToDiagnostic,
};

pub use types::{
    hash_file_contents, Element, FileOrigin, GitProvenance, Literal, LocalProvenance, Occurrence,
    OccurrenceKind, OpKind, Sentence, SentenceId, SourceFile, SymbolId,
};

pub use semantics::consts::{DEFAULT_LANGUAGE, HIGHER_ORDER_CATEGORIES, NATURAL_LANGUAGE_CLASS};
pub use semantics::types::DocEntry;
pub use semantics::types::{TaxDirection, TaxRelation};

pub use cache::CacheConfig;
pub use kb::dis::SentenceForm;
pub use kb::man::{ManKind, ManPage, ParentEdge, SentenceRef, SortSig};
#[cfg(any(feature = "external-prover", feature = "native-prover"))]
pub use kb::prove::{AuditBatch, AuditSample, SampledAudit};
pub use kb::search::{
    RankComponent, SearchHit, SearchOpts, SearchSource, TaxConstraint, DEFAULT_CANDIDATE_LIMIT,
};
pub use kb::semantics::{ValidationTarget, VocabStats};
pub use kb::KnowledgeBase;
pub use parse::dialect::{tptp_highlight, ConvertedStmt, EmitResult, Emitter};
pub use parse::doc::{DocItem, MetaNode};
pub use parse::kif::dis::AstKif;
pub use parse::kif::{format_document, format_forms};
pub use parse::kif::{tokenize as tokenize_kif, OpTok, Token, TokenKind};
pub use parse::tptp::parser::TptpParseOptions;
pub use parse::tptp::syntax::detect_tptp_lang;
pub use parse::tptp::test_case::parse_tptp_test_content;
pub use parse::KifParseOptions;
pub use parse::{
    parse_document, sentence_fingerprint, try_parse_file, AstNode, CommentBlock, ParsedDocument,
    Parser, Span,
};
pub use syntactic::position::ElementHit;

#[cfg(any(feature = "external-prover", feature = "native-prover"))]
pub use prover::{Binding, ProverResult, ProverStatus, ProverTimings};
// `ProverMode` is a plain data enum in `prover::result` with no
// prover-backend dependency (see its doc comment for why it lives there
// rather than under `external::backends`), but it is reached through the
// `prover` module, whose declaration is gated -- so this re-export carries
// the same gate.
#[cfg(any(feature = "external-prover", feature = "native-prover"))]
pub use prover::ProverMode;
// Pure SZS/TSTP parsing for a captured Vampire transcript (status +
// `KifProofStep`s), no subprocess spawning. Available on wasm32, which
// builds with `native-prover`; reached through the gated `prover` module,
// so the re-export carries that gate. See `prover::vampire_proof`.
#[cfg(any(feature = "external-prover", feature = "native-prover"))]
#[cfg(feature = "external-prover")]
pub use prover::vampire_proof::{result_from_transcript, vampire_cli_args};
// `ProverRunner`/`Prover` are the subprocess-backend trait and handle — they
// live in the `ask`-only `external` module, absent on native/wasm builds.
pub use parse::tq::{is_tq_directive, parse_test_content, TestCase};
#[cfg(any(feature = "external-prover", feature = "native-prover"))]
pub use prover::proof::{emit_proof, render_graphviz, IrProofStep, KifProofStep};
#[cfg(any(feature = "external-prover", feature = "native-prover"))]
pub use prover::CommonProverOpts;
#[cfg(feature = "external-prover")]
pub use prover::Prover;
#[cfg(feature = "external-prover")]
pub use prover::ProverRunner;
#[cfg(any(feature = "external-prover", feature = "native-prover"))]
pub use prover::{Conjecture, ProvingLayer};

pub use syntactic::sine::{SineIndex, SineParams};

pub use progress::{DynSink, LogLevel, PhaseGuard, ProgressEvent, ProgressSink, ProveCtx};

pub use kb::export::TptpOptions;
pub use kb::ingest::{IngestResult, PromoteError};
pub use semantics::errors::{BoxedError, SemanticError};

pub use parse::tptp::syntax::TptpLang;
