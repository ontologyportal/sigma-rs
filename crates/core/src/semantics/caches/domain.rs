//! `semantic::domain` cache: memoises a relation's argument-domain sorts.

use std::sync::Arc;

use crate::cache::events::{Event, EventKind};
use crate::cache::{CacheBehavior, EagerBehavior, EagerMapBehavior, EntryCache};
use crate::semantics::consts::{CLASS_SYMBOL, DOMAIN_SUBCLASS_RELATION};
use crate::semantics::errors::BoxedError;
use crate::semantics::taxonomy::TaxRelation;
use crate::semantics::types::{RelationDomain, Scope, Scoped};
use crate::semantics::validate::validators::arity::ArityMismatch;
use crate::semantics::validate::validators::domain::DomainMismatch;
use crate::semantics::SemanticLayer;
use crate::syntactic::caches::session::session_id;
use crate::{Element, Literal, Sentence, SentenceId, SymbolId, ToDiagnostic};

/// Behavior for the `semantic::domain` cache: the argument-position sorts
/// declared for a relation via `domain` / `domainSubclass` axioms, ordered by
/// position (gaps filled with `RelationDomain::Unknown`), then any position
/// still open filled from the relation's `subrelation` parents.
///
/// `generate` walks `tax_edges` for the parents, but that cache is not in
/// `reads`: it consumes the `DomainRangeChanged` this one produces, so a data
/// edge here would close a reactor cycle. Invalidation instead keys off the
/// raw `subrelation` roots, which arrive on the same events as `domain` roots.
#[derive(Debug, Default)]
pub(crate) struct Domain;

impl CacheBehavior for Domain {
    type Parent = SemanticLayer;
    type Key = Scoped<SymbolId>;
    /// `Arc`-wrapped so a hit returns a refcount bump, not a deep copy of the
    /// positional domain vector.
    type Value = Arc<Vec<RelationDomain>>;
    type Side = ();
    type SideSnapshot = ();
    type Tag = ();

    const NAME: &'static str = "semantic::domain";

    fn generate(
        &self,
        parent: &SemanticLayer,
        &Scoped { scope, key: rel }: &Scoped<SymbolId>,
    ) -> Arc<Vec<RelationDomain>> {
        let mut result = declared_domain(parent, rel, scope);
        // A subrelation inherits every position it doesn't declare itself
        // (`(subrelation ?R1 ?R2) ^ (domain ?R2 ?N ?C) => (domain ?R1 ?N ?C)`);
        // an own declaration keeps precedence so a subrelation may narrow a slot.
        for (sup, tax) in parent.parents_of_scoped(rel, scope) {
            if tax != TaxRelation::Subrelation {
                continue;
            }
            let inherited = parent.domain_scoped(sup, scope);
            if inherited.len() > result.len() {
                result.resize(inherited.len(), RelationDomain::Unknown);
            }
            for (slot, rd) in result.iter_mut().zip(inherited.iter()) {
                if matches!(slot, RelationDomain::Unknown) {
                    *slot = rd.clone();
                }
            }
        }
        while matches!(result.last(), Some(RelationDomain::Unknown)) {
            result.pop();
        }
        Arc::new(result)
    }

