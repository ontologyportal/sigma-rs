//! Semantic query and validation layer.

pub mod consts;
pub mod types;
pub mod caches;
pub mod errors;
pub mod validate;
pub mod query;
pub mod taxonomy;
pub(crate) mod render;
// Only the native prover's theory oracle reaches most of this module, but
// `TaxonomyRoles` itself (the field below, `tax_role_of`, `domain_role`,
// `range_role`, `query.rs`) is referenced unconditionally, so the module
// cannot be cfg'd out wholesale — suppress dead_code instead.
#[cfg_attr(not(feature = "native-prover"), allow(dead_code))]
pub(crate) mod roles;

use crate::syntactic::SyntacticLayer;
use crate::cache::{Cache, CacheBehavior, CacheConfig, EagerMap};
use crate::layer::Layer;
use crate::semantics::taxonomy::TaxRelation;
use crate::types::SymbolId;

use caches::tax_edges::TaxEdges;
use caches::is_instance::IsInstance;
use caches::is_class::IsClass;
use caches::is_relation::IsRelation;
use caches::is_predicate::IsPredicate;
use caches::is_function::IsFunction;
use caches::has_ancestor::HasAncestor;
use caches::arity::Arity;
use caches::domain::Domain;
use caches::range::Range;
use caches::documentation::Documentation;
use caches::inferred_class::InferredClass;
use caches::validate::Validate;
use caches::subrel_lattice::SubrelLattice;
use caches::trans_reach::TransReach;

/// Middle layer of the KB stack, owning the [`SyntacticLayer`] and providing
/// the semantic queries built on top of it.
///
/// Nominally `pub` but unnameable outside the crate (the `semantics` module
/// is `pub(crate)`): the pub-in-private-module pattern, so the `pub`
/// [`TopLayer`](crate::layer::TopLayer) methods can mention it without
/// tripping `private_interfaces`.
#[derive(Debug)]
pub struct SemanticLayer {
    /// Inner layer: raw parse store.
    pub(crate) syntactic:     SyntacticLayer,

    /// Taxonomy edges, eagerly maintained.  See [`caches::tax_edges`].
    pub(crate) tax_edges:     EagerMap<TaxEdges>,

    /// Whether a symbol is an instance.  See [`caches::is_instance`].
    pub(crate) is_instance:   Cache<IsInstance>,
    /// Whether a symbol is a class.  See [`caches::is_class`].
    pub(crate) is_class:      Cache<IsClass>,
    /// Whether a symbol is a relation.  See [`caches::is_relation`].
    pub(crate) is_relation:   Cache<IsRelation>,
    /// Whether a symbol is a predicate.  See [`caches::is_predicate`].
    pub(crate) is_predicate:  Cache<IsPredicate>,
    /// Whether a symbol is a function.  See [`caches::is_function`].
    pub(crate) is_function:   Cache<IsFunction>,
    /// Whether `ancestor` lies in `sym`'s taxonomy chain.  See [`caches::has_ancestor`].
    pub(crate) has_ancestor:  Cache<HasAncestor>,
    /// Relation arity.  See [`caches::arity`].
    pub(crate) arity:         Cache<Arity>,
    /// Relation argument-domain sorts.  See [`caches::domain`].
    pub(crate) domain:        Cache<Domain>,
    /// Relation range sort(s).  See [`caches::range`].
    pub(crate) range:         Cache<Range>,
    /// `(documentation …)` entries.  See [`caches::documentation`].
    pub(crate) documentation: Cache<Documentation>,
    /// Memoized type inference.  See [`caches::inferred_class`].
    pub(crate) inferred_class: Cache<InferredClass>,
    /// Validation cache. See [`caches::validate`]. Disabled by default
    pub(crate) validate: Cache<Validate>,

    /// A relation's below-set (subrelation lattice incl. mined
    /// rule-edges), with witness pointers.  See [`caches::subrel_lattice`].
    pub(crate) subrel_lattice: Cache<SubrelLattice>,
    /// Ground-fact reachability per (relation, start), with witness
    /// pointers.  See [`caches::trans_reach`].
    pub(crate) trans_reach: Cache<TransReach>,

    /// Shape-recognized taxonomy roles (`SIGMA_RECOGNIZE_ROLES`).  When set,
    /// `tax_role_of` classifies edges against these ids instead of the
    /// hard-coded English names, so a renamed dialect still builds its
    /// taxonomy.  Unset (the default) uses the global names.  Installed once
    /// via [`Self::ensure_taxonomy_roles`].
    pub(crate) tax_roles: std::sync::OnceLock<crate::semantics::roles::TaxonomyRoles>,
}

impl SemanticLayer {
    /// Create a new SemanticLayer with a default (all-enabled) cache config.
    pub(crate) fn new(syntactic: SyntacticLayer) -> Self {
        let cfg = CacheConfig::default();
        cfg.disable(Validate::NAME);
        Self::with_config(syntactic, &cfg)
    }

