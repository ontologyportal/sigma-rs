//! `semantic::disjoint` cache: the classes declared disjoint with a class,
//! each paired with the fact sid that declares it.
//!
//! Three fact shapes declare disjointness, all ground and at the root:
//!   * `(disjoint A B)` -- `A` and `B` are disjoint (either argument order);
//!   * `(partition C M1 M2 ...)` and `(disjointDecomposition C M1 M2 ...)` --
//!     the members `M1 M2 ...` are pairwise disjoint (`C` is not).
//!
//! Candidates are shape-filtered from the class's axiom-occurrence set
//! (`axiom_index`) plus, in a session scope, the session's own roots.

use std::sync::Arc;

use crate::cache::events::{Event, EventKind};
use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::types::{Scope, Scoped};
use crate::semantics::SemanticLayer;
use crate::types::{Element, SentenceId, Symbol, SymbolId};

/// Classes disjoint with the keyed class, as `(class, declaring fact sid)`.
pub(crate) type DisjointSet = Vec<(SymbolId, SentenceId)>;

/// Behavior for the `semantic::disjoint` cache.
#[derive(Debug, Default)]
pub(crate) struct Disjoint;

impl CacheBehavior for Disjoint {
    type Parent = SemanticLayer;
    type Key = Scoped<SymbolId>;
    type Value = Arc<DisjointSet>;
    type Side = ();
    type SideSnapshot = ();
    type Tag = ();

    const NAME: &'static str = "semantic::disjoint";

    fn generate(
        &self,
        parent: &SemanticLayer,
        &Scoped { scope, key: class }: &Scoped<SymbolId>,
    ) -> Arc<DisjointSet> {
        let mut candidates: Vec<SentenceId> = parent
            .syntactic
            .axiom_sentences_of(class)
            .iter()
            .copied()
            .collect();
        if let Scope::Session(s) = scope {
            candidates.extend(parent.syntactic.sessions.session_sentences_by_id(s));
        }
        let mut out: DisjointSet = parent
            .scope_filter_sids(candidates, scope)
            .into_iter()
            .flat_map(|sid| {
                disjoint_members(parent, sid)
                    .filter(|members| members.contains(&class))
                    .into_iter()
                    .flatten()
                    .filter(move |&m| m != class)
                    .map(move |m| (m, sid))
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        Arc::new(out)
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[
            EventKind::RootAdded,
            EventKind::RootRemoved,
            EventKind::AxiomsPromoted,
            EventKind::SessionReferenced,
            EventKind::SessionRetracted,
        ]
    }

    fn reads(&self) -> &'static [&'static str] {
        &[
            "syntactic::sentences",
            "syntactic::axiom_index",
            "syntactic::sessions",
        ]
    }

    fn react(
        &self,
        parent: &SemanticLayer,
        events: &[&Event],
        store: &EntryCache<Scoped<SymbolId>, Arc<DisjointSet>>,
        _side: &Self::Side,
    ) -> Vec<Event> {
        let dirty = events.iter().any(|e| match e {
            Event::RootAdded { sid } => disjoint_members(parent, *sid).is_some(),
            Event::AxiomsPromoted { sids } => sids
                .iter()
                .any(|&sid| disjoint_members(parent, sid).is_some()),
            Event::RootRemoved { .. }
            | Event::SessionReferenced { .. }
            | Event::SessionRetracted { .. } => true,
            _ => false,
        });
        if dirty {
            store.clear();
        }
        Vec::new()
    }
}

/// The pairwise-disjoint classes a ground disjointness fact declares, or
/// `None` when `sid` is not one.
fn disjoint_members(parent: &SemanticLayer, sid: SentenceId) -> Option<Vec<SymbolId>> {
    let s = parent.syntactic.sentence(sid)?;
    let head = s.head_symbol()?;
    let first_member = if head == parent.disjoint_role() {
        1
    } else if head == parent.partition_role() || head == Symbol::hash_name("disjointDecomposition")
    {
        2
    } else {
        return None;
    };
    s.elements
        .get(first_member..)
        .filter(|members| !members.is_empty())?
        .iter()
        .map(|el| match el {
            Element::Symbol(sym) => Some(sym.id()),
            _ => None,
        })
        .collect()
}

impl SemanticLayer {
    /// The classes declared disjoint with `class` in `scope`, each with the
    /// sid of the declaring fact.
    pub(crate) fn disjoint_with_scoped(&self, class: SymbolId, scope: Scope) -> Arc<DisjointSet> {
        self.disjoint.get(self, Scoped { scope, key: class })
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::kif_layer;
    use crate::semantics::types::Scope;

    fn names(layer: &crate::semantics::SemanticLayer, class: &str) -> Vec<String> {
        let id = layer.syntactic.sym_id(class).unwrap();
        let mut out: Vec<String> = layer
            .disjoint_with_scoped(id, Scope::Base)
            .iter()
            .map(|(c, _)| layer.syntactic.sym_name(*c).unwrap().name().to_string())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn binary_disjoint_is_symmetric() {
        let layer = kif_layer("(disjoint Plant Animal)");
        assert_eq!(names(&layer, "Animal"), vec!["Plant"]);
        assert_eq!(names(&layer, "Plant"), vec!["Animal"]);
    }

    #[test]
    fn partition_members_are_pairwise_disjoint_but_not_the_whole() {
        let layer = kif_layer(
            "(partition Organism Animal Plant Fungus)\n(disjointDecomposition Shape Round Square)",
        );
        assert_eq!(names(&layer, "Animal"), vec!["Fungus", "Plant"]);
        assert!(names(&layer, "Organism").is_empty());
        assert_eq!(names(&layer, "Round"), vec!["Square"]);
    }

    #[test]
    fn base_scope_sees_a_disjoint_fact_once_its_session_is_promoted() {
        use crate::{KnowledgeBase, SourceFile};
        use std::path::PathBuf;

        let mut kb = KnowledgeBase::new();
        kb.stage(
            SourceFile::kif(PathBuf::from("d.kif"), "(disjoint Animal Plant)".into()),
            "d.kif",
        );
        kb.commit("d.kif");
        let layer = &kb.layer.semantic;
        let animal = layer.syntactic.sym_id("Animal").unwrap();
        assert!(layer.disjoint_with_scoped(animal, Scope::Base).is_empty());

        kb.make_session_axiomatic("d.kif").unwrap();
        let layer = &kb.layer.semantic;
        assert_eq!(layer.disjoint_with_scoped(animal, Scope::Base).len(), 1);
    }

    #[test]
    fn non_ground_and_unrelated_facts_are_ignored() {
        let layer = kif_layer(
            "
            (=> (instance ?X Animal) (disjoint ?X Plant))
            (exhaustiveDecomposition Organism Animal Plant)
        ",
        );
        assert!(names(&layer, "Animal").is_empty());
    }
}