    /// A `subrelation` cycle re-enters this cache on the same key; there is no
    /// inherited signature to report for it.
    fn on_cycle(
        &self,
        _parent: &SemanticLayer,
        _key: &Scoped<SymbolId>,
    ) -> Arc<Vec<RelationDomain>> {
        Arc::new(Vec::new())
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
            crate::syntactic::caches::sentences::SentenceCache::NAME,
            crate::syntactic::caches::residue_index::ResidueCache::NAME,
            crate::syntactic::caches::session::SessionCache::NAME,
            super::range::Range::NAME,
        ]
    }

    fn react(
        &self,
        parent: &SemanticLayer,
        events: &[&Event],
        store: &EntryCache<Scoped<SymbolId>, Arc<Vec<RelationDomain>>>,
        _side: &Self::Side,
    ) -> Vec<Event> {
        // A `domain`/`domainSubclass` edge add/remove clears the memo wholesale,
        // since Base axioms are shared across every scope. Session
        // scope-membership changes invalidate only the affected session's entries.
        let mut out = vec![];
        let mut dirty = false;
        for event in events {
            if let Event::AxiomsPromoted { sids } = event {
                for sid in sids {
                    if let Some(rel) = domain_edge_relation(parent, *sid)
                        .or_else(|| parent.subrelation_child(*sid))
                    {
                        dirty = true;
                        out.push(Event::DomainRangeChanged { syms: vec![rel] });
                    }
                }
                continue;
            }
            // A session newly references a `domain` or `subrelation` edge →
            // drop that session's entries. The edge's relation is affected and
            // so is everything below it in the subrelation lattice, which only
            // `tax_edges` knows, hence the whole scope rather than one key.
            if let Event::SessionReferenced { session, sids } = event {
                let s = Scope::Session(session_id(session));
                for sid in sids {
                    if let Some(rel) = domain_edge_relation(parent, *sid)
                        .or_else(|| parent.subrelation_child(*sid))
                    {
                        store.retain(|scoped, _| scoped.scope != s);
                        out.push(Event::DomainRangeChanged { syms: vec![rel] });
                    }
                }
                continue;
            }
            // A retracted session's whole scope is gone — drop just its entries.
            if let Event::SessionRetracted { session } = event {
                let s = Scope::Session(session_id(session));
                store.retain(|scoped, _| scoped.scope != s);
                continue;
            }
            // O(1) head filter: only `domain` / `domainSubclass` roots are
            // edges, plus `subrelation` roots, which move inherited positions.
            let domain_id = parent.domain_role();
            let extracted = match event {
                Event::RelationAdded { sid, head_id } => {
                    let h = *head_id;
                    if h != domain_id && h != DOMAIN_SUBCLASS_RELATION.id() {
                        if let Some(child) = parent.subrelation_child(*sid) {
                            dirty = true;
                            out.push(Event::DomainRangeChanged { syms: vec![child] });
                        }
                        continue;
                    }
                    parent.try_extract_domain(*sid)
                }
                Event::RelationRemoved { sid, sentence } => {
                    let Some(head_sym) = sentence.head_symbol_name() else {
                        continue;
                    };
                    let h = head_sym.id();
                    if h != domain_id && h != DOMAIN_SUBCLASS_RELATION.id() {
                        if let Some(child) = parent.subrelation_child_of(sentence) {
                            dirty = true;
                            out.push(Event::DomainRangeChanged { syms: vec![child] });
                        }
                        continue;
                    }
                    // On remove the body rides on the event (the store copy is gone).
                    Some(try_extract_domain_from(
                        parent, h, domain_id, *sid, sentence,
                    ))
                }
                _ => None,
            };
            let Some(res) = extracted else { continue };

            let (rel, _pos, _rd) = match res {
                // A malformed *added* domain axiom is a user error — surface it.
                Err(err) if matches!(event, Event::RelationAdded { .. }) => {
                    out.push(Event::Diagnostic(err.to_diagnostic()));
                    continue;
                }
                Err(_) => continue,
                Ok(t) => t,
            };

            dirty = true;
            out.push(Event::DomainRangeChanged { syms: vec![rel] });
        }
        if dirty {
            store.clear();
        }
        out
    }
}

/// The positions `rel` declares for itself in `scope` via `domain` /
/// `domainSubclass` axioms -- no subrelation inheritance. Trailing gaps are
/// kept so the caller can overlay inherited positions by index.
pub(crate) fn declared_domain(
    parent: &SemanticLayer,
    rel: SymbolId,
    scope: Scope,
) -> Vec<RelationDomain> {
    // A malformed axiom is skipped here; `react` surfaces it.
    //
    // Conflict rule: Base claims its positions first; a session may only
    // fill positions Base left open.
    let mut entries: Vec<(usize, RelationDomain, bool)> = Vec::new();
    // `domain` head id may be shape-recognized (renamed dialect);
    // `domainSubclass` stays on its global name.
    let domain_id = parent.domain_role();
    for head_id in [domain_id, DOMAIN_SUBCLASS_RELATION.id()] {
        for sid in parent.subject_sids_scoped(head_id, rel, scope) {
            let Some(sentence) = parent.syntactic.sentence(sid) else {
                continue;
            };
            if let Ok((r, pos, rd)) =
                try_extract_domain_from(parent, head_id, domain_id, sid, &sentence)
            {
                if r == rel {
                    let is_base = parent.syntactic.is_axiom(sid);
                    entries.push((pos, rd, is_base));
                }
            }
        }
    }
    let max = entries
        .iter()
        .map(|&(p, ..)| p)
        .max()
        .map(|p| p + 1)
        .unwrap_or(0);
    let mut result = vec![RelationDomain::Unknown; max];
    let mut base_claimed = vec![false; max];
    // Base axioms (e.2 == is_base) claim positions and overrule the session.
    for (pos, rd, _) in entries.iter().filter(|e| e.2) {
        result[*pos] = rd.clone();
        base_claimed[*pos] = true;
    }
    // Session assertions fill only the positions Base left open.
    for (pos, rd, _) in entries.iter().filter(|e| !e.2) {
        if !base_claimed[*pos] {
            result[*pos] = rd.clone();
        }
    }
    result
}

