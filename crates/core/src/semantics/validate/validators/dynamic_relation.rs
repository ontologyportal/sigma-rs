//! W036 dynamic-relation-unusable: a declared dynamic relation that can never
//! fire -- no Partial/Moded trigger, a Moded relation without `dynamicInputs`,
//! no script exporting its function, or more than one.  Reported on the
//! relation's `(instance R C)` declaration.

use thiserror::Error;

use crate::semantics::caches::dynamic_relations::RegistryProblem;
use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::SentenceValidator;
use crate::{Element, SentenceId};

#[derive(Debug, Clone, Error)]
#[error("dynamic relation '{rel}' {why}")]
pub struct DynamicRelationUnusable {
    pub sid: SentenceId,
    pub rel: String,
    pub why: String,
}
semantic_error!(
    DynamicRelationUnusable,
    "W036",
    "dynamic-relation-unusable",
    Warning,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], 1)
    },
);

pub(crate) struct DynamicRelationCheck;

impl SentenceValidator for DynamicRelationCheck {
    type Error = DynamicRelationUnusable;

    fn check(&self, cx: &Cx<'_>, sid: SentenceId) -> Vec<DynamicRelationUnusable> {
        let Some(sentence) = cx.sentence(sid) else {
            return Vec::new();
        };
        let Some(Element::Symbol(rel)) = sentence.elements.get(1) else {
            return Vec::new();
        };
        let registry = cx.dynamic_relations();
        if registry.decls.get(&rel.id()) != Some(&sid) {
            return Vec::new();
        }
        let name = cx.sym_name(rel.id());
        registry
            .problems_for(rel.id(), &name)
            .into_iter()
            .map(|p| DynamicRelationUnusable {
                sid,
                rel: name.clone(),
                why: match p {
                    RegistryProblem::NoTrigger { .. } => {
                        "is neither a PartialDynamicRelation nor a ModedDynamicRelation".into()
                    }
                    RegistryProblem::NoInputs { .. } => {
                        "is Moded but has no (dynamicInputs ...) declaration".into()
                    }
                    RegistryProblem::NoFunction { .. } => {
                        format!("has no `export function {name}` in any #|@js |# block")
                    }
                    RegistryProblem::DuplicateFunction { scripts, .. } => format!(
                        "has its function exported by {} scripts; the first is used",
                        scripts.len()
                    ),
                },
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::{codes_in, kif_layer};

    const CLASSES: &str = "
        (subclass DynamicRelation Relation)
        (subclass PartialDynamicRelation DynamicRelation)
        (subclass ModedDynamicRelation DynamicRelation)
    ";

    fn codes_for_decl(kif: &str, rel: &str) -> Vec<&'static str> {
        let layer = kif_layer(&format!("{CLASSES}{kif}"));
        let sid = layer
            .syntactic
            .by_head("instance")
            .into_iter()
            .find(|sid| {
                layer
                    .syntactic
                    .sentence(*sid)
                    .is_some_and(|s| matches!(s.elements.get(1), Some(crate::Element::Symbol(sym)) if &*sym.name() == rel))
            })
            .expect("declaration");
        codes_in(&layer, sid)
    }

    #[test]
    fn w036_flags_a_relation_without_a_function() {
        let codes = codes_for_decl("(instance weather PartialDynamicRelation)", "weather");
        assert!(codes.contains(&"W036"), "{codes:?}");
    }

    #[test]
    fn w036_flags_moded_without_inputs() {
        let codes = codes_for_decl(
            "(instance weather ModedDynamicRelation)\n#|@js\nexport function weather() {}\n|#",
            "weather",
        );
        assert!(codes.contains(&"W036"), "{codes:?}");
    }

    #[test]
    fn w036_quiet_for_a_usable_relation() {
        let codes = codes_for_decl(
            "(instance weather PartialDynamicRelation)\n#|@js\nexport function weather() {}\n|#",
            "weather",
        );
        assert!(!codes.contains(&"W036"), "{codes:?}");
    }
}