    /// Create a SemanticLayer where all caches share `cfg`.
    pub(crate) fn with_config(syntactic: SyntacticLayer, cfg: &CacheConfig) -> Self {
        let layer = Self {
            syntactic,
            tax_edges:     EagerMap::new(cfg, TaxEdges),
            is_instance:   Cache::new(cfg, IsInstance),
            is_class:      Cache::new(cfg, IsClass),
            is_relation:   Cache::new(cfg, IsRelation),
            is_predicate:  Cache::new(cfg, IsPredicate),
            is_function:   Cache::new(cfg, IsFunction),
            has_ancestor:  Cache::new(cfg, HasAncestor),
            arity:         Cache::new(cfg, Arity),
            domain:        Cache::new(cfg, Domain),
            range:         Cache::new(cfg, Range),
            validate:      Cache::new(cfg, Validate),

            documentation:  Cache::new(cfg, Documentation),
            inferred_class: Cache::new(cfg, InferredClass),
            subrel_lattice: Cache::new(cfg, SubrelLattice),
            trans_reach:    Cache::new(cfg, TransReach),
            tax_roles:      std::sync::OnceLock::new(),
        };
        use crate::layer::Layer;
        layer.initialize_caches();
        layer
    }
}

impl Default for SemanticLayer {
    fn default() -> Self {
        Self::new(SyntacticLayer::default())
    }
}

impl SemanticLayer {
    // -- Shape-recognized taxonomy roles ---------------------------------------

    /// Classify a head symbol as a taxonomy edge relation.  Consults the
    /// recognized roles when installed, else the hard-coded global names;
    /// recognized ids that miss fall through to the globals too.
    pub(crate) fn tax_role_of(&self, head_id: SymbolId) -> Option<TaxRelation> {
        if let Some(roles) = self.tax_roles.get() {
            if let Some(rel) = roles.classify(head_id) {
                return Some(rel);
            }
        }
        TaxRelation::from_id(head_id)
    }

    /// Recognize and install the taxonomy roles once (if not already
    /// installed), then rebuild the taxonomy so the renamed edges are
    /// classified.  Idempotent.  Must run after the ontology is loaded, as it
    /// scans the full root set.
    #[cfg(feature = "native-prover")]
    pub(crate) fn ensure_taxonomy_roles(&self) {
        if self.tax_roles.get().is_some() {
            return;
        }
        let roles = crate::semantics::roles::TaxonomyRoles::recognize(
            &self.syntactic,
            self.syntactic.root_sids(),
        );
        if self.tax_roles.set(roles).is_ok() {
            self.rebuild_taxonomy();
        }
    }

    /// The recognized roles, if installed.
    // Called unconditionally from `query.rs` (a path only live under
    // native-prover), so it must stay compiled in every build.
    #[cfg_attr(not(feature = "native-prover"), allow(dead_code))]
    pub(crate) fn recognized_roles(&self) -> Option<crate::semantics::roles::TaxonomyRoles> {
        self.tax_roles.get().copied()
    }

    /// The `domain` relation head id — recognized (renamed dialect) or the
    /// default `hash_name("domain")`.
    pub(crate) fn domain_role(&self) -> SymbolId {
        self.tax_roles.get().map_or_else(
            || crate::semantics::roles::TaxonomyRoles::default().domain,
            |r| r.domain,
        )
    }

    /// The `range` relation head id — recognized or the default.
    pub(crate) fn range_role(&self) -> SymbolId {
        self.tax_roles.get().map_or_else(
            || crate::semantics::roles::TaxonomyRoles::default().range,
            |r| r.range,
        )
    }

    /// Re-prime the taxonomy adjacency (and drop the lazy caches derived
    /// from it) so a newly-installed role vocabulary takes effect.
    #[cfg(feature = "native-prover")]
    fn rebuild_taxonomy(&self) {
        self.tax_edges.side().clear();
        self.tax_edges.clear();
        self.is_instance.clear();
        self.is_class.clear();
        self.is_relation.clear();
        self.is_predicate.clear();
        self.is_function.clear();
        self.has_ancestor.clear();
        self.domain.clear();
        self.range.clear();
        self.documentation.clear();
        self.inferred_class.clear();
        self.subrel_lattice.clear();
        self.trans_reach.clear();
        self.tax_edges.initialize(self);
    }
}

impl Layer for SemanticLayer {
    type Inner = SyntacticLayer;
    type Outer = crate::layer::NoLayer;

    fn inner(&self) -> Option<&SyntacticLayer> { Some(&self.syntactic) }
    fn outer(&self) -> Option<&crate::layer::NoLayer> { None }

    fn schedule_cell(&self) -> &'static crate::layer::ScheduleCell {
        static CELL: crate::layer::ScheduleCell = std::sync::OnceLock::new();
        &CELL
    }

    fn cache_config(&self) -> &crate::cache::CacheConfig { self.syntactic.cache_config() }

    fn own_reactors(&self) -> Vec<crate::cache::router::ReactorEntry<'_>> {
        use crate::cache::router::bind;
        vec![
            bind(&self.tax_edges,      self),
            bind(&self.is_instance,    self),
            bind(&self.is_class,       self),
            bind(&self.is_relation,    self),
            bind(&self.is_predicate,   self),
            bind(&self.is_function,    self),
            bind(&self.has_ancestor,   self),
            bind(&self.arity,          self),
            bind(&self.domain,         self),
            bind(&self.range,          self),
            bind(&self.documentation,  self),
            bind(&self.inferred_class, self),
            bind(&self.subrel_lattice, self),
            bind(&self.trans_reach,    self),
            bind(&self.validate,       self),
        ]
    }

    fn own_persistable(&self) -> Vec<&dyn crate::cache::persistence::PersistableCache> {
        // Only the eagerly-maintained taxonomy edges are primary state; every
        // other semantic cache is a lazy query cache that recomputes on a miss.
        vec![&self.tax_edges]
    }

    fn initialize_own_caches(&self) {
        self.tax_edges.initialize(self);
    }
}
