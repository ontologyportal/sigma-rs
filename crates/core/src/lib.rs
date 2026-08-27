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
pub(crate) mod types;

pub(crate) mod trans;

#[cfg(any(feature = "ask", feature = "native-prover"))]
pub mod prover;

// Crate-internal alias so `crate::saturate::…` paths resolve.
#[cfg(feature = "native-prover")]
pub(crate) use prover::saturate;

// Backend-agnostic persistence abstraction; the heed/LMDB internals inside it
// stay `cfg(feature = "persist")`.
pub(crate) mod persist;

pub mod kb;

#[doc(hidden)]
pub use crate::trans::TranslationLayer;

/// A generic trait used to control the active KB layer.
pub use crate::layer::TopLayer;

pub use crate::trans::HasTranslation;

#[cfg(feature = "native-prover")]
#[doc(hidden)]
pub use crate::prover::saturate::ProverLayer;

#[cfg(feature = "ask")]
#[doc(hidden)]
pub use crate::prover::ExternalProverLayer;

/// External-prover options (selection, session, budget, TPTP mode).
#[cfg(feature = "ask")]
pub use crate::prover::ExternalOpts;

/// Native-prover options (budget, step caps, `Strategy`).
#[cfg(feature = "native-prover")]
pub use crate::prover::saturate::prover::NativeOpts;
/// One portfolio lane's worth of search-shaping knobs. Serializable, so sweep /
/// portfolio specs can live in JSON.
#[cfg(feature = "native-prover")]
pub use crate::prover::saturate::strategy::Strategy;

// -- Public re-exports --------------------------------------------------------

/// Parse-only syntax check of KIF text — no KB, no ingestion, no state.
/// Returns the tokenizer/parser diagnostics; empty means the text is
/// syntactically well-formed.  `file` names the source in each diagnostic's
/// span.  Use this to vet a transient editor buffer BEFORE staging it into a
/// [`KnowledgeBase`]: staging syntactically broken content reads as "the file
/// is now empty" and retracts every sentence the file previously contributed.
pub fn kif_parse_diagnostics(text: &str, file: &str) -> Vec<Diagnostic> {
    parse::Parser::Kif
        .parse(text, file)
        .1
        .iter()
        .map(|(_, e)| e.to_diagnostic())
        .collect()
}

#[cfg(feature = "ask")]
pub use kb::natural_lang::RenderReport;

pub use diagnostic::{
    DiagResult, Diagnostic, DiagnosticSource, RelatedInfo, Severity, ToDiagnostic,
};
pub use types::{
    hash_file_contents, Element, FileOrigin, GitProvenance, Literal, LocalProvenance, Occurrence,
    OccurrenceKind, OpKind, Sentence, SentenceId, SourceFile, SymbolId,
};

pub use semantics::types::DocEntry;
pub use semantics::types::{TaxDirection, TaxRelation};

pub use cache::CacheConfig;
pub use kb::man::{ManKind, ManPage, ParentEdge, SentenceRef, SortSig};
pub use kb::search::{SearchHit, SearchOpts, SearchSource};
pub use kb::KnowledgeBase;
pub use parse::dialect::{tptp_highlight, DroppedStmt, EmitResult, Emitter};
pub use parse::kif::dis::AstKif;
pub use parse::kif::{tokenize as tokenize_kif, Token, TokenKind};
pub use parse::tptp::syntax::detect_tptp_lang;
pub use parse::{
    parse_document, sentence_fingerprint, AstNode, CommentBlock, ParsedDocument, Parser, Span,
};
pub use syntactic::position::ElementHit;

#[cfg(any(feature = "ask", feature = "native-prover"))]
pub use prover::{Binding, ProverResult, ProverStatus, ProverTimings};
// `ProverMode` is a plain data enum in `prover::result` with no
// prover-backend dependency (see its doc comment for why it lives there
// rather than under `external::backends`), but it is reached through the
// `prover` module, whose declaration is gated -- so this re-export carries
// the same gate.
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub use prover::ProverMode;
// Pure SZS/TSTP parsing for a captured Vampire transcript (status +
// `KifProofStep`s), no subprocess spawning. Available on wasm32, which
// builds with `native-prover`; reached through the gated `prover` module,
// so the re-export carries that gate. See `prover::vampire_proof`.
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub use prover::vampire_proof::{parse_vampire_result, VampireProofResult};
// `ProverRunner`/`Prover` are the subprocess-backend trait and handle — they
// live in the `ask`-only `external` module, absent on native/wasm builds.
pub use parse::tq::{parse_test_content, TestCase};
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub use prover::axiom_source::{AxiomSource, AxiomSourceIndex};
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub use prover::proof::{emit_proof, render_graphviz, IrProofStep, KifProofStep};
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub use prover::CommonProverOpts;
#[cfg(feature = "ask")]
pub use prover::Prover;
#[cfg(feature = "ask")]
pub use prover::ProverRunner;
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub use prover::{Conjecture, ProvingLayer};

