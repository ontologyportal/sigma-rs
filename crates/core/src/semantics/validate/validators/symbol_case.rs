//! SUMO casing conventions: W011 function-case, W012 predicate-case,
//! W031 term-case.

use thiserror::Error;

use crate::semantics::consts::ROOT_SYMBOL;
use crate::semantics::errors::{semantic_error, BoxedError};
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::{SymbolPos, SymbolValidator};
use crate::SymbolId;

#[derive(Debug, Clone, Error)]
#[error("function '{sym}' should start with an uppercase letter and end in \"Fn\"")]
pub struct FunctionCase {
    pub sym: String,
}
semantic_error!(FunctionCase, "W011", "function-case", Warning);

#[derive(Debug, Clone, Error)]
#[error("predicate '{sym}' should start with a lowercase letter")]
pub struct PredicateCase {
    pub sym: String,
}
semantic_error!(PredicateCase, "W012", "predicate-case", Warning);

#[derive(Debug, Clone, Error)]
#[error("term '{sym}' should start with an uppercase letter")]
pub struct TermCase {
    pub sym: String,
}
semantic_error!(TermCase, "W031", "term-case", Warning);

/// Casing conventions over every symbol, in head or argument position alike.
///
/// The three rules partition on what the symbol is declared to be, so at most
/// one fires per symbol: a function starts uppercase and ends in `Fn`, a
/// predicate starts lowercase, and every other term starts uppercase. A
/// relation that is neither a function nor a predicate falls in the last group,
/// so `predicate-case` never fires for it.
///
/// The uppercase rule additionally requires a derivation to `Entity`. Without
/// one, `is_predicate` is false because nothing is *known* about the symbol,
/// not because it has been established as a non-predicate -- a lowercase
/// predicate declared in a not-yet-loaded constituent looks identical to a
/// mis-cased term. Demanding a capital there would report a second finding
/// that is purely an artifact of the missing derivation the E001 already
/// names. The function and predicate rules rest on a positive declaration and
/// need no such guard.
pub(crate) struct SymbolCase;

impl SymbolCase {
    const TAG: &'static str = "symbol-case";
}

impl SymbolValidator for SymbolCase {
    type Error = BoxedError;