/// The relation a sentence declares a domain for, iff `sid` is a well-formed
/// `(domain | domainSubclass rel POS Class)` edge — used to target session-scope
/// invalidation at just the affected relation's entry.
fn domain_edge_relation(parent: &SemanticLayer, sid: SentenceId) -> Option<SymbolId> {
    let sentence = parent.syntactic.sentence(sid)?;
    let head = sentence.head_symbol()?;
    let domain_id = parent.domain_role();
    if head != domain_id && head != DOMAIN_SUBCLASS_RELATION.id() {
        return None;
    }
    try_extract_domain_from(parent, head, domain_id, sid, &sentence)
        .ok()
        .map(|(rel, _, _)| rel)
}

impl SemanticLayer {
    /// The argument-domain sorts of relation `rel` in the `Base` taxonomy
    /// (empty if not a relation).
    pub(crate) fn domain(&self, rel: SymbolId) -> Arc<Vec<RelationDomain>> {
        self.domain_scoped(rel, Scope::Base)
    }

    /// [`Self::domain`] in an explicit [`Scope`]: a session sees `Base` axioms
    /// plus its own transient `domain` rules (filling positions Base left open;
    /// a global rule always overrules a session assertion).
    pub(crate) fn domain_scoped(&self, rel: SymbolId, scope: Scope) -> Arc<Vec<RelationDomain>> {
        self.domain.get(self, Scoped { scope, key: rel })
    }

    /// [`Self::domain`] without subrelation inheritance: only what `rel`
    /// declares for itself. Uncached; a direct axiom lookup.
    pub(crate) fn domain_declared(&self, rel: SymbolId) -> Vec<RelationDomain> {
        declared_domain(self, rel, Scope::Base)
    }

    /// Try to extract a single `(domain | domainSubclass rel POS Class)` edge
    /// from sentence `sid`, returning `(rel, 0-based position, RelationDomain)`
    /// or a `SemanticError` for a malformed axiom.
    fn try_extract_domain(
        &self,
        sid: SentenceId,
    ) -> Option<Result<(SymbolId, usize, RelationDomain), BoxedError>> {
        let sentence = self.syntactic.sentence(sid)?;
        let head = sentence.head_symbol()?;
        Some(try_extract_domain_from(
            self,
            head,
            self.domain_role(),
            sid,
            &sentence,
        ))
    }
}

