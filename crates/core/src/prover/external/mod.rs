// crates/core/src/prover/external/mod.rs
//
// External Prover integration layer. What distinguishes this
// from the native prover is that Sentences must be downconverted
// to Formulas (ir) via the TranslationLayer, whereas the Native
// prover operates on Sentences

pub mod backends;
pub(crate) mod consistency;
pub(crate) mod prove;

pub use backends::{Prover, ProverRunner};

use std::sync::Arc;

use super::ProvingLayer;

use super::result::ProverResult;
use crate::cache::events::Event;
use crate::prover::CommonProverOpts;
use crate::trans::HasTranslation;
use crate::types::{FileOrigin, SentenceId, SourceFile};
use crate::{
    cache::CacheConfig,
    layer::{Layer, NoLayer, TopLayer},
    TranslationLayer,
};
use crate::{Parser, ProveCtx, SineParams, TptpLang};

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
    pub selection: SineParams,
    /// Optional in-memory session whose assertions ride in as hypotheses.
    pub session: Option<String>,
    /// Wall-clock budget in seconds (0 = unlimited).
    pub timeout_secs: u64,
    /// TPTP language for the generated problem: `Auto` / `Fof` / `Tff` go
    /// through the first-order pipeline (`Auto` upgrades to TFF when the
    /// selected axioms carry numerals); `Thf` assembles through the
    /// translation layer's higher-order pipeline instead.
    pub mode: TptpLang,
}

impl CommonProverOpts for ExternalOpts {
    fn selection(&self) -> SineParams {
        self.selection
    }
    fn timeout(&self) -> u64 {
        self.timeout_secs
    }
    fn set_timeout(&mut self, secs: u64) {
        self.timeout_secs = secs;
    }
    fn set_session(&mut self, session: Option<String>) {
        self.session = session;
    }
    fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }
}

// The conjecture is the shared [`crate::prover::Conjecture`].  External interns
// it into the shared store (under a per-ask query tag) in `intern_conjecture`,
// resolves the roots into `Conjecture.sents`, and truncates the tag in `cleanup`.
pub(crate) use super::Conjecture;

/// The external-prover top layer: `backend` drives a TPTP prover over problems
/// the stack beneath translates.  `T` is that stack -- the bare
/// [`TranslationLayer`] for the CLI's external backends, or the native
/// [`ProverLayer<TranslationLayer>`](crate::prover::ProverLayer) when both
/// engines must share one KB (the browser toggles per query).  External sits
/// on top because it registers no reactors of its own, so every event still
/// reaches the native layer's caches beneath it.
#[derive(Debug)]
pub struct ExternalProverLayer<T: HasTranslation + 'static = TranslationLayer> {
    /// The external prover configured for this layer
    backend: Prover,
    /// The stack beneath: translation, possibly with the native prover on top
    inner: T,
    /// The cache config object
    config: crate::cache::CacheConfig,
    /// Query tags staged by `intern_conjecture` and not yet cleaned up, keyed
    /// by the conjecture's root sids.  Each ask stages under its own tag so
    /// concurrent asks never truncate each other's conjecture; two live asks
    /// of the same conjecture may release either tag, since each holds its
    /// own reference to the shared sentences.
    query_tags: dashmap::DashMap<Vec<SentenceId>, Vec<String>>,
}

impl<T: HasTranslation + 'static> ExternalProverLayer<T> {
    /// Create a new external prover layer from a backend and the stack beneath
    pub(crate) fn new(backend: Prover, inner: T) -> Self {
        Self {
            backend,
            inner,
            config: CacheConfig::default(),
            query_tags: Default::default(),
        }
    }

    /// Override the configured prover backend (e.g. after opening a persisted KB,
    /// which installs the default runner).
    pub fn set_backend(&mut self, backend: Prover) {
        self.backend = backend;
    }

    /// The stack beneath this layer (e.g. the native prover, when nested).
    pub fn inner_layer(&self) -> &T {
        &self.inner
    }

    /// The translation layer somewhere beneath this one.
    fn translation(&self) -> &TranslationLayer {
        self.inner.translation()
    }

    /// Re-ingest a query tag staged by `intern_conjecture` empty.
    fn truncate_query_tag(&self, tag: String) {
        let _ = self.cascade(vec![Event::SourceAdded {
            file: SourceFile::truncate(std::path::PathBuf::from(&tag)),
            session: Arc::new(tag),
            staged: false,
        }]);
    }
}

impl<T: HasTranslation + 'static> Layer for ExternalProverLayer<T> {
    type Inner = T;

    type Outer = NoLayer;

    fn inner(&self) -> Option<&Self::Inner> {
        Some(&self.inner)
    }

    fn own_reactors(&self) -> Vec<crate::cache::router::ReactorEntry<'_>> {
        vec![]
    }

    // One cell per concrete `T`: a `static` inside a generic fn is shared by
    // every instantiation, which would dispatch one stack's cascade schedule
    // to another's reactors (same guard as `ProverLayer<S>`).
    fn schedule_cell(&self) -> &'static crate::layer::ScheduleCell {
        use std::any::TypeId;
        use std::collections::HashMap;
        use std::sync::Mutex;
        static CELLS: Mutex<Option<HashMap<TypeId, &'static crate::layer::ScheduleCell>>> =
            Mutex::new(None);
        let mut cells = CELLS.lock().unwrap();
        cells
            .get_or_insert_with(HashMap::new)
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::leak(Box::new(std::sync::OnceLock::new())))
    }

    fn cache_config(&self) -> &crate::cache::CacheConfig {
        &self.config
    }

    fn outer(&self) -> Option<&NoLayer> {
        None
    }
}

