// `KnowledgeBase<L = TranslationLayer>` is `pub` but its top-layer bound
// `L: TopLayer` is `pub(crate)` (sealed-layer design), so `private_bounds` is
// silenced crate-wide.
#![allow(private_bounds)]

#[cfg(all(feature = "ask", target_arch = "wasm32"))]
compile_error!(
    "The 'ask' feature is not supported on wasm32 targets. \
     Remove 'ask' from the features list for wasm builds."
);

#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
compile_error!(
    "The 'parallel' feature is not supported on wasm32 targets. \
     Remove 'parallel' from the features list for wasm builds, \
     or enable it only on non-wasm targets via target-conditional \
     dependency declarations."
);

// -- Module declarations ------------------------------------------------------

pub(crate) mod clock;
pub(crate) mod parse;
pub(crate) mod gf64;
pub(crate) mod diagnostic;
pub(crate) mod types;
pub(crate) mod layer;
pub(crate) mod cache;
pub(crate) mod numeric;
pub mod progress;
pub(crate) mod syntactic;
pub(crate) mod semantics;

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

/// One portfolio lane's worth of search-shaping knobs. Serializable, so sweep /
/// portfolio specs can live in JSON.
#[cfg(feature = "native-prover")]
pub use crate::prover::saturate::strategy::Strategy;
/// Native-prover options (budget, step caps, `Strategy`).
#[cfg(feature = "native-prover")]
pub use crate::prover::saturate::prover::NativeOpts;

// -- Public re-exports --------------------------------------------------------

#[cfg(feature = "ask")]
pub use kb::natural_lang::RenderReport;

pub use diagnostic::{Diagnostic, DiagnosticSource, RelatedInfo, Severity, ToDiagnostic};
pub use types::{
    SymbolId, SentenceId,
    Element, Literal, Sentence,
    Occurrence, OccurrenceKind,
    OpKind,
    SourceFile, FileOrigin, GitProvenance, LocalProvenance, hash_file_contents,
};

pub use semantics::types::{TaxDirection, TaxRelation};
pub use semantics::types::DocEntry;

pub use kb::KnowledgeBase;
pub use kb::man::{ManKind, ManPage, ParentEdge, SortSig};
pub use kb::search::{SearchHit, SearchOpts, SearchSource};
pub use syntactic::position::ElementHit;
pub use parse::{
    AstNode, Parser, ParsedDocument, parse_document,sentence_fingerprint,
    Span
};
pub use parse::kif::dis::AstKif;
pub use parse::kif::{Token, TokenKind, tokenize as tokenize_kif};
pub use parse::dialect::{tptp_highlight, DroppedStmt, EmitResult, Emitter};
pub use parse::tptp::syntax::detect_tptp_lang;

#[cfg(any(feature = "ask", feature = "native-prover"))]
pub use prover::{
    ProverStatus,
    ProverResult,
    Binding,
    ProverTimings,
};
// `ProverRunner`/`Prover` are the subprocess-backend trait and handle — they
// live in the `ask`-only `external` module, absent on native/wasm builds.
#[cfg(feature = "ask")]
pub use prover::ProverRunner;
#[cfg(feature = "ask")]
pub use prover::Prover;
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub use prover::proof::{emit_proof, KifProofStep, IrProofStep};
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub use prover::CommonProverOpts;
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub use prover::{ProvingLayer, Conjecture};
pub use parse::tq::{TestCase, parse_test_content};
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub use prover::axiom_source::{AxiomSource, AxiomSourceIndex};

pub use syntactic::sine::{SineIndex, SineParams};

pub use progress::{LogLevel, PhaseGuard, ProgressEvent, ProgressSink, DynSink, ProveCtx};

pub use kb::ingest::{IngestResult, PromoteError};
pub type TellResult = IngestResult;
pub use kb::export::TptpOptions;
pub use kb::session_tags;
pub use semantics::errors::{SemanticError, Findings};

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
        pub synthetic_count:        usize,
        /// Number of root SIDs in `TranslationLayer::suppressed`.
        pub suppressed_count:       usize,
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
        let syn   = &trans.semantic.syntactic;
        let greater_than_id = syn.sym_id("greaterThan");

        let mut has_guard = false;
        if let Some(gt_id) = greater_than_id {
            for (&sid, _) in syn.synthetic_origin.iter() {
                if trans.suppressed.read().unwrap().contains(&sid) { continue; }
                let Some(sent) = syn.sentence(sid) else { continue };
                if !matches!(sent.elements.first(),
                    Some(Element::Op(OpKind::Implies))) { continue; }
                let Some(Element::Sub(ant_sid)) = sent.elements.get(1) else { continue };
                // If the antecedent is an `(and ...)`, scan its conjuncts;
                // otherwise treat it as the single conjunct.
                let ant = syn.sentence(*ant_sid).expect("ant exists");
                let conjuncts: Vec<&Element> = match ant.elements.first() {
                    Some(Element::Op(OpKind::And)) => ant.elements[1..].iter().collect(),
                    _ => vec![sent.elements.get(1).unwrap()],
                };
                for c in conjuncts {
                    let Element::Sub(csid) = c else { continue };
                    let Some(cs) = syn.sentence(*csid) else { continue };
                    if matches!(cs.elements.first(),
                        Some(Element::Symbol(sym)) if sym.id() == gt_id)
                    {
                        has_guard = true;
                        break;
                    }
                }
                if has_guard { break; }
            }
        }

        SyntheticReport {
            synthetic_count:        syn.synthetic_origin.len(),
            suppressed_count:       trans.suppressed.read().unwrap().len(),
            has_greater_than_guard: has_guard,
        }
    }
}

// Process-global semantic-error classification knobs: promote specific warning
// codes to errors, or flip every warning to an error.
pub use semantics::errors::{
    clear_promoted_errors, promote_to_error, set_all_errors,
};

