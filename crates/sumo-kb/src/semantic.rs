// crates/sumo-kb/src/semantic.rs
//
// Semantic query and validation layer.
//
// Ported from sumo-parser-core/src/kb.rs -- semantic methods only.
// `SemanticLayer` owns the `KifStore` and wraps it with a lazy semantic
// cache.  `KnowledgeBase` (kb.rs) holds a `SemanticLayer` as its only store
// of truth and delegates all semantic queries through it.

use std::sync::{RwLock, RwLockReadGuard};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::error::SemanticError;
use crate::kif_store::KifStore;
use crate::types::{Element, Literal, OpKind, SentenceId, SymbolId, TaxEdge, TaxRelation};

// -- Sort ----------------------------------------------------------------------

/// Primitive sort of a SUMO term, independent of any proof target.
///
/// Ordered by specificity: Individual (least) < Real < Rational < Integer (most).
/// `Ord` lets `max(a, b)` pick the more specific sort when multiple constraints
/// conflict -- the winner is always the strongest supported sort.
///
/// TPTP mapping (call `.tptp()` at the tptp/ boundary only):
///   Individual -> "$i"
///   Real       -> "$real"
///   Rational   -> "$rat"
///   Integer    -> "$int"
///
/// `$o` (formula/Boolean sort) is NOT in this enum. It is a TPTP-specific
/// concept with no semantic meaning and is emitted as a literal string inside
/// `tptp/tff.rs` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord,
          serde::Serialize, serde::Deserialize)]
pub enum Sort {
    Individual = 1,
    Real       = 2,
    Rational   = 3,
    Integer    = 4,
}

impl Sort {
    /// Convert to the TPTP sort string.
    /// Call only inside `tptp/` -- never let this string escape into semantic logic.
    ///
    /// Currently only exercised from this module's tests; kept on the
    /// public API so the TFF-emitter code paths in `vampire/converter.rs`
    /// and downstream clausify consumers can call it without a re-export
    /// gymnastics.
    #[allow(dead_code)]
    pub fn tptp(self) -> &'static str {
        match self {
            Sort::Individual => "$i",
            Sort::Real       => "$real",
            Sort::Rational   => "$rat",
            Sort::Integer    => "$int",
        }
    }
}

// -- ArithCond -----------------------------------------------------------------

/// Arithmetic condition characterizing numeric-class membership.
///
/// When `(instance ?X C)` appears in TFF mode and `?X` has a numeric sort, the
/// translator substitutes this condition for the otherwise-unsound `$true` drop.
/// The variable is always implicit (the instance variable being checked).
/// `bound` is the raw numeric literal string from the source KIF (e.g. `"0"`, `"1"`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) enum ArithCond {
    GreaterThan          { bound: String },
    GreaterThanOrEqualTo { bound: String },
    LessThan             { bound: String },
    LessThanOrEqualTo    { bound: String },
    And(Vec<ArithCond>),
    /// `(equal (fn_name ?VAR other_arg) result)` — e.g. `(equal (RemainderFn ?X 2) 0)`.
    EqualFn { fn_name: String, other_arg: String, result: String },
}

// -- RelationDomain ------------------------------------------------------------

/// Describes the expected type of a relation argument or return value.
#[derive(Debug, Clone)]
pub(crate) enum RelationDomain {
    /// Argument must be an instance of this class.
    Domain(SymbolId),
    /// Argument must be a subclass of this class.
    DomainSubclass(SymbolId),
}

impl RelationDomain {
    pub(crate) fn id(&self) -> SymbolId {
        match self {
            Self::Domain(id) | Self::DomainSubclass(id) => *id,
        }
    }
}

// -- SemanticCache -------------------------------------------------------------

#[derive(Debug, Default)]
struct SemanticCache {
    is_instance:  HashMap<SymbolId, bool>,
    is_class:     HashMap<SymbolId, bool>,
    is_relation:  HashMap<SymbolId, bool>,
    is_predicate: HashMap<SymbolId, bool>,
    is_function:  HashMap<SymbolId, bool>,
    has_ancestor: HashMap<(SymbolId, SymbolId), bool>,
    arity:        HashMap<SymbolId, Option<i32>>,
    domain:       HashMap<SymbolId, Vec<RelationDomain>>,
    range:        HashMap<SymbolId, RelationDomain>,
}

// -- Numeric sort roots --------------------------------------------------------
//
// The three SUMO class names that anchor the TFF numeric sort hierarchy.
// Everything in the KB that is a subclass of one of these (discovered
// dynamically at taxonomy-build time) maps to the corresponding Sort.
// Only these three strings are ever hardcoded; all subclasses are found
// automatically by walking the subclass edges downward.
//
// Order matters for the BFS: process least-specific (Real) first so that
// a more-specific sort (Integer) overwrites it when a class descends from
// multiple roots (e.g. NonnegativeInteger is under both Integer and
// NonnegativeRealNumber -> gets Sort::Integer because Integer is last).
const NUMERIC_ROOTS: &[(&str, Sort)] = &[
    ("RealNumber",    Sort::Real),
    ("RationalNumber", Sort::Rational),
    ("Integer",       Sort::Integer),
];

// -- SortAnnotations -----------------------------------------------------------

/// Precomputed TFF sort signatures for all relations and functions in the KB.
///
/// Derived from SUMO `domain` and `range` axioms, keyed by SymbolId.
/// Equivalent to what `TffContext::signatures` and `TffContext::return_sorts`
/// accumulate lazily during translation, but precomputed for the whole KB.
///
/// `DomainSubclass` argument positions map to `Sort::Individual` (variables in
/// subclass positions are ontological individuals in TFF).
/// The sentinel `u64::MAX` in a `RelationDomain` also maps to `Sort::Individual`.
///
/// Built lazily; cleared by `invalidate_cache()`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SortAnnotations {
    /// Ordered argument sorts for all relations, predicates, and functions
    /// that have at least one `domain` axiom.
    pub symbol_arg_sorts:    HashMap<SymbolId, Vec<Sort>>,
    /// Return sort for all function symbols.
    /// Only populated for functions; predicates/relations are absent.
    pub symbol_return_sorts: HashMap<SymbolId, Sort>,
    /// Sort of individual constants (non-function, non-relation symbols) that
    /// are `instance`-related to a numeric SUMO class.
    /// E.g. `(instance Pi PositiveRealNumber)` -> `Pi -> Sort::Real`.
    pub symbol_individual_sorts: HashMap<SymbolId, Sort>,
}

// -- SemanticLayer -------------------------------------------------------------

/// Owns the `KifStore` and provides all semantic queries on top of it.
///
/// Semantic results are cached in a `RefCell<SemanticCache>` so that query
/// methods take `&self`, allowing `to_tptp` and similar readers to hold
/// `&self.store` while calling semantic methods without borrow-checker conflicts.
///
/// The taxonomy graph (`tax_edges`, `tax_incoming`) lives here rather than in
/// `KifStore` because it is derived semantic structure, not raw storage.
#[derive(Debug)]
pub(crate) struct SemanticLayer {
    pub store:           KifStore,
    /// Taxonomy edges (subclass, instance, subrelation, subAttribute).
    pub tax_edges:       Vec<TaxEdge>,
    /// `tax_incoming[sym_id]` = indices into `tax_edges` where `edge.to == sym_id`.
    pub tax_incoming:    HashMap<SymbolId, Vec<usize>>,
    /// Maps every known SUMO numeric class `SymbolId` -> its TFF [`Sort`].
    ///
    /// Built by `rebuild_taxonomy` via a downward BFS from the three roots
    /// in `NUMERIC_ROOTS`.  Lookups are O(1) integer comparisons -- no string
    /// operations after the initial taxonomy warm-up.
    numeric_sort_cache:     HashMap<SymbolId, Sort>,
    /// All SUMO class `SymbolId`s that are ancestors (superclasses) of the
    /// three numeric roots -- i.e., the classes through which numeric classes
    /// inherit: Entity, Abstract, Quantity, Number, RealNumber, etc.
    ///
    /// Used in VTI resolution: a variable constrained by both a numeric class
    /// AND an ancestor class (e.g. [Integer, Entity]) should get the numeric
    /// sort, because Integer IS-A Entity.  A constraint from a non-ancestor
    /// class (e.g. Animal) is a genuine conflict and the variable is left
    /// unannotated (defaults to `$i`).
    ///
    /// Built by `rebuild_taxonomy` via an upward BFS from `NUMERIC_ROOTS`.
    numeric_ancestor_set:   HashSet<SymbolId>,
    /// Relation/function `SymbolId`s that have at least one argument position
    /// whose SUMO domain class is a numeric-ancestor class (in
    /// `numeric_ancestor_set`) but is NOT itself a numeric class (i.e., it
    /// maps to `$i` in TFF, not `$int`/`$rat`/`$real`).
    ///
    /// These symbols need polymorphic TFF variant declarations so that
    /// numeric-sorted arguments (e.g. `$int`) can be passed to positions
    /// whose base declaration says `$i`.  The canonical example: `ListFn`
    /// with `(domain ListFn 1 Entity)` -- Entity is an ancestor of Integer,
    /// so a `$int`-sorted variable may legally appear there; the variant
    /// `s__ListFn__1__int: ($int) > $i` makes the TFF type system agree.
    ///
    /// Built by `rebuild_taxonomy` after `numeric_ancestor_set` is ready.
    poly_variant_symbols:   HashSet<SymbolId>,
    /// Arithmetic characterizations of numeric subclasses.
    /// Built by `build_numeric_char_cache()` after `numeric_sort_cache` is ready.
    numeric_char_cache:     HashMap<SymbolId, ArithCond>,
    cache:                  RwLock<SemanticCache>,
    sort_annotations:       RwLock<Option<SortAnnotations>>,
}

