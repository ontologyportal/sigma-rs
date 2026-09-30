//! `Copula`: `NP is/are [not] (a NOUN | ADJ | PREP NP)` and the inverted
//! question `Is/Are NP [not] ... ?`.
//!
//! The reading depends on the subject and the predicate (CELT paper 4.3,
//! 4.5, 4.6):
//!
//! | subject                | predicate         | logic                                  |
//! |------------------------|-------------------|----------------------------------------|
//! | referent (Rex, the X)  | `a dog`           | `(instance Rex Dog)`                   |
//! | referent               | `a pianist` (role)| `(attribute Rex Musician)`             |
//! | referent               | `loud`            | `(attribute Rex Loud)`                 |
//! | referent               | `in the garden`   | `(located Rex Garden-1)`               |
//! | class (a/every/plural) | `a mammal`        | `(subclass Dog Mammal)`                |
//! | class                  | `loud` / role / PP| `(=> (instance ?X Dog) ...)`           |
//! | `no dog`               | any               | `(not (exists (?X) ...))`              |
//! | `some dogs`            | any               | `(exists (?X) ...)`                    |

use super::super::grammar::{
    attach_pp, best, Cx, Head, Kind, Logic, Np, Scope, SentenceRule, Step,
};
use super::super::ir::{Drs, Term};
use super::super::refuse::{Construct, Refusal, Refused};
use super::super::token::{Closed, Det};
use super::{force, frame, Force};

pub struct Copula;

#[derive(Clone)]
enum Pred {
    Nominal(Np),
    Adjective(String, usize),
    Pp(super::super::grammar::Pp),
}

impl SentenceRule for Copula {
    fn name(&self) -> &'static str {
        "copula"
    }

    fn pattern(&self) -> &'static str {
        "NP is/are [not] (a NOUN | ADJECTIVE | PREPOSITION NP)"
    }

    fn apply(&self, cx: &mut Cx, at: usize) -> Step<Logic> {
        let inverted = cx.question
            && cx
                .tok(at)
                .is_some_and(|t| matches!(t.closed(), Some(Closed::Copula(..))));
        let i = if inverted { at + 1 } else { at };
        let subjects = match cx.nps(i) {
            Ok(v) if v.is_empty() => return cx.miss(i, "a subject noun phrase"),
            Ok(v) => v,
            Err(r) => return Step::Refuse(r),
        };
        let mut attempts = Vec::new();
        for (subject, next) in subjects {
            let step = cx.attempt(|cx| self.with_subject(cx, inverted, subject, next));
            let done = matches!(&step, Step::Hit(_, n) if cx.is_end(*n));
            attempts.push(step);
            if done {
                break;
            }
        }
        best(cx, attempts)
    }
}

impl Copula {
    fn with_subject(&self, cx: &mut Cx, inverted: bool, subject: Np, mut i: usize) -> Step<Logic> {
        if !inverted {
            match cx.tok(i).and_then(|t| t.closed()) {
                Some(Closed::Copula(..)) => i += 1,
                _ => return cx.miss(i, "is/are"),
            }
        }
        let negated = if cx.tok(i).is_some_and(|t| t.is(Closed::Not)) {
            i += 1;
            true
        } else {
            false
        };
        let preds = match self.predicates(cx, i) {
            Ok(v) if v.is_empty() => return Step::Miss,
            Ok(v) => v,
            Err(r) => return Step::Refuse(r),
        };
        let mut attempts = Vec::new();
        for (pred, next) in preds {
            let subject = subject.clone();
            let step = cx.attempt(|cx| self.with_predicate(cx, subject, negated, pred, next));
            let done = matches!(&step, Step::Hit(_, n) if cx.is_end(*n));
            attempts.push(step);
            if done {
                break;
            }
        }
        best(cx, attempts)
    }

