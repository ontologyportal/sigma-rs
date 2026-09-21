//! `semantic::range` cache: memoises a relation's range sort(s).

use thiserror::Error;

use crate::semantics::errors::{ArityMismatch, BoxedError, DomainMismatch, SemanticError};

use crate::cache::events::{Event, EventKind};
use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::consts::RANGE_SUB_REL_CLASS;
use crate::semantics::errors::semantic_error;
use crate::semantics::taxonomy::TaxRelation;
use crate::semantics::types::{Scope, Scoped};
use crate::semantics::SemanticLayer;
use crate::syntactic::caches::session::session_id;
use crate::types::RelationRange;
use crate::{Element, Sentence, SentenceId, SymbolId, ToDiagnostic};

/// Conflicting `range` / `rangeSubclass` declarations. Raised by the `range`
/// cache reactor on ingest, not by a validator.
#[derive(Debug, Clone, Error)]
#[error("function '{sym}' has multiple range declarations")]
pub struct DoubleRange {
    pub sym: String,
}
semantic_error!(DoubleRange, "E007", "double-range", Error);

/// Behavior for the `semantic::range` cache: the range sort(s) declared for a
/// relation via `range` / `rangeSubclass` axioms, falling back to a
/// `subrelation` parent's range when the relation declares none.
///
/// `tax_edges` is deliberately absent from `reads` (it consumes this cache's
/// `DomainRangeChanged`); see [`super::domain::Domain`].
#[derive(Debug, Default)]
pub(crate) struct Range;

impl CacheBehavior for Range {
    type Parent = SemanticLayer;
    type Key = Scoped<SymbolId>;
    type Value = RelationRange;
    type Side = ();
    type SideSnapshot = ();
    type Tag = ();

    const NAME: &'static str = "semantic::range";

    fn generate(
        &self,
        parent: &SemanticLayer,
        &Scoped { scope, key: rel }: &Scoped<SymbolId>,
    ) -> RelationRange {
        if let Some(own) = declared_range(parent, rel, scope) {
            return own;
        }
        // Undeclared: inherit from a `subrelation` parent
        // (`(subrelation ?R1 ?R2) ^ (range ?R2 ?C) => (range ?R1 ?C)`).
        parent
            .parents_of_scoped(rel, scope)
            .into_iter()
            .filter(|(_, tax)| *tax == TaxRelation::Subrelation)
            .map(|(sup, _)| parent.range_scoped(sup, scope))
            .find(|r| !matches!(r, RelationRange::Unknown))
            .unwrap_or(RelationRange::Unknown)
    }

    /// A self-referential declaration such as `(range FooFn (FooFn X))` would
    /// otherwise recurse into this cache for the same key. There is no range to
    /// report for it.
    fn on_cycle(&self, _parent: &SemanticLayer, _key: &Scoped<SymbolId>) -> RelationRange {
        RelationRange::Unknown
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[
            EventKind::RelationAdded,
            EventKind::RelationRemoved,
            EventKind::AxiomsPromoted,
            EventKind::SessionReferenced,
            EventKind::SessionRetracted,
        ]
    }