pub use syntactic::sine::{SineIndex, SineParams};

pub use progress::{DynSink, LogLevel, PhaseGuard, ProgressEvent, ProgressSink, ProveCtx};

pub use kb::ingest::{IngestResult, PromoteError};
pub type TellResult = IngestResult;
pub use kb::export::TptpOptions;
pub use kb::session_tags;
pub use semantics::errors::{
    ArityMismatch, BoxedError, DisjointInstance, DisjointSubclass, DomainMismatch, DoubleRange,
    ExistentialInAntecedent, ExistentialInIff, FreeVarInConsequent, FunctionCase, HeadInvalid,
    HeadNotRelation, InstanceSubclassConflict, MissingArity, MissingConstituentDep,
    MissingDocumentation, MissingDomain, MissingFormatString, MissingRange, MissingTermFormat,
    MultipleDocumentation, MutualConstituentDep, NoEntityAncestor, NonLogicalArg, Other,
    PartitionNonMember, PartitionViolation, PredicateCase, QuantifierVacuous, SemanticError,
    SingleArity, SingleUseVariable, TermCamelCase, TermCase, TermNoRule, TooGeneralRel,
};

pub use parse::tptp::syntax::TptpLang;

/// Test-only inspection hooks for the formula rewrite pass.
#[doc(hidden)]
pub mod test {
    use crate::kb::KnowledgeBase;
    use crate::parse::ast::OpKind;
    use crate::types::Element;

    /// Snapshot of the synthetic-sentence state after KB load + rewrite.
    ///
    /// Returned by [`peek_synthetic_implications`].
    #[derive(Debug)]
    pub struct SyntheticReport {
        /// Total number of synthetic sentences allocated.
        pub synthetic_count: usize,
        /// Number of root SIDs in `TranslationLayer::suppressed`.
        pub suppressed_count: usize,
        /// True when at least one non-suppressed synthetic implication
        /// has `(greaterThan ?V ...)` as a conjunct in its antecedent.
        pub has_greater_than_guard: bool,
    }

    /// Inspect the synthetic-sentence store and suppressed set produced by
    /// the rewrite pass.  Walks each non-suppressed synthetic implication
    /// (`(=> (and ...) ...)` shape) and scans the antecedent conjuncts
    /// for any `(greaterThan ?V ...)` atom.
    pub fn peek_synthetic_implications(kb: &KnowledgeBase) -> SyntheticReport {
        let trans = kb.translation();
        let syn = &trans.semantic.syntactic;
        let greater_than_id = syn.sym_id("greaterThan");

        let mut has_guard = false;
        if let Some(gt_id) = greater_than_id {
            for &sid in syn.synthetic_origin.keys() {
                if trans.suppressed.read().unwrap().contains(&sid) {
                    continue;
                }
                let Some(sent) = syn.sentence(sid) else {
                    continue;
                };
                if !matches!(sent.elements.first(), Some(Element::Op(OpKind::Implies))) {
                    continue;
                }
                let Some(Element::Sub(ant_sid)) = sent.elements.get(1) else {
                    continue;
                };
                // If the antecedent is an `(and ...)`, scan its conjuncts;
                // otherwise treat it as the single conjunct.
                let ant = syn.sentence(*ant_sid).expect("ant exists");
                let conjuncts: Vec<&Element> = match ant.elements.first() {
                    Some(Element::Op(OpKind::And)) => ant.elements[1..].iter().collect(),
                    _ => vec![sent.elements.get(1).unwrap()],
                };
                for c in conjuncts {
                    let Element::Sub(csid) = c else { continue };
                    let Some(cs) = syn.sentence(*csid) else {
                        continue;
                    };
                    if matches!(cs.elements.first(),
                        Some(Element::Symbol(sym)) if sym.id() == gt_id)
                    {
                        has_guard = true;
                        break;
                    }
                }
                if has_guard {
                    break;
                }
            }
        }

        SyntheticReport {
            synthetic_count: syn.synthetic_origin.len(),
            suppressed_count: trans.suppressed.read().unwrap().len(),
            has_greater_than_guard: has_guard,
        }
    }
}
