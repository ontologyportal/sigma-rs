// crates/core/src/prover/external/mod.rs
//
// External Prover integration layer. What distinguishes this
// from the native prover is that Sentences must be downconverted
// to Formulas (ir) via the TranslationLayer, whereas the Native
// prover operates on Sentences

pub mod backends;
pub(crate) mod consistency;
pub(crate) mod prove;

pub use backends::{ProverRunner, Prover};

use std::sync::Arc;

use super::ProvingLayer;

use crate::{TranslationLayer, cache::CacheConfig, layer::{Layer, NoLayer, TopLayer}};
use crate::cache::events::Event;
use crate::kb::session_tags::SESSION_QUERY;
use crate::types::{FileOrigin, SentenceId, SourceFile};
use crate::{Parser, ProveCtx, SineParams, TptpLang};
use crate::prover::CommonProverOpts;
use super::result::ProverResult;

/// The external prover layer's single consolidated params struct — the shared
/// cross-backend inputs (SInE `selection`, `session`, wall-clock budget, TPTP
/// `mode`) for the translate-then-run path.  Implements [`CommonProverOpts`] so
/// the backend-agnostic [`ProvingLayer::prove`] loop reads selection / timeout
/// off it.  (The native layer's peer is
/// [`NativeOpts`](crate::NativeOpts).)  The lower-level `ProverOpts` handed to
/// [`ProverRunner::prove`] — carrying the runner's `ProverMode` instruction —
/// is a distinct runner-ABI struct, built locally per attempt.
#[derive(Debug, Clone, Default)]
pub struct ExternalOpts {
    /// SInE axiom-selection seed (the autoscaling loop's base selection).
    pub selection:    SineParams,
    /// Optional in-memory session whose assertions ride in as hypotheses.
    pub session:      Option<String>,
    /// Wall-clock budget in seconds (0 = unlimited).
    pub timeout_secs: u64,
    /// TPTP language for the generated problem file (FOF / TFF).
    pub mode:         TptpLang,
    /// Higher-order mode: assemble a THF problem through the translation
    /// layer's HO pipeline instead of `mode`'s first-order one.  A separate
    /// flag (rather than a `TptpLang` variant) so the parse subsystem's
    /// dialect enum stays untouched.
    pub hol:          bool,
}

impl CommonProverOpts for ExternalOpts {
    fn selection(&self) -> SineParams { self.selection }
    fn timeout(&self) -> u64 { self.timeout_secs }
    fn set_timeout(&mut self, secs: u64) { self.timeout_secs = secs; }
    fn set_session(&mut self, session: Option<String>) { self.session = session; }
}

// The conjecture is the shared [`crate::prover::Conjecture`].  External interns
// it into the shared store (under the query tag) in `intern_conjecture`, resolves
// the roots into `Conjecture.sents`, and truncates the tag in `cleanup`.
pub(crate) use super::Conjecture;

#[derive(Debug)]
pub struct ExternalProverLayer {
    /// The external prover configured for this layer
    backend: Prover,
    /// The translation sublayer
    translation: TranslationLayer,
    /// The cache config object
    config: crate::cache::CacheConfig
}

impl ExternalProverLayer {
    /// Create a new external prover layer from a backend and translation layer
    pub(crate) fn new(backend: Prover, translation: TranslationLayer) -> Self {
        Self { backend, translation, config: CacheConfig::default() }
    }

    /// Override the configured prover backend (e.g. after opening a persisted KB,
    /// which installs the default runner).
    pub fn set_backend(&mut self, backend: Prover) {
        self.backend = backend;
    }
}

impl Layer for ExternalProverLayer {
    type Inner = TranslationLayer;

    type Outer = NoLayer;

    fn inner(&self) -> Option<&Self::Inner> { Some(&self.translation) }