impl<T: HasTranslation + 'static> ProvingLayer for ExternalProverLayer<T> {
    type Opts = ExternalOpts;

    /// Deferred rewrite pass + predicate-variable schema detection (idempotent).
    fn warm_up(&self) {
        self.translation().ensure_rewrite_pass();
    }

    /// Intern the conjecture into the **shared store** under a fresh query tag
    /// (the `&self` cascade) so the TPTP builder can resolve + mark it, and
    /// resolve its roots into `Conjecture.sents`.  The shared `prepare`
    /// default wraps this and errors on an empty result; `cleanup` truncates
    /// the tag.
    fn intern_conjecture(
        &self,
        asts: &[crate::AstNode],
    ) -> Vec<(std::sync::Arc<crate::types::Sentence>, SentenceId)> {
        let syn = &self.translation().semantic.syntactic;
        let tag = syn.unique_source_key("query");
        let mut asts = asts.to_vec();
        for ast in &mut asts {
            ast.attribute_to(&tag);
        }
        let _ = self.cascade(vec![Event::SourceAdded {
            session: Arc::new(tag.clone()),
            file: SourceFile {
                parser: Parser::Kif { options: None },
                name: tag.clone(),
                path: std::path::PathBuf::new(),
                origin: FileOrigin::Inline,
                contents: String::new(),
                prebuilt: Some(asts),
            },
            staged: false,
        }]);
        // Full tag membership (new + content-addressed dups alike), resolved.
        let sents: Vec<_> = syn
            .file_root_sids(&tag)
            .into_iter()
            .filter_map(|sid| syn.sentence(sid).map(|arc| (arc, sid)))
            .collect();
        if sents.is_empty() {
            self.truncate_query_tag(tag);
        } else {
            self.query_tags
                .entry(sents.iter().map(|(_, sid)| *sid).collect())
                .or_default()
                .push(tag);
        }
        sents
    }

    /// Roll the conjecture parse back -- re-ingest its query tag empty.
    fn cleanup(&self, conj: Conjecture) {
        let key: Vec<SentenceId> = conj.sents.iter().map(|(_, sid)| *sid).collect();
        let tag = self
            .query_tags
            .get_mut(&key)
            .and_then(|mut tags| tags.pop());
        self.query_tags.remove_if(&key, |_, tags| tags.is_empty());
        if let Some(tag) = tag {
            self.truncate_query_tag(tag);
        }
    }

    fn prove_once(
        &self,
        conj: &Conjecture,
        params: SineParams,
        slice: u32,
        opts: &ExternalOpts,
        ctx: &ProveCtx,
    ) -> (ProverResult, usize) {
        self.ext_prove_once(conj, params, slice, opts, ctx)
    }

    fn check_consistency(
        &self,
        opts: &Self::Opts,
        ctx: &crate::ProveCtx,
    ) -> super::result::ProverResult {
        self.ext_check_consistency(&[], opts, ctx)
    }

    /// Focused audit: one satisfiability check over `focus`'s SInE
    /// neighborhood (the external backend has no enumerator, so `limit` is
    /// ignored and at most one contradiction comes back).
    fn audit_consistency(
        &self,
        focus: &[crate::SentenceId],
        opts: &Self::Opts,
        _limit: usize,
        ctx: &crate::ProveCtx,
    ) -> super::result::ProverResult {
        self.ext_check_consistency(focus, opts, ctx)
    }
}

impl<T: HasTranslation + 'static> TopLayer for ExternalProverLayer<T> {
    fn from_semantic(semantic: crate::semantics::SemanticLayer) -> Self {
        Self {
            inner: T::from_semantic(semantic),
            backend: Prover::default(),
            config: crate::cache::CacheConfig::default(),
            query_tags: Default::default(),
        }
    }

    /// Carry the configured prover backend + cache config onto a clone -- the
    /// default `from_semantic` would reset them, leaving the clone unable to
    /// invoke the external prover.  The stack beneath carries its own config
    /// the same way (the translation layer's emission mode, for one).
    fn fresh_config_clone(&self, semantic: crate::semantics::SemanticLayer) -> Self {
        Self {
            inner: self.inner.fresh_config_clone(semantic),
            backend: self.backend.clone(),
            config: self.config.clone(),
            query_tags: Default::default(),
        }
    }

    fn semantic(&self) -> &crate::semantics::SemanticLayer {
        self.inner.semantic()
    }

    fn semantic_mut(&mut self) -> &mut crate::semantics::SemanticLayer {
        self.inner.semantic_mut()
    }
}

impl<T: HasTranslation + 'static> HasTranslation for ExternalProverLayer<T> {
    fn translation(&self) -> &TranslationLayer {
        self.inner.translation()
    }
    fn translation_mut(&mut self) -> &mut TranslationLayer {
        self.inner.translation_mut()
    }
}
