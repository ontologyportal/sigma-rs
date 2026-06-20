//! Pure-Rust formula IR.

use super::symbol::{Predicate, Sort};
use super::term::{Term, VarId};

/// A first-order (or TFF) formula.
///
/// TFF typed variants ([`EqTyped`](Formula::EqTyped),
/// [`ForallTyped`](Formula::ForallTyped),
/// [`ExistsTyped`](Formula::ExistsTyped)) carry an explicit [`Sort`]; the
/// untyped variants leave the sort implicit (`$i`). When a problem is
/// serialised to TPTP, the enclosing problem's logic mode determines
/// whether the output is `fof(...)` or `tff(...)`.
///
/// Smart constructors ([`Formula::and`], [`Formula::or`], [`Formula::not`])
/// apply light normalisation — see their individual docs. The logical
/// meaning is preserved; only the tree shape differs. Use the enum
/// variants directly if you want an un-normalised formula.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum Formula {
    /// `p(t1, ..., tn)` — a predicate application.
    Atom { pred: Predicate, args: Vec<Term> },

    /// `t1 = t2` — untyped equality.
    Eq(Term, Term),

    /// `t1 = t2` — TFF equality carrying an explicit sort annotation.
    EqTyped { lhs: Term, rhs: Term, sort: Sort },

    /// `F1 & F2 & ... & Fn`.
    And(Vec<Formula>),

    /// `F1 | F2 | ... | Fn`.
    Or(Vec<Formula>),

    /// `~F`.
    Not(Box<Formula>),

    /// `F1 => F2`.
    Imp(Box<Formula>, Box<Formula>),

    /// `F1 <=> F2`.
    Iff(Box<Formula>, Box<Formula>),

    /// `![X] : F` — universal quantification over an untyped variable.
    Forall(VarId, Box<Formula>),

    /// `![X: sort] : F` — universal quantification over a TFF-typed
    /// variable.
    ForallTyped(VarId, Sort, Box<Formula>),

    /// `?[X] : F` — existential quantification over an untyped variable.
    Exists(VarId, Box<Formula>),

    /// `?[X: sort] : F` — existential quantification over a TFF-typed
    /// variable.
    ExistsTyped(VarId, Sort, Box<Formula>),

    /// `$true`.
    True,

    /// `$false`.
    False,
}

impl Formula {
    /// Builds a predicate application `pred(args...)`.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `pred.arity() != args.len()`.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::{Formula, Function, Predicate, Term};
    ///
    /// let mortal   = Predicate::new("mortal", 1);
    /// let socrates = Term::constant(Function::new("socrates", 0));
    /// let f = Formula::atom(mortal, vec![socrates]);
    /// assert_eq!(f.to_tptp(), "mortal(socrates)");
    /// ```
    pub fn atom(pred: Predicate, args: Vec<Term>) -> Self {
        debug_assert_eq!(
            pred.arity() as usize, args.len(),
            "Formula::atom arity mismatch for {}", pred.name(),
        );
        Self::Atom { pred, args }
    }

    /// Builds an untyped equality `lhs = rhs`.
    pub fn eq(lhs: Term, rhs: Term) -> Self {
        Self::Eq(lhs, rhs)
    }

    /// Builds a TFF-typed equality `lhs = rhs` with the given sort
    /// annotation.
    pub fn eq_typed(lhs: Term, rhs: Term, sort: Sort) -> Self {
        Self::EqTyped { lhs, rhs, sort }
    }

