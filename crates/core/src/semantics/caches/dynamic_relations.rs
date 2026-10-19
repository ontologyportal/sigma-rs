//! `semantic::dynamic_relations` -- the registry behind dynamic relations:
//! every instance of the configured `DynamicRelation` class (or a class
//! below it, `Base` scope), how it fires, and the embedded `#|@js |#` script
//! that exports its function.
//!
//! A relation fires as a Partial relation (any ground argument) or a Moded
//! one (every position named by its `dynamicInputs` fact ground).  Anything
//! that keeps a relation from firing is recorded as a [`RegistryProblem`]
//! for validation to report, and the relation is left out of `relations`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::cache::events::{Event, EventKind};
use crate::cache::{LayerCache, WholeCacheBehavior};
use crate::parse::script::Script;
use crate::semantics::consts::{
    DYNAMIC_INPUTS_RELATION, DYNAMIC_RELATION_CLASS, INSTANCE_RELATION,
    MODED_DYNAMIC_RELATION_CLASS, PARTIAL_DYNAMIC_RELATION_CLASS,
};
use crate::semantics::taxonomy::TaxRelation;
use crate::semantics::SemanticLayer;
use crate::types::{SentenceId, SymbolId};
use crate::{Element, Literal};

/// When a dynamic relation's function is called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Trigger {
    /// Any atom with at least one ground argument.
    Partial,
    /// Once every listed argument position (0-based) is ground.
    Moded(Vec<usize>),
}

/// One callable dynamic relation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DynamicRel {
    /// The relation's name, which is also its JS function's name.
    pub(crate) name: String,
    pub(crate) trigger: Trigger,
    /// Index into [`DynamicRegistry::scripts`] of the exporting script.
    pub(crate) script: usize,
    /// The `(instance R C)` root making the relation dynamic -- the premise
    /// a proof cites for facts the relation's function supplies.
    pub(crate) decl: Option<SentenceId>,
}

/// Why a declared dynamic relation, or a script, cannot be used.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RegistryProblem {
    /// An instance of the base class that is neither Partial nor Moded.
    NoTrigger { rel: SymbolId },
    /// A Moded relation without a usable `dynamicInputs` fact.
    NoInputs { rel: SymbolId },
    /// No script exports a function named after the relation.
    NoFunction { rel: SymbolId },
    /// More than one script exports `name`; the first in source order wins.
    DuplicateFunction { name: String, scripts: Vec<usize> },
}

/// The whole registry.
#[derive(Debug, Default)]
pub(crate) struct DynamicRegistry {
    /// Every embedded script in the KB, as `(source key, script)`.
    pub(crate) scripts: Vec<(String, Script)>,
    /// The relations that can fire.
    pub(crate) relations: HashMap<SymbolId, DynamicRel>,
    /// Every dynamic relation's `(instance R C)` declaration, firing or not
    /// -- where validation reports a relation's problems.
    pub(crate) decls: HashMap<SymbolId, SentenceId>,
    pub(crate) problems: Vec<RegistryProblem>,
}

/// Behavior for the `semantic::dynamic_relations` cache.
#[derive(Debug, Default)]
pub(crate) struct DynamicRelations;

