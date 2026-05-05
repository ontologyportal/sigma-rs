// crates/core/src/semantics/cache.rs
//
// The `Inner` is the cached data structures comprising the 
// `SemanticLayer` from which persistent KBs will reconstruct their
// `SemanticLayer`. Owned by the resulting `SemanticLayer`

use std::collections::{HashMap, HashSet};

#[cfg(feature = "persist")]
use crate::TaxEdge;
#[cfg(feature = "persist")]
use crate::syntactic::SyntacticLayer;
use crate::semantics::relation::RelationRelation;
use crate::{Element, OpKind, SentenceId, SymbolId, TaxRelation};

use super::query::DocEntry;

use super::relation::RelationDomain;
use super::SemanticLayer;

impl SemanticLayer {
    /// Construct a SemanticLayer with its taxonomy state prepopulated
    /// from a persisted cache.  The `tax_incoming` reverse index is
    /// rederived in one linear pass over `tax_edges` (we don't persist
    /// it because derivation is cheaper than reading it back).
    ///
    /// Skips the full `rebuild_taxonomy` scan -- Phase D's core
    /// cold-open optimisation.
    #[cfg(feature = "persist")]
    pub(crate) fn from_cached_taxonomy(
        syntactic:            SyntacticLayer,
        tax_edges:            Vec<TaxEdge>
    ) -> Self {
        // Rebuild the reverse index (edge_index -> tax_incoming[to]).

        use std::sync::RwLock;
        let mut tax_incoming: HashMap<SymbolId, Vec<usize>> = HashMap::new();
        for (i, edge) in tax_edges.iter().enumerate() {
            tax_incoming.entry(edge.to).or_default().push(i);
        }
        Self {
            syntactic,
            tax_edges,
            tax_incoming,
            cache: RwLock::new(Inner::default()),
        }
    }

    // Phase D accessors for persistence.  These expose internal state
    // to the persist layer in `write_axioms` without letting arbitrary
    // callers mutate it.

    #[cfg(feature = "persist")]
    pub(crate) fn tax_edges_snapshot(&self) -> Vec<TaxEdge> {
        self.tax_edges.clone()
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
        *self.cache.write().unwrap() = Inner::default();
    }

    /// Granular cache eviction for a specific set of symbols.
    ///
    /// Evicts every entry in the symbol-keyed caches whose key is in
    /// `symbols`, plus every entry in the `(SymbolId, SymbolId)`
    /// `has_ancestor` cache whose *either* key is in the set
    /// (conservative over-eviction -- see plan risk #9).  Leaves
    /// unrelated entries untouched, so a single-sentence edit doesn't
    /// flush the whole cache.
    ///
    /// Does **not** touch `SortAnnotations` -- that cache rebuilds
    /// wholesale via `invalidate_sort_annotations` since its
    /// dependency tracking is coarser.
    pub(crate) fn invalidate_symbols(&self, symbols: &HashSet<SymbolId>) {
        if symbols.is_empty() { return; }
        let mut cache = self.cache.write().unwrap();
        for &id in symbols {
            cache.is_instance.remove(&id);
            cache.is_class.remove(&id);
            cache.is_relation.remove(&id);
            cache.is_predicate.remove(&id);
            cache.is_function.remove(&id);
            cache.arity.remove(&id);
            cache.domain.remove(&id);
            cache.range.remove(&id);
            cache.documentation.remove(&id);
            cache.term_format.remove(&id);
            cache.format.remove(&id);
        }
        cache.has_ancestor.retain(|&(a, b), _| {
            !symbols.contains(&a) && !symbols.contains(&b)
        });
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
        &self,
        sid: SentenceId,
    ) -> CacheImpact {
        let mut out = CacheImpact::none();
        self.classify_sid_into(sid, &mut out);
        out
    }

    fn classify_sid_into(&self, sid: SentenceId, out: &mut CacheImpact) {
        if !self.syntactic.has_sentence(sid) {
            return;
        }
        let sentence = &self.syntactic.sentences[self.syntactic.sent_idx(sid)];

        // Operator-headed sentences (<=>, =>, forall, etc.) have no
        // symbol head; instead we check the operator + body shape.
        // Numeric-char biconditionals are the main case that matters:
        // `(<=> (instance ?X PositiveInteger) (greaterThan ?X 0))`.
        if let Some(op) = sentence.op() {
            if matches!(op, OpKind::Iff | OpKind::Implies) && sentence.elements.len() >= 3 {
                if self.contains_instance_pattern(&sentence.elements[1])
                    || self.contains_instance_pattern(&sentence.elements[2])
                {
                    out.numeric_char = true;
                }
            }
        }

        // Direct head classification (symbol-headed sentences only).
        if let Some(head_id) = sentence.head_symbol() {
            self.classify_head_name_into(self.syntactic.sym_name(head_id), out);
        }

        // Recurse into sub-sentences.  Sub-sentences may be direct
        // facts (e.g. a subclass edge nested inside an implication's
        // consequent).
        for el in &sentence.elements {
            if let Element::Sub { sid: sub_sid, .. } = el {
                self.classify_sid_into(*sub_sid, out);
            }
        }
    }

    fn classify_head_name_into(
        &self,
        head: &str,
        out:  &mut CacheImpact,
    ) {
        match TaxRelation::from_str(head) {
            // Taxonomy-edge heads.  The argument shape is validated by
            // `extract_tax_edge_for` at extraction time; if the sentence
            // is malformed (wrong arity, non-symbol args), the extraction
            // silently skips it, so flagging here is safe -- extraction
            // may be a no-op even when the flag is set.
            Some(_) => {
                out.taxonomy       = true;
                out.semantic_cache = true;
            }
            // Domain/range axioms.
            None => {
                match RelationRelation::from_str(head) { 
                    Some(_) => {
                        out.sort_annotations = true;
                        out.semantic_cache   = true;
                    },
                    None => {}
                }
            }
        }
    }
}

/// Inner cache state for [`super::SemanticLayer`].
///
/// Held under one `RwLock` on `SemanticLayer.cache` so granular
/// invalidation (`invalidate_symbols`) can mutate every map atomically
/// under a single guard.  Private to this module — callers interact
/// through the `impl SemanticLayer` methods in this file rather than
/// reaching the inner state directly.
#[derive(Debug, Default)]
pub(crate) struct Inner {
    pub(in super) is_instance:  HashMap<SymbolId, bool>,
    pub(in super) is_class:     HashMap<SymbolId, bool>,
    pub(in super) is_relation:  HashMap<SymbolId, bool>,
    pub(in super) is_predicate: HashMap<SymbolId, bool>,
    pub(in super) is_function:  HashMap<SymbolId, bool>,
    pub(in super) has_ancestor: HashMap<(SymbolId, SymbolId), bool>,
    pub(in super) arity:        HashMap<SymbolId, Option<i32>>,
    pub(in super) domain:       HashMap<SymbolId, Vec<RelationDomain>>,
    pub(in super) range:        HashMap<SymbolId, RelationDomain>,

    // Ontology-native doc-relation caches.  Each stores the full
    // list of entries (across all languages) for a symbol; per-call
    // language filtering happens at the lookup boundary.  Populated
    // lazily on first query; cleared wholesale by
    // `invalidate_semantic_cache` or granularly by
    // `invalidate_symbols`.
    pub(in super) documentation: HashMap<SymbolId, Vec<DocEntry>>,
    pub(in super) term_format:   HashMap<SymbolId, Vec<DocEntry>>,
    pub(in super) format:        HashMap<SymbolId, Vec<DocEntry>>,
}

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