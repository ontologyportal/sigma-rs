//! E006 domain-mismatch: each argument must satisfy the domain class declared
//! for its position.

use thiserror::Error;

use crate::semantics::consts::{CLASS_SYMBOL, FORMULA_SYMBOL, ROOT_SYMBOL};
use crate::semantics::errors::semantic_error;
use crate::semantics::types::RelationDomain;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::SentenceValidator;
use crate::types::RelationRange;
use crate::{Element, SentenceId};

/// Argument-domain mismatch. Raised by the `domain` validator and by the
/// declaration-parsing caches.
#[derive(Debug, Clone, Error)]
#[error("domain mismatch for '{rel}' argument #{arg}: expected '{domain}'")]
pub struct DomainMismatch {
    pub sid: SentenceId,
    pub rel: String,
    pub arg: usize,
    pub domain: String,
}
semantic_error!(
    DomainMismatch,
    "E006",
    "domain-mismatch",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], self.arg as i32)
    },
);

pub(crate) struct Domain;

impl SentenceValidator for Domain {
    type Error = DomainMismatch;

    fn check(&self, cx: &Cx<'_>, sid: SentenceId) -> Vec<DomainMismatch> {
        let Some(sentence) = cx.sentence(sid) else {
            return Vec::new();
        };
        let Some(Element::Symbol(head)) = sentence.elements.first() else {
            return Vec::new();
        };
        let domain = cx.domain(head.id());
        if domain.is_empty() {
            return Vec::new();
        }
        sentence.elements[1..]
            .iter()
            .zip(domain.iter())
            .enumerate()
            .filter(|(_, (_, dom))| !matches!(dom, RelationDomain::Unknown))
            .filter(|(_, (arg, dom))| !arg_satisfies_domain(cx, arg, dom))
            .map(|(i, (_, dom))| DomainMismatch {
                sid,
                rel: cx.sym_name(head.id()),
                arg: i + 1,
                domain: cx.sym_name(dom.id().unwrap_or(u64::MAX)),
            })
            .collect()
    }
}