/// Extract a domain edge directly from a sentence body — used on removal, where
/// the sentence rides on the `RelationRemoved` event because the store copy is
/// already gone.  Shape: `(domain | domainSubclass  rel  POSITION  Class)`.
fn try_extract_domain_from(
    parent: &SemanticLayer,
    head_id: SymbolId,
    domain_id: SymbolId,
    sid: SentenceId,
    sentence: &Sentence,
) -> Result<(SymbolId, usize, RelationDomain), BoxedError> {
    let head_name = || {
        parent
            .syntactic
            .sym_name(head_id)
            .map_or_else(String::new, |s| s.to_string())
    };
    let mut els = sentence.elements.iter().skip(1);

    // arg 0 — the relation being described.
    let Some(Element::Symbol(rel)) = els.next() else {
        return Err(Box::new(DomainMismatch {
            sid,
            rel: head_name(),
            arg: 0,
            domain: "Relation".to_string(),
        }));
    };
    // arg 1 — the 1-based argument position (a numeric literal).
    let pos = match els.next() {
        Some(Element::Literal(Literal::Number(n))) => match n.parse::<usize>() {
            Ok(p) if p >= 1 => p - 1,
            _ => {
                return Err(Box::new(DomainMismatch {
                    sid,
                    rel: head_name(),
                    arg: 1,
                    domain: "PositiveInteger".to_string(),
                }))
            }
        },
        _ => {
            return Err(Box::new(DomainMismatch {
                sid,
                rel: head_name(),
                arg: 1,
                domain: "PositiveInteger".to_string(),
            }))
        }
    };
    // arg 2 — the class constraining that position.
    let class_el = els.next();
    let class_id = match class_el {
        Some(Element::Symbol(class)) => class.id(),
        other => match parent.class_denoted_by(other, Scope::Base) {
            Some(id) => id,
            None => {
                return Err(Box::new(DomainMismatch {
                    sid,
                    rel: head_name(),
                    arg: 2,
                    domain: CLASS_SYMBOL.name().to_string(),
                }))
            }
        },
    };
    let remaining = els.count();
    if remaining > 0 {
        return Err(Box::new(ArityMismatch {
            sid,
            rel: head_name(),
            expected: 3,
            got: 3 + remaining,
        }));
    }

    let rd = if head_id == domain_id {
        RelationDomain::Domain(class_id)
    } else {
        debug_assert_eq!(
            head_id,
            DOMAIN_SUBCLASS_RELATION.id(),
            "domain head should have been checked by the filter"
        );
        RelationDomain::DomainSubclass(class_id)
    };
    Ok((rel.id(), pos, rd))
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::kif_layer;
    use crate::semantics::types::RelationDomain;

    #[test]
    fn domain_two_positions() {
        let layer = kif_layer(
            "
            (domain likes 1 Animal)
            (domain likes 2 Animal)
        ",
        );
        let likes = layer.syntactic.sym_id("likes").unwrap();
        let animal = layer.syntactic.sym_id("Animal").unwrap();
        let d = layer.domain(likes);
        assert_eq!(d.len(), 2);
        assert!(matches!(&d[0], RelationDomain::Domain(id) if *id == animal));
        assert!(matches!(&d[1], RelationDomain::Domain(id) if *id == animal));
    }

    #[test]
    fn domain_subclass_variant() {
        let layer = kif_layer("(domainSubclass subclassOf 1 Class)");
        let rel = layer.syntactic.sym_id("subclassOf").unwrap();
        let class = layer.syntactic.sym_id("Class").unwrap();
        let d = layer.domain(rel);
        assert_eq!(d.len(), 1);
        assert!(matches!(&d[0], RelationDomain::DomainSubclass(id) if *id == class));
    }

    #[test]
    fn domain_out_of_order_axioms_sorted_by_position() {
        let layer = kif_layer(
            "
            (domain myRel 2 ClassB)
            (domain myRel 1 ClassA)
        ",
        );
        let rel = layer.syntactic.sym_id("myRel").unwrap();
        let class_a = layer.syntactic.sym_id("ClassA").unwrap();
        let class_b = layer.syntactic.sym_id("ClassB").unwrap();
        let d = layer.domain(rel);
        assert_eq!(d.len(), 2);
        assert!(
            matches!(&d[0], RelationDomain::Domain(id) if *id == class_a),
            "position 1 (ClassA) must be first regardless of declaration order"
        );
        assert!(
            matches!(&d[1], RelationDomain::Domain(id) if *id == class_b),
            "position 2 (ClassB) must be second"
        );
    }

    #[test]
    fn domain_gap_is_unknown() {
        // Only position 2 declared → position 1 is an `Unknown` gap.
        let layer = kif_layer("(domain sparse 2 Animal)");
        let rel = layer.syntactic.sym_id("sparse").unwrap();
        let animal = layer.syntactic.sym_id("Animal").unwrap();
        let d = layer.domain(rel);
        assert_eq!(d.len(), 2);
        assert!(matches!(&d[0], RelationDomain::Unknown));
        assert!(matches!(&d[1], RelationDomain::Domain(id) if *id == animal));
    }

    #[test]
    fn domain_empty_when_no_axiom() {
        let layer = kif_layer("(subclass Foo Bar)");
        let foo = layer.syntactic.sym_id("Foo").unwrap();
        assert!(layer.domain(foo).is_empty());
    }
}

