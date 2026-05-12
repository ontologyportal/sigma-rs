//! `semantic::range` cache: memoises a relation's range sort(s).

use crate::types::RelationRange;
use crate::{Element, SemanticError, Sentence, SentenceId, SymbolId, ToDiagnostic};
use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::SemanticLayer;
use crate::semantics::consts::RANGE_SUB_REL_CLASS;
use crate::semantics::types::{Scope, Scoped};
use crate::syntactic::caches::session::session_id;
use crate::cache::events::{Event, EventKind};

/// Behavior for the `semantic::range` cache: the range sort(s) declared for a
/// relation via `range` / `rangeSubclass` axioms.
#[derive(Debug, Default)]
pub(crate) struct Range;

impl CacheBehavior for Range {
    type Parent = SemanticLayer;
    type Key    = Scoped<SymbolId>;
    type Value  = RelationRange;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "semantic::range";

    fn generate(
        &self,
        parent: &SemanticLayer,
        &Scoped { scope, key: rel }: &Scoped<SymbolId>,
    ) -> RelationRange {
        // A global (axiom) rule overrules a session assertion: resolve Base
        // first, fall to the session only when Base declares no range.
        let resolve = |only_base: bool| -> Option<RelationRange> {
            let pick = |head: SymbolId, make: fn(SymbolId) -> RelationRange| -> Option<RelationRange> {
                for sid in parent.subject_sids_scoped(head, rel, scope) {
                    if only_base && !parent.syntactic.is_axiom(sid) { continue; }
                    if !only_base && parent.syntactic.is_axiom(sid) { continue; }
                    let Some(sentence) = parent.syntactic.sentence(sid) else { continue };
                    let class_id = match sentence.elements.get(2) {
                        Some(Element::Symbol(sym)) => sym.id(),
                        _ => continue,
                    };
                    return Some(make(class_id));
                }
                None
            };
            // `range` head id may be shape-recognized (renamed dialect);
            // `rangeSubclass` stays on its global name.
            let range          = pick(parent.range_role(),      RelationRange::Range);
            let range_subclass = pick(RANGE_SUB_REL_CLASS.id(), RelationRange::RangeSubclass);
            match (range, range_subclass) {
                (None, None)        => None,
                (None, Some(rs))    => Some(rs),
                (Some(r), None)     => Some(r),
                (Some(_), Some(_))  => Some(RelationRange::Unknown), // conflict
            }
        };

        if let Some(base) = resolve(true) {
            return base;
        }
        if matches!(scope, Scope::Session(_)) {
            if let Some(session) = resolve(false) {
                return session;
            }
        }
        RelationRange::Unknown
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
        events:  &[&Event],
        store:   &EntryCache<Scoped<SymbolId>, RelationRange>,
        _side:   &Self::Side,
    ) -> Vec<Event> {
        // An edge add/remove changes the relation's range in every scope (clear
        // wholesale); a session scope-membership change only moves which scope
        // sees an existing edge (invalidate only that session's entries).
        let mut out = vec![];
        let mut dirty = false;
        for event in events {
            if let Event::SessionReferenced { session, sids } = event {
                let s = Scope::Session(session_id(session));
                for sid in sids {
                    if let Some(rel) = range_edge_relation(parent, *sid) {
                        store.evict_keys(&[Scoped { scope: s, key: rel }]);
                    }
                }
                continue;
            }
            if let Event::SessionRetracted { session } = event {
                let s = Scope::Session(session_id(session));
                store.retain(|scoped, _| scoped.scope != s);
                continue;
            }
            // Only `range` / `rangeSubclass` roots are edges.
            let range_id = parent.range_role();
            let (f, _c) = match event {
                Event::RelationAdded { sid, head_id } => {
                    let h = *head_id;
                    if h != range_id && h != RANGE_SUB_REL_CLASS.id() {
                        continue;
                    }
                    match parent.try_extract_range(*sid) {
                        Some(Err(e)) => {
                            out.push(Event::Diagnostic(e.to_diagnostic()));
                            continue;
                        },
                        None => continue,
                        Some(Ok(res)) => res
                    }
                },
                Event::RelationRemoved { sid, sentence } => {
                    let Some(head_sym) = sentence.head_symbol_name() else { continue };
                    let h = head_sym.id();
                    if h != range_id && h != RANGE_SUB_REL_CLASS.id() {
                        continue;
                    }
                    match try_extract_range_from(parent, h, range_id, *sid, sentence) {
                        Ok(res) => res,
                        _ => continue
                    }
                },
                _ => { continue }
            };
            // A relation carrying both a `range` and a `rangeSubclass` base axiom
            // is the conflict `generate` collapses to `Unknown`; surface it as the
            // `DoubleRange` user error. Read raw axiom presence, not the memo,
            // which is stale until the trailing `clear`.
            if matches!(event, Event::RelationAdded { .. })
                && !parent.subject_sids_scoped(range_id, f, Scope::Base).is_empty()
                && !parent.subject_sids_scoped(RANGE_SUB_REL_CLASS.id(), f, Scope::Base).is_empty()
            {
                if let Some(sym) = parent.syntactic.sym_name(f) {
                    out.push(Event::Diagnostic(
                        SemanticError::DoubleRange { sym: sym.to_string() }.to_diagnostic(),
                    ));
                }
            }
            dirty = true;
            out.push(Event::DomainRangeChanged { syms: vec![f] });
        }
        if dirty { store.clear(); }
        out
    }
}

