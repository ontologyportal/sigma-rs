// crates/core/src/kb/mod.rs
//
// KnowledgeBase -- the single public API type for sigmakee-rs-core.
// The modules in this folder are divided by which API they expose. 
// e.g. semantics.rs -> ../semantics/*

// Module specific imports

use std::collections::HashMap;
use std::sync::RwLock;

use crate::syntactic::SyntacticLayer;
use crate::semantics::SemanticLayer;
use crate::trans::TranslationLayer;
use crate::types::SentenceId;
use crate::sine::{SineIndex, SineParams};

#[cfg(feature = "cnf")]
use crate::types::Clause;
#[cfg(feature = "cnf")]
use crate::ClausifyOptions;

#[cfg(feature = "persist")]
use crate::persist::LmdbEnv;

// Module declarations

#[macro_use]
pub mod progress;
pub mod error;
#[cfg(feature = "ask")]
pub mod prove;
pub mod reconcile;
pub mod export;
#[cfg(feature = "persist")]
pub mod persist;
pub mod man;
pub mod ingest;
pub mod store;
pub mod semantics;
pub mod sine;
#[cfg(feature = "cnf")]
pub mod cnf;
pub mod dis;

// Public re-exports
pub use error::KbError;

// KnowledgeBase structure definition
/// The base structure defining a knowledge base
pub struct KnowledgeBase {
    /// Top of the layer chain.  Owns the entire
    /// `SyntacticLayer` → `SemanticLayer` → `TranslationLayer` stack;
    /// callers reach the inner layers via [`Self::syntactic`],
    /// [`Self::semantic`], and [`Self::translation`].
    pub(in crate::kb) layer: TranslationLayer,

    /// In-memory session assertions: session name → `Vec<SentenceId>`.
    /// Sentences here have NOT been promoted to axioms yet.
    pub(in crate::kb) sessions: HashMap<String, Vec<SentenceId>>,

    /// Deduplication table: formula-hash -> (SentenceId, session).
    /// session=None means promoted axiom; Some(s) means assertion in session s.
    ///
    /// Populated from `DB_FORMULA_HASHES` on open and from fresh
    /// clausifications in `tell` / `load_kif` / `promote_*`.  Gated on
    /// `cnf`: without that feature no clausifier is linked, so there
    /// is no way to compute the canonical hash and duplicate axioms are
    /// accepted silently.
    #[cfg(feature = "cnf")]
    pub(in crate::kb) fingerprints: HashMap<u64, (SentenceId, Option<String>)>,

    /// CNF side-car: pre-computed clauses per sentence.  Populated by
    /// the ingestion path and drained at promote time into the LMDB
    /// clause-dedup tables.
    #[cfg(feature = "cnf")]
    pub(in crate::kb) clauses: HashMap<SentenceId, Vec<Clause>>,

    #[cfg(feature = "cnf")]
    pub(in crate::kb) cnf_mode: bool,

    #[cfg(feature = "cnf")]
    pub(in crate::kb) cnf_opts: ClausifyOptions,

    /// LMDB handle. None = purely in-memory.
    #[cfg(feature = "persist")]
    pub(in crate::kb) db: Option<LmdbEnv>,

    /// Pre-built TFF TPTP for the current axiom set; None when invalidated.
    /// Rebuilt lazily on the first `ask_embedded()` call after the axiom
    /// set changes.  Holds both TFF and FOF shapes — built eagerly in
    /// a single `ensure_axiom_cache` pass so either-mode `ask` /
    /// `ask_embedded` hits a warm IR without a per-query rebuild.
    /// The subprocess `ask` path reuses the cache by seeding a
    /// `NativeConverter` from it and applying SInE filtering at
    /// TPTP-assembly time (see [`assemble_tptp`]'s `axiom_filter`).
    ///
    /// [`assemble_tptp`]: crate::vampire::assemble::assemble_tptp
    #[cfg(feature = "ask")]
    pub(in crate::kb) axiom_cache: Option<crate::vampire::VampireAxiomCacheSet>,