/// Whether `arg` can satisfy the declared domain `dom` at its position.
///
/// A variable always satisfies: it carries no statically-knowable type and is
/// *constrained* by the very declaration being checked, so it can never
/// violate one. Asking `is_class` of a variable's scoped id (`?NUM` ->
/// `NUM__<scope>`) would treat every edge-less symbol as a root class and fail
/// every variable -- see `semantics/caches/is_class.rs`.
fn arg_satisfies_domain(cx: &Cx<'_>, arg: &Element, dom: &RelationDomain) -> bool {
    match arg {
        Element::Symbol(sym) => {
            let sym_id = sym.id();
            match dom {
                RelationDomain::Domain(dom_id) => {
                    let sym_name = cx.sym_name(*dom_id);
                    if sym_name == *ROOT_SYMBOL.name() {
                        return true;
                    }
                    if cx.is_class(sym_id) {
                        let class_id = CLASS_SYMBOL.id();
                        if class_id == *dom_id || cx.has_ancestor(class_id, *dom_id) {
                            return true;
                        }
                    }
                    cx.is_instance(sym_id) && cx.has_ancestor(sym_id, *dom_id)
                }
                RelationDomain::DomainSubclass(dom_id) => {
                    if *dom_id == ROOT_SYMBOL.id() {
                        true
                    } else if *dom_id == CLASS_SYMBOL.id() {
                        cx.is_class(sym_id)
                    } else {
                        cx.is_class(sym_id) && cx.has_ancestor(sym_id, *dom_id)
                    }
                }
                RelationDomain::Unknown => true,
            }
        }
        Element::Sub(sub_sid) if let Some(sub_sent) = cx.sentence(*sub_sid) => match dom {
            RelationDomain::Domain(dom_id) => {
                let Some(sym) = sub_sent.head_symbol() else {
                    return sub_sent.is_operator() && *dom_id == FORMULA_SYMBOL.id();
                };
                if cx.is_predicate(sym) {
                    *dom_id == FORMULA_SYMBOL.id()
                } else if *dom_id == ROOT_SYMBOL.id() {
                    true
                } else if *dom_id == FORMULA_SYMBOL.id() {
                    false
                } else {
                    match cx.range(sym) {
                        RelationRange::Range(range) => cx.has_ancestor(range, *dom_id),
                        RelationRange::RangeSubclass(_) => {
                            let class_id = CLASS_SYMBOL.id();
                            class_id == *dom_id || cx.has_ancestor(class_id, *dom_id)
                        }
                        RelationRange::Unknown => false,
                    }
                }
            }
            RelationDomain::DomainSubclass(dom_id) => {
                let Some(sym) = sub_sent.head_symbol() else {
                    return false;
                };
                if cx.is_function(sym) {
                    if let RelationRange::RangeSubclass(range) = cx.range(sym) {
                        cx.has_ancestor(range, *dom_id)
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            RelationDomain::Unknown => true,
        },
        Element::Variable { .. } | Element::Literal(_) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::{codes_in, kif_layer, root_by_head};

    #[test]
    fn domain_check_accepts_class_for_superclass_of_class_domain() {
        // Every class is an instance of `Class`, hence of its superclass
        // `SetOrClass`, so a class argument satisfies a `SetOrClass` domain.
        let layer = kif_layer(
            r#"
            (subclass SetOrClass Entity)
            (subclass Class SetOrClass)
            (instance lexicon BinaryPredicate)
            (domain lexicon 1 SetOrClass)
            (subclass Multipole Entity)
            (subclass Twopole Multipole)
            (lexicon Twopole Multipole)
        "#,
        );
        let sid = root_by_head(&layer, "lexicon");
        let codes = codes_in(&layer, sid);
        assert!(
            !codes.contains(&"E006"),
            "a class must satisfy a SetOrClass domain (superclass of Class); got {codes:?}"
        );
    }

    #[test]
    fn domain_check_does_not_flag_variable_arguments() {
        // A variable argument carries no statically-knowable type and is
        // constrained by the domain it sits in, so it can never violate one.
        let layer = kif_layer(
            r#"
            (subclass Human Entity)
            (instance Human Class)
            (instance brother BinaryPredicate)
            (domain brother 1 Human)
            (domain brother 2 Human)
            (brother ?A ?B)
        "#,
        );
        let sid = root_by_head(&layer, "brother");
        let codes = codes_in(&layer, sid);
        assert!(
            !codes.contains(&"E006"),
            "E006 must not fire on variable args; got {codes:?}"
        );
    }

    #[test]
    fn e006_flags_argument_outside_its_domain() {
        let layer = kif_layer(
            r#"
            (subclass Relation Entity)
            (subclass BinaryRelation Relation)
            (subclass Human Entity)
            (subclass Rock Entity)
            (instance Rock Class)
            (instance brother BinaryRelation)
            (domain brother 1 Human)
            (instance Pebble Rock)
            (brother Pebble Pebble)
        "#,
        );
        let sid = root_by_head(&layer, "brother");
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"E006"),
            "a Rock instance must not satisfy a Human domain; got {codes:?}"
        );
    }
}

#[cfg(test)]
mod test {
    use crate::semantics::validate::test_support::{
        codes_in, kif_layer, root_by_head, root_by_head_with,
    };

    /// github.com/ontologyportal/sigma-rs/issues/47 -- `BodySideFn` has
    /// `rangeSubclass`, so `(BodySideFn Left Hand)` denotes a class and is a
    /// legitimate argument 2 of `subclass`, whose domain is `Class`.
    const FIXTURE: &str = "
        (subclass Abstract Entity)
        (subclass SetOrClass Abstract)
        (subclass Class SetOrClass)
        (subclass Relation Abstract)
        (subclass Function Relation)
        (subclass BinaryFunction Function)
        (subclass Predicate Relation)
        (subclass BinaryPredicate Predicate)
        (subclass BinaryRelation Relation)
        (subclass BinaryPredicate BinaryRelation)
        (instance subclass BinaryPredicate)
        (domain subclass 1 Class)
        (domain subclass 2 Class)
        (instance instance BinaryPredicate)
        (domain instance 1 Entity)
        (domain instance 2 Class)
        (subclass Attribute Abstract)
        (subclass PositionalAttribute Attribute)
        (subclass AntiSymmetricPositionalAttribute PositionalAttribute)
        (instance Left AntiSymmetricPositionalAttribute)
        (subclass Object Entity)
        (subclass BodyPart Object)
        (subclass Hand BodyPart)
        (instance BodySideFn BinaryFunction)
        (domain BodySideFn 1 AntiSymmetricPositionalAttribute)
        (domainSubclass BodySideFn 2 BodyPart)
        (rangeSubclass BodySideFn BodyPart)
        (subclass LeftHand (BodySideFn Left Hand))
    ";

    #[test]
    fn function_returning_a_class_satisfies_a_class_domain() {
        let layer = kif_layer(FIXTURE);
        let sid = root_by_head_with(&layer, "subclass", "LeftHand");
        let codes = codes_in(&layer, sid);
        assert!(
            !codes.contains(&"E006"),
            "a `rangeSubclass` function term denotes a class and must satisfy \
             `(domain subclass 2 Class)`; got {codes:?}"
        );
    }

    /// Guard against over-accepting: `(domain grasps 2 BodyPart)` asks for an
    /// *instance* of BodyPart, and a `rangeSubclass` function denotes a class,
    /// which is not one.
    #[test]
    fn function_term_does_not_satisfy_an_instance_domain() {
        let layer = kif_layer(&format!(
            "{FIXTURE}
            (instance grasps BinaryPredicate)
            (domain grasps 1 Entity)
            (domain grasps 2 BodyPart)
            (grasps Left (BodySideFn Left Hand))"
        ));
        let sid = root_by_head(&layer, "grasps");
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"E006"),
            "a class-denoting term must not satisfy an instance domain; got {codes:?}"
        );
    }

    /// `(domainSubclass BodySideFn 2 BodyPart)` with `Hand` below BodyPart: the
    /// nested function term in argument 2 resolves through `rangeSubclass`.
    #[test]
    fn function_term_satisfies_a_matching_domain_subclass() {
        let layer = kif_layer(&format!(
            "{FIXTURE}
            (subclass LeftLeftHand (BodySideFn Left (BodySideFn Left Hand)))"
        ));
        let sid = root_by_head_with(&layer, "subclass", "LeftLeftHand");
        let codes = codes_in(&layer, sid);
        assert!(
            !codes.contains(&"E006"),
            "a rangeSubclass term must satisfy `domainSubclass ... BodyPart`; got {codes:?}"
        );
    }

    /// The symptom quoted in the issue is E001, not E006: it needs the
    /// `tax_edges` cache to resolve the function term to its range class so
    /// `LeftHand` gains a derivation to Entity.
    #[test]
    fn class_defined_by_a_function_term_derives_to_entity() {
        let layer = kif_layer(FIXTURE);
        let sid = root_by_head_with(&layer, "subclass", "LeftHand");
        let codes = codes_in(&layer, sid);
        assert!(
            !codes.contains(&"E001"),
            "`LeftHand` should reach Entity through `rangeSubclass BodySideFn \
             BodyPart`; got {codes:?}"
        );
    }

    const FIXTURE2: &str = "
        (subclass Abstract Entity)
        (subclass Relation Abstract)
        (subclass Function Relation)
        (subclass BinaryFunction Function)
        (subclass Object Entity)
        (subclass Bar Object)
        (subclass Something Bar)
        (instance FooFn BinaryFunction)
        (rangeSubclass FooFn Bar)
        (instance BazFn BinaryFunction)
        (domain BazFn 1 (FooFn Something))
        (subclass Unrelated Entity)
        (instance Nope Unrelated)
        (instance Yep Something)
        (BazFn Yep Yep)
        (BazFn Nope Nope)
    ";

    /// A domain declared through a function term still constrains arguments:
    /// `(domain BazFn 1 (FooFn Something))` under `(rangeSubclass FooFn Bar)`
    /// types argument 1 as Bar.
    #[test]
    fn resolved_domain_accepts_a_conforming_argument() {
        let layer = kif_layer(FIXTURE2);
        let sid = root_by_head_with(&layer, "BazFn", "Yep");
        let codes = codes_in(&layer, sid);
        assert!(!codes.contains(&"E006"), "got {codes:?}");
    }

    #[test]
    fn resolved_domain_rejects_a_non_conforming_argument() {
        let layer = kif_layer(FIXTURE2);
        let sid = root_by_head_with(&layer, "BazFn", "Nope");
        let codes = codes_in(&layer, sid);
        assert!(codes.contains(&"E006"), "got {codes:?}");
    }

    const FIXTURE3: &str = "
        (subclass Abstract Entity)
        (subclass Relation Abstract)
        (subclass Function Relation)
        (subclass BinaryFunction Function)
        (subclass Predicate Relation)
        (subclass BinaryPredicate Predicate)
        (subclass Object Entity)
        (subclass Bar Object)
        (subclass Something Bar)
        (instance Blah Something)
        (instance Yep Something)
        (instance FooFn BinaryFunction)
        (domain FooFn 1 Formula)
        (instance grasps BinaryPredicate)
        (domain grasps 1 Something)
        (domain grasps 2 Something)
        (instance PredTag Something)
        (instance OpTag Something)
        (instance BareTag Something)
        (instance FuncTag Something)
        (FooFn (grasps Yep Yep) PredTag)
        (FooFn (not (grasps Yep Yep)) OpTag)
        (FooFn Blah BareTag)
        (FooFn (FooFn Blah Yep) FuncTag)
    ";

    /// `(domain FooFn 1 Formula)`: a predicate- or operator-headed sentence
    /// denotes a formula and satisfies the domain, but a bare symbol or a
    /// function-headed sentence (which denotes an object, not a formula)
    /// does not.
    #[test]
    fn predicate_and_operator_headed_sentences_are_formulas() {
        let layer = kif_layer(FIXTURE3);

        let predicate_headed = root_by_head_with(&layer, "FooFn", "PredTag");
        assert!(
            !codes_in(&layer, predicate_headed).contains(&"E006"),
            "a predicate-headed sentence denotes a formula and must satisfy \
             `(domain FooFn 1 Formula)`"
        );

        let operator_headed = root_by_head_with(&layer, "FooFn", "OpTag");
        assert!(
            !codes_in(&layer, operator_headed).contains(&"E006"),
            "an operator-headed sentence denotes a formula and must satisfy \
             `(domain FooFn 1 Formula)`"
        );

        let bare_symbol = root_by_head_with(&layer, "FooFn", "BareTag");
        assert!(
            codes_in(&layer, bare_symbol).contains(&"E006"),
            "a bare symbol does not denote a formula and must not satisfy \
             `(domain FooFn 1 Formula)`"
        );

        let function_headed = root_by_head_with(&layer, "FooFn", "FuncTag");
        assert!(
            codes_in(&layer, function_headed).contains(&"E006"),
            "a function-headed sentence denotes an object, not a formula, and \
             must not satisfy `(domain FooFn 1 Formula)`"
        );
    }
}