/// Every instance of `class` or of a class below it.
fn instances_below(parent: &SemanticLayer, class: SymbolId) -> Vec<SymbolId> {
    let mut classes = vec![class];
    parent.walk_subclass_closure(class, false, |c| {
        classes.push(c);
        true
    });
    let mut out: Vec<SymbolId> = classes
        .into_iter()
        .flat_map(|c| parent.children_of(c))
        .filter(|(_, rel)| matches!(rel, TaxRelation::Instance))
        .map(|(sym, _)| sym)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// For each relation, an `(instance R C)` root with `C` the base dynamic
/// class or a class below it.
fn declarations(parent: &SemanticLayer) -> HashMap<SymbolId, SentenceId> {
    let base = DYNAMIC_RELATION_CLASS.id();
    let mut classes = std::collections::HashSet::from([base]);
    parent.walk_subclass_closure(base, false, |c| {
        classes.insert(c);
        true
    });
    let syn = &parent.syntactic;
    let mut out = HashMap::new();
    for sid in syn.by_head_id(&INSTANCE_RELATION.id()) {
        let Some(sentence) = syn.sentence(sid) else {
            continue;
        };
        if let (Some(Element::Symbol(rel)), Some(Element::Symbol(class))) =
            (sentence.elements.get(1), sentence.elements.get(2))
        {
            if classes.contains(&class.id()) {
                out.entry(rel.id()).or_insert(sid);
            }
        }
    }
    out
}

/// The 0-based input positions each relation's `(dynamicInputs R i ...)`
/// fact declares.  A fact with a non-positive or non-integer position is
/// ignored; several facts for one relation are merged.
fn declared_inputs(parent: &SemanticLayer) -> HashMap<SymbolId, Vec<usize>> {
    let syn = &parent.syntactic;
    let mut out: HashMap<SymbolId, Vec<usize>> = HashMap::new();
    for sid in syn.by_head_id(&DYNAMIC_INPUTS_RELATION.id()) {
        let Some(sentence) = syn.sentence(sid) else {
            continue;
        };
        let Some(Element::Symbol(rel)) = sentence.elements.get(1) else {
            continue;
        };
        let positions: Option<Vec<usize>> = sentence.elements[2..]
            .iter()
            .map(|e| match e {
                Element::Literal(Literal::Number(n)) => {
                    n.parse::<usize>().ok().filter(|&p| p > 0).map(|p| p - 1)
                }
                _ => None,
            })
            .collect();
        if let Some(positions) = positions.filter(|p| !p.is_empty()) {
            let entry = out.entry(rel.id()).or_default();
            entry.extend(positions);
            entry.sort_unstable();
            entry.dedup();
        }
    }
    out
}

impl WholeCacheBehavior for DynamicRelations {
    type Parent = SemanticLayer;
    type Value = Arc<DynamicRegistry>;

    const NAME: &'static str = "semantic::dynamic_relations";

    fn generate(&self, parent: &SemanticLayer) -> Arc<DynamicRegistry> {
        let scripts = parent.syntactic.scripts();
        let mut problems = Vec::new();

        let mut exporters: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, (_, script)) in scripts.iter().enumerate() {
            for name in &script.exports {
                exporters.entry(name.as_str()).or_default().push(i);
            }
        }
        let mut duplicates: Vec<(&str, Vec<usize>)> = exporters
            .iter()
            .filter(|(_, s)| s.len() > 1)
            .map(|(name, s)| (*name, s.clone()))
            .collect();
        duplicates.sort();
        problems.extend(duplicates.into_iter().map(|(name, scripts)| {
            RegistryProblem::DuplicateFunction {
                name: name.to_string(),
                scripts,
            }
        }));

        let partial = instances_below(parent, PARTIAL_DYNAMIC_RELATION_CLASS.id());
        let moded = instances_below(parent, MODED_DYNAMIC_RELATION_CLASS.id());
        let inputs = declared_inputs(parent);
        let decls = declarations(parent);
        let mut relations = HashMap::new();
        for rel in instances_below(parent, DYNAMIC_RELATION_CLASS.id()) {
            let trigger = if moded.contains(&rel) {
                match inputs.get(&rel) {
                    Some(positions) => Trigger::Moded(positions.clone()),
                    None => {
                        problems.push(RegistryProblem::NoInputs { rel });
                        continue;
                    }
                }
            } else if partial.contains(&rel) {
                Trigger::Partial
            } else {
                problems.push(RegistryProblem::NoTrigger { rel });
                continue;
            };
            let Some(name) = parent.syntactic.sym_name(rel) else {
                continue;
            };
            let name = name.name().to_string();
            let Some(&script) = exporters.get(name.as_str()).and_then(|s| s.first()) else {
                problems.push(RegistryProblem::NoFunction { rel });
                continue;
            };
            relations.insert(
                rel,
                DynamicRel {
                    name,
                    trigger,
                    script,
                    decl: decls.get(&rel).copied(),
                },
            );
        }

        Arc::new(DynamicRegistry {
            scripts,
            relations,
            decls,
            problems,
        })
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[
            EventKind::TaxonomyChanged,
            EventKind::RootAdded,
            EventKind::RootRemoved,
            EventKind::ScriptsChanged,
        ]
    }

    fn reads(&self) -> &'static [&'static str] {
        &[
            "semantic::tax_edges",
            "syntactic::sentences",
            "syntactic::source",
        ]
    }

    fn react(
        &self,
        _parent: &SemanticLayer,
        events: &[&Event],
        store: &LayerCache<Arc<DynamicRegistry>>,
    ) -> Vec<Event> {
        if events.iter().any(|e| {
            matches!(
                e,
                Event::TaxonomyChanged { .. }
                    | Event::RootAdded { .. }
                    | Event::RootRemoved { .. }
                    | Event::ScriptsChanged
            )
        }) {
            store.invalidate();
        }
        Vec::new()
    }
}