    fn check(&self, cx: &Cx<'_>, sym: SymbolId, _pos: SymbolPos) -> Vec<BoxedError> {
        if !cx.claim_symbol(Self::TAG, sym) {
            return Vec::new();
        }
        let name = cx.sym_name(sym);
        let Some(first) = name.chars().next() else {
            return Vec::new();
        };
        let starts_upper = first.is_ascii_uppercase();

        if cx.is_function(sym) {
            if !starts_upper || !name.ends_with("Fn") {
                return vec![Box::new(FunctionCase { sym: name })];
            }
        } else if cx.is_predicate(sym) {
            if starts_upper {
                return vec![Box::new(PredicateCase { sym: name })];
            }
        } else if !starts_upper && cx.has_ancestor_by_name(sym, &ROOT_SYMBOL.name()) {
            return vec![Box::new(TermCase { sym: name })];
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::validate::test_support::{
        codes_in, kif_layer, root_by_head, root_by_head_with,
    };

    const BASE: &str = "
        (subclass Abstract Entity)
        (subclass Relation Abstract)
        (subclass Predicate Relation)
        (subclass BinaryPredicate Predicate)
        (subclass BinaryRelation Relation)
        (subclass Function Relation)
        (subclass UnaryFunction Function)
        (subclass Class Abstract)
        (instance instance BinaryPredicate)
        (instance subclass BinaryPredicate)
        (subclass Human Entity)
    ";

    fn codes_for(kif: &str, head: &str) -> Vec<&'static str> {
        let layer = kif_layer(&format!("{BASE}\n{kif}"));
        let sid = root_by_head(&layer, head);
        codes_in(&layer, sid)
    }

    #[test]
    fn w011_flags_function_not_ending_in_fn() {
        let codes = codes_for(
            "(instance AbsoluteValue UnaryFunction)\n(AbsoluteValue N)",
            "AbsoluteValue",
        );
        assert!(codes.contains(&"W011"), "got {codes:?}");
    }

    #[test]
    fn w011_flags_lowercase_function() {
        let codes = codes_for(
            "(instance absoluteValueFn UnaryFunction)\n(absoluteValueFn N)",
            "absoluteValueFn",
        );
        assert!(codes.contains(&"W011"), "got {codes:?}");
    }

    #[test]
    fn w011_not_flagged_for_conventional_function() {
        let codes = codes_for(
            "(instance AbsoluteValueFn UnaryFunction)\n(AbsoluteValueFn N)",
            "AbsoluteValueFn",
        );
        assert!(!codes.contains(&"W011"), "got {codes:?}");
    }

    #[test]
    fn w012_flags_uppercase_predicate_in_head_position() {
        let codes = codes_for("(instance Likes BinaryPredicate)\n(Likes A B)", "Likes");
        assert!(codes.contains(&"W012"), "got {codes:?}");
    }

    #[test]
    fn w012_flags_uppercase_predicate_in_argument_position() {
        let layer = kif_layer(&format!("{BASE}\n(instance Foo BinaryPredicate)"));
        let sid = root_by_head_with(&layer, "instance", "Foo");
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"W012"),
            "`(instance Foo BinaryPredicate)` must flag `Foo` as an uppercase \
             predicate even though it is an argument, not the head; got {codes:?}"
        );
    }

    #[test]
    fn w012_not_flagged_for_lowercase_predicate() {
        let codes = codes_for("(instance likes BinaryPredicate)\n(likes A B)", "likes");
        assert!(!codes.contains(&"W012"), "got {codes:?}");
    }

    #[test]
    fn w012_not_flagged_for_function() {
        let codes = codes_for(
            "(instance AbsoluteValueFn UnaryFunction)\n(AbsoluteValueFn N)",
            "AbsoluteValueFn",
        );
        assert!(!codes.contains(&"W012"), "got {codes:?}");
    }

    #[test]
    fn w012_not_flagged_for_relation_that_is_neither_predicate_nor_function() {
        let layer = kif_layer(&format!("{BASE}\n(instance Adjacent BinaryRelation)"));
        let sid = root_by_head_with(&layer, "instance", "Adjacent");
        let codes = codes_in(&layer, sid);
        assert!(
            !codes.contains(&"W012"),
            "predicate-case must not fire for a plain BinaryRelation; got {codes:?}"
        );
    }

    #[test]
    fn w031_flags_lowercase_class() {
        let layer = kif_layer(&format!("{BASE}\n(subclass lowercaseThing Entity)"));
        let sid = root_by_head_with(&layer, "subclass", "lowercaseThing");
        let codes = codes_in(&layer, sid);
        assert!(codes.contains(&"W031"), "got {codes:?}");
    }

    #[test]
    fn w031_flags_lowercase_non_relation_instance() {
        let layer = kif_layer(&format!("{BASE}\n(instance adam Human)"));
        let sid = root_by_head_with(&layer, "instance", "adam");
        let codes = codes_in(&layer, sid);
        assert!(codes.contains(&"W031"), "got {codes:?}");
    }

    #[test]
    fn w031_flags_lowercase_relation_that_is_neither_predicate_nor_function() {
        let layer = kif_layer(&format!("{BASE}\n(instance adjacent BinaryRelation)"));
        let sid = root_by_head_with(&layer, "instance", "adjacent");
        let codes = codes_in(&layer, sid);
        assert!(
            codes.contains(&"W031"),
            "a lowercase BinaryRelation is not a predicate, so the uppercase \
             rule applies; got {codes:?}"
        );
    }

    #[test]
    fn w031_not_flagged_for_uppercase_term() {
        let layer = kif_layer(&format!("{BASE}\n(instance Adam Human)"));
        let sid = root_by_head_with(&layer, "instance", "Adam");
        let codes = codes_in(&layer, sid);
        assert!(!codes.contains(&"W031"), "got {codes:?}");
    }

    #[test]
    fn w031_not_flagged_for_lowercase_predicate() {
        let codes = codes_for("(instance likes BinaryPredicate)\n(likes A B)", "likes");
        assert!(
            !codes.contains(&"W031"),
            "predicates are the exception to the uppercase rule; got {codes:?}"
        );
    }

    #[test]
    fn one_finding_per_symbol_per_formula() {
        let layer = kif_layer(&format!(
            "{BASE}
            (subclass Human Entity)
            (instance adam Human)
            (=> (instance adam Human) (instance adam Human))"
        ));
        let sid =
            crate::semantics::validate::test_support::root_by_op(&layer, crate::OpKind::Implies);
        let errs = layer
            .validator_scoped(crate::semantics::types::Scope::Base)
            .validate_sentence_collect(sid);
        let n = errs.iter().filter(|e| e.code() == "W031").count();
        assert_eq!(
            n, 1,
            "expected one W031 for `adam` across both occurrences, got {n}"
        );
    }
}

#[cfg(test)]
mod interaction {
    use crate::semantics::types::Scope;
    use crate::semantics::validate::test_support::{kif_layer, root_by_head_with};

    const BASE: &str = "
        (subclass Abstract Entity)
        (subclass Relation Abstract)
        (subclass Predicate Relation)
        (subclass BinaryPredicate Predicate)
        (instance instance BinaryPredicate)
    ";

    fn codes_for(kif: &str, sym: &str) -> Vec<&'static str> {
        let layer = kif_layer(&format!("{BASE}\n{kif}"));
        let sid = root_by_head_with(&layer, "instance", sym);
        layer
            .validator_scoped(Scope::Base)
            .validate_sentence_collect(sid)
            .iter()
            .map(|e| e.code())
            .collect()
    }

    /// A finding from one symbol validator never stops the others running.
    #[test]
    fn e001_does_not_short_circuit_later_symbol_validators() {
        let codes = codes_for("(instance dis_connected Whatever)", "dis_connected");
        assert!(codes.contains(&"E001"), "got {codes:?}");
        assert!(
            codes.contains(&"W032"),
            "camel-case is lexical and applies regardless of declaration; got {codes:?}"
        );
    }

    #[test]
    fn w031_suppressed_without_a_derivation_to_entity() {
        let codes = codes_for("(instance disconnected Whatever)", "disconnected");
        assert!(codes.contains(&"E001"), "got {codes:?}");
        assert!(
            !codes.contains(&"W031"),
            "`is_predicate` is unknowable for a symbol with no derivation, so \
             the uppercase rule must not fire; got {codes:?}"
        );
    }

    #[test]
    fn w031_still_fires_for_a_connected_lowercase_term() {
        let codes = codes_for("(subclass Human Entity)\n(instance adam Human)", "adam");
        assert!(!codes.contains(&"E001"), "got {codes:?}");
        assert!(codes.contains(&"W031"), "got {codes:?}");
    }
}