    fn with_predicate(
        &self,
        cx: &mut Cx,
        subject: Np,
        negated: bool,
        pred: Pred,
        i: usize,
    ) -> Step<Logic> {
        let kind = if cx.question {
            Kind::Query
        } else {
            Kind::Assert
        };
        let subject_force = force(&subject, true);
        // Class subject + class predicate is a taxonomic statement.
        if let (Force::Universal, Pred::Nominal(pn), false) = (&subject_force, &pred, negated) {
            if pn.det.is_none_or(|d| d == Det::Indefinite) {
                match self.taxonomic(cx, &subject, pn) {
                    Ok(Some(drs)) => {
                        return Step::Hit(
                            Logic {
                                kind,
                                drs,
                                choices: Vec::new(),
                                rule: self.name(),
                                declared: Drs::default(),
                            },
                            i,
                        )
                    }
                    Ok(None) => {}
                    Err(r) => return Step::Refuse(r),
                }
            }
        }
        // A Process subject ("the party is on Monday") takes event roles.
        let subject_is_event = match &subject.head {
            Head::Noun { word, .. } => cx
                .lex
                .noun_class(word)
                .is_ok_and(|c| cx.lex.has_ancestor(&c.term, "Process")),
            Head::Name(n) => cx.lex.has_ancestor(n, "Process"),
            Head::NewName(_) => false,
        };
        let subject_class = match &subject.head {
            Head::Noun { word, .. } => cx.lex.noun_class(word).ok().map(|c| c.term),
            Head::Name(n) => Some(n.clone()),
            Head::NewName(_) => Some("Human".into()),
        };
        let result = frame(
            cx,
            &subject,
            negated,
            subject_force,
            |cx, subj, scope| match &pred {
                Pred::Nominal(pn) => {
                    Self::nominal(cx, subj, pn, scope, subject_class.as_deref(), subject.start)
                }
                Pred::Adjective(w, at) => {
                    let a = cx.lex.adjective(w).map_err(|r| cx.refused(r, *at))?;
                    cx.record(&a);
                    if let Term::Const(c) = subj {
                        if *c == a.term {
                            return Err(
                                cx.refused(Refusal::TrivialFact { term: a.term }, subject.start)
                            );
                        }
                    }
                    let mut d = Drs::default();
                    d.atom("attribute", vec![subj.clone(), Term::Const(a.term)]);
                    Ok(d)
                }
                Pred::Pp(pp) => attach_pp(cx, subj, pp, subject_is_event, scope),
            },
        );
        match result {
            Ok(drs) => Step::Hit(
                Logic {
                    kind,
                    drs,
                    choices: Vec::new(),
                    rule: self.name(),
                    declared: Drs::default(),
                },
                i,
            ),
            Err(r) => Step::Refuse(r),
        }
    }

    /// Predicate readings at `at`: a prepositional phrase, a lone
    /// adjective, or the noun-phrase readings (longest first).
    fn predicates(&self, cx: &mut Cx, at: usize) -> Result<Vec<(Pred, usize)>, Refused> {
        if cx.tok(at).is_some_and(|t| t.is(Closed::Preposition)) {
            return match cx.pp(at) {
                Step::Hit(pp, next) => Ok(vec![(Pred::Pp(pp), next)]),
                Step::Miss => Ok(Vec::new()),
                Step::Refuse(r) => Err(r),
            };
        }
        let mut out = Vec::new();
        // A lone adjectival word is an adjective, never a bare noun: "small"
        // must not slide to the noun sense when the adjective is refused.
        if let Some(t) = cx.tok(at).filter(|t| t.is_open()) {
            let alone = !cx.tok(at + 1).is_some_and(|n| n.is_open());
            if alone && cx.lex.is_adjectival(&t.lower) {
                out.push((Pred::Adjective(t.lower.clone(), at), at + 1));
                return Ok(out);
            }
        }
        for (np, next) in cx.nps(at)? {
            match np.det {
                Some(Det::Definite) => {
                    return Err(cx.refused(
                        Refusal::Unsupported {
                            construct: Construct::Identity,
                            at: cx.text(np.start, np.end),
                        },
                        np.start,
                    ))
                }
                Some(Det::Universal | Det::Negative | Det::Some | Det::Demonstrative) => {
                    return Err(cx.refused(
                        Refusal::Unsupported {
                            construct: Construct::QuantifiedPredicate,
                            at: cx.text(np.start, np.end),
                        },
                        np.start,
                    ))
                }
                _ => {}
            }
            if matches!(np.head, Head::Name(_) | Head::NewName(_)) {
                return Err(cx.refused(
                    Refusal::Unsupported {
                        construct: Construct::Identity,
                        at: cx.text(np.start, np.end),
                    },
                    np.start,
                ));
            }
            out.push((Pred::Nominal(np), next));
        }
        if out.is_empty() {
            cx.miss::<()>(at, "a predicate: a noun, an adjective or a preposition");
        }
        Ok(out)
    }

