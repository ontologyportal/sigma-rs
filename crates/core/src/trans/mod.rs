//! Top layer of the KB stack: translation of a KB to a query-ready form for
//! an external prover (TPTP / TFF).
//!
//! Owns the [`TranslationLayer`], which sits above `SemanticLayer` and caches
//! the per-symbol sort annotations and numeric-class characterisations needed
//! to render SUMO formulas as TFF in one pass.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

// Modules
pub mod errors;
pub mod types;
pub mod ir;
pub mod caches;
pub mod sort;
pub(crate) mod lower_thf;
pub(crate) mod term_sorts;
#[cfg(feature = "ask")]
pub(crate) mod poly_expand;
pub mod arith;
pub mod literal;
pub mod symbol;
pub mod operator;
pub mod builtins;
pub(crate) mod rewrite;
pub(crate) mod formulas;
pub(crate) mod lower;

// Public level imports
pub use sort::Sort;
pub use caches::sort_annotations::SortAnnotation;
pub use errors::TranslationError;
// Consumed only by the integrated-prover binding extractor
// (prover/external/backends/vampire/bindings.rs).
#[cfg(feature = "integrated-prover")]
pub use lower::QueryVarMap;
pub(crate) use types::CachedFormula;

use caches::formulas_thf::FormulasThf;
use caches::ho_signatures::HoSignatures;
use caches::symbol_sort::SymbolSort;
use caches::formulas_tff::FormulasTff;
use caches::formulas_fof::FormulasFof;
use caches::sort_annotations::SortAnnotationsCache;
use caches::numeric_sorts::NumericSorts;
use caches::numeric_ancestor_set::NumericAncestorSet;
use caches::poly_variant_symbols::PolyVariantSymbols;
use caches::rewrite_rules::RewriteRulesCache;
#[allow(unused_imports)]

use crate::semantics::SemanticLayer;
use crate::layer::{Layer, NoLayer, TopLayer};
use crate::types::{SentenceId, SymbolId};
use crate::cache::{Cache, CacheConfig, EagerMap, WholeCache};

#[cfg(test)]
mod tests;

