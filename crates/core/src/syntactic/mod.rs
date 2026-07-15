//! Bottom layer of the KB stack.  The [`SyntacticLayer`] provides the
//! syntactical construction structure and methods for KIF-based symbol
//! tables.  This is the primary persistent layer, constructed following
//! parsing of input strings.
//!
//! Submodules split the impl across responsibilities:
//!   intern.rs   -- symbol interning + name/id lookup
//!   sentence.rs -- sentence allocation, AST -> Sentence build, ScopeCtx
//!   index.rs    -- occurrence + head + axiom-symbol indices
//!   remove.rs   -- sentence/file removal + orphaned-symbol pruning
//!   lookup.rs   -- head-based sentence lookup (`by_head`)
//!   pattern.rs  -- typed structural pattern matching (`SentencePattern`, `find_by_pattern`)
//!   position.rs -- position-based queries (byte offset -> element)
//!   display.rs  -- ANSI / plain KIF rendering
//!   load.rs     -- top-level `load_kif` driver
//!   persist.rs  -- LMDB persistence helpers (cfg `persist`)

use std::collections::HashMap;

use crate::cache::events::Event;
use crate::cache::{Cache, CacheBehavior, CacheConfig, Eager, EagerMap};
use crate::layer::{Layer, NoLayer};
use crate::semantics::SemanticLayer;
use crate::types::{ElementVec, SentenceId};

use caches::axiom_index::AxiomIndex;
use caches::residue_index::ResidueCache;
use caches::occurrences::OccurrenceIndex;
use caches::sentence_symbols::SentenceSymbols;
use caches::sentence_vars::SentenceVars;
use caches::term_facts::TermFactsCache;
use caches::session::SessionCache;
use caches::sine_index::SineCache;
use caches::sentences::SentenceCache;
use caches::source::SourceCache;
use caches::symbol::SymbolCache;

pub mod collect;
#[cfg(test)]
#[path = "tests.rs"]
mod e2e;
pub mod sentence;
pub mod position;
pub mod display;
pub mod sine;
mod select;
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub(crate) use select::SelectionParams;
pub mod caches;
pub(crate) mod pattern;

pub(crate) use display::{SourceMode, sentence_to_plain_kif};

/// The parsed store containing parsed sentences, symbols, and literals.
///
/// Populated incrementally by [`Self::load_kif`].  Symbol and sentence IDs are
/// stable `u64` values driven by explicit atomic-style counters that can be
/// seeded from LMDB on `open()`, ensuring no ID collision between in-memory
/// and persisted data.
#[derive(Debug)]
pub(crate) struct SyntacticLayer {
    /// Store for all AST Source node information.  Disabled when source caching
    /// is off.
    pub source:                     EagerMap<SourceCache>,
    /// Primary sentence store: content-addressed `EagerMap`
    /// (`SentenceId → Arc<Sentence>`) with provenance / refcount / scope
    /// companion state in its `Side`.
    pub sentences:                  EagerMap<SentenceCache>,
    /// Symbol table: content-addressed (`SymbolId = hash(name)`) name store with
    /// sparse Skolem side-data.  Written imperatively by the sentence build path
    /// (`intern`); reacts to no events.
    pub symbols:                    EagerMap<SymbolCache>,
    /// Per-session sentence membership + axiom status.  Membership is written
    /// imperatively by the build path (`register`); the cache reacts to
    /// `SessionAxiomatized` to flip a session's axiom flag.
    pub(crate) sessions:            EagerMap<SessionCache>,
    /// Reverse index: SymbolId -> every occurrence of that symbol in the KB.
    pub occurrences:                EagerMap<OccurrenceIndex>,
    /// Root sentences indexed by residue fingerprints — the view lattice
    /// unifying head/subject/partial-pattern lookup, with decodable two-word
    /// sketches.  See [`caches::residue_index`].
    pub residue:                    Eager<ResidueCache>,
    /// Axiom-occurrence reverse index: symbol -> axiom SentenceIds it appears in.
    pub(crate) axiom_index:         EagerMap<AxiomIndex>,
    /// Compute cache listing all symbols inside a sentence (recursive).
    /// Memoization and persistence disabled by default.
    pub(crate) sentence_symbols:    Cache<SentenceSymbols>,
    /// Compute cache listing all variables inside a sentence (recursive).
    /// Memoization and persistence disabled by default.
    pub(crate) sentence_vars:       Cache<SentenceVars>,
    /// Lazy per-sid structural term facts (ground / size / depth /
    /// symbol Bloom), content-addressed and reactive on `RootRemoved`.
    /// See [`caches::term_facts`].
    pub(crate) term_facts:          Cache<TermFactsCache>,
    /// Synthetic SentenceId → origin root.
    pub(crate) synthetic_origin:    HashMap<SentenceId, SentenceId>,