impl DynamicRegistry {
    /// The problems to report on the declaration `sid` of relation `rel`:
    /// its own, plus a duplicate export of its function.
    pub(crate) fn problems_for(&self, rel: SymbolId, name: &str) -> Vec<&RegistryProblem> {
        self.problems
            .iter()
            .filter(|p| match p {
                RegistryProblem::NoTrigger { rel: r }
                | RegistryProblem::NoInputs { rel: r }
                | RegistryProblem::NoFunction { rel: r } => *r == rel,
                RegistryProblem::DuplicateFunction { name: n, .. } => n == name,
            })
            .collect()
    }
}

impl SemanticLayer {
    /// The dynamic-relation registry.  See [`DynamicRelations`].
    pub(crate) fn dynamic_relations(&self) -> Arc<DynamicRegistry> {
        self.dynamic_relations.get(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantics::caches::test_support::kif_layer;

    const CLASSES: &str = "
        (subclass DynamicRelation Relation)
        (subclass PartialDynamicRelation DynamicRelation)
        (subclass ModedDynamicRelation DynamicRelation)
    ";

    fn registry(kif: &str) -> (SemanticLayer, Arc<DynamicRegistry>) {
        let layer = kif_layer(&format!("{CLASSES}{kif}"));
        let reg = layer.dynamic_relations();
        (layer, reg)
    }

    fn id(layer: &SemanticLayer, name: &str) -> SymbolId {
        layer.syntactic.sym_id(name).expect(name)
    }

    #[test]
    fn moded_relation_with_inputs_and_function_registers() {
        let (layer, reg) = registry(
            "(instance weather ModedDynamicRelation)
             (dynamicInputs weather 2 3)
             #|@js
             export function weather(x, y, z) { return []; }
             |#",
        );
        let rel = &reg.relations[&id(&layer, "weather")];
        assert_eq!(rel.trigger, Trigger::Moded(vec![1, 2]));
        assert!(rel.decl.is_some());
        assert_eq!(rel.name, "weather");
        assert!(reg.problems.is_empty(), "{:?}", reg.problems);
    }

    #[test]
    fn partial_relation_through_a_subclass_registers() {
        let (layer, reg) = registry(
            "(subclass SensorRelation PartialDynamicRelation)
             (instance reading SensorRelation)
             #|@js
             export function reading(a, b) { return []; }
             |#",
        );
        assert_eq!(
            reg.relations[&id(&layer, "reading")].trigger,
            Trigger::Partial
        );
    }

    #[test]
    fn unusable_relations_are_problems_not_entries() {
        let (layer, reg) = registry(
            "(instance bare DynamicRelation)
             (instance moded ModedDynamicRelation)
             (instance orphan PartialDynamicRelation)",
        );
        assert!(reg.relations.is_empty());
        let mut problems = reg.problems.clone();
        problems.sort_by_key(|p| format!("{p:?}"));
        assert!(problems.contains(&RegistryProblem::NoTrigger {
            rel: id(&layer, "bare")
        }));
        assert!(problems.contains(&RegistryProblem::NoInputs {
            rel: id(&layer, "moded")
        }));
        assert!(problems.contains(&RegistryProblem::NoFunction {
            rel: id(&layer, "orphan")
        }));
    }

    #[test]
    fn duplicate_exports_are_reported() {
        let (_, reg) = registry(
            "#|@js
             export function twice() {}
             |#
             #|@js
             export function twice() {}
             |#",
        );
        assert!(reg
            .problems
            .iter()
            .any(|p| matches!(p, RegistryProblem::DuplicateFunction { name, scripts } if name == "twice" && scripts.len() == 2)));
    }

    #[test]
    fn non_dynamic_relations_are_ignored() {
        let (_, reg) = registry(
            "(instance likes BinaryPredicate)
             #|@js
             export function likes(a, b) { return []; }
             |#",
        );
        assert!(reg.relations.is_empty());
        assert!(reg.problems.is_empty());
    }
}