    /// `(subclass S C)` for a class subject and a class predicate; `None`
    /// when the predicate noun is an attribute (then it is a rule instead).
    fn taxonomic(
        &self,
        cx: &mut Cx,
        subject: &Np,
        pred: &Np,
    ) -> Result<Option<Drs>, super::super::refuse::Refused> {
        let Head::Noun { word, .. } = &pred.head else {
            return Ok(None);
        };
        let (choice, is_attr) = cx
            .lex
            .predicate_noun(word)
            .map_err(|r| cx.refused(r, pred.end - 1))?;
        if is_attr {
            return Ok(None);
        }
        if let Some(m) = pred.mods.first() {
            return Err(cx.refused(Refusal::ModifiedClass { word: m.clone() }, pred.start));
        }
        let sub = subject.class(cx)?;
        cx.record(&choice);
        if sub.term == choice.term {
            return Err(cx.refused(Refusal::TrivialFact { term: sub.term }, subject.start));
        }
        if let Some(category) = cx.lex.cross_category(&sub.term, &choice.term) {
            return Err(cx.refused(
                Refusal::CrossCategory {
                    sub: sub.term,
                    sup: choice.term,
                    category,
                },
                subject.start,
            ));
        }
        let mut d = Drs::default();
        d.atom(
            "subclass",
            vec![Term::Const(sub.term), Term::Const(choice.term)],
        );
        Ok(Some(d))
    }

    /// Predicate nominal about a particular subject: class membership or an
    /// attribute (role) plus any adjectives on the predicate.
    fn nominal(
        cx: &mut Cx,
        subj: &Term,
        pred: &Np,
        _scope: Scope,
        subject_class: Option<&str>,
        subject_at: usize,
    ) -> Result<Drs, super::super::refuse::Refused> {
        let Head::Noun { word, .. } = &pred.head else {
            unreachable!("predicates() rejects name predicates")
        };
        let (choice, is_attr) = cx
            .lex
            .predicate_noun(word)
            .map_err(|r| cx.refused(r, pred.end - 1))?;
        cx.record(&choice);
        if let (Some(sc), false) = (subject_class, is_attr) {
            if let Some(category) = cx.lex.cross_category(sc, &choice.term) {
                return Err(cx.refused(
                    Refusal::CrossCategory {
                        sub: sc.to_string(),
                        sup: choice.term.clone(),
                        category,
                    },
                    subject_at,
                ));
            }
        }
        let mut d = Drs::default();
        let rel = if is_attr { "attribute" } else { "instance" };
        d.atom(rel, vec![subj.clone(), Term::Const(choice.term)]);
        for m in &pred.mods {
            let a = cx.lex.adjective(m).map_err(|r| cx.refused(r, pred.start))?;
            cx.record(&a);
            d.atom("attribute", vec![subj.clone(), Term::Const(a.term)]);
        }
        Ok(d)
    }
}
