// crates/core/src/trans/mod.rs
//
// Top layer of the KB stack.  Translation is the process of converting a
// KB to a query-ready form for an external prover (TPTP / TFF).  This
// module owns the `TranslationLayer` which sits above `SemanticLayer`
// and caches the per-symbol sort annotations and numeric-class
// characterisations needed to render SUMO formulas as TFF in one pass.

use std::{collections::{HashMap, HashSet, VecDeque}, sync::RwLock};

// Modules
pub mod cache;
pub mod sort;
pub mod arith;
pub mod numeric;
pub mod annotations;

// Public level imports
pub use sort::Sort;
pub use annotations::SortAnnotations;
pub(crate) use arith::ArithCond;

// Private imports
use cache::TranslationLayerCache;
use numeric::NUMERIC_ROOTS;

use crate::{SymbolId, TaxRelation, semantics::SemanticLayer};
use crate::layer::{Layer, NoLayer};
use crate::types::{Element, Literal, OpKind, SentenceId};

// Translation layer declaration

/// The translation layer translates SUMO formulas into a form parsable
/// as TFF in a single pass.  It pre-caches information like
/// `Domain -> Sort` assertions and other macro expansions used by Sigma
/// to produce valid TPTP from SUMO formulas.
#[derive(Debug)]
pub struct TranslationLayer {
    /// Inner layer: the semantic layer that this translation layer
    /// queries for taxonomy / domain / range / classification info.
    pub(crate) semantic:                SemanticLayer,
    /// Typed sort annotations
    pub(crate) sort_annotations:        RwLock<Option<SortAnnotations>>,
    /// Cached info for helping to construct translations
    pub(crate) cache:                   TranslationLayerCache,
}

// Several helpers below (`build_numeric_ancestor_set`,
// `build_poly_variant_symbols`, `build_numeric_char_cache`,
// `extract_instance_clause`, `extract_arith_cond`,
// `is_numeric_instance_of_var`, `make_cmp_cond`, `sort_for_id`)
// populate the `cache` field but are not yet driven from the public
// `TranslationLayer::new` path — they will be once cache priming
// is wired in.  Allowed dead so default builds stay warning-clean.
#[allow(dead_code)]
impl TranslationLayer {
    pub(crate) fn new(semantic: SemanticLayer) -> Self {
        Self {
            semantic,
            sort_annotations: RwLock::new(None),
            cache:            TranslationLayerCache::default(),
        }
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
            if let Some(root_id) = self.semantic.syntactic.sym_id(root_name) {
                if ancestors.insert(root_id) {
                    queue.push_back(root_id);
                }
            }
        }