    fn produces(&self) -> &'static [EventKind] {
        &[EventKind::DomainRangeChanged]
    }

    fn reads(&self) -> &'static [&'static str] {
        &[
            "syntactic::sentences",
            "syntactic::residue_index",
            "syntactic::sessions",
        ]
    }

    fn react(
        &self,
        parent: &SemanticLayer,
        events: &[&Event],
        store: &EntryCache<Scoped<SymbolId>, RelationRange>,
        _side: &Self::Side,
    ) -> Vec<Event> {
        // An edge add/remove changes the relation's range in every scope (clear
        // wholesale); a session scope-membership change only moves which scope
        // sees an existing edge (invalidate only that session's entries).
        let mut out = vec![];
        let mut dirty = false;
        for event in events {
            if let Event::AxiomsPromoted { sids } = event {
                for sid in sids {
                    if let Some(rel) =
                        range_edge_relation(parent, *sid).or_else(|| parent.subrelation_child(*sid))
                    {
                        dirty = true;
                        out.push(Event::DomainRangeChanged { syms: vec![rel] });
                    }
                }
                continue;
            }
            // Whole session scope: the edge's relation and everything below it
            // in the subrelation lattice are affected (see `Domain`).
            if let Event::SessionReferenced { session, sids } = event {
                let s = Scope::Session(session_id(session));
                for sid in sids {
                    if let Some(rel) =
                        range_edge_relation(parent, *sid).or_else(|| parent.subrelation_child(*sid))
                    {
                        store.retain(|scoped, _| scoped.scope != s);
                        out.push(Event::DomainRangeChanged { syms: vec![rel] });
                    }
                }
                continue;
            }
            if let Event::SessionRetracted { session } = event {
                let s = Scope::Session(session_id(session));
                store.retain(|scoped, _| scoped.scope != s);
                continue;
            }
            // Only `range` / `rangeSubclass` roots are edges, plus
            // `subrelation` roots, which move the inherited range.
            let range_id = parent.range_role();
            let (f, _c) = match event {
                Event::RelationAdded { sid, head_id } => {
                    let h = *head_id;
                    if h != range_id && h != RANGE_SUB_REL_CLASS.id() {
                        if let Some(child) = parent.subrelation_child(*sid) {
                            dirty = true;
                            out.push(Event::DomainRangeChanged { syms: vec![child] });
                        }
                        continue;
                    }
                    match parent.try_extract_range(*sid) {
                        Some(Err(e)) => {
                            out.push(Event::Diagnostic(e.to_diagnostic()));
                            continue;
                        }
                        None => continue,
                        Some(Ok(res)) => res,
                    }
                }
                Event::RelationRemoved { sid, sentence } => {
                    let Some(head_sym) = sentence.head_symbol_name() else {
                        continue;
                    };
                    let h = head_sym.id();
                    if h != range_id && h != RANGE_SUB_REL_CLASS.id() {
                        if let Some(child) = parent.subrelation_child_of(sentence) {
                            dirty = true;
                            out.push(Event::DomainRangeChanged { syms: vec![child] });
                        }
                        continue;
                    }
                    match try_extract_range_from(parent, h, range_id, *sid, sentence) {
                        Ok(res) => res,
                        _ => continue,
                    }
                }
                _ => continue,
            };
            // A relation carrying both a `range` and a `rangeSubclass` base axiom
            // is the conflict `generate` collapses to `Unknown`; surface it as the
            // `DoubleRange` user error. Read raw axiom presence, not the memo,
            // which is stale until the trailing `clear`.
            if matches!(event, Event::RelationAdded { .. })
                && !parent
                    .subject_sids_scoped(range_id, f, Scope::Base)
                    .is_empty()
                && !parent
                    .subject_sids_scoped(RANGE_SUB_REL_CLASS.id(), f, Scope::Base)
                    .is_empty()
            {
                if let Some(sym) = parent.syntactic.sym_name(f) {
                    out.push(Event::Diagnostic(
                        DoubleRange {
                            sym: sym.to_string(),
                        }
                        .diagnostic(),
                    ));
                }
            }
            dirty = true;
            out.push(Event::DomainRangeChanged { syms: vec![f] });
        }
        if dirty {
            store.clear();
        }
        out
    }
}

/// The range `rel` declares for itself in `scope` via `range` /
/// `rangeSubclass` axioms -- no subrelation inheritance. `None` when it
/// declares nothing; `Some(Unknown)` for a conflicting pair.
pub(crate) fn declared_range(
    parent: &SemanticLayer,
    rel: SymbolId,
    scope: Scope,
) -> Option<RelationRange> {
    // A global (axiom) rule overrules a session assertion: resolve Base
    // first, fall to the session only when Base declares no range.
    let resolve = |only_base: bool| -> Option<RelationRange> {
        let pick = |head: SymbolId, make: fn(SymbolId) -> RelationRange| -> Option<RelationRange> {
            for sid in parent.subject_sids_scoped(head, rel, scope) {
                if only_base && !parent.syntactic.is_axiom(sid) {
                    continue;
                }
                if !only_base && parent.syntactic.is_axiom(sid) {
                    continue;
                }
                let Some(sentence) = parent.syntactic.sentence(sid) else {
                    continue;
                };
                let class_id = match sentence.elements.get(2) {
                    Some(Element::Symbol(sym)) => sym.id(),
                    other => match parent.class_denoted_by(other, scope) {
                        Some(id) => id,
                        None => continue,
                    },
                };
                return Some(make(class_id));
            }
            None
        };
        // `range` head id may be shape-recognized (renamed dialect);
        // `rangeSubclass` stays on its global name.
        let range = pick(parent.range_role(), RelationRange::Range);
        let range_subclass = pick(RANGE_SUB_REL_CLASS.id(), RelationRange::RangeSubclass);
        match (range, range_subclass) {
            (None, None) => None,
            (None, Some(rs)) => Some(rs),
            (Some(r), None) => Some(r),
            (Some(_), Some(_)) => Some(RelationRange::Unknown), // conflict
        }
    };

    if let Some(base) = resolve(true) {
        return Some(base);
    }
    if matches!(scope, Scope::Session(_)) {
        return resolve(false);
    }
    None
}

