//! Pure-Rust CNF clause IR.
//!
//! A [`Clause`] is a disjunction of [`Literal`]s. A [`Literal`] carries a
//! [`LitKind`] (either an ordinary predicate application or an equality
//! between two terms) plus a sign. Callers build clauses directly.
//!
//! # Examples
//!
//! ```
//! use sigmakee_rs_core::trans::ir::{Clause, Function, Literal, LitKind, Predicate, Term};
//!
//! let p = Predicate::new("P", 1);
//! let a = Function::new("a", 0);
//!
//! // Clause: P(a) | ~P(X0)
//! let c = Clause::new(vec![
//!     Literal::atom(true, p.clone(), vec![Term::constant(a)]),
//!     Literal::atom(false, p, vec![Term::var(0)]),
//! ]);
//! assert_eq!(c.len(), 2);
//! assert!(!c.is_empty());
//! ```

use super::symbol::Predicate;
use super::term::Term;

/// The kind of a CNF literal.
///
/// `Eq` carries its two sides as [`Term`]s directly; it is *not* modelled
/// as a binary predicate call, because equality has its own normalisation
/// and canonicalisation rules (for instance `lhs = rhs` is symmetric and
/// is treated as unordered in dedup hashing).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum LitKind {
    /// An ordinary predicate application `p(t1, ..., tn)`.
    Atom {
        pred: Predicate,
        args: Vec<Term>,
    },
    /// An equality `lhs = rhs`.  The sign on the containing [`Literal`]
    /// distinguishes `=` (`positive = true`) from `!=` (`positive = false`).
    Eq(Term, Term),
}

/// A signed CNF literal.
///
/// `positive == true` means the literal appears as `p(...)` or `l = r`;
/// `positive == false` means `~p(...)` or `l != r`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Literal {
    pub positive: bool,
    pub kind:     LitKind,
}

impl Literal {
    /// Constructs a predicate-application literal.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `pred.arity() != args.len()`.
    #[allow(dead_code)]
    pub fn atom(positive: bool, pred: Predicate, args: Vec<Term>) -> Self {
        debug_assert_eq!(
            pred.arity() as usize,
            args.len(),
            "Literal::atom arity mismatch for {}",
            pred.name(),
        );
        Self {
            positive,
            kind: LitKind::Atom { pred, args },
        }
    }

    /// Constructs an equality literal `lhs = rhs` (if `positive`) or
    /// `lhs != rhs` (if not).
    #[allow(dead_code)] // IR builder (cnf clausifier + tests)
    pub fn eq(positive: bool, lhs: Term, rhs: Term) -> Self {
        Self {
            positive,
            kind: LitKind::Eq(lhs, rhs),
        }
    }

    /// `true` if this is an equality literal (positive or negative).
    #[allow(dead_code)]
    pub fn is_equality(&self) -> bool {
        matches!(self.kind, LitKind::Eq(..))
    }

    /// The predicate symbol, for a non-equality literal.  Returns `None`
    /// for `LitKind::Eq`.
    #[allow(dead_code)]
    pub fn predicate(&self) -> Option<&Predicate> {
        match &self.kind {
            LitKind::Atom { pred, .. } => Some(pred),
            LitKind::Eq(..) => None,
        }
    }
}

/// A CNF clause — a (possibly empty) disjunction of [`Literal`]s.
///
/// An empty clause (`literals.is_empty()`) represents the empty disjunction
/// `⊥`, the canonical refutation witness produced by a complete saturation
/// run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Clause {
    pub literals: Vec<Literal>,
}

impl Clause {
    /// Constructs a clause from a list of literals.
    #[allow(dead_code)] // IR builder (cnf clausifier + tests)
    pub fn new(literals: Vec<Literal>) -> Self {
        Self { literals }
    }

    /// The empty clause — the canonical `⊥`.
    #[allow(dead_code)] // IR builder (cnf clausifier + tests)
    pub fn empty() -> Self {
        Self { literals: Vec::new() }
    }

    /// Number of literals in this clause.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.literals.len()
    }

    /// `true` if this is the empty clause (i.e. `⊥`).
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.literals.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::symbol::Function;

    #[test]
    fn literal_atom_constructors() {
        let p  = Predicate::new("P", 2);
        let x  = Term::var(0);
        let a  = Term::constant(Function::new("a", 0));
        let l  = Literal::atom(true, p.clone(), vec![x.clone(), a.clone()]);
        assert!(l.positive);
        assert!(!l.is_equality());
        assert_eq!(l.predicate().unwrap().name(), "P");

        let neg = Literal::atom(false, p, vec![x, a]);
        assert!(!neg.positive);
    }

    #[test]
    fn literal_eq_constructor() {
        let x = Term::var(0);
        let y = Term::var(1);
        let l = Literal::eq(true, x.clone(), y.clone());
        assert!(l.is_equality());
        assert!(l.predicate().is_none());

        let neq = Literal::eq(false, x, y);
        assert!(neq.is_equality());
        assert!(!neq.positive);
    }

    #[test]
    fn clause_empty_is_bot() {
        let c = Clause::empty();
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn clause_non_empty() {
        let p = Predicate::new("P", 0);
        let q = Predicate::new("Q", 0);
        let c = Clause::new(vec![
            Literal::atom(true,  p, vec![]),
            Literal::atom(false, q, vec![]),
        ]);
        assert_eq!(c.len(), 2);
        assert!(!c.is_empty());
    }
}