impl SemanticLayer {
    pub(crate) fn new(store: KifStore) -> Self {
        let mut layer = Self {
            store,
            tax_edges:            Vec::new(),
            tax_incoming:         HashMap::new(),
            numeric_sort_cache:   HashMap::new(),
            numeric_ancestor_set: HashSet::new(),
            poly_variant_symbols: HashSet::new(),
            numeric_char_cache:   HashMap::new(),
            cache:                RwLock::new(SemanticCache::default()),
            sort_annotations:     RwLock::new(None),
        };
        layer.rebuild_taxonomy();
        layer
    }

    /// Construct a SemanticLayer with its taxonomy state prepopulated
    /// from a persisted cache.  The `tax_incoming` reverse index is
    /// rederived in one linear pass over `tax_edges` (we don't persist
    /// it because derivation is cheaper than reading it back).
    ///
    /// Skips the full `rebuild_taxonomy` scan -- Phase D's core
    /// cold-open optimisation.
    #[cfg(feature = "persist")]
    pub(crate) fn from_cached_taxonomy(
        store:                KifStore,
        tax_edges:            Vec<TaxEdge>,
        numeric_sort_cache:   HashMap<SymbolId, Sort>,
        numeric_ancestor_set: HashSet<SymbolId>,
        poly_variant_symbols: HashSet<SymbolId>,
        numeric_char_cache:   HashMap<SymbolId, ArithCond>,
    ) -> Self {
        // Rebuild the reverse index (edge_index -> tax_incoming[to]).
        let mut tax_incoming: HashMap<SymbolId, Vec<usize>> = HashMap::new();
        for (i, edge) in tax_edges.iter().enumerate() {
            tax_incoming.entry(edge.to).or_default().push(i);
        }
        Self {
            store,
            tax_edges,
            tax_incoming,
            numeric_sort_cache,
            numeric_ancestor_set,
            poly_variant_symbols,
            numeric_char_cache,
            cache:              RwLock::new(SemanticCache::default()),
            sort_annotations:   RwLock::new(None),
        }
    }

    // Phase D accessors for persistence.  These expose internal state
    // to the persist layer in `write_axioms` without letting arbitrary
    // callers mutate it.

    #[cfg(feature = "persist")]
    pub(crate) fn tax_edges_snapshot(&self) -> Vec<TaxEdge> {
        self.tax_edges.clone()
    }
    #[cfg(feature = "persist")]
    pub(crate) fn numeric_sort_cache_snapshot(&self) -> HashMap<SymbolId, Sort> {
        self.numeric_sort_cache.clone()
    }
    #[cfg(feature = "persist")]
    pub(crate) fn numeric_ancestor_set_snapshot(&self) -> HashSet<SymbolId> {
        self.numeric_ancestor_set.clone()
    }
    #[cfg(feature = "persist")]
    pub(crate) fn poly_variant_symbols_snapshot(&self) -> HashSet<SymbolId> {
        self.poly_variant_symbols.clone()
    }
    #[cfg(feature = "persist")]
    pub(crate) fn numeric_char_cache_snapshot(&self) -> HashMap<SymbolId, ArithCond> {
        self.numeric_char_cache.clone()
    }

    /// Phase D: install a precomputed `SortAnnotations` directly into
    /// the cache slot, bypassing the usual build-on-first-access path.
    #[cfg(all(feature = "persist", feature = "ask"))]
    pub(crate) fn install_sort_annotations(&self, sa: SortAnnotations) {
        *self.sort_annotations.write().unwrap() = Some(sa);
    }

    /// Phase D: snapshot of the current `SortAnnotations`, triggering
    /// the lazy build if needed.
    #[cfg(all(feature = "persist", feature = "ask"))]
    pub(crate) fn sort_annotations_snapshot(&self) -> SortAnnotations {
        // The guard returned by `sort_annotations()` holds the read
        // lock for the duration of this scope; clone the inner
        // `SortAnnotations` out before the guard drops.
        let guard = self.sort_annotations();
        guard.as_ref()
            .expect("sort_annotations() populates the slot")
            .clone()
    }

    /// Invalidate the semantic query cache (call after structural changes to the store).
    /// Does not clear the taxonomy -- call `rebuild_taxonomy` explicitly when sentences
    /// are added or removed.
    ///
    /// This is the "everything" hammer.  Prefer the granular
    /// [`invalidate_semantic_cache`](Self::invalidate_semantic_cache)
    /// and [`invalidate_sort_annotations`](Self::invalidate_sort_annotations)
    /// methods when you know which pieces are actually affected.
    /// Phase B's `extend_taxonomy_with` picks the right granularity
    /// automatically from a sentence impact classification.
    pub(crate) fn invalidate_cache(&self) {
        self.invalidate_semantic_cache();
        self.invalidate_sort_annotations();
    }

    /// Clear the `is_instance` / `is_class` / `is_relation` /
    /// `is_predicate` / `is_function` / `has_ancestor` / `arity` /
    /// `domain` / `range` query cache.
    ///
    /// Invalidate whenever a change could flip one of those queries:
    /// adding a taxonomy edge, a domain/range axiom, or any sentence
    /// that affects symbol classifications (is_function, is_relation,
    /// etc.).
    pub(crate) fn invalidate_semantic_cache(&self) {
        *self.cache.write().unwrap() = SemanticCache::default();
    }

    /// Clear the `SortAnnotations` cache.
    ///
    /// Invalidate whenever a `domain` / `range` / `domainSubclass`
    /// axiom is added or removed -- those are the direct sources of
    /// entries in `SortAnnotations.symbol_arg_sorts` and
    /// `symbol_return_sorts`.
    pub(crate) fn invalidate_sort_annotations(&self) {
        *self.sort_annotations.write().unwrap() = None;
    }

    // -- Taxonomy management ---------------------------------------------------

    /// Extract a taxonomy edge from a single sentence, if applicable.
    ///
    /// Called for every sentence (roots and sub-sentences) when rebuilding.
    /// Non-taxonomy sentences (those not headed by subclass/instance/etc.) are
    /// silently ignored.
    fn extract_tax_edge_for(&mut self, sid: SentenceId) {
        let sentence  = &self.store.sentences[self.store.sent_idx(sid)];
        let head_sym  = match sentence.head_symbol() { Some(id) => id, None => return };
        let head_name = self.store.sym_name(head_sym).to_owned();
        let rel       = match TaxRelation::from_str(&head_name) { Some(r) => r, None => return };
        let arg1 = match sentence.elements.get(1) {
            Some(Element::Symbol(id))                        => *id,
            Some(Element::Variable { id, is_row: false, .. }) => *id,
            _ => return,
        };
        let arg2 = match sentence.elements.get(2) {
            Some(Element::Symbol(id))                        => *id,
            Some(Element::Variable { id, is_row: false, .. }) => *id,
            _ => return,
        };
        let edge_idx = self.tax_edges.len();
        self.tax_edges.push(TaxEdge { from: arg2, to: arg1, rel });
        self.tax_incoming.entry(arg1).or_default().push(edge_idx);
        log::trace!(target: "sumo_kb::semantic",
            "tax edge: {} -{}-> {}", self.store.sym_name(arg2), head_name, self.store.sym_name(arg1));
    }

    /// Rebuild the taxonomy from scratch by scanning all known sentences.
    ///
    /// Call after `store.remove_file` (which removes sentences) or after
    /// loading from LMDB (where sentences are inserted without going through
    /// `build_sentence`).  Also called internally by `SemanticLayer::new`.
    pub(crate) fn rebuild_taxonomy(&mut self) {
        self.tax_edges.clear();
        self.tax_incoming.clear();
        // Scan roots and all sub-sentences.  Taxonomy predicates are always
        // top-level in SUMO, but sub-sentences are included for completeness.
        let mut all_sids = self.store.roots.clone();
        all_sids.extend(self.store.sub_sentences.iter().copied());
        for sid in all_sids {
            self.extract_tax_edge_for(sid);
        }
        log::debug!(target: "sumo_kb::semantic",
            "taxonomy rebuilt: {} edges", self.tax_edges.len());
        self.numeric_sort_cache   = self.build_numeric_sort_cache();
        self.numeric_ancestor_set = self.build_numeric_ancestor_set();
        self.poly_variant_symbols = self.build_poly_variant_symbols();
        self.numeric_char_cache   = self.build_numeric_char_cache();
        log::debug!(target: "sumo_kb::semantic",
            "numeric sort cache: {} classes, {} numeric-ancestor classes, {} poly-variant symbols, \
             {} numeric characterizations",
            self.numeric_sort_cache.len(), self.numeric_ancestor_set.len(),
            self.poly_variant_symbols.len(), self.numeric_char_cache.len());
    }

    /// Build the numeric sort cache by BFS downward from each root in
    /// `NUMERIC_ROOTS`.
    ///
    /// A temporary children index (`parent_id -> [child_ids]`) is constructed
    /// from `tax_edges` so the BFS can walk downward efficiently.  The three
    /// root names are the only hardcoded strings; all subclass SymbolIds are
    /// discovered dynamically.
    ///
    /// Processing order is least-specific -> most-specific (Real -> Rational ->
    /// Integer) so that a more-specific sort overwrites a less-specific one
    /// when a class descends from multiple roots (e.g. NonnegativeInteger is
    /// both under Integer and NonnegativeRealNumber -- it ends up as Integer).
    fn build_numeric_sort_cache(&self) -> HashMap<SymbolId, Sort> {
        // Build a temporary children index: parent_id -> [child_id, ...]
        // In tax_edges: from = parent (superclass), to = child (subclass).
        let mut children: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
        for edge in &self.tax_edges {
            if edge.rel == TaxRelation::Subclass {
                children.entry(edge.from).or_default().push(edge.to);
            }
        }

        let mut cache: HashMap<SymbolId, Sort> = HashMap::new();

        for &(root_name, sort) in NUMERIC_ROOTS {
            let root_id = match self.store.sym_id(root_name) {
                Some(id) => id,
                None     => continue,  // root class not present in this KB
            };

            // BFS downward from root_id, including the root itself.
            let mut queue:   VecDeque<SymbolId> = VecDeque::new();
            let mut visited: HashSet<SymbolId>  = HashSet::new();
            queue.push_back(root_id);
            while let Some(id) = queue.pop_front() {
                if !visited.insert(id) { continue; }  // cycle guard
                cache.insert(id, sort);
                if let Some(kids) = children.get(&id) {
                    for &kid in kids {
                        if !visited.contains(&kid) {
                            queue.push_back(kid);
                        }
                    }
                }
            }
        }

        cache
    }