/// The relation named by a `range` / `rangeSubclass` root, including one whose
/// class term cannot yet be resolved - used to target session-scope
/// invalidation at just the affected relation's entry.
fn range_edge_relation(parent: &SemanticLayer, sid: SentenceId) -> Option<SymbolId> {
    let sentence = parent.syntactic.sentence(sid)?;
    let head = sentence.head_symbol()?;
    let range_id = parent.range_role();
    if head != range_id && head != RANGE_SUB_REL_CLASS.id() {
        return None;
    }
    match sentence.elements.get(1) {
        Some(Element::Symbol(rel)) => Some(rel.id()),
        _ => None,
    }
}

impl SemanticLayer {
    /// The range sort of relation `rel` in the `Base` taxonomy, if any.
    pub(crate) fn range(&self, rel: SymbolId) -> RelationRange {
        self.range_scoped(rel, Scope::Base)
    }

    /// [`Self::range`] in an explicit [`Scope`]: a session sees its own transient
    /// `range` rule only when `Base` declares none (a global rule overrules).
    pub(crate) fn range_scoped(&self, rel: SymbolId, scope: Scope) -> RelationRange {
        self.range.get(self, Scoped { scope, key: rel })
    }

    /// [`Self::range`] without subrelation inheritance: only what `rel`
    /// declares for itself (`Unknown` when nothing). Uncached.
    pub(crate) fn range_declared(&self, rel: SymbolId) -> RelationRange {
        declared_range(self, rel, Scope::Base).unwrap_or(RelationRange::Unknown)
    }

    // -- Taxonomy management ---------------------------------------------------

    /// Try to extract the relation and range from sentence `sid`.
    ///
    /// Returns `None` when `sid` is not headed by a `range`/`rangeSubclass`
    /// predicate; otherwise `Some(Ok(..))` for a well-formed edge or
    /// `Some(Err(..))` for a malformed one.
    fn try_extract_range(
        &self,
        sid: SentenceId,
    ) -> Option<Result<(SymbolId, RelationRange), BoxedError>> {
        let sentence = self.syntactic.sentence(sid)?;
        let head = sentence.head_symbol()?;
        Some(try_extract_range_from(
            self,
            head,
            self.range_role(),
            sid,
            &sentence,
        ))
    }
}

