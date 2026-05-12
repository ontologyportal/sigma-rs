//! `semantic::domain` cache: memoises a relation's argument-domain sorts.

use std::sync::Arc;

use crate::{Element, Literal, SemanticError, Sentence, SentenceId, SymbolId, ToDiagnostic};
use crate::cache::{CacheBehavior, EntryCache};
use crate::cache::events::{Event, EventKind};
use crate::semantics::SemanticLayer;
use crate::semantics::consts::DOMAIN_SUBCLASS_RELATION;
use crate::semantics::types::{RelationDomain, Scope, Scoped};
use crate::syntactic::caches::session::session_id;

/// Behavior for the `semantic::domain` cache: the argument-position sorts
/// declared for a relation via `domain` / `domainSubclass` axioms, ordered by
/// position (gaps filled with `RelationDomain::Unknown`).
#[derive(Debug, Default)]
pub(crate) struct Domain;

impl CacheBehavior for Domain {
    type Parent = SemanticLayer;
    type Key    = Scoped<SymbolId>;
    /// `Arc`-wrapped so a hit returns a refcount bump, not a deep copy of the
    /// positional domain vector.
    type Value  = Arc<Vec<RelationDomain>>;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "semantic::domain";

    fn generate(
        &self,
        parent: &SemanticLayer,
        &Scoped { scope, key: rel }: &Scoped<SymbolId>,
    ) -> Arc<Vec<RelationDomain>> {
        // `generate` can't emit diagnostics, so a malformed axiom is skipped
        // (`react` surfaces it).
        //
        // Conflict rule: Base claims its positions first; a session may only
        // fill positions Base left open.
        let mut entries: Vec<(usize, RelationDomain, bool)> = Vec::new();
        // `domain` head id may be shape-recognized (renamed dialect);
        // `domainSubclass` stays on its global name.
        let domain_id = parent.domain_role();
        for head_id in [domain_id, DOMAIN_SUBCLASS_RELATION.id()] {
            for sid in parent.subject_sids_scoped(head_id, rel, scope) {
                let Some(sentence) = parent.syntactic.sentence(sid) else { continue };
                if let Ok((r, pos, rd)) = try_extract_domain_from(parent, head_id, domain_id, sid, &sentence) {
                    if r == rel {
                        let is_base = parent.syntactic.is_axiom(sid);
                        entries.push((pos, rd, is_base));
                    }
                }
            }
        }
        let max = entries.iter().map(|&(p, ..)| p).max().map(|p| p + 1).unwrap_or(0);
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
        while matches!(result.last(), Some(RelationDomain::Unknown)) { result.pop(); }
        Arc::new(result)
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::RelationAdded, EventKind::RelationRemoved,
          EventKind::SessionReferenced, EventKind::SessionRetracted]
    }

    fn produces(&self) -> &'static [EventKind] {
        &[EventKind::DomainRangeChanged]
    }

    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences", "syntactic::residue_index", "syntactic::sessions"]
    }

    fn react(
        &self,
        parent: &SemanticLayer,
        events: &[&Event],
        store:  &EntryCache<Scoped<SymbolId>, Arc<Vec<RelationDomain>>>,
        _side:   &Self::Side,
    ) -> Vec<Event> {
        // A `domain`/`domainSubclass` edge add/remove clears the memo wholesale,
        // since Base axioms are shared across every scope. Session
        // scope-membership changes invalidate only the affected session's entries.
        let mut out = vec![];
        let mut dirty = false;
        for event in events {
            // A session newly references a `domain` edge → drop only that
            // session's entry for the edge's relation.
            if let Event::SessionReferenced { session, sids } = event {
                let s = Scope::Session(session_id(session));
                for sid in sids {
                    if let Some(rel) = domain_edge_relation(parent, *sid) {
                        store.evict_keys(&[Scoped { scope: s, key: rel }]);
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
            // O(1) head filter: only `domain` / `domainSubclass` roots are edges.
            let domain_id = parent.domain_role();
            let extracted = match event {
                Event::RelationAdded { sid, head_id } => {
                    let h = *head_id;
                    if h != domain_id && h != DOMAIN_SUBCLASS_RELATION.id() { continue; }
                    parent.try_extract_domain(*sid)
                }
                Event::RelationRemoved { sid, sentence } => {
                    let Some(head_sym) = sentence.head_symbol_name() else { continue };
                    let h = head_sym.id();
                    if h != domain_id && h != DOMAIN_SUBCLASS_RELATION.id() { continue; }
                    // On remove the body rides on the event (the store copy is gone).
                    Some(try_extract_domain_from(parent, h, domain_id, *sid, sentence))
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
        if dirty { store.clear(); }
        out
    }
}

/// The relation a sentence declares a domain for, iff `sid` is a well-formed
/// `(domain | domainSubclass rel POS Class)` edge — used to target session-scope
/// invalidation at just the affected relation's entry.
fn domain_edge_relation(parent: &SemanticLayer, sid: SentenceId) -> Option<SymbolId> {
    let sentence  = parent.syntactic.sentence(sid)?;
    let head      = sentence.head_symbol()?;
    let domain_id = parent.domain_role();
    if head != domain_id && head != DOMAIN_SUBCLASS_RELATION.id() { return None; }
    try_extract_domain_from(parent, head, domain_id, sid, &sentence).ok().map(|(rel, _, _)| rel)
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

    /// Try to extract a single `(domain | domainSubclass rel POS Class)` edge
    /// from sentence `sid`, returning `(rel, 0-based position, RelationDomain)`
    /// or a `SemanticError` for a malformed axiom.
    fn try_extract_domain(
        &self,
        sid: SentenceId,
    ) -> Option<Result<(SymbolId, usize, RelationDomain), SemanticError>> {
        let sentence = self.syntactic.sentence(sid)?;
        let head     = sentence.head_symbol()?;
        Some(try_extract_domain_from(self, head, self.domain_role(), sid, &sentence))
    }
}

/// Extract a domain edge directly from a sentence body — used on removal, where
/// the sentence rides on the `RelationRemoved` event because the store copy is
/// already gone.  Shape: `(domain | domainSubclass  rel  POSITION  Class)`.
fn try_extract_domain_from(
    parent:    &SemanticLayer,
    head_id:   SymbolId,
    domain_id: SymbolId,
    sid:       SentenceId,
    sentence:  &Sentence,
) -> Result<(SymbolId, usize, RelationDomain), SemanticError> {
    let head_name = || parent.syntactic.sym_name(head_id)
        .map_or_else(String::new, |s| s.to_string());
    let mut els = sentence.elements.iter().skip(1);

    // arg 0 — the relation being described.
    let Some(Element::Symbol(rel)) = els.next() else {
        return Err(SemanticError::DomainMismatch {
            sid, rel: head_name(), arg: 0, domain: "Relation".to_string(),
        });
    };
    // arg 1 — the 1-based argument position (a numeric literal).
    let pos = match els.next() {
        Some(Element::Literal(Literal::Number(n))) => match n.parse::<usize>() {
            Ok(p) if p >= 1 => p - 1,
            _ => return Err(SemanticError::DomainMismatch {
                sid, rel: head_name(), arg: 1, domain: "PositiveInteger".to_string(),
            }),
        },
        _ => return Err(SemanticError::DomainMismatch {
            sid, rel: head_name(), arg: 1, domain: "PositiveInteger".to_string(),
        }),
    };
    // arg 2 — the class constraining that position.
    let Some(Element::Symbol(class)) = els.next() else {
        return Err(SemanticError::DomainMismatch {
            sid, rel: head_name(), arg: 2, domain: "Class".to_string(),
        });
    };
    let remaining = els.count();
    if remaining > 0 {
        return Err(SemanticError::ArityMismatch {
            sid, rel: head_name(), expected: 3, got: 3 + remaining,
        });
    }

    let rd = if head_id == domain_id {
        RelationDomain::Domain(class.id())
    } else {
        debug_assert_eq!(head_id, DOMAIN_SUBCLASS_RELATION.id(),
            "domain head should have been checked by the filter");
        RelationDomain::DomainSubclass(class.id())
    };
    Ok((rel.id(), pos, rd))
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::kif_layer;
    use crate::semantics::types::RelationDomain;

    #[test]
    fn domain_two_positions() {
        let layer = kif_layer("
            (domain likes 1 Animal)
            (domain likes 2 Animal)
        ");
        let likes  = layer.syntactic.sym_id("likes").unwrap();
        let animal = layer.syntactic.sym_id("Animal").unwrap();
        let d = layer.domain(likes);
        assert_eq!(d.len(), 2);
        assert!(matches!(&d[0], RelationDomain::Domain(id) if *id == animal));
        assert!(matches!(&d[1], RelationDomain::Domain(id) if *id == animal));
    }

    #[test]
    fn domain_subclass_variant() {
        let layer = kif_layer("(domainSubclass subclassOf 1 Class)");
        let rel   = layer.syntactic.sym_id("subclassOf").unwrap();
        let class = layer.syntactic.sym_id("Class").unwrap();
        let d = layer.domain(rel);
        assert_eq!(d.len(), 1);
        assert!(matches!(&d[0], RelationDomain::DomainSubclass(id) if *id == class));
    }

    #[test]
    fn domain_out_of_order_axioms_sorted_by_position() {
        let layer = kif_layer("
            (domain myRel 2 ClassB)
            (domain myRel 1 ClassA)
        ");
        let rel     = layer.syntactic.sym_id("myRel").unwrap();
        let class_a = layer.syntactic.sym_id("ClassA").unwrap();
        let class_b = layer.syntactic.sym_id("ClassB").unwrap();
        let d = layer.domain(rel);
        assert_eq!(d.len(), 2);
        assert!(matches!(&d[0], RelationDomain::Domain(id) if *id == class_a),
            "position 1 (ClassA) must be first regardless of declaration order");
        assert!(matches!(&d[1], RelationDomain::Domain(id) if *id == class_b),
            "position 2 (ClassB) must be second");
    }

    #[test]
    fn domain_gap_is_unknown() {
        // Only position 2 declared → position 1 is an `Unknown` gap.
        let layer = kif_layer("(domain sparse 2 Animal)");
        let rel    = layer.syntactic.sym_id("sparse").unwrap();
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