#[cfg(test)]
mod inheritance_tests {
    use super::super::test_support::kif_layer;
    use crate::semantics::types::RelationDomain;
    use crate::semantics::SemanticLayer;

    fn ids(layer: &SemanticLayer, names: &[&str]) -> Vec<crate::SymbolId> {
        names
            .iter()
            .map(|n| layer.syntactic.sym_id(n).unwrap())
            .collect()
    }

    #[test]
    fn subrelation_inherits_undeclared_positions() {
        let layer = kif_layer(
            "
            (domain parent 1 Human)
            (domain parent 2 Human)
            (subrelation mother parent)
        ",
        );
        let [mother, human] = ids(&layer, &["mother", "Human"])[..] else {
            unreachable!()
        };
        let d = layer.domain(mother);
        assert_eq!(d.len(), 2, "got {d:?}");
        assert!(matches!(&d[0], RelationDomain::Domain(id) if *id == human));
        assert!(matches!(&d[1], RelationDomain::Domain(id) if *id == human));
    }

    #[test]
    fn own_declaration_narrows_a_slot_and_inherits_the_rest() {
        let layer = kif_layer(
            "
            (domain parent 1 Human)
            (domain parent 2 Human)
            (subrelation mother parent)
            (domain mother 1 Woman)
        ",
        );
        let [mother, human, woman] = ids(&layer, &["mother", "Human", "Woman"])[..] else {
            unreachable!()
        };
        let d = layer.domain(mother);
        assert_eq!(d.len(), 2, "got {d:?}");
        assert!(
            matches!(&d[0], RelationDomain::Domain(id) if *id == woman),
            "own `Woman` must win over inherited `Human` at position 1; got {d:?}"
        );
        assert!(matches!(&d[1], RelationDomain::Domain(id) if *id == human));
    }

    #[test]
    fn inheritance_follows_a_multi_level_chain() {
        let layer = kif_layer(
            "
            (domain related 1 Entity)
            (domain related 2 Entity)
            (subrelation parent related)
            (domain parent 2 Human)
            (subrelation mother parent)
        ",
        );
        let [mother, entity, human] = ids(&layer, &["mother", "Entity", "Human"])[..] else {
            unreachable!()
        };
        let d = layer.domain(mother);
        assert_eq!(d.len(), 2, "got {d:?}");
        assert!(matches!(&d[0], RelationDomain::Domain(id) if *id == entity));
        assert!(matches!(&d[1], RelationDomain::Domain(id) if *id == human));
    }

    #[test]
    fn domain_subclass_positions_are_inherited_as_declared() {
        let layer = kif_layer(
            "
            (domainSubclass typedBy 1 Class)
            (subrelation strictlyTypedBy typedBy)
        ",
        );
        let [sub, class] = ids(&layer, &["strictlyTypedBy", "Class"])[..] else {
            unreachable!()
        };
        let d = layer.domain(sub);
        assert!(
            matches!(d.first(), Some(RelationDomain::DomainSubclass(id)) if *id == class),
            "got {d:?}"
        );
    }

    #[test]
    fn subrelation_cycle_yields_no_signature_without_panicking() {
        let layer = kif_layer(
            "
            (subrelation a b)
            (subrelation b a)
        ",
        );
        let [a, b] = ids(&layer, &["a", "b"])[..] else {
            unreachable!()
        };
        assert!(layer.domain(a).is_empty());
        assert!(layer.domain(b).is_empty());
    }

    #[test]
    fn unrelated_taxonomy_edges_do_not_inherit() {
        // `subclass`/`instance` parents are not subrelation parents.
        let layer = kif_layer(
            "
            (domain parent 1 Human)
            (instance mother parent)
            (subclass father parent)
        ",
        );
        let [mother, father] = ids(&layer, &["mother", "father"])[..] else {
            unreachable!()
        };
        assert!(layer.domain(mother).is_empty());
        assert!(layer.domain(father).is_empty());
    }
}