    /// Builds a conjunction with light normalisation:
    ///
    /// - Empty input → [`Formula::True`] (the neutral element).
    /// - Single input → the lone operand.
    /// - Nested `And` children are flattened into the outer vector.
    /// - `True` operands are dropped.
    /// - A `False` operand short-circuits to [`Formula::False`].
    ///
    /// If you need the un-normalised tree, construct `Formula::And(...)`
    /// directly via the enum variant.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::{Formula, Predicate};
    ///
    /// let p = Predicate::new("P", 0);
    /// let q = Predicate::new("Q", 0);
    /// let r = Predicate::new("R", 0);
    ///
    /// // Flattening.
    /// let pq   = Formula::and(vec![Formula::atom(p.clone(), vec![]),
    ///                              Formula::atom(q.clone(), vec![])]);
    /// let pqr  = Formula::and(vec![pq, Formula::atom(r, vec![])]);
    /// assert_eq!(pqr.to_tptp(), "P & Q & R");
    ///
    /// // $true is dropped; $false absorbs.
    /// let tp = Formula::and(vec![Formula::True, Formula::atom(p, vec![])]);
    /// assert_eq!(tp.to_tptp(), "P");
    /// assert_eq!(
    ///     Formula::and(vec![Formula::atom(q, vec![]), Formula::False]).to_tptp(),
    ///     "$false",
    /// );
    /// ```
    pub fn and(fs: Vec<Formula>) -> Self {
        let mut flat: Vec<Formula> = Vec::with_capacity(fs.len());
        for f in fs {
            match f {
                Formula::True       => continue,
                Formula::False      => return Formula::False,
                Formula::And(inner) => flat.extend(inner),
                other               => flat.push(other),
            }
        }
        match flat.len() {
            0 => Formula::True,
            1 => flat.into_iter().next().unwrap(),
            _ => Formula::And(flat),
        }
    }

    /// Builds a disjunction with light normalisation (dual of
    /// [`Formula::and`]):
    ///
    /// - Empty input → [`Formula::False`].
    /// - Single input → the lone operand.
    /// - Nested `Or` children are flattened.
    /// - `False` operands are dropped; a `True` operand short-circuits to
    ///   [`Formula::True`].
    pub fn or(fs: Vec<Formula>) -> Self {
        let mut flat: Vec<Formula> = Vec::with_capacity(fs.len());
        for f in fs {
            match f {
                Formula::False     => continue,
                Formula::True      => return Formula::True,
                Formula::Or(inner) => flat.extend(inner),
                other              => flat.push(other),
            }
        }
        match flat.len() {
            0 => Formula::False,
            1 => flat.into_iter().next().unwrap(),
            _ => Formula::Or(flat),
        }
    }

    /// Builds a negation with light normalisation:
    ///
    /// - `Not(Not(f))` collapses to `f` (double-negation elimination).
    /// - `Not(True)` collapses to `False` and vice versa.
    /// - All other inputs wrap normally.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::{Formula, Predicate};
    ///
    /// let p = Predicate::new("P", 0);
    /// let double_neg = Formula::not(Formula::not(Formula::atom(p, vec![])));
    /// assert_eq!(double_neg.to_tptp(), "P");
    /// ```
    pub fn not(inner: Formula) -> Self {
        match inner {
            Formula::Not(f) => *f,
            Formula::True   => Formula::False,
            Formula::False  => Formula::True,
            other           => Formula::Not(Box::new(other)),
        }
    }

    /// Builds an implication `lhs => rhs`.
    pub fn imp(lhs: Formula, rhs: Formula) -> Self {
        Self::Imp(Box::new(lhs), Box::new(rhs))
    }

    /// Builds a biconditional `lhs <=> rhs`.
    pub fn iff(lhs: Formula, rhs: Formula) -> Self {
        Self::Iff(Box::new(lhs), Box::new(rhs))
    }

    /// Builds `![var] : body` — untyped universal quantification.
    pub fn forall(var: VarId, body: Formula) -> Self {
        Self::Forall(var, Box::new(body))
    }

    /// Builds `![var: sort] : body` — typed universal quantification.
    pub fn forall_typed(var: VarId, sort: Sort, body: Formula) -> Self {
        Self::ForallTyped(var, sort, Box::new(body))
    }

    /// Builds `?[var] : body` — untyped existential quantification.
    pub fn exists(var: VarId, body: Formula) -> Self {
        Self::Exists(var, Box::new(body))
    }

    /// Builds `?[var: sort] : body` — typed existential quantification.
    pub fn exists_typed(var: VarId, sort: Sort, body: Formula) -> Self {
        Self::ExistsTyped(var, sort, Box::new(body))
    }
}
