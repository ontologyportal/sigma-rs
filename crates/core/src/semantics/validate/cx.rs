//! The validation context: everything a validator may read, with the
//! [`Scope`] already applied.
//!
//! Validators never touch the [`SemanticLayer`] directly. Every semantic query
//! the layer exposes has a `*_scoped` form, and threading the right `Scope`
//! into each call by hand is the easiest thing to get wrong -- a missed scope
//! silently validates a session buffer against base-only declarations. `Cx`
//! closes that off: it owns the scope and every accessor here applies it.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use crate::semantics::caches::disjoint::DisjointSet;
use crate::semantics::caches::doc_coverage::DocCoverage;
use crate::semantics::types::{RelationDomain, RelationRange, Scope, TaxRelation};
use crate::semantics::SemanticLayer;
use crate::syntactic::sentence::Sentence;
use crate::{SentenceId, SymbolId};

/// A borrow of the layer plus the scope and per-root dedup state one
/// validation pass needs.
pub(crate) struct Cx<'a> {
    pub(super) layer: &'a SemanticLayer,
    scope: Scope,
    /// Sub-sentences already walked in the current root pass, so a sub
    /// referenced twice by one root is validated once. Reset per root.
    visited: RefCell<HashSet<SentenceId>>,
    /// Symbols already seen in the current root pass, keyed by the claiming
    /// validator, so a symbol occurring several times in one formula yields one
    /// finding per validator rather than one per occurrence. Reset per root.
    symbols_seen: RefCell<HashSet<(&'static str, SymbolId)>>,
}

impl SemanticLayer {
    /// A [`Cx`] borrowing this layer, reasoning in an explicit [`Scope`].
    pub(crate) fn validation_cx(&self, scope: Scope) -> Cx<'_> {
        Cx {
            layer: self,
            scope,
            visited: Default::default(),
            symbols_seen: Default::default(),
        }
    }
}

impl<'a> Cx<'a> {
    /// Resolve a symbol id to its name, empty when the id is not interned.
    pub(crate) fn sym_name(&self, id: SymbolId) -> String {
        self.layer
            .syntactic
            .sym_name(id)
            .map(|s| s.name().to_string())
            .unwrap_or_default()
    }

    pub(crate) fn sentence(&self, sid: SentenceId) -> Option<Arc<Sentence>> {
        self.layer.syntactic.sentence(sid)
    }

    // -- Scope-applied semantic queries ------------------------------------

    pub(crate) fn is_relation(&self, sym: SymbolId) -> bool {
        self.layer.is_relation_scoped(sym, self.scope)
    }

    pub(crate) fn is_function(&self, sym: SymbolId) -> bool {
        self.layer.is_function_scoped(sym, self.scope)
    }

    pub(crate) fn is_predicate(&self, sym: SymbolId) -> bool {
        self.layer.is_predicate_scoped(sym, self.scope)
    }

    pub(crate) fn is_class(&self, sym: SymbolId) -> bool {
        self.layer.is_class_scoped(sym, self.scope)
    }

    pub(crate) fn is_instance(&self, sym: SymbolId) -> bool {
        self.layer.is_instance_scoped(sym, self.scope)
    }

    pub(crate) fn has_ancestor(&self, sym: SymbolId, ancestor: SymbolId) -> bool {
        self.layer.has_ancestor_scoped(sym, ancestor, self.scope)
    }

    pub(crate) fn has_ancestor_by_name(&self, sym: SymbolId, ancestor: &str) -> bool {
        self.layer
            .has_ancestor_by_name_scoped(sym, ancestor, self.scope)
    }

    /// Immediate taxonomy parents of `sym`, as `(parent, relation)` pairs.
    pub(crate) fn parents(&self, sym: SymbolId) -> Vec<(SymbolId, TaxRelation)> {
        self.layer.parents_of_scoped(sym, self.scope)
    }

    /// The taxonomy edge kind a head symbol asserts, if any.
    pub(crate) fn tax_role(&self, head: SymbolId) -> Option<TaxRelation> {
        self.layer.tax_role_of(head)
    }

    /// The ground facts `(head subject OBJ)`, as `(OBJ, sid)` pairs.
    pub(crate) fn binary_objects(
        &self,
        head: SymbolId,
        subject: SymbolId,
    ) -> Vec<(SymbolId, SentenceId)> {
        self.layer.ground_binary_objects(head, subject, self.scope)
    }

    /// The classes declared disjoint with `class`, each with its declaring
    /// fact.
    pub(crate) fn disjoint_with(&self, class: SymbolId) -> Arc<DisjointSet> {
        self.layer.disjoint_with_scoped(class, self.scope)
    }

    /// Whether `rel` is a propositional-attitude relation.
    pub(crate) fn is_attitude(&self, rel: SymbolId) -> bool {
        self.layer.is_attitude_scoped(rel, self.scope)
    }

    /// `agent`'s asserted `attitude` facts, as `(content, root)` pairs.
    pub(crate) fn attitude_facts(
        &self,
        attitude: SymbolId,
        agent: SymbolId,
    ) -> Vec<(SentenceId, SentenceId)> {
        self.layer
            .attitude_facts_scoped(attitude, agent, self.scope)
    }

    /// The id the store-normalized negation of `sid` would have.
    pub(crate) fn negation_id(&self, sid: SentenceId) -> Option<SentenceId> {
        self.layer.syntactic.negation_id(sid)
    }

    /// The sentence that defines `sym`, if any.
    pub(crate) fn defining_sentence(&self, sym: SymbolId) -> Option<SentenceId> {
        self.layer.defining_sentence(sym)
    }

    /// Whole-KB documentation coverage.
    pub(crate) fn doc_coverage(&self) -> Arc<DocCoverage> {
        self.layer.doc_coverage()
    }

    /// Whether `sym` is a CNF skolem constant rather than KB vocabulary.
    pub(crate) fn is_skolem(&self, sym: SymbolId) -> bool {
        self.layer.syntactic.is_skolem(sym)
    }

    /// The exhaustive decompositions of `class`, as `(fact sid, members)`.
    pub(crate) fn exhaustive_decompositions(
        &self,
        class: SymbolId,
    ) -> Vec<(SentenceId, Vec<SymbolId>)> {
        self.layer
            .exhaustive_decompositions_scoped(class, self.scope)
    }

    pub(crate) fn domain(&self, rel: SymbolId) -> Arc<Vec<RelationDomain>> {
        self.layer.domain_scoped(rel, self.scope)
    }

    pub(crate) fn range(&self, rel: SymbolId) -> RelationRange {
        self.layer.range_scoped(rel, self.scope)
    }

    /// Declared arity. Not scope-sensitive in the layer.
    pub(crate) fn arity(&self, rel: SymbolId) -> Option<i32> {
        self.layer.arity(rel)
    }

    // -- Per-root dedup state ----------------------------------------------

    /// Claim `sid` for this root pass. `false` means it was already walked.
    pub(super) fn claim_sentence(&self, sid: SentenceId) -> bool {
        self.visited.borrow_mut().insert(sid)
    }

    /// Claim `sym` for `tag` in this root pass. `false` means that validator
    /// has already considered this symbol under this root.
    pub(crate) fn claim_symbol(&self, tag: &'static str, sym: SymbolId) -> bool {
        self.symbols_seen.borrow_mut().insert((tag, sym))
    }

    pub(super) fn reset_root(&self) {
        self.visited.borrow_mut().clear();
        self.symbols_seen.borrow_mut().clear();
    }
}