/// Translates SUMO formulas into a prover-ready form in a single pass.
///
/// Pre-caches information like `Domain -> Sort` assertions and other macro
/// expansions used to produce valid TPTP from SUMO formulas.
#[derive(Debug)]
pub struct TranslationLayer {
    /// The semantic layer this layer queries for taxonomy / domain / range /
    /// classification info.
    pub(crate) semantic:                SemanticLayer,
    /// Typed sort annotations, lazily built on first access and invalidated
    /// when domain/range axioms change.
    pub(crate) sort_annotations:        Cache<SortAnnotationsCache>,
    /// Every known SUMO numeric class `SymbolId` mapped to its TFF [`Sort`].
    pub(crate) numeric_sorts:           EagerMap<NumericSorts>,
    /// All SUMO class `SymbolId`s that are ancestors (superclasses) of the
    /// three numeric roots.
    pub(crate) numeric_ancestor_set:    WholeCache<NumericAncestorSet>,
    /// Relation `SymbolId`s that need polymorphic TFF variant declarations.
    pub(crate) poly_variant_symbols:    WholeCache<PolyVariantSymbols>,
    /// The extracted rewrite program (Case-1/Case-2 rules, predicate-variable
    /// schemas, and derived suppression) for the current implication set.
    pub(crate) rewrite_rules:           WholeCache<RewriteRulesCache>,
    /// SentenceIds excluded from TPTP emission: template sentences (numeric
    /// subclass characterizations processed by the rewrite pass) and
    /// augmented-away originals.
    pub(crate) suppressed:              RwLock<HashSet<SentenceId>>,
    /// `true` when the rewrite pass must be re-run before the next TPTP
    /// emission / ask / formula-cache fill.  Set by `on_change`, cleared by
    /// `ensure_rewrite_pass()`.
    pub(crate) rewrite_dirty:           std::sync::atomic::AtomicBool,
    /// Reals-only TFF numerics: when set, every numeric sort the lowering
    /// would emit collapses to `$real` (integer literals emit `40.0`,
    /// rationals emit `$quotient(a.0, b.0)`), so no `$int`/`$rat` sorts and no
    /// `$to_real`/`$to_rat` coercions ever appear.  Must be set before the lazy
    /// TFF caches fill, as cached formulas bake it in.  The underlying caches
    /// keep true sorts; the collapse happens at the trans read helpers.
    pub(crate) reals_only:              std::sync::atomic::AtomicBool,
    /// Lazy memoization table populated by `sort_for_symbol`.
    pub(crate) symbol_sort:             Cache<SymbolSort>,
    /// Memoised THF (bi-sorted) signatures for the higher-order lowering.
    pub(crate) ho_signatures:           Cache<HoSignatures>,
    /// SentenceId to THF lowering (higher-order, bi-sorted) with structured
    /// drop reasons.
    pub(crate) formulas_thf:            Cache<FormulasThf>,
    /// SentenceId to [`CachedFormula`] in TFF mode (`hide_numbers = false`).
    /// Suppressed sentences are absent.  Invalidated on taxonomy and
    /// domain/range changes.
    pub(crate) formulas_tff:            Cache<FormulasTff>,
    /// SentenceId to [`CachedFormula`] in FOF mode (`hide_numbers = true`).
    /// `*_decls` fields are always empty (FOF emits no type declarations).
    pub(crate) formulas_fof:            Cache<FormulasFof>,
    /// Memo table for per-query predicate-variable instantiation:
    /// `(schema_sid, binding) -> instantiated synthetic sid`, where `binding`
    /// is the tuple of concrete relations assigned to the schema's predicate
    /// variables (in `pred_vars` order).  Cleared when the rewrite pass
    /// re-runs.
    pub(crate) predvar_cache:           RwLock<HashMap<(SentenceId, Vec<SymbolId>), SentenceId>>,
    /// Every synthetic sentence id ever produced by `instantiate_predvars`.
    /// Lets `synthetic_replacements` reject them so they never leak between
    /// problems.  Persists across the rewrite pass (unlike `predvar_cache`).
    pub(crate) predvar_instances:       RwLock<HashSet<SentenceId>>,
}

impl TranslationLayer {
    /// Construct a `TranslationLayer` over `semantic` with default cache config.
    pub(crate) fn new(semantic: SemanticLayer) -> Self {
        Self::with_config(semantic, &crate::cache::CacheConfig::default())
    }

    /// Construct a `TranslationLayer` whose caches share `cfg`.
    pub(crate) fn with_config(semantic: SemanticLayer, cfg: &CacheConfig) -> Self {
        let layer = Self {
            semantic,
            sort_annotations:     Cache::new(cfg, SortAnnotationsCache),
            numeric_sorts:        EagerMap::new(cfg, NumericSorts),
            numeric_ancestor_set: WholeCache::new(cfg, NumericAncestorSet),
            poly_variant_symbols: WholeCache::new(cfg, PolyVariantSymbols),
            rewrite_rules:        WholeCache::new(cfg, RewriteRulesCache),
            suppressed:           RwLock::new(HashSet::new()),
            rewrite_dirty:        std::sync::atomic::AtomicBool::new(false),
            reals_only:           std::sync::atomic::AtomicBool::new(false),
            symbol_sort:          Cache::new(cfg, SymbolSort),
            ho_signatures:        Cache::new(cfg, HoSignatures),
            formulas_thf:         Cache::new(cfg, FormulasThf),
            formulas_tff:         Cache::new(cfg, FormulasTff),
            formulas_fof:         Cache::new(cfg, FormulasFof),
            predvar_cache:        RwLock::new(HashMap::new()),
            predvar_instances:    RwLock::new(HashSet::new()),
        };
        layer.initialize_own_caches();
        layer
    }
}

impl Layer for TranslationLayer {
    type Inner = SemanticLayer;
    type Outer = NoLayer;

    fn inner(&self) -> Option<&SemanticLayer> { Some(&self.semantic) }
    fn outer(&self) -> Option<&NoLayer> { None }