        while let Some(id) = queue.pop_front() {
            // tax_incoming[id] = edge indices where `edge.to == id` (id is the subclass).
            // edge.from = the superclass of id -- walk upward.
            if let Some(edge_indices) = self.semantic.tax_incoming.get(&id) {
                for &edge_idx in edge_indices {
                    let edge = &self.semantic.tax_edges[edge_idx];
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
        for &sid in self.semantic.syntactic.by_head("domain") {
            let sentence = &self.semantic.syntactic.sentences[self.semantic.syntactic.sent_idx(sid)];
            // (domain Relation Position Class)
            let rel_id = match sentence.elements.get(1) {
                Some(Element::Symbol { id, .. }) => *id,
                _ => continue,
            };
            let class_id = match sentence.elements.get(3) {
                Some(Element::Symbol { id, .. }) => *id,
                _ => continue,
            };
            if self.cache.numeric_ancestor_set.contains(&class_id)
                && !self.cache.numeric_sorts.contains_key(&class_id)
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
            .filter_map(|(name, _)| self.semantic.syntactic.sym_id(name))
            .collect();

        for &root_sid in &self.semantic.syntactic.roots {
            let sentence = &self.semantic.syntactic.sentences[self.semantic.syntactic.sent_idx(root_sid)];

            // Form A: (<=> (instance ?VAR C) conditions)
            if matches!(sentence.elements.first(), Some(Element::Op { op: OpKind::Iff, .. })) {
                if let (Some(Element::Sub { sid: lhs, .. }), Some(Element::Sub { sid: rhs, .. })) =
                    (sentence.elements.get(1), sentence.elements.get(2))
                {
                    if let Some((class_id, var_name)) = self.extract_instance_clause(*lhs) {
                        if !root_ids.contains(&class_id)
                            && self.cache.numeric_sorts.contains_key(&class_id)
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
            if matches!(sentence.elements.first(), Some(Element::Op { op: OpKind::Implies, .. })) {
                if let (Some(Element::Sub { sid: ant, .. }), Some(Element::Sub { sid: con, .. })) =
                    (sentence.elements.get(1), sentence.elements.get(2))
                {
                    // Form B: consequent is the instance check
                    if let Some((class_id, var_name)) = self.extract_instance_clause(*con) {
                        if !root_ids.contains(&class_id)
                            && self.cache.numeric_sorts.contains_key(&class_id)
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
                            && self.cache.numeric_sorts.contains_key(&class_id)
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
        let sentence = &self.semantic.syntactic.sentences[self.semantic.syntactic.sent_idx(sid)];
        if let (
            Some(Element::Symbol { id: inst_id, .. }),
            Some(Element::Variable { name, .. }),
            Some(Element::Symbol { id: class_id, .. }),
        ) = (
            sentence.elements.get(0),
            sentence.elements.get(1),
            sentence.elements.get(2),
        ) {
            if self.semantic.syntactic.sym_name(*inst_id) == "instance" {
                return Some((*class_id, name.clone()));
            }
        }
        None
    }

    /// Recursively extract an `ArithCond` from `sid`, treating `var_name` as
    /// the implicit instance variable.  Strips `(instance var_name C)` conjuncts
    /// where C is any numeric class.  Returns `None` for unrecognised patterns.
    fn extract_arith_cond(&self, sid: SentenceId, var_name: &str) -> Option<ArithCond> {
        let sentence = &self.semantic.syntactic.sentences[self.semantic.syntactic.sent_idx(sid)];

        // (and ...) is an operator sentence: elements[0] is Op(And), not a Symbol.
        if matches!(sentence.elements.first(), Some(Element::Op { op: OpKind::And, .. })) {
            let parts: Vec<ArithCond> = sentence.elements[1..]
                .iter()
                .filter_map(|e| {
                    if let Element::Sub { sid: sub_sid, .. } = e {
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
        if matches!(sentence.elements.first(), Some(Element::Op { op: OpKind::Equal, .. })) {
            let arg0 = sentence.elements.get(1)?;
            let arg1 = sentence.elements.get(2)?;
            // (equal (FnName ?VAR other_literal) result_literal)
            if let (Element::Sub { sid: fn_sid, .. }, Element::Literal { lit: Literal::Number(result), .. }) = (arg0, arg1) {
                let fn_sent = &self.semantic.syntactic.sentences[self.semantic.syntactic.sent_idx(*fn_sid)];
                if let (
                    Some(Element::Symbol { id: fn_id, .. }),
                    Some(Element::Variable { name, .. }),
                    Some(Element::Literal { lit: Literal::Number(other_arg), .. }),
                ) = (fn_sent.elements.get(0), fn_sent.elements.get(1), fn_sent.elements.get(2))
                {
                    if name == var_name {
                        return Some(ArithCond::EqualFn {
                            fn_name:   self.semantic.syntactic.sym_name(*fn_id).to_string(),
                            other_arg: other_arg.clone(),
                            result:    result.clone(),
                        });
                    }
                }
            }
            // (equal result_literal (FnName ?VAR other_literal)) — reversed
            if let (Element::Literal { lit: Literal::Number(result), .. }, Element::Sub { sid: fn_sid, .. }) = (arg0, arg1) {
                let fn_sent = &self.semantic.syntactic.sentences[self.semantic.syntactic.sent_idx(*fn_sid)];
                if let (
                    Some(Element::Symbol { id: fn_id, .. }),
                    Some(Element::Variable { name, .. }),
                    Some(Element::Literal { lit: Literal::Number(other_arg), .. }),
                ) = (fn_sent.elements.get(0), fn_sent.elements.get(1), fn_sent.elements.get(2))
                {
                    if name == var_name {
                        return Some(ArithCond::EqualFn {
                            fn_name:   self.semantic.syntactic.sym_name(*fn_id).to_string(),
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
        let head    = self.semantic.syntactic.sym_name(head_id);

        match head {
            "greaterThan" | "greaterThanOrEqualTo" | "lessThan" | "lessThanOrEqualTo" => {
                let arg0 = sentence.elements.get(1)?;
                let arg1 = sentence.elements.get(2)?;
                // (pred ?VAR literal) — normal order
                if matches!(arg0, Element::Variable { name, .. } if name == var_name) {
                    if let Element::Literal { lit: Literal::Number(n), .. } = arg1 {
                        return Some(self.make_cmp_cond(head, n.clone(), false));
                    }
                }
                // (pred literal ?VAR) — reversed; flip the comparison direction
                if matches!(arg1, Element::Variable { name, .. } if name == var_name) {
                    if let Element::Literal { lit: Literal::Number(n), .. } = arg0 {
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
        let sentence = &self.semantic.syntactic.sentences[self.semantic.syntactic.sent_idx(sid)];
        if let (
            Some(Element::Symbol { id: inst_id, .. }),
            Some(Element::Variable { name, .. }),
            Some(Element::Symbol { id: class_id, .. }),
        ) = (
            sentence.elements.get(0),
            sentence.elements.get(1),
            sentence.elements.get(2),
        ) {
            return self.semantic.syntactic.sym_name(*inst_id) == "instance"
                && name == var_name
                && self.cache.numeric_sorts.contains_key(class_id);
        }
        false
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
        self.cache.numeric_sorts.get(&class_id).copied().unwrap_or(Sort::Individual)
    }
}

impl Layer for TranslationLayer {
    type Inner = SemanticLayer;
    type Outer = NoLayer;

    fn inner(&self) -> Option<&SemanticLayer> { Some(&self.semantic) }
    fn outer(&self) -> Option<&NoLayer> { None }
}

