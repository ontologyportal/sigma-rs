//! The baseline rules of the controlled language, one file per rule.
//!
//! To cover a new construct: add a file here with a struct implementing
//! [`SentenceRule`](super::grammar::SentenceRule) or
//! [`NpRule`](super::grammar::NpRule), register it in
//! [`Grammar::standard`](super::grammar::Grammar::standard) (order matters:
//! first match wins), and add its sentence shapes to the tests in `mod.rs`.

pub mod attitude;
pub mod copula;
pub mod event;
pub mod have;
pub mod modal;
pub mod np;

use super::grammar::{best, Cx, Head, Logic, Np, Scope, Step};
use super::ir::{Cond, Drs, Term};
use super::refuse::{Construct, Refusal, Refused};
use super::token::Det;

/// A clause rule entered with the subject already parsed.
type Clause = dyn Fn(&mut Cx, Np, usize) -> Step<Logic>;

/// The clause a modal or an attitude verb scopes over, from `at` with
/// `subject` already in hand: a copula with "be", "have", or an event
/// with its object and prepositional phrases, parsed under a bound scope
/// with the subject's number and genericity left to the outer clause.
pub(super) fn complement(cx: &mut Cx, subject: Np, at: usize) -> Step<Logic> {
    let saved = (cx.relaxed_agreement, cx.generic_subject, cx.under_modal);
    cx.relaxed_agreement = true;
    cx.generic_subject = true;
    cx.under_modal = true;
    let mut attempts = Vec::new();
    let clauses: [&Clause; 3] = [
        &|cx, s, i| copula::Copula.with_subject(cx, false, s, i),
        &|cx, s, i| have::Have.with_subject(cx, None, s, i),
        &|cx, s, i| event::Event.with_subject(cx, None, s, i),
    ];
    for clause in clauses {
        let subject = subject.clone();
        let step = cx.attempt(|cx| clause(cx, subject, at));
        let done = matches!(&step, Step::Hit(_, n) if cx.is_end(*n));
        attempts.push(step);
        if done {
            break;
        }
    }
    (cx.relaxed_agreement, cx.generic_subject, cx.under_modal) = saved;
    best(cx, attempts)
}

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
    if subject.possessor.is_some() {
        return Force::Particular;
    }
    match (&subject.head, subject.det) {
        (Head::Name(_) | Head::NewName(_) | Head::Pronoun { .. } | Head::Gap, _) => {
            Force::Particular
        }
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
    // In a conditional an indefinite or bare plural is a bound variable of
    // the implication, not a generic statement of its own.
    let force = match force {
        Force::Universal if cx.conditional && subject.det != Some(Det::Universal) => {
            Force::Particular
        }
        f => f,
    };
    match force {
        Force::Particular => {
            let scope = if cx.question
                || negated
                || cx.generic_you.is_some()
                || cx.under_modal
                || cx.conditional
            {
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
            let v = cx.discourse.var("X");
            let x = Term::Var(v.clone());
            let mut ante = subject.restrict(cx, &x)?;
            ante.bind(&v);
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
            let v = cx.discourse.var("X");
            let x = Term::Var(v.clone());
            let mut inner = subject.restrict(cx, &x)?;
            inner.bind(&v);
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