    fn schedule_cell(&self) -> &'static crate::layer::ScheduleCell {
        static CELL: crate::layer::ScheduleCell = std::sync::OnceLock::new();
        &CELL
    }

    fn cache_config(&self) -> &crate::cache::CacheConfig { self.semantic.cache_config() }

    fn own_reactors(&self) -> Vec<crate::cache::router::ReactorEntry<'_>> {
        use crate::cache::router::bind;
        vec![
            bind(&self.symbol_sort,          self),
            bind(&self.ho_signatures,        self),
            bind(&self.formulas_thf,         self),
            bind(&self.sort_annotations,     self),
            bind(&self.formulas_tff,         self),
            bind(&self.formulas_fof,         self),
            bind(&self.numeric_sorts,        self),
            bind(&self.numeric_ancestor_set, self),
            bind(&self.poly_variant_symbols, self),
            bind(&self.rewrite_rules,        self),
        ]
    }

    fn initialize_own_caches(&self) {
        // Dependency order: `numeric_sorts` first, then the two whole-value sets
        // prime via their compute-on-miss `get`, each reading the cache(s) above.
        self.numeric_sorts.initialize(self);
        let _ = self.numeric_ancestor_set.get(self);
        let _ = self.poly_variant_symbols.get(self);
    }

    fn own_persistable(&self) -> Vec<&dyn crate::cache::persistence::PersistableCache> {
        // Only the eagerly-built whole/keyed indices are snapshotted. Restore
        // performs no cascade replay, so a thawed lazy entry would be served
        // unvalidated against the restored state; lazy caches recompute on miss.
        vec![
            &self.numeric_sorts,
            &self.numeric_ancestor_set,
            &self.poly_variant_symbols,
        ]
    }
}

impl TopLayer for TranslationLayer {
    fn from_semantic(semantic: crate::semantics::SemanticLayer) -> Self {
        Self::new(semantic)
    }

    fn fresh_config_clone(&self, semantic: crate::semantics::SemanticLayer) -> Self {
        let layer = Self::from_semantic(semantic);
        // Emission config is not a cache — carry it across clones.
        layer.set_reals_only(self.reals_only());
        layer
    }

    fn semantic(&self) -> &crate::semantics::SemanticLayer {
        &self.semantic
    }

    fn semantic_mut(&mut self) -> &mut crate::semantics::SemanticLayer {
        &mut self.semantic
    }
}

impl TranslationLayer {
    /// Switch the reals-only TFF numeric mode (see the `reals_only` field).
    /// Must be set before the first ask / TFF cache fill.
    pub fn set_reals_only(&self, on: bool) {
        self.reals_only.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether the reals-only TFF numeric mode is active.
    #[inline]
    pub(crate) fn reals_only(&self) -> bool {
        self.reals_only.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Collapse a numeric sort to `$real` under reals-only mode; identity
    /// otherwise.  Applied at every sort-producing read the lowering uses.
    #[inline]
    pub(crate) fn collapse_numeric(&self, s: Sort) -> Sort {
        if s != Sort::Individual && self.reals_only() { Sort::Real } else { s }
    }
}

/// Set the reals-only numeric emission mode on any KB whose stack carries a
/// translation layer (see [`TranslationLayer::set_reals_only`]).
impl<L: HasTranslation> crate::KnowledgeBase<L> {
    /// Set the reals-only TFF numeric emission mode.
    /// See [`TranslationLayer::set_reals_only`].
    pub fn set_reals_only(&self, on: bool) {
        self.layer.translation().set_reals_only(on);
    }
}

/// A [`TopLayer`] that carries a [`TranslationLayer`] in its stack.
pub trait HasTranslation: TopLayer {
    /// Shared access to the translation layer.
    fn translation(&self) -> &TranslationLayer;
    /// Mutable access to the translation layer.
    fn translation_mut(&mut self) -> &mut TranslationLayer;
}

impl HasTranslation for TranslationLayer {
    fn translation(&self) -> &TranslationLayer { self }
    fn translation_mut(&mut self) -> &mut TranslationLayer { self }
}