    /// Eagerly-maintained SInE axiom-selection index.
    ///
    /// Every axiom promotion incrementally updates this index (adding
    /// the new axiom and recomputing triggers for axioms that share a
    /// symbol with it).  Consumers:
    ///
    /// - `ask()` (feature = "ask"): SInE-filters the axiom set sent
    ///   to the prover per-conjecture.
    /// - `reconcile_file()`: uses `remove_axiom` on removed sids and
    ///   `select` on the altered-symbol set for smart revalidation.
    /// - External callers (LSP "related axioms", linters, explore
    ///   UIs): free to read via the public `sine_select_for_query`
    ///   / `symbols_of_axiom` / `generality` methods.
    ///
    /// Maintenance cost is microseconds per promote/reconcile edit,
    /// bounded by the shared-symbol fan-out; cheap enough to keep
    /// on every build regardless of whether the prover is linked.
    pub(in crate::kb) sine_index: RwLock<SineIndex>,

    /// Optional progress sink.  When set, the KB's internal
    /// instrumentation emits `ProgressEvent`s through it.  Phase-
    /// timing events (`PhaseStarted` / `PhaseFinished`) flow through
    /// the same sink and are aggregated by whichever consumer cares
    /// (the CLI's `--profile` flag installs an aggregator that
    /// computes per-phase totals from the event stream).
    ///
    /// `None` by default; set via [`KnowledgeBase::set_progress_sink`].
    pub(in crate::kb) progress: Option<crate::progress::DynSink>,
}

// Layer-accessor helpers.  Most call sites today reach through the
// owned chain directly (`self.layer.semantic.syntactic.X`); the
// accessors are kept for callers that prefer the named indirection
// and for future code that wants to swap the layer ownership shape
// without touching every site.  `#[allow(dead_code)]` because most
// of them aren't called yet under default features.
#[allow(dead_code)]
impl KnowledgeBase {
    // -- Layer accessors -------------------------------------------------------

    /// Top layer (translation).
    pub(crate) fn translation(&self) -> &TranslationLayer { &self.layer }

    /// Middle layer (semantic).
    pub(crate) fn semantic(&self) -> &SemanticLayer { &self.layer.semantic }

    /// Bottom layer (raw parse store).
    pub(crate) fn syntactic(&self) -> &SyntacticLayer { &self.layer.semantic.syntactic }

    /// Mut access to the top layer.
    pub(crate) fn translation_mut(&mut self) -> &mut TranslationLayer { &mut self.layer }

    /// Mut access to the middle layer.
    pub(crate) fn semantic_mut(&mut self) -> &mut SemanticLayer { &mut self.layer.semantic }

    /// Mut access to the bottom layer.
    pub(crate) fn syntactic_mut(&mut self) -> &mut SyntacticLayer { &mut self.layer.semantic.syntactic }

    /// Crate-internal read-only access to the underlying [`SyntacticLayer`].
    /// Kept under the historical name for the few external callers
    /// (`#[cfg(test)]` integrations) that already depend on it; new code
    /// should prefer [`Self::syntactic`].
    #[allow(dead_code)]
    pub(crate) fn store_for_testing(&self) -> &SyntacticLayer { self.syntactic() }

    // -- Construction ----------------------------------------------------------
    /// Constructs a new KnowledgeBase
    pub fn new() -> Self {
        let syntactic   = SyntacticLayer::default();
        let semantic    = SemanticLayer::new(syntactic);
        let translation = TranslationLayer::new(semantic);
        Self {
            layer:        translation,
            sessions:     HashMap::new(),
            #[cfg(feature = "cnf")] fingerprints: HashMap::new(),
            #[cfg(feature = "cnf")] clauses:  HashMap::new(),
            #[cfg(feature = "cnf")] cnf_mode: true,
            #[cfg(feature = "cnf")] cnf_opts: ClausifyOptions::default(),
            #[cfg(feature = "persist")] db:   None,
            #[cfg(feature = "ask")]  axiom_cache: None,
            sine_index: RwLock::new(
                SineIndex::new(SineParams::default().tolerance)
            ),
            progress: None,
        }
    }
}

impl Default for KnowledgeBase {
    fn default() -> Self { Self::new() }
}