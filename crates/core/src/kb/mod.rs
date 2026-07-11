//! `KnowledgeBase` -- the single public API type for sigmakee-rs-core.
//!
//! The modules in this folder are divided by which API they expose
//! (e.g. `semantics.rs` -> `../semantics/*`).

use std::collections::HashMap;

use crate::layer::{TopLayer, Layer};
#[cfg(feature = "ask")]
use crate::prover::ProvingLayer;
use crate::syntactic::SyntacticLayer;
use crate::semantics::SemanticLayer;
use crate::trans::{HasTranslation, TranslationLayer};
use crate::types::SentenceId;

#[cfg(feature = "persist")]
use crate::persist::LmdbEnv;

#[macro_use]
pub mod progress;
#[cfg(feature = "ask")]
pub mod prove;
pub mod export;
#[cfg(feature = "persist")]
pub mod persist;
pub mod man;
pub mod search;
pub mod ingest;
pub mod store;
pub mod semantics;
pub mod sine;
pub mod dis;
pub mod doxastic;
pub mod session_tags;
pub(crate) mod assemble;
#[cfg(feature = "ask")]
pub(crate) mod natural_lang;
#[cfg(feature = "ask")]
pub(crate) mod proof_prose;

/// The base structure defining a knowledge base.
///
/// Generic over the top layer of the stack: `L` defaults to
/// [`TranslationLayer`] (the TPTP/Vampire pipeline); the native prover
/// instantiates `KnowledgeBase<ProverLayer>`. Layer-agnostic methods live in
/// `impl<L: TopLayer> KnowledgeBase<L>` blocks; pipeline-specific methods live
/// on the concrete instantiations.
pub struct KnowledgeBase<L = TranslationLayer> {
    /// Top of the layer chain. Owns the entire
    /// `SyntacticLayer` → `SemanticLayer` → `<top>` stack; callers reach the
    /// inner layers via [`Self::syntactic`] and [`Self::semantic`].
    pub(crate) layer: L,

    /// In-memory session assertions: session name → `Vec<SentenceId>`.
    /// Sentences here have NOT been promoted to axioms yet.
    pub(in crate::kb) sessions: HashMap<String, Vec<SentenceId>>,

    /// Syntax-level dedup table.
    ///
    /// Maps `sentence_fingerprint(ast) -> SentenceId` for every accepted
    /// root, letting syntactically-identical sentences (same token structure,
    /// modulo whitespace and comments) be rejected without paying the
    /// clausification cost. Evicted in `flush_session`; kept across
    /// `make_session_axiomatic` since promoted axioms remain in the store and
    /// should still block future duplicates.
    pub(in crate::kb) syntax_fingerprints: HashMap<u64, SentenceId>,

    /// LMDB handle. None = purely in-memory.
    #[cfg(feature = "persist")]
    pub(in crate::kb) db: Option<LmdbEnv>,

    /// Optional progress sink. When set, the KB's internal instrumentation
    /// emits `ProgressEvent`s through it, including phase-timing events
    /// (`PhaseStarted` / `PhaseFinished`).
    ///
    /// `None` by default; set via [`KnowledgeBase::set_progress_sink`].
    pub(in crate::kb) progress: Option<crate::progress::DynSink>,
}

#[allow(dead_code)]
impl<L: TopLayer + Layer> KnowledgeBase<L> {
    // -- Layer accessors -------------------------------------------------------

    /// Middle layer (semantic).
    pub(crate) fn semantic(&self) -> &SemanticLayer { self.layer.semantic() }

    /// Bottom layer (raw parse store).
    pub(crate) fn syntactic(&self) -> &SyntacticLayer { &self.layer.semantic().syntactic }

    /// Mut access to the middle layer.
    pub(crate) fn semantic_mut(&mut self) -> &mut SemanticLayer { self.layer.semantic_mut() }

    /// Mut access to the bottom layer.
    pub(crate) fn syntactic_mut(&mut self) -> &mut SyntacticLayer { &mut self.layer.semantic_mut().syntactic }

    /// Crate-internal read-only access to the underlying [`SyntacticLayer`].
    /// New code should prefer [`Self::syntactic`].
    #[allow(dead_code)]
    pub(crate) fn store_for_testing(&self) -> &SyntacticLayer { self.syntactic() }

    // -- Construction ----------------------------------------------------------

    /// Initializes the shared KB fields over an already-built top layer.
    /// Concrete constructors (`new`, `new_native`, `open`) delegate here.
    pub(in crate::kb) fn from_layer(layer: L) -> Self {
        Self {
            layer,
            sessions:                       HashMap::new(),
            syntax_fingerprints:            HashMap::new(),
            #[cfg(feature = "persist")]     db:   None,
            progress:                       None,
        }
    }
}

impl<L: HasTranslation + TopLayer> KnowledgeBase<L> {
    /// Top layer (translation).
    pub(crate) fn translation(&self) -> &TranslationLayer { &self.layer.translation() }
}

impl KnowledgeBase {
    /// Constructs a new KnowledgeBase over the translation (TPTP) stack.
    pub fn new() -> Self {
        Self::from_layer(TranslationLayer::new(
            SemanticLayer::new(SyntacticLayer::default())))
    }
}

impl Default for KnowledgeBase {
    fn default() -> Self { Self::new() }
}

#[cfg(feature = "ask")]
impl<L: ProvingLayer> KnowledgeBase<L> {
    /// Read-only access to the proving top layer.
    pub fn prover(&self) -> &L {
        &self.layer
    }
}

#[cfg(feature = "native-prover")]
impl KnowledgeBase<crate::prover::ProverLayer> {
    /// Constructs a new KnowledgeBase over the native-prover stack:
    /// the same syntactic/semantic layers, topped by [`ProverLayer`]
    /// instead of the TPTP translation layer.
    ///
    /// [`ProverLayer`]: crate::saturate::ProverLayer
    pub fn new_native() -> Self {
        Self::from_layer(crate::prover::ProverLayer::new(
            SemanticLayer::new(SyntacticLayer::default())))
    }
}

#[cfg(feature = "ask")]
impl KnowledgeBase<crate::prover::ExternalProverLayer> {
    /// Constructs a new KnowledgeBase over the external-prover stack: the
    /// translation layer topped by an [`ExternalProverLayer`] driving `backend`.
    ///
    /// [`ExternalProverLayer`]: crate::prover::ExternalProverLayer
    pub fn new_external(backend: crate::prover::external::Prover) -> Self {
        Self::from_layer(crate::prover::ExternalProverLayer::new(backend,
            TranslationLayer::new(
                SemanticLayer::new(SyntacticLayer::default()))))
    }

    /// Swaps the external prover backend (e.g. E or a custom Vampire path)
    /// without rebuilding the KB.
    pub fn set_prover(&mut self, backend: crate::prover::external::Prover) {
        self.layer.set_backend(backend);
    }
}