/// Extract the relation and range directly from a sentence body.
///
/// Used on removal, where the sentence rides on the `RelationRemoved` event
/// because the store copy is already gone.  Returns `Err` when the sentence is
/// not a well-formed `(range | rangeSubclass rel Class)` edge.
fn try_extract_range_from(
    parent: &SemanticLayer,
    head_id: SymbolId,
    range_id: SymbolId,
    sid: SentenceId,
    sentence: &Sentence,
) -> Result<(SymbolId, RelationRange), BoxedError> {
    let head_name = || {
        parent
            .syntactic
            .sym_name(head_id)
            .map_or_else(String::new, |s| s.to_string())
    };
    let mut els = sentence.elements.iter().skip(1);
    let Some(Element::Symbol(func)) = els.next() else {
        return Err(Box::new(DomainMismatch {
            sid,
            rel: head_name(),
            arg: 0,
            domain: "Function".to_string(),
        }));
    };
    let class_el = els.next();
    let class_id = match class_el {
        Some(Element::Symbol(class)) => class.id(),
        other => match parent.class_denoted_by(other, Scope::Base) {
            Some(id) => id,
            None => {
                return Err(Box::new(DomainMismatch {
                    sid,
                    rel: head_name(),
                    arg: 1,
                    domain: "Function".to_string(),
                }))
            }
        },
    };
    let remaining = els.count();
    if remaining > 0 {
        return Err(Box::new(ArityMismatch {
            sid,
            rel: head_name(),
            expected: 2,
            got: 2 + remaining,
        }));
    }
    if head_id == range_id {
        Ok((func.id(), RelationRange::Range(class_id)))
    } else {
        debug_assert_eq!(
            head_id,
            RANGE_SUB_REL_CLASS.id(),
            "range head should have been checked by the filter"
        );
        Ok((func.id(), RelationRange::RangeSubclass(class_id)))
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::kif_layer;
    use crate::semantics::types::RelationRange;

    #[test]
    fn range_domain_variant() {
        let layer = kif_layer("(range parent Human)");
        let parent = layer.syntactic.sym_id("parent").unwrap();
        let human_id = layer.syntactic.sym_id("Human").unwrap();
        match layer.range(parent) {
            RelationRange::Range(id) => assert_eq!(id, human_id),
            other => panic!("expected Domain(Human), got {other:?}"),
        }
    }

    #[test]
    fn range_domain_subclass_variant() {
        let layer = kif_layer("(rangeSubclass powerSet Class)");
        let power_set = layer.syntactic.sym_id("powerSet").unwrap();
        let class_id = layer.syntactic.sym_id("Class").unwrap();
        match layer.range(power_set) {
            RelationRange::RangeSubclass(id) => assert_eq!(id, class_id),
            other => panic!("expected DomainSubclass(Class), got {other:?}"),
        }
    }

    #[test]
    fn range_none_when_no_axiom() {
        let layer = kif_layer("(subclass Foo Bar)");
        let foo = layer.syntactic.sym_id("Foo").unwrap();
        assert!(matches!(layer.range(foo), RelationRange::Unknown));
    }
}

#[cfg(test)]
mod class_term_tests {
    use super::super::test_support::kif_layer;
    use crate::semantics::types::{RelationRange, Scope};

    const FIXTURE: &str = "
        (subclass Abstract Entity)
        (subclass Relation Abstract)
        (subclass Function Relation)
        (subclass BinaryFunction Function)
        (subclass Object Entity)
        (subclass Bar Object)
        (subclass Something Bar)
        (instance FooFn BinaryFunction)
        (rangeSubclass FooFn Bar)
        (instance MooFn BinaryFunction)
        (range MooFn (FooFn Something))
        (instance NooFn BinaryFunction)
        (rangeSubclass NooFn (FooFn Something))
    ";

    #[test]
    fn range_slot_resolves_a_range_subclass_function_term() {
        let layer = kif_layer(FIXTURE);
        let moo = layer.syntactic.sym_id("MooFn").unwrap();
        let bar = layer.syntactic.sym_id("Bar").unwrap();
        assert!(
            matches!(layer.range_scoped(moo, Scope::Base), RelationRange::Range(id) if id == bar),
            "`(range MooFn (FooFn Something))` with `(rangeSubclass FooFn Bar)` \
             should give MooFn the range Bar"
        );
    }

    #[test]
    fn range_subclass_slot_resolves_a_range_subclass_function_term() {
        let layer = kif_layer(FIXTURE);
        let noo = layer.syntactic.sym_id("NooFn").unwrap();
        let bar = layer.syntactic.sym_id("Bar").unwrap();
        assert!(matches!(
            layer.range_scoped(noo, Scope::Base),
            RelationRange::RangeSubclass(id) if id == bar
        ));
    }

    #[test]
    fn self_referential_range_declaration_does_not_recurse() {
        let layer = kif_layer(
            "
            (subclass Abstract Entity)
            (subclass Relation Abstract)
            (subclass Function Relation)
            (instance FooFn Function)
            (range FooFn (FooFn Something))
        ",
        );
        let foo = layer.syntactic.sym_id("FooFn").unwrap();
        assert!(
            matches!(layer.range_scoped(foo, Scope::Base), RelationRange::Unknown),
            "the cycle sentinel should stand in rather than recursing"
        );
    }
}

#[cfg(test)]
mod inheritance_tests {
    use crate::semantics::caches::test_support::kif_layer;
    use crate::semantics::types::RelationRange;

    #[test]
    fn subfunction_inherits_an_undeclared_range() {
        let layer = kif_layer(
            "
            (range AgeFn Quantity)
            (subrelation YearsOldFn AgeFn)
        ",
        );
        let sub = layer.syntactic.sym_id("YearsOldFn").unwrap();
        let quantity = layer.syntactic.sym_id("Quantity").unwrap();
        assert!(
            matches!(layer.range(sub), RelationRange::Range(id) if id == quantity),
            "got {:?}",
            layer.range(sub)
        );
    }

    #[test]
    fn own_range_wins_over_the_inherited_one() {
        let layer = kif_layer(
            "
            (range AgeFn Quantity)
            (subrelation YearsOldFn AgeFn)
            (range YearsOldFn Integer)
        ",
        );
        let sub = layer.syntactic.sym_id("YearsOldFn").unwrap();
        let integer = layer.syntactic.sym_id("Integer").unwrap();
        assert!(matches!(layer.range(sub), RelationRange::Range(id) if id == integer));
    }

    #[test]
    fn range_subclass_is_inherited_as_declared_through_a_chain() {
        let layer = kif_layer(
            "
            (rangeSubclass KindFn Class)
            (subrelation MidFn KindFn)
            (subrelation LeafFn MidFn)
        ",
        );
        let leaf = layer.syntactic.sym_id("LeafFn").unwrap();
        let class = layer.syntactic.sym_id("Class").unwrap();
        assert!(matches!(layer.range(leaf), RelationRange::RangeSubclass(id) if id == class));
    }

    #[test]
    fn subrelation_cycle_is_unknown_without_panicking() {
        let layer = kif_layer(
            "
            (subrelation a b)
            (subrelation b a)
        ",
        );
        let a = layer.syntactic.sym_id("a").unwrap();
        assert!(matches!(layer.range(a), RelationRange::Unknown));
    }
}