    /// Eagerly-maintained SInE (Sine Qua Non) axiom-selection index.
    ///
    /// Participates in [`CacheConfig`] (can be disabled in tests) and is
    /// persisted.
    ///
    /// Use the `sine_add_axiom` / `sine_remove_axiom` / `sine_add_axioms` /
    /// `sine_rebuild` wrapper methods, which handle symbol extraction.
    /// For read-only access use `sine.with_ref(|opt| …)`.  See [`caches::sine_index`].
    pub(crate) sine:                Eager<SineCache>,

    /// Shared cache config (an `Arc`-backed clone every cache here also holds).
    pub(crate) config:             CacheConfig,
}

impl SyntacticLayer {
    /// Load `text` (tagged `file`) as base-KB **axioms**: ingest, then
    /// axiomatize the session so its roots are permanent.  Returns hard parse
    /// errors.
    ///
    /// Formulas containing row variables (`@VAR`) are automatically expanded into
    /// up to [`crate::row_vars::MAX_ARITY`] concrete variants before being stored.
    ///
    /// ## Error-recovery semantics
    ///
    /// The KIF parser is error-recovering: it returns every top-level
    /// sentence it *could* parse alongside a diagnostic for each bad
    /// one.  Recovered nodes are committed, so a mid-edit file keeps the
    /// rest of its symbols even when one sentence fails to parse.
    pub(crate) fn load_kif(&mut self, text: &str, file: &str) -> Vec<crate::Diagnostic> {
        let errors = self.load_kif_assert(text, file);
        let _ = self.cascade(vec![Event::SessionAxiomatized { session: file.to_owned() }]);
        errors
    }

    /// Load `text` (tagged `file`) as transient **assertions** — ingest only,
    /// no axiomatization.  Roots stay transient (and are evictable when the
    /// session is dropped, unless another session also produced them).
    ///
    /// Hands the raw file to the source reactor as a single `SourceAdded`; the
    /// cascade does the rest: `source` parses + macro-expands + dedups by
    /// fingerprint and emits `FormulaAdded`/`FormulaRemoved`; `store` builds
    /// + CAF-normalizes and emits `RootAdded`; the occurrence / head / axiom
    /// indices react.  Parse errors and duplicate-formula warnings surface as
    /// `Event::Diagnostic`.
    pub(crate) fn load_kif_assert(&mut self, text: &str, file: &str) -> Vec<crate::Diagnostic> {
        let source = crate::types::SourceFile {
            parser:   crate::Parser::Kif,
            name:     file.to_owned(),
            // `path` is stamped into each node's `span.file`; keep it equal to
            // the file tag.
            path:     std::path::PathBuf::from(file),
            origin:   crate::types::FileOrigin::Inline,
            contents: text.to_owned(),
            prebuilt: None,
        };
        let outcome = self.cascade(vec![Event::SourceAdded {
            session: std::sync::Arc::new(file.to_owned()),
            file:    source,
            staged:  false,
        }]);
        let errors = outcome.errors;

        crate::log!(
            Info,
            "sigmakee_rs_core::syntactic",
            format!("loaded '{}': {} root sentences, {} errors",
                file,
                self.num_roots(),
                errors.len())
        );
        errors
    }

    /// Create an empty `SyntacticLayer` with caches configured by `cfg`.
    pub(crate) fn with_config(cfg: &CacheConfig) -> Self {
        // The per-sentence symbol/variable scans are disabled by default;
        // enable via `cfg.enable("syntactic::sentence_*")`.
        cfg.disable(SentenceSymbols::NAME);
        cfg.disable(SentenceVars::NAME);
        Self {
            source:              EagerMap::new(cfg, SourceCache),
            sentences:           EagerMap::new(cfg, SentenceCache),
            symbols:             EagerMap::new(cfg, SymbolCache),
            sessions:            EagerMap::new(cfg, SessionCache),
            occurrences:         EagerMap::new(cfg, OccurrenceIndex),
            residue:             Eager::new(cfg, ResidueCache),
            axiom_index:         EagerMap::new(cfg, AxiomIndex),
            sentence_symbols:    Cache::new(cfg, SentenceSymbols),
            sentence_vars:       Cache::new(cfg, SentenceVars),
            term_facts:          Cache::new(cfg, TermFactsCache),
            synthetic_origin:    HashMap::new(),
            sine:                Eager::new(cfg, SineCache),
            config:              cfg.clone(),
        }
    }
}