    /// Build the set of all SUMO classes that are *ancestors* (superclasses)
    /// of the numeric roots -- the classes through which numeric classes inherit:
    /// Integer -> RationalNumber -> RealNumber -> Number -> Quantity -> Abstract -> Entity.
    ///
    /// BFS walks *upward* from each root in `NUMERIC_ROOTS` through subclass
    /// edges.  `tax_incoming[id]` gives edges where `id` is the subclass (`to`),
    /// so `edge.from` is the direct superclass.
    ///
    /// The numeric roots themselves are included in the returned set so that
    /// numeric-class constraints (e.g. [Integer, RealNumber]) are always
    /// treated as compatible with one another.
    fn build_numeric_ancestor_set(&self) -> HashSet<SymbolId> {
        let mut ancestors: HashSet<SymbolId> = HashSet::new();
        let mut queue: VecDeque<SymbolId>    = VecDeque::new();

        for &(root_name, _) in NUMERIC_ROOTS {
            if let Some(root_id) = self.store.sym_id(root_name) {
                if ancestors.insert(root_id) {
                    queue.push_back(root_id);
                }
            }
        }

        while let Some(id) = queue.pop_front() {
            // tax_incoming[id] = edge indices where `edge.to == id` (id is the subclass).
            // edge.from = the superclass of id -- walk upward.
            if let Some(edge_indices) = self.tax_incoming.get(&id) {
                for &edge_idx in edge_indices {
                    let edge = &self.tax_edges[edge_idx];
                    if edge.rel == TaxRelation::Subclass {
                        let parent = edge.from;
                        if ancestors.insert(parent) {
                            queue.push_back(parent);
                        }
                    }
                }
            }
        }

        ancestors
    }

    /// Build the set of relation/function `SymbolId`s that need polymorphic
    /// TFF variant declarations.
    ///
    /// A symbol qualifies when at least one of its `domain` axiom classes is:
    ///   1. In `numeric_ancestor_set` -- a superclass of the numeric hierarchy
    ///      (Entity, Quantity, Abstract, Number, ...), meaning a numeric-sorted
    ///      value legitimately satisfies the position; AND
    ///   2. NOT in `numeric_sort_cache` -- it is not itself a numeric class
    ///      (which would already produce a numeric sort in the base declaration).
    ///
    /// Examples:
    ///   `(domain ListFn 1 Entity)` -> Entity qualifies -> ListFn added.
    ///   `(domain GCDFn 1 Integer)` -> Integer fails condition 2 -> not added.
    ///   `(domain foo 1 Animal)`    -> Animal fails condition 1 -> not added.
    fn build_poly_variant_symbols(&self) -> HashSet<SymbolId> {
        let mut result: HashSet<SymbolId> = HashSet::new();
        let sids = self.store.by_head("domain").to_vec();
        for sid in sids {
            let sentence = &self.store.sentences[self.store.sent_idx(sid)];
            // (domain Relation Position Class)
            let rel_id = match sentence.elements.get(1) {
                Some(Element::Symbol(id)) => *id,
                _ => continue,
            };
            let class_id = match sentence.elements.get(3) {
                Some(Element::Symbol(id)) => *id,
                _ => continue,
            };
            if self.numeric_ancestor_set.contains(&class_id)
                && !self.numeric_sort_cache.contains_key(&class_id)
            {
                result.insert(rel_id);
            }
        }
        result
    }

    // -- Numeric characterization cache ----------------------------------------

    /// Build arithmetic characterizations of numeric subclasses.
    ///
    /// Scans root sentences for:
    ///   Form A: `(<=> (instance ?VAR C) conditions)` — biconditional (preferred)
    ///   Form B: `(=> ANT (instance ?VAR C))` — forward implication (fallback)
    ///
    /// The extracted condition is stored with the variable implicit; at emit time
    /// the actual variable name is substituted.  Root numeric classes
    /// (RealNumber, RationalNumber, Integer) are excluded — their sort membership
    /// is already encoded by the TFF quantifier annotation.
    fn build_numeric_char_cache(&self) -> HashMap<SymbolId, ArithCond> {
        let mut result: HashMap<SymbolId, ArithCond> = HashMap::new();

        let root_ids: HashSet<SymbolId> = NUMERIC_ROOTS.iter()
            .filter_map(|(name, _)| self.store.sym_id(name))
            .collect();

        for &root_sid in &self.store.roots {
            let sentence = &self.store.sentences[self.store.sent_idx(root_sid)];

            // Form A: (<=> (instance ?VAR C) conditions)
            if matches!(sentence.elements.first(), Some(Element::Op(OpKind::Iff))) {
                if let (Some(Element::Sub(lhs)), Some(Element::Sub(rhs))) =
                    (sentence.elements.get(1), sentence.elements.get(2))
                {
                    if let Some((class_id, var_name)) = self.extract_instance_clause(*lhs) {
                        if !root_ids.contains(&class_id)
                            && self.numeric_sort_cache.contains_key(&class_id)
                        {
                            if let Some(cond) = self.extract_arith_cond(*rhs, &var_name) {
                                result.insert(class_id, cond);
                            }
                        }
                    }
                }
            }

            // Form B: (=> ANT (instance ?VAR C)) — sufficient condition; only if not already found.
            // Form C: (=> (instance ?VAR C) CON) — necessary condition; only if not already found.
            if matches!(sentence.elements.first(), Some(Element::Op(OpKind::Implies))) {
                if let (Some(Element::Sub(ant)), Some(Element::Sub(con))) =
                    (sentence.elements.get(1), sentence.elements.get(2))
                {
                    // Form B: consequent is the instance check
                    if let Some((class_id, var_name)) = self.extract_instance_clause(*con) {
                        if !root_ids.contains(&class_id)
                            && self.numeric_sort_cache.contains_key(&class_id)
                            && !result.contains_key(&class_id)
                        {
                            if let Some(cond) = self.extract_arith_cond(*ant, &var_name) {
                                result.insert(class_id, cond);
                            }
                        }
                    }
                    // Form C: antecedent is the instance check
                    if let Some((class_id, var_name)) = self.extract_instance_clause(*ant) {
                        if !root_ids.contains(&class_id)
                            && self.numeric_sort_cache.contains_key(&class_id)
                            && !result.contains_key(&class_id)
                        {
                            if let Some(cond) = self.extract_arith_cond(*con, &var_name) {
                                result.insert(class_id, cond);
                            }
                        }
                    }
                }
            }
        }

        result
    }

