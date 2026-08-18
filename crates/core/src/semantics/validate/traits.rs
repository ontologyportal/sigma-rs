//! The validator traits.
//!
//! A validator is one struct in one file under `validators/`, implementing one
//! of these traits, owning the finding type it raises and the tests that cover
//! it. Which trait it implements is decided by *when the driver should call
//! it* -- the traversal shape -- not by what the check is about:
//!
//! | Trait                | Called on                                    |
//! |----------------------|----------------------------------------------|
//! | [`FormulaValidator`] | once per root, over the whole formula tree    |
//! | [`SentenceValidator`]| every relation-headed sentence               |
//! | [`OperatorValidator`]| every operator sentence whose kind it claims  |
//! | [`SymbolValidator`]  | every symbol in head or argument position     |
//!
//! Each trait has an object-safe `*Dyn` companion with a blanket impl, because
//! an associated type makes the authored trait unusable behind `dyn`. Authors
//! implement only the trait; registry membership comes free.

use crate::semantics::errors::SemanticError;
use crate::{OpKind, SentenceId, SymbolId};

use super::cx::Cx;

/// Runs once per root sentence, over the whole formula tree.
pub(crate) trait FormulaValidator {
    type Error: SemanticError + 'static;
    fn check(&self, cx: &Cx<'_>, root: SentenceId) -> Vec<Self::Error>;
}

/// Runs on every relation-headed (non-operator) sentence.
pub(crate) trait SentenceValidator {
    type Error: SemanticError + 'static;
    fn check(&self, cx: &Cx<'_>, sid: SentenceId) -> Vec<Self::Error>;
}

/// Runs on every operator sentence whose [`OpKind`] appears in `OPS`.
pub(crate) trait OperatorValidator {
    type Error: SemanticError + 'static;
    /// Which operators this validator claims. Empty means every operator.
    const OPS: &'static [OpKind];
    fn check(&self, cx: &Cx<'_>, sid: SentenceId, op: &OpKind) -> Vec<Self::Error>;
}

/// Where a symbol sits in its sentence. Head and argument positions are not
/// interchangeable: relation-signature checks (arity, domain, range, casing)
/// apply only to a head, while taxonomy checks apply to both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SymbolPos {
    Head,
    Argument,
}

/// Runs on every symbol appearing in head or argument position.
pub(crate) trait SymbolValidator {
    type Error: SemanticError + 'static;
    fn check(&self, cx: &Cx<'_>, sym: SymbolId, pos: SymbolPos) -> Vec<Self::Error>;
}

// -- Object-safe companions ------------------------------------------------

pub(crate) trait FormulaValidatorDyn: Send + Sync {
    fn run(&self, cx: &Cx<'_>, root: SentenceId, out: &mut Vec<Box<dyn SemanticError>>);
}

pub(crate) trait SentenceValidatorDyn: Send + Sync {
    fn run(&self, cx: &Cx<'_>, sid: SentenceId, out: &mut Vec<Box<dyn SemanticError>>);
}

pub(crate) trait OperatorValidatorDyn: Send + Sync {
    fn claims(&self, op: &OpKind) -> bool;
    fn run(&self, cx: &Cx<'_>, sid: SentenceId, op: &OpKind, out: &mut Vec<Box<dyn SemanticError>>);
}

pub(crate) trait SymbolValidatorDyn: Send + Sync {
    fn run(
        &self,
        cx: &Cx<'_>,
        sym: SymbolId,
        pos: SymbolPos,
        out: &mut Vec<Box<dyn SemanticError>>,
    );
}

impl<T: FormulaValidator + Send + Sync> FormulaValidatorDyn for T {
    fn run(&self, cx: &Cx<'_>, root: SentenceId, out: &mut Vec<Box<dyn SemanticError>>) {
        out.extend(
            self.check(cx, root)
                .into_iter()
                .map(|e| Box::new(e) as Box<dyn SemanticError>),
        );
    }
}

impl<T: SentenceValidator + Send + Sync> SentenceValidatorDyn for T {
    fn run(&self, cx: &Cx<'_>, sid: SentenceId, out: &mut Vec<Box<dyn SemanticError>>) {
        out.extend(
            self.check(cx, sid)
                .into_iter()
                .map(|e| Box::new(e) as Box<dyn SemanticError>),
        );
    }
}

impl<T: OperatorValidator + Send + Sync> OperatorValidatorDyn for T {
    fn claims(&self, op: &OpKind) -> bool {
        T::OPS.is_empty() || T::OPS.contains(op)
    }
    fn run(
        &self,
        cx: &Cx<'_>,
        sid: SentenceId,
        op: &OpKind,
        out: &mut Vec<Box<dyn SemanticError>>,
    ) {
        out.extend(
            self.check(cx, sid, op)
                .into_iter()
                .map(|e| Box::new(e) as Box<dyn SemanticError>),
        );
    }
}

impl<T: SymbolValidator + Send + Sync> SymbolValidatorDyn for T {
    fn run(
        &self,
        cx: &Cx<'_>,
        sym: SymbolId,
        pos: SymbolPos,
        out: &mut Vec<Box<dyn SemanticError>>,
    ) {
        out.extend(
            self.check(cx, sym, pos)
                .into_iter()
                .map(|e| Box::new(e) as Box<dyn SemanticError>),
        );
    }
}