impl Default for SyntacticLayer {
    fn default() -> Self {
        Self::with_config(&CacheConfig::default())
    }
}

// -- Rewrite / session support ----------------------------------------------
impl SyntacticLayer {
    /// All implication-shaped roots.  Every root is CAF-normalized, so this is a
    /// straight scan for `(=> …)`-headed roots.
    pub(crate) fn normal_implications(&self) -> Vec<SentenceId> {
        self.root_sids().into_iter()
            .filter(|&sid| self.sentence(sid)
                .is_some_and(|s| matches!(s.op(), Some(crate::parse::OpKind::Implies))))
            .collect()
    }

    /// Allocate a synthetic (rewritten) sentence from `elements` into the main
    /// sentence store, returning its content-hash id.  `_origin` is the root the
    /// rewrite derived it from; the synthetic is stored parent-less.
    pub(crate) fn push_synthetic_sentence(&self, elements: ElementVec, _origin: SentenceId) -> SentenceId {
        self.sentences.push_sentence(elements)
    }
}

impl Layer for SyntacticLayer {
    type Inner = NoLayer;
    type Outer = SemanticLayer;

    fn inner(&self) -> Option<&NoLayer> { None }
    fn outer(&self) -> Option<&SemanticLayer> { None }

    fn schedule_cell(&self) -> &'static crate::layer::ScheduleCell {
        static CELL: crate::layer::ScheduleCell = std::sync::OnceLock::new();
        &CELL
    }

    fn cache_config(&self) -> &CacheConfig { &self.config }

    fn initialize_own_caches(&self) {
        // The residue index is a derived view not in `own_persistable`; the
        // persist-open thaw leaves it empty, so it must be rebuilt from the
        // restored sentence store here.
        self.residue.initialize(self);
    }

    fn own_reactors(&self) -> Vec<crate::cache::router::ReactorEntry<'_>> {
        use crate::cache::router::bind;
        // The cascade, in schedule order:
        //   source  SourceAdded            -> FormulaAdded / FormulaRemoved
        //   store   FormulaAdded/Removed   -> RootAdded / RootRemoved / SentencesChanged
        //   occurrences / residue_index react to RootAdded/Removed
        //   (residue_index re-emits RelationAdded/Removed for the semantic layer).
        //   sessions  SessionAxiomatized -> AxiomsPromoted; RootRemoved cleanup.
        //   axiom_index / sine  react to AxiomsPromoted (add) / RootRemoved (drop).
        vec![
            bind(&self.source,      self),
            bind(&self.sentences,       self),
            bind(&self.sessions,    self),
            bind(&self.occurrences, self),
            bind(&self.residue,     self),
            bind(&self.axiom_index, self),
            bind(&self.sine,        self),
            bind(&self.term_facts,  self),
        ]
    }

    fn own_persistable(&self) -> Vec<&dyn crate::cache::persistence::PersistableCache> {
        // `symbols` is the dedup source the sentence store resolves
        // `Element::Symbol` ids against on thaw.  Freeze order is irrelevant;
        // thaw order is enforced by `restore_caches_from`.
        vec![
            &self.symbols,
            &self.source,
            &self.sentences,
            &self.sessions,
            &self.occurrences,
            &self.axiom_index,
            &self.sine,
        ]
    }

    /// Thaw the syntactic caches, symbol table first.
    ///
    /// `Element::Symbol` serializes as a bare [`crate::SymbolId`]; the name lives
    /// once in the `syntactic::symbols` table.  The symbol table is thawed first
    /// to seed the deserialize-time [`crate::syntactic::sentence`] pool, so the
    /// sentence store's `Element::Symbol` leaves resolve each id to the one shared
    /// `Arc<str>` rather than a fresh string per occurrence.  The pool is torn
    /// down once this layer is restored, success or failure.
    fn restore_caches_from(
        &self,
        backend: &dyn crate::persist::PersistenceBackend,
    ) -> Result<(), crate::Diagnostic> {
        use crate::cache::persistence::PersistableCache;
        use crate::syntactic::sentence::{clear_thaw_pool, seed_thaw_pool};

        self.symbols.thaw(backend)?;
        seed_thaw_pool(self.symbols.snapshot());

        let symbols_key = self.symbols.cache_key();
        let result = (|| -> Result<(), crate::Diagnostic> {
            for cache in self.own_persistable() {
                if cache.cache_key() == symbols_key { continue; }
                cache.thaw(backend)?;
            }
            Ok(())
        })();

        clear_thaw_pool();
        result
    }
}
