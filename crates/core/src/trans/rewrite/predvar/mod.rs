//! Stage 4 - Predicate-variable instantiation (per-problem, lazy).
//!
//! [`detect_predvar_schemas`] finds schema templates among the CAF-normalized
//! implications (`detect`); `TranslationLayer::instantiate_predvars` binds
//! them per problem and emits concrete first-order rules (`instantiate`,
//! with definitional term reduction in `reduce`).

use crate::parse::ast::OpKind;
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, SentenceId, SymbolId};

mod detect;
#[cfg(feature = "external-prover")]
mod instantiate;
#[cfg(feature = "external-prover")]
mod reduce;
#[cfg(all(test, feature = "external-prover"))]
mod tests;

pub(crate) use detect::detect_predvar_schemas;

/// A taxonomy guard constraining a schema's predicate variables.
// Constructed by `detect_predvar_schemas` (live without `ask`); the fields are
// only READ by the `ask`-gated instantiation path below.
#[cfg_attr(not(feature = "external-prover"), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) enum PvGuard {
    /// `(instance ?V class)` - `?V` must be an instance of `class`.
    Instance { var: SymbolId, class: SymbolId },
    /// `(subrelation ?V1 ?V2)` - `?V1`,`?V2` bound by a subrelation fact.
    Subrelation { v1: SymbolId, v2: SymbolId },
    /// A `domain*` or `range*` signature guard, bound from semantic caches.
    Signature {
        kind: SignatureKind,
        head: SymbolId,
        vars: Vec<SymbolId>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignatureKind {
    Domain,
    DomainSubclass,
    Range,
    RangeSubclass,
}

/// Concrete value for one guard-bound variable.
#[cfg_attr(not(feature = "external-prover"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Bound {
    Sym(SymbolId),
    Pos(u32),
}

/// A schema template instantiated lazily per query from taxonomy or signature
/// guards. Predicate variables and function variables are replaced with
/// concrete symbols, and signature positions with numeric literals.
///
/// Examples (post-CAF, post-row-var-expansion):
///   - transitivity: `(=> (instance ?R TransitiveRelation) (forall ... (=> (and (?R ...)(?R ...)) (?R ...))))`
///   - subrelation propagation: `(=> (and (subrelation ?R1 ?R2)(instance ?R1 Predicate)(instance ?R2 Predicate)(?R1 ...)) (?R2 ...))`
// Constructed by `detect_predvar_schemas` (live without `ask`); most fields are
// only READ by the `ask`-gated instantiation path below.
#[cfg_attr(not(feature = "external-prover"), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) struct PredVarSchema {
    /// The (CAF-normalized) implication carrying the schema.
    pub schema_sid: SentenceId,
    /// The predicate variables (head-position), in a deterministic order.
    pub pred_vars: Vec<SymbolId>,
    /// All variables bound by guards, in a deterministic order.
    pub bound_vars: Vec<SymbolId>,
    /// The taxonomy guards constraining `pred_vars`.
    guards: Vec<PvGuard>,
    /// The arity at which the predicate variables are *applied* in the schema
    /// body (row-var expansion stamps each variant at a fixed arity).  A
    /// concrete relation is only eligible for this variant when its declared
    /// arity matches; variadic / unknown arities accept any variant.
    pub body_arity: Option<usize>,
}

/// `true` iff the atom `elements` has exactly the variables `vars` as its
/// arguments, in order.
fn atom_is_variables(elements: &[Element], vars: &[SymbolId]) -> bool {
    elements.len() == vars.len() + 1
        && elements[1..].iter().zip(vars).all(
            |(element, expected)| matches!(element, Element::Variable { id, .. } if id == expected),
        )
}

/// `true` iff `sid` is a **bare positive relation assertion** - a (possibly
/// universally quantified) atom headed by a concrete relation symbol, with no
/// surrounding propositional structure (no `=>`, `not`, `and`, `or`).
///
/// Used to reject unsound instantiations: when a schema's antecedent is *only*
/// taxonomy guards, instantiation reduces the body to its consequent, and a
/// bare atom asserted for *all* arguments would hold the relation universally.
pub(crate) fn is_bare_positive_assertion(syntactic: &SyntacticLayer, sid: SentenceId) -> bool {
    let Some(s) = syntactic.sentence(sid) else {
        return false;
    };
    match s.elements.first() {
        // Strip universal quantifiers - recurse into the body (last Sub).
        Some(Element::Op(OpKind::ForAll)) => match s.elements.last() {
            Some(Element::Sub(body)) => is_bare_positive_assertion(syntactic, *body),
            _ => false,
        },
        // A bare atom headed by a concrete relation symbol.
        Some(Element::Symbol(_)) => true,
        _ => false,
    }
}
