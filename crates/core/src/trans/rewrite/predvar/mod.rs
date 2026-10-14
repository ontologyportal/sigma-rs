//! Stage 4 - Predicate-variable instantiation (per-problem, lazy).
//!
//! [`detect_predvar_schemas`] finds schema templates among the CAF-normalized
//! implications (`detect`); `TranslationLayer::instantiate_predvars` binds
//! them per problem and emits concrete first-order rules (`instantiate`,
//! with definitional term reduction in `reduce`).

use std::collections::{HashMap, HashSet};

use crate::parse::ast::OpKind;
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, Literal, SentenceId, SymbolId};

mod detect;
#[cfg(feature = "external-prover")]
mod instantiate;
#[cfg(feature = "external-prover")]
mod reduce;
#[cfg(all(test, feature = "external-prover"))]
mod tests;

pub(crate) use detect::detect_predvar_schemas;

/// An antecedent atom that constrains a schema's higher-order (predicate or
/// function) variables. Every guard is discharged when a binding is chosen
/// and dropped from the instantiated rule; the other antecedent atoms are
/// substituted and kept.
// Constructed by `detect_predvar_schemas` (live without `ask`); the fields are
// only READ by the `ask`-gated instantiation path below.
#[cfg_attr(not(feature = "external-prover"), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) enum PvGuard {
    /// `(instance ?V class)` - `?V` must be an instance of `class`.
    Instance { var: SymbolId, class: SymbolId },
    /// `(subrelation ?V1 ?V2)` - `?V1`,`?V2` bound by a subrelation edge.
    Subrelation { v1: SymbolId, v2: SymbolId },
    /// A `domain*` or `range*` signature guard, bound from semantic caches.
    Signature {
        kind: SignatureKind,
        vars: Vec<SymbolId>,
    },
    /// Any other flat atom over a higher-order variable, e.g.
    /// `(disjointRelation ?R1 ?R2)`: bound by the ground facts with that head
    /// visible in the query scope.
    Fact { head: SymbolId, args: Vec<GuardArg> },
}

/// One argument of a [`PvGuard::Fact`] atom.
#[cfg_attr(not(feature = "external-prover"), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) enum GuardArg {
    /// Bound from the matching fact's symbol in this position.
    Var(SymbolId),
    /// The matching fact must hold exactly this symbol.
    Sym(SymbolId),
    /// The matching fact must hold exactly this literal.
    Lit(Literal),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Bound {
    Sym(SymbolId),
    Pos(u32),
}

/// A schema template instantiated lazily per query: an implication whose
/// higher-order variables are all bound by [`PvGuard`]s. Each binding
/// replaces predicate and function variables with concrete symbols (and
/// signature positions with numeric literals).
///
/// Examples (post-CAF, post-row-var-expansion):
///   - transitivity: `(=> (instance ?R TransitiveRelation) (forall ... (=> (and (?R ...)(?R ...)) (?R ...))))`
///   - subrelation propagation: `(=> (and (subrelation ?R1 ?R2)(instance ?R1 Predicate)(instance ?R2 Predicate)(?R1 ...)) (?R2 ...))`
///   - fact-guarded: `(=> (and (disjointRelation ?R1 ?R2)(?R1 ...)) (not (?R2 ...)))`
// Constructed by `detect_predvar_schemas` (live without `ask`); most fields are
// only READ by the `ask`-gated instantiation path below.
#[cfg_attr(not(feature = "external-prover"), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) struct PredVarSchema {
    /// The (CAF-normalized) implication carrying the schema.
    pub schema_sid: SentenceId,
    /// Variables applied in head position (`(?REL ...)`, `(holds ?REL ...)`),
    /// sorted.
    pub pred_vars: Vec<SymbolId>,
    /// Variables applied as functions (`(AssignmentFn ?F ...)`), sorted.
    pub function_vars: Vec<SymbolId>,
    /// All variables bound by guards, sorted.
    pub bound_vars: Vec<SymbolId>,
    /// The guards binding `pred_vars` and `function_vars`.
    guards: Vec<PvGuard>,
    /// The antecedent conjuncts that are guards, dropped on instantiation.
    guard_sids: HashSet<SentenceId>,
    /// `(antecedent, consequent)` of a `(forall (..) (=> A C))` consequent
    /// whose antecedent holds a guard. `(=> G (forall (..) (=> A C)))` is
    /// equivalent to `(=> (and G A) C)` when the quantified variables do not
    /// occur in `G`, so instantiation emits that flat form.
    inner: Option<(SentenceId, SentenceId)>,
    /// The argument count at which each higher-order variable is applied
    /// (row-var expansion stamps each variant at one fixed arity).  A concrete
    /// symbol is only eligible when its declared arity matches; variadic /
    /// unknown arities accept any variant.
    pub call_arity: HashMap<SymbolId, usize>,
}

impl PredVarSchema {
    /// Predicate and function variables, the ones every binding must cover.
    #[cfg_attr(not(feature = "external-prover"), allow(dead_code))]
    fn higher_order_vars(&self) -> impl Iterator<Item = SymbolId> + '_ {
        self.pred_vars
            .iter()
            .chain(self.function_vars.iter())
            .copied()
    }
}

/// `true` iff `sid` is a **bare positive relation assertion** - a (possibly
/// universally quantified) atom headed by a concrete relation symbol, with no
/// surrounding propositional structure (no `=>`, `not`, `and`, `or`).
///
/// Used to reject unsound instantiations: when a schema's antecedent is *only*
/// guards, instantiation reduces the body to its consequent, and a
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