    fn own_reactors(&self) -> Vec<crate::cache::router::ReactorEntry<'_>> {
       vec![]
    }

    fn schedule_cell(&self) -> &'static crate::layer::ScheduleCell {
        static CELL: crate::layer::ScheduleCell = std::sync::OnceLock::new();
        &CELL
    }

    fn cache_config(&self) -> &crate::cache::CacheConfig {
        &self.config
    }
    
    fn outer(&self) -> Option<&NoLayer> { None }
}

impl ProvingLayer for ExternalProverLayer {
    type Opts = ExternalOpts;

    /// Deferred rewrite pass + predicate-variable schema detection (idempotent).
    fn warm_up(&self) {
        self.translation.ensure_rewrite_pass();
    }

    /// Intern the conjecture into the **shared store** under the query tag (the
    /// `&self` cascade) so the TPTP builder can resolve + mark it, and resolve
    /// its roots into `Conjecture.sents`.  The shared `prepare` default wraps
    /// this and errors on an empty result; `cleanup` truncates the tag.
    fn intern_conjecture(&self, asts: &[crate::AstNode])
        -> Vec<(std::sync::Arc<crate::types::Sentence>, SentenceId)> {
        let tag = SESSION_QUERY;
        let _ = self.cascade(vec![Event::SourceAdded {
            session: Arc::new(tag.to_owned()),
            file:    SourceFile {
                parser:   Parser::Kif,
                name:     tag.to_string(),
                path:     std::path::PathBuf::new(),
                origin:   FileOrigin::Inline,
                contents: String::new(),
                prebuilt: Some(asts.to_vec()),
            },
            staged: false,
        }]);
        // Full tag membership (new + content-addressed dups alike), resolved.
        let syn = &self.translation.semantic.syntactic;
        syn.file_root_sids(tag).into_iter()
            .filter_map(|sid| syn.sentence(sid).map(|arc| (arc, sid)))
            .collect()
    }

    /// Roll the conjecture parse back — re-ingest the query tag empty.
    fn cleanup(&self, _conj: Conjecture) {
        let tag = SESSION_QUERY;
        let _ = self.cascade(vec![Event::SourceAdded {
            session: Arc::new(tag.to_owned()),
            file:    SourceFile::truncate(std::path::PathBuf::from(tag)),
            staged:  false,
        }]);
    }

    fn prove_once(
        &self,
        conj:     &Conjecture,
        params:   SineParams,
        slice:    u32,
        opts:     &ExternalOpts,
        ctx:      &ProveCtx,
    ) -> (ProverResult, usize) {
        self.ext_prove_once(conj, params, slice, opts, ctx)
    }

    fn check_consistency(&self, opts: &Self::Opts, ctx: &crate::ProveCtx)
        -> super::result::ProverResult
    {
        self.ext_check_consistency(opts, ctx)
    }
}

impl TopLayer for ExternalProverLayer {
    fn from_semantic(semantic: crate::semantics::SemanticLayer) -> Self {
        Self {
            translation: TranslationLayer::from_semantic(semantic),
            backend: Prover::default(),
            config: crate::cache::CacheConfig::default()
        }
    }

    /// Carry the configured prover backend + cache config onto a clone — the
    /// default `from_semantic` would reset them, leaving the clone unable to
    /// invoke the external prover.
    fn fresh_config_clone(&self, semantic: crate::semantics::SemanticLayer) -> Self {
        let translation = TranslationLayer::from_semantic(semantic);
        // Emission config is not a cache — carry it (the per-test
        // `snapshot_clone` path otherwise silently drops it).
        translation.set_reals_only(self.translation.reals_only());
        Self {
            translation,
            backend: self.backend.clone(),
            config:  self.config.clone(),
        }
    }

    fn semantic(&self) -> &crate::semantics::SemanticLayer {
        &self.translation.semantic
    }

    fn semantic_mut(&mut self) -> &mut crate::semantics::SemanticLayer {
        &mut self.translation.semantic
    }
}

impl crate::trans::HasTranslation for ExternalProverLayer {        // trans is one hop down
    fn translation(&self) -> &TranslationLayer { &self.translation }
    fn translation_mut(&mut self) -> &mut TranslationLayer { &mut self.translation }
}