    /// If `sid` represents `(instance ?VAR C)`, return `(C_id, var_name)`.
    fn extract_instance_clause(&self, sid: SentenceId) -> Option<(SymbolId, String)> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        if let (
            Some(Element::Symbol(inst_id)),
            Some(Element::Variable { name, .. }),
            Some(Element::Symbol(class_id)),
        ) = (
            sentence.elements.get(0),
            sentence.elements.get(1),
            sentence.elements.get(2),
        ) {
            if self.store.sym_name(*inst_id) == "instance" {
                return Some((*class_id, name.clone()));
            }
        }
        None
    }

    /// Recursively extract an `ArithCond` from `sid`, treating `var_name` as
    /// the implicit instance variable.  Strips `(instance var_name C)` conjuncts
    /// where C is any numeric class.  Returns `None` for unrecognised patterns.
    fn extract_arith_cond(&self, sid: SentenceId, var_name: &str) -> Option<ArithCond> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];

        // (and ...) is an operator sentence: elements[0] is Op(And), not a Symbol.
        if matches!(sentence.elements.first(), Some(Element::Op(OpKind::And))) {
            let parts: Vec<ArithCond> = sentence.elements[1..]
                .iter()
                .filter_map(|e| {
                    if let Element::Sub(sub_sid) = e {
                        if self.is_numeric_instance_of_var(*sub_sid, var_name) {
                            None  // strip (instance var_name NumericClass) conjuncts
                        } else {
                            self.extract_arith_cond(*sub_sid, var_name)
                        }
                    } else {
                        None
                    }
                })
                .collect();
            return match parts.len() {
                0 => None,
                1 => Some(parts.into_iter().next().unwrap()),
                _ => Some(ArithCond::And(parts)),
            };
        }

        // (equal ...) is an operator sentence: Op(Equal)
        // Handles: (equal (FnName ?VAR literal) literal) — e.g. (equal (RemainderFn ?X 2) 0)
        if matches!(sentence.elements.first(), Some(Element::Op(OpKind::Equal))) {
            let arg0 = sentence.elements.get(1)?;
            let arg1 = sentence.elements.get(2)?;
            // (equal (FnName ?VAR other_literal) result_literal)
            if let (Element::Sub(fn_sid), Element::Literal(Literal::Number(result))) = (arg0, arg1) {
                let fn_sent = &self.store.sentences[self.store.sent_idx(*fn_sid)];
                if let (
                    Some(Element::Symbol(fn_id)),
                    Some(Element::Variable { name, .. }),
                    Some(Element::Literal(Literal::Number(other_arg))),
                ) = (fn_sent.elements.get(0), fn_sent.elements.get(1), fn_sent.elements.get(2))
                {
                    if name == var_name {
                        return Some(ArithCond::EqualFn {
                            fn_name:   self.store.sym_name(*fn_id).to_string(),
                            other_arg: other_arg.clone(),
                            result:    result.clone(),
                        });
                    }
                }
            }
            // (equal result_literal (FnName ?VAR other_literal)) — reversed
            if let (Element::Literal(Literal::Number(result)), Element::Sub(fn_sid)) = (arg0, arg1) {
                let fn_sent = &self.store.sentences[self.store.sent_idx(*fn_sid)];
                if let (
                    Some(Element::Symbol(fn_id)),
                    Some(Element::Variable { name, .. }),
                    Some(Element::Literal(Literal::Number(other_arg))),
                ) = (fn_sent.elements.get(0), fn_sent.elements.get(1), fn_sent.elements.get(2))
                {
                    if name == var_name {
                        return Some(ArithCond::EqualFn {
                            fn_name:   self.store.sym_name(*fn_id).to_string(),
                            other_arg: other_arg.clone(),
                            result:    result.clone(),
                        });
                    }
                }
            }
            return None;
        }

        // Symbol-headed sentences: greaterThan, greaterThanOrEqualTo, etc.
        let head_id = sentence.head_symbol()?;
        let head    = self.store.sym_name(head_id);

        match head {
            "greaterThan" | "greaterThanOrEqualTo" | "lessThan" | "lessThanOrEqualTo" => {
                let arg0 = sentence.elements.get(1)?;
                let arg1 = sentence.elements.get(2)?;
                // (pred ?VAR literal) — normal order
                if matches!(arg0, Element::Variable { name, .. } if name == var_name) {
                    if let Element::Literal(Literal::Number(n)) = arg1 {
                        return Some(self.make_cmp_cond(head, n.clone(), false));
                    }
                }
                // (pred literal ?VAR) — reversed; flip the comparison direction
                if matches!(arg1, Element::Variable { name, .. } if name == var_name) {
                    if let Element::Literal(Literal::Number(n)) = arg0 {
                        return Some(self.make_cmp_cond(head, n.clone(), true));
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn make_cmp_cond(&self, pred: &str, bound: String, flip: bool) -> ArithCond {
        match (pred, flip) {
            ("greaterThan",          false) | ("lessThan",             true)  => ArithCond::GreaterThan          { bound },
            ("greaterThanOrEqualTo", false) | ("lessThanOrEqualTo",    true)  => ArithCond::GreaterThanOrEqualTo { bound },
            ("lessThan",             false) | ("greaterThan",          true)  => ArithCond::LessThan             { bound },
            ("lessThanOrEqualTo",    false) | ("greaterThanOrEqualTo", true)  => ArithCond::LessThanOrEqualTo    { bound },
            _ => unreachable!(),
        }
    }

    /// Returns `true` if `sid` is `(instance var_name C)` where C is a numeric class.
    fn is_numeric_instance_of_var(&self, sid: SentenceId, var_name: &str) -> bool {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        if let (
            Some(Element::Symbol(inst_id)),
            Some(Element::Variable { name, .. }),
            Some(Element::Symbol(class_id)),
        ) = (
            sentence.elements.get(0),
            sentence.elements.get(1),
            sentence.elements.get(2),
        ) {
            return self.store.sym_name(*inst_id) == "instance"
                && name == var_name
                && self.numeric_sort_cache.contains_key(class_id);
        }
        false
    }

    /// Extend the taxonomy and selectively invalidate derived caches
    /// based on what the new sentences actually contain.
    ///
    /// Phase B + C of the semantic-cache optimisation series.  The
    /// expensive part of a full `rebuild_taxonomy` is the
    /// `extract_tax_edge_for` scan across every root + sub-sentence
    /// in the store -- tens of thousands of sentences at SUMO scale.
    /// This function walks **only the new sids** passed in:
    ///
    /// 1. For each new root, classify whether the sentence (or any of
    ///    its sub-sentences) could affect a derived cache.  The four
    ///    categories are taxonomy, sort_annotations, numeric_char,
    ///    semantic_cache (see [`CacheImpact`]).
    /// 2. For each new sid with a taxonomy impact, extract tax edges
    ///    via `extract_tax_edge_for` (walking root + sub-sentences of
    ///    that sid only, not the whole KB).
    /// 3. If any tax edges were added, rebuild the four derived
    ///    taxonomy caches (`numeric_sort_cache`, `numeric_ancestor_set`,
    ///    `poly_variant_symbols`, `numeric_char_cache`).  These
    ///    rebuilds already scan the taxonomy tables, not sentences,
    ///    so they're O(edges) and fast.
    /// 4. Selectively invalidate the other caches based on the
    ///    per-sentence classification.
    ///
    /// When none of the new sentences have a cache impact (the common
    /// case for SUMO tells like `(attribute X Y)`), this function is
    /// effectively free -- no scans, no invalidations, no rebuilds.
    pub(crate) fn extend_taxonomy_with(&mut self, new_sids: &[SentenceId]) {
        if new_sids.is_empty() {
            return;
        }

        // -- Classify: union of impact across all new sentences -------
        let mut impact = CacheImpact::none();
        for &sid in new_sids {
            impact = impact.union(&classify_sentence_tree(&self.store, sid));
            if impact.all_set() {
                break;  // already at worst case, no point in more classification
            }
        }

        log::debug!(target: "sumo_kb::semantic",
            "extend_taxonomy_with: {} sids -> impact {:?}", new_sids.len(), impact);

        if !impact.any() {
            // Most common case: no derived state is affected.
            return;
        }

        // -- Extract tax edges from new sentences ---------------------
        //
        // Only scan the new sids (and their sub-sentences), not the
        // entire KB.  Edge duplicates are handled by `extract_tax_edge_for`
        // itself -- it appends unconditionally, and the downstream
        // cache rebuilders tolerate duplicates.  For this to be safe
        // the new sids must not have been extracted before, which
        // holds by construction in the ingest path.
        if impact.taxonomy {
            let before = self.tax_edges.len();
            for &sid in new_sids {
                // Extract from the root...
                self.extract_tax_edge_for(sid);
                // ...and all its sub-sentences.  Sub-sentence ids are
                // tracked globally in store.sub_sentences; instead of
                // iterating that whole list, we recursively walk this
                // sentence tree (small -- typically <20 nested sids).
                self.extract_tax_edges_from_subtree(sid);
            }
            let added = self.tax_edges.len() - before;
            log::debug!(target: "sumo_kb::semantic",
                "extend_taxonomy_with: {} new tax edges added (total now {})",
                added, self.tax_edges.len());

            if added > 0 {
                // Rebuild the four derived taxonomy caches.  These
                // walk tax_edges (O(edges)) + a targeted sentence
                // scan for numeric_char_cache.  Cheap relative to a
                // full extract_tax_edge_for-everything rebuild.
                self.numeric_sort_cache   = self.build_numeric_sort_cache();
                self.numeric_ancestor_set = self.build_numeric_ancestor_set();
                self.poly_variant_symbols = self.build_poly_variant_symbols();
                // numeric_char_cache build is sentence-scanning today;
                // only rebuild if we flagged a numeric_char impact.
                if impact.numeric_char {
                    self.numeric_char_cache = self.build_numeric_char_cache();
                }
            }
        } else if impact.numeric_char {
            // Numeric-char biconditional added with no taxonomy edge
            // (unusual -- most numeric biconditionals come with
            // their subclass declaration elsewhere).  Rebuild that
            // one cache, leave the rest.
            self.numeric_char_cache = self.build_numeric_char_cache();
        }

        // -- Selective invalidation based on impact -------------------
        if impact.semantic_cache {
            self.invalidate_semantic_cache();
        }
        if impact.sort_annotations {
            self.invalidate_sort_annotations();
        }
    }

    /// Walk every `Element::Sub(ssid)` under the tree rooted at `sid`
    /// and call `extract_tax_edge_for` for each.  This covers the
    /// "sub-sentence taxonomy edges" the original full-rebuild picked
    /// up by iterating `store.sub_sentences`.
    fn extract_tax_edges_from_subtree(&mut self, sid: SentenceId) {
        // Collect sub-sids first to avoid borrow conflicts between
        // iterating the sentence and mutating self.tax_edges.
        let mut sub_sids: Vec<SentenceId> = Vec::new();
        collect_sub_sids(&self.store, sid, &mut sub_sids);
        for ssid in sub_sids {
            self.extract_tax_edge_for(ssid);
        }
    }


    // -- Basic semantic queries -------------------------------------------------

    pub(crate) fn is_instance(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.read().unwrap().is_instance.get(&sym) { return v; }
        let v = self.compute_is_instance(sym, &mut HashSet::new());
        self.cache.write().unwrap().is_instance.insert(sym, v);
        v
    }

    fn compute_is_instance(&self, sym: SymbolId, visited: &mut HashSet<SymbolId>) -> bool {
        if visited.contains(&sym) { return false; }
        visited.insert(sym);
        let edges = match self.tax_incoming.get(&sym) {
            Some(v) => v.clone(),
            None    => return false,
        };
        for &ei in &edges {
            let edge = &self.tax_edges[ei];
            match edge.rel {
                TaxRelation::Instance => return true,
                TaxRelation::Subrelation | TaxRelation::SubAttribute => {
                    if self.compute_is_instance(edge.from, visited) { return true; }
                }
                TaxRelation::Subclass => {}
            }
        }
        false
    }

    pub(crate) fn is_class(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.read().unwrap().is_class.get(&sym) { return v; }
        let v = match self.tax_incoming.get(&sym) {
            None    => true,
            Some(edges) => edges.iter().all(|&ei| {
                self.tax_edges[ei].rel == TaxRelation::Subclass
            }),
        };
        self.cache.write().unwrap().is_class.insert(sym, v);
        v
    }

    pub(crate) fn is_relation(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.read().unwrap().is_relation.get(&sym) { return v; }
        let v = self.is_instance(sym) && self.has_ancestor_by_name(sym, "Relation");
        self.cache.write().unwrap().is_relation.insert(sym, v);
        v
    }

    pub(crate) fn is_predicate(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.read().unwrap().is_predicate.get(&sym) { return v; }
        let v = self.is_instance(sym) && self.has_ancestor_by_name(sym, "Predicate");
        self.cache.write().unwrap().is_predicate.insert(sym, v);
        v
    }

    pub(crate) fn is_function(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.read().unwrap().is_function.get(&sym) { return v; }
        let v = self.is_instance(sym) && self.has_ancestor_by_name(sym, "Function");
        self.cache.write().unwrap().is_function.insert(sym, v);
        v
    }

    pub(crate) fn has_ancestor_by_name(&self, sym: SymbolId, ancestor: &str) -> bool {
        let anc_id = match self.store.sym_id(ancestor) {
            Some(id) => id,
            None     => return false,
        };
        self.has_ancestor(sym, anc_id)
    }

    pub(crate) fn has_ancestor(&self, sym: SymbolId, ancestor: SymbolId) -> bool {
        if sym == ancestor { return true; }
        if let Some(&v) = self.cache.read().unwrap().has_ancestor.get(&(sym, ancestor)) {
            return v;
        }
        let v = self.compute_has_ancestor(sym, ancestor, &mut HashSet::new());
        self.cache.write().unwrap().has_ancestor.insert((sym, ancestor), v);
        v
    }

    fn compute_has_ancestor(
        &self, sym: SymbolId, ancestor: SymbolId, visited: &mut HashSet<SymbolId>,
    ) -> bool {
        if sym == ancestor { return true; }
        if visited.contains(&sym) { return false; }
        visited.insert(sym);
        let edges = match self.tax_incoming.get(&sym) {
            Some(v) => v.clone(),
            None    => return false,
        };
        for &ei in &edges {
            let from = self.tax_edges[ei].from;
            if self.compute_has_ancestor(from, ancestor, visited) { return true; }
        }
        false
    }

    pub(crate) fn arity(&self, sym: SymbolId) -> Option<i32> {
        if let Some(&v) = self.cache.read().unwrap().arity.get(&sym) { return v; }
        let v = if !self.is_relation(sym) {
            None
        } else {
            self.compute_arity(sym)
        };
        self.cache.write().unwrap().arity.insert(sym, v);
        v
    }

    fn compute_arity(&self, sym: SymbolId) -> Option<i32> {
        const MAPPINGS: &[(&str, i32)] = &[
            ("BinaryRelation",        2),
            ("TernaryRelation",       3),
            ("QuaternaryRelation",    4),
            ("QuintaryRelation",      5),
            ("VariableArityRelation", -1),
        ];
        for &(class, n) in MAPPINGS {
            if self.has_ancestor_by_name(sym, class) {
                let arity = if n > 0 && self.is_function(sym) { n - 1 } else { n };
                return Some(arity);
            }
        }
        None
    }

    pub(crate) fn range(
        &self, rel: SymbolId,
    ) -> Result<Option<RelationDomain>, SemanticError> {
        if let Some(v) = self.cache.read().unwrap().range.get(&rel) {
            return Ok(Some(v.clone()));
        }
        match self.compute_range(rel)? {
            Some(r) => {
                self.cache.write().unwrap().range.insert(rel, r.clone());
                Ok(Some(r))
            }
            None => Ok(None),
        }
    }

    fn compute_range(
        &self, rel: SymbolId,
    ) -> Result<Option<RelationDomain>, SemanticError> {
        let process = |head: &str, make: fn(SymbolId) -> RelationDomain| -> Option<RelationDomain> {
            let sids = self.store.by_head(head).to_vec();
            for sid in sids {
                let sentence = &self.store.sentences[self.store.sent_idx(sid)];
                let arg1_ok = matches!(
                    sentence.elements.get(1),
                    Some(Element::Symbol(id)) if *id == rel
                );
                if !arg1_ok { continue; }
                // `range` has 2 args: (range rel class) -> class is at index 2.
                // `domain` has 3 args: (domain rel argNum class) -> class at index 3.
                let class_id = match sentence.elements.get(2) {
                    Some(Element::Symbol(id)) => *id,
                    _ => continue,
                };
                return Some(make(class_id));
            }
            None
        };

        let range           = process("range",        RelationDomain::Domain);
        let range_subclass  = process("rangeSubclass", RelationDomain::DomainSubclass);
        match (range, range_subclass) {
            (None, None)               => Ok(None),
            (None, Some(rs))           => Ok(Some(rs)),
            (Some(r), None)            => Ok(Some(r)),
            (Some(r), Some(_))         => {
                SemanticError::DoubleRange {
                    sym: self.store.sym_name(rel).to_string(),
                }.handle(&self.store)?;
                Ok(Some(r))
            }
        }
    }

    pub(crate) fn domain(&self, rel: SymbolId) -> Vec<RelationDomain> {
        if let Some(v) = self.cache.read().unwrap().domain.get(&rel) { return v.clone(); }
        let v = self.compute_domain(rel);
        self.cache.write().unwrap().domain.insert(rel, v.clone());
        v
    }

    fn compute_domain(&self, rel: SymbolId) -> Vec<RelationDomain> {
        let mut entries: Vec<(usize, RelationDomain)> = Vec::new();
        let mut process = |head: &str, make: fn(SymbolId) -> RelationDomain| {
            let sids = self.store.by_head(head).to_vec();
            for sid in sids {
                let sentence = &self.store.sentences[self.store.sent_idx(sid)];
                let arg1_ok = matches!(
                    sentence.elements.get(1),
                    Some(Element::Symbol(id)) if *id == rel
                );
                if !arg1_ok { continue; }
                let pos = match sentence.elements.get(2) {
                    Some(Element::Literal(Literal::Number(n))) => {
                        n.parse::<usize>().unwrap_or(0).saturating_sub(1)
                    }
                    _ => continue,
                };
                let class_id = match sentence.elements.get(3) {
                    Some(Element::Symbol(id)) => *id,
                    _ => continue,
                };
                entries.push((pos, make(class_id)));
            }
        };
        process("domain",         RelationDomain::Domain);
        process("domainSubclass", RelationDomain::DomainSubclass);
        entries.sort_by_key(|&(p, _)| p);
        let max = entries.iter().map(|&(p, _)| p).max().map(|p| p + 1).unwrap_or(0);
        let mut result = vec![RelationDomain::Domain(u64::MAX); max];
        for (pos, rd) in entries {
            if pos < max { result[pos] = rd; }
        }
        result
    }

    // -- Validation ------------------------------------------------------------

    pub(crate) fn validate_element(&self, el: &Element) -> Result<(), SemanticError> {
        let id = match el {
            Element::Variable { is_row: false, .. } => return Ok(()),
            Element::Symbol(id)  => *id,
            Element::Sub(sid)    => return self.validate_sentence(*sid),
            _                    => return Ok(()),
        };
        if !self.has_ancestor_by_name(id, "Entity") {
            SemanticError::NoEntityAncestor { sym: self.store.sym_name(id).to_string() }
                .handle(&self.store)?;
        }
        if self.is_relation(id) {
            let entity = *self.store.symbols.get("Entity").unwrap_or(&u64::MAX);
            let domain = self.domain(id);
            let _domain: Vec<SymbolId> = domain.iter().enumerate().map(|(idx, rd)| {
                if matches!(rd, RelationDomain::Domain(e) if *e == u64::MAX) {
                    SemanticError::MissingDomain {
                        sym: self.store.sym_name(rd.id()).to_string(), idx,
                    }.handle(&self.store)?;
                    Ok(entity)
                } else {
                    Ok(rd.id())
                }
            }).collect::<Result<Vec<_>, SemanticError>>()?;

            let arity = match self.arity(id) {
                Some(a) => a,
                None => {
                    SemanticError::MissingArity { sym: self.store.sym_name(id).to_string() }
                        .handle(&self.store)?;
                    -1
                }
            };
            if arity > 0 && arity < domain.len().try_into().unwrap() {
                SemanticError::ArityMismatch {
                    sid: id,
                    rel:      self.store.sym_name(id).to_string(),
                    expected: arity.try_into().unwrap(),
                    got:      domain.len(),
                }.handle(&self.store)?;
            }
            if self.is_function(id) {
                match self.range(id) {
                    Err(e) => return Err(e),
                    Ok(None) => {
                        SemanticError::MissingRange { sym: self.store.sym_name(id).to_string() }
                            .handle(&self.store)?;
                    }
                    Ok(Some(_)) => {}
                }
                let fun_name = self.store.sym_name(id);
                if !fun_name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    SemanticError::FunctionCase { sym: fun_name.to_string() }
                        .handle(&self.store)?;
                }
            } else if self.is_predicate(id) {
                let rel_name = self.store.sym_name(id);
                if rel_name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    SemanticError::PredicateCase { sym: rel_name.to_string() }
                        .handle(&self.store)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn validate_sentence(&self, sid: SentenceId) -> Result<(), SemanticError> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        if sentence.is_operator() {
            return self.validate_operator_sentence(sid);
        }
        log::trace!(target: "sumo_kb::semantic",
            "validating sentence sid={}", sid);

        let head_id = match sentence.elements.first() {
            Some(Element::Symbol(id))                    => *id,
            Some(Element::Variable { id, is_row: false, .. }) => *id,
            _ => unreachable!("parser ensures sentence head is a symbol or variable"),
        };
        self.validate_element(sentence.elements.first().unwrap())?;
        if !self.is_relation(head_id) {
            SemanticError::HeadNotRelation {
                sid,
                sym: self.store.sym_name(head_id).to_owned(),
            }.handle(&self.store)?;
        }

        let arg_count = sentence.elements.len().saturating_sub(1);
        if let Some(ar) = self.arity(head_id) {
            if ar > 0 && ar as usize != arg_count {
                SemanticError::ArityMismatch {
                    sid,
                    rel:      self.store.sym_name(head_id).to_owned(),
                    expected: ar as usize,
                    got:      arg_count,
                }.handle(&self.store)?;
            }
        }

        let domain = self.domain(head_id);
        if !domain.is_empty() {
            let args: Vec<Element> =
                self.store.sentences[self.store.sent_idx(sid)].elements[1..].to_vec();
            for (i, (arg, dom)) in args.iter().zip(domain.iter()).enumerate() {
                if !self.arg_satisfies_domain(arg, dom) {
                    SemanticError::DomainMismatch {
                        sid,
                        rel:    self.store.sym_name(head_id).to_owned(),
                        arg:    i + 1,
                        domain: self.store.sym_name(dom.id()).to_owned(),
                    }.handle(&self.store)?;
                }
            }
        }
        Ok(())
    }

    fn validate_operator_sentence(&self, sid: SentenceId) -> Result<(), SemanticError> {
        let op: OpKind = match self.store.sentences[self.store.sent_idx(sid)].op().cloned() {
            Some(op) => op,
            None     => return Ok(()),
        };
        if op == OpKind::Equal { return Ok(()); }

        let is_quantifier = matches!(op, OpKind::ForAll | OpKind::Exists);
        let args_start = if is_quantifier { 2 } else { 1 };

        let sub_ids: Vec<SentenceId> = self.store.sentences[self.store.sent_idx(sid)]
            .elements[args_start..]
            .iter()
            .filter_map(|e| if let Element::Sub(id) = e { Some(*id) } else { None })
            .collect();

        for (idx, sub_id) in sub_ids.iter().enumerate() {
            if !self.is_logical_sentence(*sub_id) {
                SemanticError::NonLogicalArg { sid, arg: idx + 1, op: op.to_string() }.handle(&self.store)?;
            }
        }
        Ok(())
    }

    pub(crate) fn is_logical_sentence(&self, sid: SentenceId) -> bool {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        if sentence.is_operator() { return true; }
        let head_id = match sentence.elements.first() {
            Some(Element::Symbol(id))    => *id,
            Some(Element::Variable { id, .. }) => *id,
            _ => return false,
        };
        // A sentence is logical if its head is a relation and not a function.
        // If the head is not declared in the taxonomy at all (unknown symbol, e.g. when
        // the full KB is not loaded), assume it is logical -- unknown != not-a-relation.
        // Only positively-declared functions are considered non-logical.
        self.is_relation(head_id) && !self.is_function(head_id)
    }

    fn arg_satisfies_domain(&self, arg: &Element, dom: &RelationDomain) -> bool {
        match arg {
            Element::Symbol(sym_id) => {
                let sym_id = *sym_id;
                match dom {
                    RelationDomain::Domain(dom_id) => {
                        let dom_name = self.store.sym_name(*dom_id);
                        if dom_name == "Entity" { return true; }
                        if dom_name == "Class"  { return self.is_class(sym_id); }
                        self.is_instance(sym_id) && self.has_ancestor(sym_id, *dom_id)
                    }
                    RelationDomain::DomainSubclass(dom_id) => {
                        let dom_name = self.store.sym_name(*dom_id);
                        if dom_name == "Entity" { return true; }
                        // `domainSubclass R N Class` means "the argument must be a
                        // class".  Any symbol that IS a class satisfies this, even
                        // if it is not itself a subclass of `Class` in the hierarchy
                        // (e.g. SetOrClass is a superclass of Class, not a subclass,
                        // yet it is a class and is a valid range for rangeSubclass).
                        if dom_name == "Class"  { return self.is_class(sym_id); }
                        self.is_class(sym_id) && self.has_ancestor(sym_id, *dom_id)
                    }
                }
            }
            Element::Variable { id, is_row: false, .. } => {
                let var_id = *id;
                match dom {
                    RelationDomain::Domain(dom_id) => {
                        let dom_name = self.store.sym_name(*dom_id);
                        if dom_name == "Entity" { return true; }
                        if dom_name == "Class"  { return self.is_class(var_id); }
                        self.is_instance(var_id) || !self.is_class(var_id)
                    }
                    RelationDomain::DomainSubclass(dom_id) => {
                        let dom_name = self.store.sym_name(*dom_id);
                        if dom_name == "Entity" { return true; }
                        if dom_name == "Class"  { return self.is_class(var_id); }
                        self.is_class(var_id) || !self.is_instance(var_id)
                    }
                }
            }
            Element::Variable { is_row: true, .. }
            | Element::Sub(_)
            | Element::Literal(_) => true,
            Element::Op(_) => false,
        }
    }

    // -- Sort inference --------------------------------------------------------

    /// Map a SUMO type name to its most specific primitive [`Sort`].
    ///
    /// Looks up the name's `SymbolId` and delegates to `sort_for_id`.
    /// The only hardcoded strings in the system are the three roots in
    /// `NUMERIC_ROOTS`; all subclass memberships are resolved at taxonomy
    /// build time and stored in `numeric_sort_cache`.
    /// Map a `SymbolId` to its most specific primitive [`Sort`].
    ///
    /// O(1) -- a single `HashMap` lookup with no string operations.
    /// Sentinel `u64::MAX` (gap in domain axioms) -> `Sort::Individual`.
    pub(crate) fn sort_for_id(&self, class_id: SymbolId) -> Sort {
        if class_id == u64::MAX { return Sort::Individual; }
        self.numeric_sort_cache.get(&class_id).copied().unwrap_or(Sort::Individual)
    }

    // -- SortAnnotations -------------------------------------------------------

    /// Compute arg and return sorts for every relation, predicate, and function
    /// in the KB by reading their `domain` and `range` axioms.
    ///
    /// Iterates all interned symbols (including scope-suffixed variable symbols
    /// such as `X__3`).  Variable symbols are naturally skipped because
    /// `is_function`, `is_relation`, and `is_predicate` return false for them.
    fn build_sort_annotations(&self) -> SortAnnotations {
        let mut symbol_arg_sorts    = HashMap::new();
        let mut symbol_return_sorts = HashMap::new();

        for &id in self.store.symbols.values() {
            if self.is_function(id) {
                let arg_sorts: Vec<Sort> = self.domain(id).iter()
                    .map(|rd| self.sort_for_id(rd.id()))
                    .collect();
                let ret_sort = match self.range(id) {
                    Ok(Some(rd)) => self.sort_for_id(rd.id()),
                    _            => Sort::Individual,
                };
                symbol_arg_sorts.insert(id, arg_sorts);
                symbol_return_sorts.insert(id, ret_sort);
            } else if self.is_relation(id) || self.is_predicate(id) {
                let arg_sorts: Vec<Sort> = self.domain(id).iter()
                    .map(|rd| self.sort_for_id(rd.id()))
                    .collect();
                if !arg_sorts.is_empty() {
                    symbol_arg_sorts.insert(id, arg_sorts);
                }
            }
        }

        // Compute sorts for individual numeric constants from `instance` edges.
        // E.g. `(instance Pi PositiveRealNumber)` -> Pi maps to Sort::Real.
        //
        // TaxEdge direction: `from = class (PositiveRealNumber), to = individual (Pi)`.
        // So the individual is edge.to and the class is edge.from.
        let mut symbol_individual_sorts = HashMap::new();
        for edge in &self.tax_edges {
            if edge.rel != crate::types::TaxRelation::Instance { continue; }
            let individual_id = edge.to;
            // Skip known functions/relations -- they already have return/arg sorts.
            if self.is_function(individual_id) || self.is_relation(individual_id) || self.is_predicate(individual_id) {
                continue;
            }
            let class_sort = self.sort_for_id(edge.from);
            if class_sort == Sort::Individual { continue; } // Not a numeric class.
            // Keep the most specific (narrowest) sort across all instance edges.
            // Sort is Ord: Individual(1) < Real(2) < Rational(3) < Integer(4).
            // max() picks the more specific sort (Integer > Real).
            let entry = symbol_individual_sorts.entry(individual_id).or_insert(class_sort);
            *entry = (*entry).max(class_sort);
        }

        SortAnnotations { symbol_arg_sorts, symbol_return_sorts, symbol_individual_sorts }
    }

    /// Returns the lazily-computed KB-wide sort annotation table.
    ///
    /// On first call iterates all KB symbols to compute arg/return sorts
    /// from domain and range axioms.  Result is cached; cleared by `invalidate_cache()`.
    pub(crate) fn sort_annotations(&self) -> RwLockReadGuard<'_, Option<SortAnnotations>> {
        {
            let mut guard = self.sort_annotations.write().unwrap();
            if guard.is_none() {
                *guard = Some(self.build_sort_annotations());
            }
        }
        self.sort_annotations.read().unwrap()
    }

    // -- Batch validation ------------------------------------------------------

    /// Validate all root sentences, returning errors (does not stop on first error).
    pub(crate) fn validate_all(&self) -> Vec<(SentenceId, SemanticError)> {
        self.store.roots.iter()
            .filter_map(|&sid| self.validate_sentence(sid).err().map(|e| (sid, e)))
            .collect()
    }
}

// ===========================================================================
//  CacheImpact classification
// ===========================================================================

/// Which derived caches a candidate sentence can affect.
///
/// Built by [`classify_sentence_tree`] over a candidate sid; unioned
/// across a batch of new sids by [`SemanticLayer::extend_taxonomy_with`]
/// to decide what to rebuild or invalidate.
///
/// All-false is the common case: a sentence that neither introduces a
/// taxonomy edge, nor a domain/range axiom, nor a numeric-class
/// biconditional, nor any new symbol classification.  Most SUMO
/// axioms are of this kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CacheImpact {
    /// The sentence (or a sub-sentence) is a
    /// `subclass`/`instance`/`subrelation`/`subAttribute` assertion
    /// with concrete arguments.  Triggers tax-edge extraction and
    /// derived-cache rebuild.
    pub taxonomy:         bool,
    /// The sentence is a `domain` / `range` / `domainSubclass`
    /// axiom.  Triggers `SortAnnotations` invalidation.
    pub sort_annotations: bool,
    /// The sentence looks like a numeric-class characterisation
    /// biconditional (`(<=> (instance ?X NC) cond)` or similar).
    /// Triggers `numeric_char_cache` rebuild.
    pub numeric_char:     bool,
    /// Any symbol-classification-affecting shape.  Conservative: we
    /// set this whenever taxonomy or sort_annotations are flagged,
    /// since those can change `is_instance` / `is_class` /
    /// `is_relation` / `is_function` answers.
    pub semantic_cache:   bool,
}

impl CacheImpact {
    pub(crate) const fn none() -> Self {
        Self { taxonomy: false, sort_annotations: false, numeric_char: false, semantic_cache: false }
    }
    pub(crate) fn any(&self) -> bool {
        self.taxonomy || self.sort_annotations || self.numeric_char || self.semantic_cache
    }
    pub(crate) fn all_set(&self) -> bool {
        self.taxonomy && self.sort_annotations && self.numeric_char && self.semantic_cache
    }
    pub(crate) fn union(&self, other: &Self) -> Self {
        Self {
            taxonomy:         self.taxonomy         || other.taxonomy,
            sort_annotations: self.sort_annotations || other.sort_annotations,
            numeric_char:     self.numeric_char     || other.numeric_char,
            semantic_cache:   self.semantic_cache   || other.semantic_cache,
        }
    }
}

/// Classify a sentence's impact on derived caches.
///
/// Walks the sentence tree: the root sentence + every `Element::Sub`
/// descendant.  A subclass/instance/etc. head flag triggers the
/// taxonomy + semantic_cache impacts; a domain/range head triggers
/// sort_annotations + semantic_cache.  A biconditional or implication
/// whose body is `(instance ?X NumericLike)` triggers numeric_char.
///
/// Conservative by design: if we're uncertain, we do NOT flag an
/// impact.  Under-flagging would cause caches to go stale
/// (correctness bug), but there's no known sentence shape today that
/// slips past this walker AND would affect a cache.  The
/// `extend_taxonomy_with` caller documents the assumption.
pub(crate) fn classify_sentence_tree(
    store: &KifStore,
    sid:   SentenceId,
) -> CacheImpact {
    let mut out = CacheImpact::none();
    classify_sid_into(store, sid, &mut out);
    out
}

fn classify_sid_into(store: &KifStore, sid: SentenceId, out: &mut CacheImpact) {
    if !store.has_sentence(sid) {
        return;
    }
    let sentence = &store.sentences[store.sent_idx(sid)];

    // Operator-headed sentences (<=>, =>, forall, etc.) have no
    // symbol head; instead we check the operator + body shape.
    // Numeric-char biconditionals are the main case that matters:
    // `(<=> (instance ?X PositiveInteger) (greaterThan ?X 0))`.
    if let Some(op) = sentence.op() {
        if matches!(op, OpKind::Iff | OpKind::Implies) && sentence.elements.len() >= 3 {
            if contains_instance_pattern(store, &sentence.elements[1])
                || contains_instance_pattern(store, &sentence.elements[2])
            {
                out.numeric_char = true;
            }
        }
    }

    // Direct head classification (symbol-headed sentences only).
    if let Some(head_id) = sentence.head_symbol() {
        classify_head_name_into(store.sym_name(head_id), out);
    }

    // Recurse into sub-sentences.  Sub-sentences may be direct
    // facts (e.g. a subclass edge nested inside an implication's
    // consequent).
    for el in &sentence.elements {
        if let Element::Sub(sub_sid) = el {
            classify_sid_into(store, *sub_sid, out);
        }
    }
}

fn classify_head_name_into(
    head: &str,
    out:  &mut CacheImpact,
) {
    match head {
        // Taxonomy-edge heads.  The argument shape is validated by
        // `extract_tax_edge_for` at extraction time; if the sentence
        // is malformed (wrong arity, non-symbol args), the extraction
        // silently skips it, so flagging here is safe -- extraction
        // may be a no-op even when the flag is set.
        "subclass" | "instance" | "subrelation" | "subAttribute" => {
            out.taxonomy       = true;
            out.semantic_cache = true;
        }
        // Domain/range axioms.
        "domain" | "range" | "domainSubclass" => {
            out.sort_annotations = true;
            out.semantic_cache   = true;
        }
        _ => {}
    }
}

/// Conservative "looks like `(instance ?X Class)`" check used only to
/// flag numeric_char_cache rebuilds.  Returns true for an
/// `Element::Sub` whose sentence has `instance` as its head; false
/// otherwise.  No sort analysis -- the subsequent rebuild pass is
/// what decides whether the class is actually numeric.
fn contains_instance_pattern(store: &KifStore, el: &Element) -> bool {
    let Element::Sub(sid) = el else { return false };
    if !store.has_sentence(*sid) { return false; }
    let s = &store.sentences[store.sent_idx(*sid)];
    match s.head_symbol() {
        Some(id) => store.sym_name(id) == "instance",
        None     => false,
    }
}

/// Collect every `Element::Sub(ssid)` descendant of `sid` (excluding
/// `sid` itself) into `out`.  Ordering is a pre-order traversal.
fn collect_sub_sids(store: &KifStore, sid: SentenceId, out: &mut Vec<SentenceId>) {
    if !store.has_sentence(sid) { return; }
    for el in &store.sentences[store.sent_idx(sid)].elements {
        if let Element::Sub(ssid) = el {
            out.push(*ssid);
            collect_sub_sids(store, *ssid, out);
        }
    }
}

// -- Tests ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kif_store::{KifStore, load_kif};

    const BASE: &str = "
        (subclass Relation Entity)
        (subclass BinaryRelation Relation)
        (subclass Predicate Relation)
        (subclass BinaryPredicate Predicate)
        (subclass BinaryPredicate BinaryRelation)
        (instance subclass BinaryRelation)
        (domain subclass 1 Class)
        (domain subclass 2 Class)
        (instance instance BinaryPredicate)
        (domain instance 1 Entity)
        (domain instance 2 Class)
        (subclass Animal Entity)
        (subclass Human Entity)
        (subclass Human Animal)
    ";

    fn base_layer() -> SemanticLayer {
        let mut store = KifStore::default();
        load_kif(&mut store, BASE, "base");
        SemanticLayer::new(store)
    }

    fn kif(kif_str: &str) -> SemanticLayer {
        let mut store = KifStore::default();
        load_kif(&mut store, kif_str, "base");
        SemanticLayer::new(store)
    }

    #[test]
    fn is_relation() {
        let layer = base_layer();
        let id = layer.store.sym_id("subclass").unwrap();
        assert!(layer.is_relation(id));
    }

    #[test]
    fn is_predicate() {
        let layer = base_layer();
        let id = layer.store.sym_id("instance").unwrap();
        assert!(layer.is_predicate(id));
    }

    #[test]
    fn is_class() {
        let layer = base_layer();
        assert!( layer.is_class(layer.store.sym_id("Human").unwrap()));
        assert!(!layer.is_class(layer.store.sym_id("subclass").unwrap()));
    }

    #[test]
    fn has_ancestor() {
        let layer = base_layer();
        let human = layer.store.sym_id("Human").unwrap();
        assert!( layer.has_ancestor_by_name(human, "Entity"));
        assert!( layer.has_ancestor_by_name(human, "Animal"));
        assert!(!layer.has_ancestor_by_name(human, "Relation"));
    }

    #[test]
    fn validate_sentence_valid() {
        let layer = base_layer();
        let sub_id = layer.store.sym_id("subclass").unwrap();
        // Find a root sentence headed by "subclass"
        let sid = layer.store.by_head("subclass")[0];
        // validate_sentence should not error for a valid sentence.
        // (Semantic errors are warnings unless ALL_ERRORS is set.)
        let _ = layer.validate_sentence(sid);
        let _ = sub_id;
    }

    #[test]
    fn validate_all_runs() {
        let layer = base_layer();
        let errors = layer.validate_all();
        // Base ontology may have warnings but no fatal errors.
        // Just check it doesn't panic.
        let _ = errors;
    }

    #[test]
    fn is_logical_sentence() {
        let layer = kif("
            (and (relation A B) (relation D C))
            (instance relation Relation)
            (relation A B)
            (NotARelation A B)
        ");
        let store = &layer.store;
        assert!(layer.is_logical_sentence(store.roots[0]));
        assert!(layer.is_logical_sentence(store.roots[2]));
        assert!(!layer.is_logical_sentence(store.roots[3]));
    }

    #[test]
    fn sort_annotations_predicate_arg_sorts() {
        let layer = kif("
            (subclass BinaryPredicate Predicate)
            (subclass Predicate Relation)
            (subclass Integer RationalNumber)
            (subclass RationalNumber RealNumber)
            (instance foo BinaryPredicate)
            (domain foo 1 Integer)
            (domain foo 2 Entity)
        ");
        let sa_guard = layer.sort_annotations();
        let sa = sa_guard.as_ref().unwrap();
        let foo_id = layer.store.sym_id("foo").unwrap();
        let args = sa.symbol_arg_sorts.get(&foo_id).expect("foo should have arg sorts");
        assert_eq!(args.get(0).copied(), Some(Sort::Integer));
        assert_eq!(args.get(1).copied(), Some(Sort::Individual));
        assert!(sa.symbol_return_sorts.get(&foo_id).is_none(),
            "predicates have no return sort entry");
    }

    #[test]
    fn sort_annotations_function_return_sort() {
        let layer = kif("
            (subclass UnaryFunction Function)
            (subclass Integer RationalNumber)
            (subclass RationalNumber RealNumber)
            (instance succFn UnaryFunction)
            (domain succFn 1 Integer)
            (range succFn Integer)
        ");
        let sa_guard = layer.sort_annotations();
        let sa = sa_guard.as_ref().unwrap();
        let fn_id = layer.store.sym_id("succFn").unwrap();
        assert_eq!(sa.symbol_return_sorts.get(&fn_id).copied(), Some(Sort::Integer));
        let args = sa.symbol_arg_sorts.get(&fn_id).expect("succFn should have arg sorts");
        assert_eq!(args.get(0).copied(), Some(Sort::Integer));
    }

    #[test]
    fn sort_annotations_cleared_on_invalidate() {
        let layer = kif("
            (subclass BinaryPredicate Predicate)
            (subclass Predicate Relation)
            (subclass Integer RationalNumber)
            (subclass RationalNumber RealNumber)
            (instance foo BinaryPredicate)
            (domain foo 1 Integer)
        ");
        { assert!(!layer.sort_annotations().as_ref().unwrap().symbol_arg_sorts.is_empty()); }
        layer.invalidate_cache();
        { assert!(!layer.sort_annotations().as_ref().unwrap().symbol_arg_sorts.is_empty()); }
    }

    #[test]
    fn sort_ordering() {
        assert!(Sort::Integer  > Sort::Rational);
        assert!(Sort::Rational > Sort::Real);
        assert!(Sort::Real     > Sort::Individual);
        assert_eq!(Sort::Integer.tptp(),    "$int");
        assert_eq!(Sort::Rational.tptp(),   "$rat");
        assert_eq!(Sort::Real.tptp(),       "$real");
        assert_eq!(Sort::Individual.tptp(), "$i");
    }

    #[test]
    fn taxonomy_edge_lives_in_layer() {
        let layer = base_layer();
        // Taxonomy edges now live in SemanticLayer, not KifStore.
        assert!(!layer.tax_edges.is_empty(),
            "tax_edges should be populated in SemanticLayer after construction");
        // has_ancestor still works -- it uses layer.tax_edges internally.
        let human = layer.store.sym_id("Human").unwrap();
        assert!(layer.has_ancestor_by_name(human, "Entity"));
        assert!(layer.has_ancestor_by_name(human, "Animal"));
    }

    #[test]
    fn taxonomy_rebuilt_after_remove() {
        // Load two files; removing one should update the taxonomy.
        let mut store = KifStore::default();
        load_kif(&mut store, "(subclass Cat Animal)", "cats");
        load_kif(&mut store, "(subclass Animal Entity)", "core");
        let mut layer = SemanticLayer::new(store);

        let cat    = layer.store.sym_id("Cat").unwrap();
        let animal = layer.store.sym_id("Animal").unwrap();

        assert!(layer.has_ancestor_by_name(cat, "Animal"),
            "Cat should have Animal as ancestor before removal");

        // Remove the cats file -- Cat -> Animal edge disappears.
        layer.store.remove_file("cats");
        layer.rebuild_taxonomy();
        layer.invalidate_cache();

        assert!(!layer.has_ancestor_by_name(cat, "Animal"),
            "Cat -> Animal should be gone after cats file is removed");
        // Animal -> Entity from "core" should still be intact.
        assert!(layer.has_ancestor_by_name(animal, "Entity"),
            "Animal -> Entity (from core file) should still exist");
    }

    // =====================================================================
    //  Phase B + C: CacheImpact classifier and incremental taxonomy
    // =====================================================================

    #[test]
    fn classify_taxonomy_heads() {
        // Each of these should flag `taxonomy: true` and nothing else
        // outside the semantic_cache pairing.
        let kif = "
            (subclass Dog Animal)
            (instance Fido Dog)
            (subrelation parent ancestor)
            (subAttribute Happy Mood)
        ";
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "t");
        for &sid in &store.roots {
            let impact = classify_sentence_tree(&store, sid);
            assert!(impact.taxonomy,
                "expected taxonomy=true for sid={sid}: {:?}", impact);
            assert!(impact.semantic_cache,
                "expected semantic_cache=true alongside taxonomy: {:?}", impact);
            assert!(!impact.sort_annotations,
                "unexpected sort_annotations: {:?}", impact);
            assert!(!impact.numeric_char,
                "unexpected numeric_char: {:?}", impact);
        }
    }

    #[test]
    fn classify_domain_range_axioms() {
        let kif = "
            (domain parent 1 Organism)
            (range mother Woman)
            (domainSubclass shapeOf 1 Object)
        ";
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "t");
        for &sid in &store.roots {
            let impact = classify_sentence_tree(&store, sid);
            assert!(impact.sort_annotations,
                "expected sort_annotations=true for sid={sid}: {:?}", impact);
            assert!(!impact.taxonomy,
                "unexpected taxonomy: {:?}", impact);
        }
    }

    #[test]
    fn classify_non_taxonomy_sentence_has_no_impact() {
        // Typical SUMO axiom that doesn't affect any cache.
        let kif = "(attribute Alice Tall)";
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "t");
        let impact = classify_sentence_tree(&store, store.roots[0]);
        assert!(!impact.any(),
            "expected no impact for plain non-taxonomy sentence, got {:?}", impact);
    }

    #[test]
    fn classify_numeric_biconditional_flags_numeric_char() {
        // (<=> (instance ?X PositiveInteger) (greaterThan ?X 0))
        let kif = "(<=> (instance ?X PositiveInteger) (greaterThan ?X 0))";
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "t");
        let impact = classify_sentence_tree(&store, store.roots[0]);
        assert!(impact.numeric_char,
            "expected numeric_char=true for numeric biconditional, got {:?}", impact);
    }

    #[test]
    fn classify_nested_subclass_in_implication() {
        // Rule: taxonomy-head inside implication.  The classifier
        // walks sub-sentences so it should still flag taxonomy --
        // even though the top-level head is `=>`, a `(subclass ...)`
        // sub-sentence exists underneath.
        let kif = "(=> (foo ?X) (subclass ?X Animal))";
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "t");
        let impact = classify_sentence_tree(&store, store.roots[0]);
        assert!(impact.taxonomy,
            "expected taxonomy=true for nested subclass, got {:?}", impact);
    }

    #[test]
    fn extend_taxonomy_with_matches_full_rebuild() {
        // Drive extend_taxonomy_with and rebuild_taxonomy against
        // the same KB and check that the derived caches match.
        // This is the central correctness invariant for Phase C.
        let kif = "
            (subclass Dog Animal)
            (subclass Animal Entity)
            (subclass Cat Animal)
            (instance Fido Dog)
            (domain parent 1 Organism)
            (range father Man)
            (attribute Alice Warm)
        ";
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "t");

        // Baseline: full rebuild.
        let mut layer_full = SemanticLayer::new({
            let mut s = KifStore::default();
            load_kif(&mut s, kif, "t");
            s
        });

        // Incremental: start empty, extend with the root sids.
        let mut layer_inc = SemanticLayer::new(KifStore::default());
        let mut store2 = KifStore::default();
        load_kif(&mut store2, kif, "t");
        let roots = store2.roots.clone();
        layer_inc.store = store2;
        layer_inc.extend_taxonomy_with(&roots);

        // Sort edges by a deterministic key for comparison (order
        // differs between the two paths because rebuild scans
        // root+sub while extend_with walks roots + sub tree per root).
        let mut full_edges: Vec<_> = layer_full.tax_edges.iter()
            .map(|e| (e.from, e.to, e.rel.clone())).collect();
        let mut inc_edges: Vec<_> = layer_inc.tax_edges.iter()
            .map(|e| (e.from, e.to, e.rel.clone())).collect();
        full_edges.sort();
        inc_edges.sort();

        assert_eq!(full_edges, inc_edges,
            "tax_edges differ between full-rebuild and incremental-extend paths");

        // Derived caches should also agree.
        let full_ns: Vec<_> = {
            let mut v: Vec<_> = layer_full.numeric_sort_cache.iter()
                .map(|(k, v)| (*k, *v)).collect();
            v.sort();
            v
        };
        let inc_ns: Vec<_> = {
            let mut v: Vec<_> = layer_inc.numeric_sort_cache.iter()
                .map(|(k, v)| (*k, *v)).collect();
            v.sort();
            v
        };
        assert_eq!(full_ns, inc_ns, "numeric_sort_cache differs");
    }

    #[test]
    fn extend_taxonomy_with_no_impact_does_nothing() {
        // A batch of purely non-taxonomy sentences should not touch
        // the taxonomy or any derived cache.
        let kif_base = "(subclass Dog Animal)";
        let mut store = KifStore::default();
        load_kif(&mut store, kif_base, "base");
        let mut layer = SemanticLayer::new(store);
        let before_edges = layer.tax_edges.len();

        // Add a non-taxonomy sentence.
        let mut kif_extra = "(attribute Alice Tall) (part Alice Earth)";
        load_kif(&mut layer.store, kif_extra, "extra");
        let _ = &mut kif_extra;  // silence warning
        let new_sids: Vec<_> = layer.store.file_roots.get("extra")
            .cloned().unwrap_or_default();
        assert_eq!(new_sids.len(), 2);

        layer.extend_taxonomy_with(&new_sids);

        // tax_edges unchanged.
        assert_eq!(layer.tax_edges.len(), before_edges,
            "no-impact batch should not change tax_edges");
    }

    #[test]
    fn granular_invalidate_independence() {
        // invalidate_semantic_cache does not touch sort_annotations.
        let mut store = KifStore::default();
        load_kif(&mut store, BASE, "base");
        let layer = SemanticLayer::new(store);

        // Populate the sort_annotations cache.
        drop(layer.sort_annotations());
        assert!(layer.sort_annotations.read().unwrap().is_some());

        layer.invalidate_semantic_cache();
        // sort_annotations is not touched by semantic-cache invalidation.
        assert!(layer.sort_annotations.read().unwrap().is_some(),
            "invalidate_semantic_cache should NOT clear sort_annotations");

        layer.invalidate_sort_annotations();
        assert!(layer.sort_annotations.read().unwrap().is_none());
    }
}
