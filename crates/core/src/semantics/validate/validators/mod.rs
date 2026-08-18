//! The validators, and the registries the driver walks.
//!
//! Adding a validator is: one new file here implementing one trait from
//! `super::traits`, plus one entry in the matching registry below. Nothing
//! else in the subsystem changes -- not the driver, not `errors.rs`.
//!
//! Which registry a validator joins is decided by its traversal shape, not by
//! what it checks; see `super::traits` for the table.

use super::traits::{
    FormulaValidatorDyn, OperatorValidatorDyn, SentenceValidatorDyn, SymbolValidatorDyn,
};

pub(crate) mod arity;
pub(crate) mod camel_case;
pub(crate) mod common;
pub(crate) mod domain;
pub(crate) mod entity_ancestor;
pub(crate) mod free_var_in_consequent;
pub(crate) mod head_is_relation;
pub(crate) mod iff_shape;
pub(crate) mod implies_shape;
pub(crate) mod non_logical_arg;
pub(crate) mod quantifier_vacuous;
pub(crate) mod relation_metadata;
pub(crate) mod single_arity;
pub(crate) mod single_use_variable;
pub(crate) mod symbol_case;

/// Run once per root sentence, over the whole formula tree.
pub(super) const FORMULA: &[&dyn FormulaValidatorDyn] = &[
    &single_use_variable::SingleUseVariableCheck,
    &free_var_in_consequent::FreeVarInConsequentCheck,
];

/// Run on every relation-headed sentence.
pub(super) const SENTENCE: &[&dyn SentenceValidatorDyn] = &[
    &head_is_relation::HeadIsRelation,
    &arity::RelationArity,
    &domain::Domain,
];

/// Run on every operator sentence whose kind the validator claims.
pub(super) const OPERATOR: &[&dyn OperatorValidatorDyn] = &[
    &arity::OperatorArity,
    &single_arity::SingleArityCheck,
    &non_logical_arg::NonLogicalArgCheck,
    &iff_shape::IffShape,
    &implies_shape::ImpliesShape,
    &quantifier_vacuous::QuantifierVacuousCheck,
];

/// Run on every symbol in head or argument position.
pub(super) const SYMBOL: &[&dyn SymbolValidatorDyn] = &[
    &entity_ancestor::EntityAncestor,
    &relation_metadata::RelationMetadata,
    &symbol_case::SymbolCase,
    &camel_case::CamelCase,
];