/// The relation a sentence declares a range for, iff `sid` is a well-formed
/// `(range | rangeSubclass rel Class)` edge — used to target session-scope
/// invalidation at just the affected relation's entry.
fn range_edge_relation(parent: &SemanticLayer, sid: SentenceId) -> Option<SymbolId> {
    let sentence = parent.syntactic.sentence(sid)?;
    let head     = sentence.head_symbol()?;
    let range_id = parent.range_role();
    if head != range_id && head != RANGE_SUB_REL_CLASS.id() { return None; }
    try_extract_range_from(parent, head, range_id, sid, &sentence).ok().map(|(rel, _)| rel)
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

    // -- Taxonomy management ---------------------------------------------------

    /// Try to extract the relation and range from sentence `sid`.
    ///
    /// Returns `None` when `sid` is not headed by a `range`/`rangeSubclass`
    /// predicate; otherwise `Some(Ok(..))` for a well-formed edge or
    /// `Some(Err(..))` for a malformed one.
    fn try_extract_range(&self, sid: SentenceId) -> Option<Result<(SymbolId, RelationRange), SemanticError>> {
        let sentence = self.syntactic.sentence(sid)?;
        let head     = sentence.head_symbol()?;
        Some(try_extract_range_from(self, head, self.range_role(), sid, &sentence))
    }
}

/// Extract the relation and range directly from a sentence body.
///
/// Used on removal, where the sentence rides on the `RelationRemoved` event
/// because the store copy is already gone.  Returns `Err` when the sentence is
/// not a well-formed `(range | rangeSubclass rel Class)` edge.
fn try_extract_range_from(
    parent:   &SemanticLayer,
    head_id:  SymbolId,
    range_id: SymbolId,
    sid:      SentenceId,
    sentence: &Sentence,
) -> Result<(SymbolId, RelationRange), SemanticError> {
    let head_name = || parent.syntactic.sym_name(head_id)
        .map_or_else(String::new, |s| s.to_string());
    let mut els = sentence.elements.iter().skip(1);
    let Some(Element::Symbol(func)) = els.next() else {
        return Err(SemanticError::DomainMismatch {
            sid,
            rel: head_name(),
            arg: 0,
            domain: "Function".to_string(),
        });
    };
    let Some(Element::Symbol(class)) = els.next() else {
        return Err(SemanticError::DomainMismatch {
            sid,
            rel: head_name(),
            arg: 1,
            domain: "Function".to_string(),
        });
    };
    let remaining = els.count();
    if remaining > 0 {
        return Err(SemanticError::ArityMismatch {
            sid,
            rel: head_name(),
            expected: 2,
            got: 2 + remaining,
        });
    }
    if head_id == range_id {
        Ok((func.id(), RelationRange::Range(class.id())))
    } else {
        debug_assert_eq!(head_id, RANGE_SUB_REL_CLASS.id(),
            "range head should have been checked by the filter");
        Ok((func.id(), RelationRange::RangeSubclass(class.id())))
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::kif_layer;
    use crate::semantics::types::RelationRange;

    #[test]
    fn range_domain_variant() {
        let layer = kif_layer("(range parent Human)");
        let parent   = layer.syntactic.sym_id("parent").unwrap();
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
        let class_id  = layer.syntactic.sym_id("Class").unwrap();
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
