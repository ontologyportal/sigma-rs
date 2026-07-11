//! Pure-Rust term IR.

use super::symbol::Function;

/// A variable identifier.
///
/// Variables are referenced by index (like De Bruijn indices, but chosen
/// explicitly by the builder rather than counted off). The index alone is
/// the identity; stringification in TPTP follows Vampire's `X<idx>`
/// convention.
///
/// # Examples
///
/// ```
/// use crate::trans::ir::VarId;
///
/// let v = VarId(3);
/// assert_eq!(v.index(),      3);
/// assert_eq!(v.tptp_name(),  "X3");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct VarId(pub u32);

impl VarId {
    /// The raw variable index.
    pub fn index(self) -> u32 { self.0 }

    /// The TPTP identifier for this variable (`X0`, `X1`, ...).
    #[cfg(feature = "ask")]
    pub fn tptp_name(self) -> String { format!("X{}", self.0) }
}

/// A first-order term.
///
/// `Apply` holds its [`Function`] inline, keeping the term tree independent
/// of an external symbol table. Literal variants (`Int`, `Real`, `Rational`)
/// store the value textually to preserve exact precision.
///
/// # Examples
///
/// ```
/// use crate::trans::ir::{Function, Term, VarId};
///
/// // A variable term.
/// let x = Term::var(0);
///
/// // A constant (0-ary function application).
/// let zero = Term::constant(Function::new("zero", 0));
///
/// // A function application: succ(x).
/// let succ = Function::new("succ", 1);
/// let succ_x = Term::apply(succ, vec![x.clone()]);
///
/// // A numeric literal.
/// let two = Term::int("2");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum Term {
    /// A variable reference, e.g. `X0`.
    Var(VarId),

    /// A function application `f(t1, ..., tn)`. Arity-0 applications are
    /// the canonical representation for constants.
    Apply(Function, Vec<Term>),

    /// An integer literal, stored textually for exact precision.
    Int(String),

    /// A real literal, stored textually.
    Real(String),

    /// A rational literal, stored textually (e.g. `"1/3"`).
    Rational(String),
}

impl Term {
    /// Constructs a variable term from a `u32` index.
    ///
    /// Equivalent to `Term::Var(VarId(idx))`, kept as a convenience for
    /// callers that carry raw indices.
    pub fn var(idx: u32) -> Self {
        Self::Var(VarId(idx))
    }

    /// Constructs a constant (arity-0 function application).
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `func.arity() != 0`.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::{Function, Term};
    ///
    /// let socrates = Term::constant(Function::new("socrates", 0));
    /// ```
    pub fn constant(func: Function) -> Self {
        debug_assert_eq!(func.arity(), 0, "Term::constant requires arity 0");
        Self::Apply(func, Vec::new())
    }

    /// Constructs `f(args...)`.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `func.arity() != args.len()`.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::{Function, Term};
    ///
    /// let plus = Function::new("plus", 2);
    /// let t = Term::apply(plus, vec![Term::var(0), Term::int("1")]);
    /// ```
    pub fn apply(func: Function, args: Vec<Term>) -> Self {
        debug_assert_eq!(
            func.arity() as usize, args.len(),
            "Term::apply arity mismatch for {}", func.name(),
        );
        Self::Apply(func, args)
    }

    /// Constructs an integer literal.
    pub fn int(value: impl Into<String>) -> Self {
        Self::Int(value.into())
    }

    /// Constructs a real literal.
    pub fn real(value: impl Into<String>) -> Self {
        Self::Real(value.into())
    }

    /// Constructs a rational literal.
    pub fn rational(value: impl Into<String>) -> Self {
        Self::Rational(value.into())
    }

    /// Returns an iterator over every [`VarId`] that appears anywhere in
    /// this term, in left-to-right traversal order.
    ///
    /// This is a plain occurrence iterator; duplicates are yielded at each
    /// occurrence and binding structure is not considered (binding lives at
    /// the formula level).
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::{Function, Term};
    ///
    /// let f = Function::new("f", 2);
    /// let t = Term::apply(f, vec![Term::var(0), Term::var(1)]);
    /// let vars: Vec<u32> = t.free_vars().map(|v| v.index()).collect();
    /// assert_eq!(vars, vec![0, 1]);
    /// ```
    #[cfg(feature = "ask")]
    pub fn free_vars(&self) -> impl Iterator<Item = VarId> + '_ {
        FreeVarsIter { stack: vec![self] }
    }
}

#[cfg(feature = "ask")]
struct FreeVarsIter<'a> {
    stack: Vec<&'a Term>,
}

#[cfg(feature = "ask")]
impl<'a> Iterator for FreeVarsIter<'a> {
    type Item = VarId;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(t) = self.stack.pop() {
            match t {
                Term::Var(v) => return Some(*v),
                Term::Apply(_, args) => self.stack.extend(args.iter().rev()),
                Term::Int(_) | Term::Real(_) | Term::Rational(_) => {}
            }
        }
        None
    }
}
