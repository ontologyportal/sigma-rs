//! TPTP serialisation — pure-Rust walkers over [`Term`] and [`Formula`].
//!
//! The output shape matches Vampire's accepted TPTP dialect for FOF
//! (`fof(...)`) and TFF (`tff(...)`). Sub-formula parenthesisation is
//! minimal: a parent only parenthesises a child whose top-level operator
//! binds more loosely than its own.

use std::fmt::Write as _;

use super::formula::Formula;
use super::symbol::Interp;
use super::term::Term;
use crate::parse::tptp::syntax;

/// Operator precedence used to decide when a child formula needs to be
/// parenthesised.  Lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    Iff = 0,
    Imp = 1,
    Or  = 2,
    And = 3,
    Not = 4,
    Quant = 5,
    Atom = 6,
}

fn top_prec(f: &Formula) -> Prec {
    match f {
        Formula::Iff(..) => Prec::Iff,
        Formula::Imp(..) => Prec::Imp,
        Formula::Or(..)  => Prec::Or,
        Formula::And(..) => Prec::And,
        Formula::Not(_)  => Prec::Not,
        Formula::Forall(..)      | Formula::ForallTyped(..)
        | Formula::Exists(..)    | Formula::ExistsTyped(..) => Prec::Quant,
        Formula::Atom { .. }     | Formula::Eq(..)
        | Formula::EqTyped { .. }
        | Formula::True          | Formula::False          => Prec::Atom,
    }
}

fn emit_sub(out: &mut String, f: &Formula, parent: Prec) {
    let child = top_prec(f);
    let is_binary = |p: Prec| matches!(p, Prec::Iff | Prec::Imp | Prec::Or | Prec::And);
    // Strict TPTP has NO precedence between binary connectives: mixing them
    // (`A & B => C`, `A & B | C`) or chaining the non-associative ones
    // (`A => B => C`) requires explicit parentheses.  Only a chain of the SAME
    // associative connective (`A & B & C`, `A | B | C`) may stay flat.
    let needs_parens = if is_binary(parent) && is_binary(child) {
        !(child == parent && matches!(parent, Prec::And | Prec::Or))
    } else {
        child < parent
    };
    if needs_parens {
        out.push('(');
        emit_formula(out, f);
        out.push(')');
    } else {
        emit_formula(out, f);
    }
}

/// Serialises a [`Term`] to TPTP syntax.
#[cfg(feature = "ask")]
pub(crate) fn term_to_tptp(t: &Term) -> String {
    let mut s = String::new();
    emit_term(&mut s, t);
    s
}

fn emit_term(out: &mut String, t: &Term) {
    match t {
        Term::Var(v) => {
            let _ = write!(out, "X{}", v.index());
        }
        Term::Apply(func, args) => {
            let name = interp_name(func.interp()).unwrap_or_else(|| func.name().to_string());
            out.push_str(&name);
            if !args.is_empty() {
                out.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    emit_term(out, a);
                }
                out.push(')');
            }
        }
        Term::Int(v) | Term::Real(v) | Term::Rational(v) => out.push_str(v),
    }
}

/// Serialises a [`Formula`] to TPTP syntax (no enclosing parentheses).
pub(crate) fn formula_to_tptp(f: &Formula) -> String {
    let mut s = String::new();
    emit_formula(&mut s, f);
    s
}

fn emit_formula(out: &mut String, f: &Formula) {
    match f {
        Formula::True  => out.push_str(syntax::TRUE),
        Formula::False => out.push_str(syntax::FALSE),

        Formula::Atom { pred, args } => {
            let name = interp_name(pred.interp()).unwrap_or_else(|| pred.name().to_string());
            out.push_str(&name);
            if !args.is_empty() {
                out.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    emit_term(out, a);
                }
                out.push(')');
            }
        }

        Formula::Eq(lhs, rhs) => {
            emit_term(out, lhs);
            out.push_str(syntax::EQ);
            emit_term(out, rhs);
        }

        // TFF equality serialises the same way as FOF equality; the sort
        // is carried for the prover's internal type-checking, not for the
        // surface syntax.
        Formula::EqTyped { lhs, rhs, sort: _ } => {
            emit_term(out, lhs);
            out.push_str(syntax::EQ);
            emit_term(out, rhs);
        }

        Formula::Not(inner) => {
            out.push_str(syntax::NOT);
            emit_sub(out, inner, Prec::Not);
        }

        Formula::And(parts) => emit_nary(out, parts, syntax::AND, Prec::And),
        Formula::Or(parts)  => emit_nary(out, parts, syntax::OR, Prec::Or),

        Formula::Imp(a, b) => {
            emit_sub(out, a, Prec::Imp);
            out.push_str(syntax::IMPLIES);
            emit_sub(out, b, Prec::Imp);
        }

        Formula::Iff(a, b) => {
            emit_sub(out, a, Prec::Iff);
            out.push_str(syntax::IFF);
            emit_sub(out, b, Prec::Iff);
        }

        Formula::Forall(v, body) => {
            let _ = write!(out, "{}[X{}] : ", syntax::FORALL, v.index());
            emit_sub(out, body, Prec::Quant);
        }
        Formula::ForallTyped(v, sort, body) => {
            let _ = write!(out, "{}[X{}: {}] : ", syntax::FORALL, v.index(), sort.tptp_name());
            emit_sub(out, body, Prec::Quant);
        }
        Formula::Exists(v, body) => {
            let _ = write!(out, "{}[X{}] : ", syntax::EXISTS, v.index());
            emit_sub(out, body, Prec::Quant);
        }
        Formula::ExistsTyped(v, sort, body) => {
            let _ = write!(out, "{}[X{}: {}] : ", syntax::EXISTS, v.index(), sort.tptp_name());
            emit_sub(out, body, Prec::Quant);
        }
    }
}

fn emit_nary(out: &mut String, parts: &[Formula], sep: &str, self_prec: Prec) {
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push_str(sep);
        }
        emit_sub(out, p, self_prec);
    }
}

fn interp_name(i: Option<Interp>) -> Option<String> {
    i.map(|i| i.tptp_name().to_string())
}

impl Term {
    /// Serialises this term to TPTP syntax.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::{Function, Term};
    ///
    /// let f = Function::new("f", 2);
    /// let t = Term::apply(f, vec![Term::var(0), Term::int("1")]);
    /// assert_eq!(t.to_tptp(), "f(X0,1)");
    /// ```
    #[cfg(feature = "ask")]
    pub fn to_tptp(&self) -> String {
        term_to_tptp(self)
    }
}

impl Formula {
    /// Serialises this formula to TPTP syntax (no enclosing parentheses).
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::{Formula, Predicate, Term};
    ///
    /// let p = Predicate::new("P", 1);
    /// let f = Formula::atom(p, vec![Term::var(0)]);
    /// assert_eq!(f.to_tptp(), "P(X0)");
    /// ```
    pub fn to_tptp(&self) -> String {
        formula_to_tptp(self)
    }
}
