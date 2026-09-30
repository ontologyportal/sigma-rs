//! The baseline rules of the controlled language, one file per rule.
//!
//! To cover a new construct: add a file here with a struct implementing
//! [`SentenceRule`](super::grammar::SentenceRule) or
//! [`NpRule`](super::grammar::NpRule), register it in
//! [`Grammar::standard`](super::grammar::Grammar::standard) (order matters:
//! first match wins), and add its sentence shapes to the tests in `mod.rs`.

pub mod copula;
pub mod event;
pub mod np;

use super::grammar::{Cx, Head, Np, Scope};
use super::ir::{Cond, Drs, Term};
use super::refuse::{Construct, Refusal, Refused};
use super::token::Det;

/// How a subject noun phrase scopes over the predicate.
pub(super) enum Force {
    /// A referent or an existential: the predicate is stated of it.
    Particular,
    /// `every`/`all`/`each`, a generic indefinite, or a bare plural: the
    /// predicate is stated of every instance of the class.
    Universal,
    /// `no`: the predicate holds of no instance of the class.
    None,
}

pub(super) fn force(subject: &Np, indefinite_is_generic: bool) -> Force {
    match (&subject.head, subject.det) {
        (Head::Name(_) | Head::NewName(_), _) => Force::Particular,
        (_, Some(Det::Definite | Det::Some)) => Force::Particular,
        (_, Some(Det::Universal)) => Force::Universal,
        (_, Some(Det::Negative)) => Force::None,
        (_, Some(Det::Indefinite)) if indefinite_is_generic => Force::Universal,
        (_, Some(Det::Indefinite)) => Force::Particular,
        (Head::Noun { plural, .. }, None) if *plural || indefinite_is_generic => Force::Universal,
        (_, None) => Force::Particular,
        (_, Some(Det::Demonstrative)) => Force::Particular,
    }
}

/// Realize the subject and wrap `build`'s predicate logic in the subject's
/// quantifier and polarity.  `build` receives the subject term and the
/// scope its own referents must use.
pub(super) fn frame<F>(
    cx: &mut Cx,
    subject: &Np,
    negated: bool,
    force: Force,
    build: F,
) -> Result<Drs, Refused>
where
    F: FnOnce(&mut Cx, &Term, Scope) -> Result<Drs, Refused>,
{
    match force {
        Force::Particular => {
            let scope = if cx.question || negated {
                Scope::Bound
            } else {
                Scope::Assert
            };
            let real = subject.realize(cx, scope)?;
            let body = build(cx, &real.term, scope)?;
            let mut drs = real.drs;
            if negated {
                drs.conds.push(Cond::Not(body));
            } else {
                drs.extend(body);
            }
            Ok(drs)
        }
        Force::Universal => {
            let class = subject.class(cx)?;
            let v = cx.discourse.var("X");
            let x = Term::Var(v.clone());
            let mut ante = Drs::default();
            ante.bind(&v);
            ante.atom("instance", vec![x.clone(), Term::Const(class.term)]);
            let body = build(cx, &x, Scope::Bound)?;
            let cons = if negated {
                Drs {
                    vars: Vec::new(),
                    conds: vec![Cond::Not(body)],
                }
            } else {
                body
            };
            Ok(Drs {
                vars: Vec::new(),
                conds: vec![Cond::Implies(ante, cons)],
            })
        }
        Force::None => {
            let class = subject.class(cx)?;
            let v = cx.discourse.var("X");
            let x = Term::Var(v.clone());
            let mut inner = Drs::default();
            inner.bind(&v);
            inner.atom("instance", vec![x.clone(), Term::Const(class.term)]);
            let body = build(cx, &x, Scope::Bound)?;
            if negated {
                inner.conds.push(Cond::Not(body));
            } else {
                inner.extend(body);
            }
            Ok(Drs {
                vars: Vec::new(),
                conds: vec![Cond::Not(inner)],
            })
        }
    }
}

/// Refuse a bare-plural subject with an action verb ("Dogs chase cats"):
/// generic statements about events are ambiguous between habits and laws.
pub(super) fn generic_subject(cx: &Cx, subject: &Np) -> Option<Refused> {
    matches!(
        (&subject.head, subject.det),
        (Head::Noun { plural: true, .. }, None)
    )
    .then(|| {
        cx.refused(
            Refusal::Unsupported {
                construct: Construct::Generic,
                at: cx.text(subject.start, subject.end),
            },
            subject.start,
        )
    })
}
