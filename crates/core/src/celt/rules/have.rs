//! `Have`: `NP has/have [not] NP`, `NP does/do not have NP` and the
//! question `Does/Do NP have NP?`.
//!
//! "Have" is read by the type of its object (CELT has no single relation
//! for it): a body part or other anatomical structure is a `part` of the
//! subject ("Birds have feathers"), an artifact is a `part` of an artifact
//! subject ("A car has an engine"), an attribute noun is an `attribute`
//! ("Rex has a red color"), and any other object is `possesses` for an
//! agent subject ("Bob has a car").  An agent as object ("a dog has an
//! owner") names a role, not a possession, and is refused.  As with the
//! copula, an indefinite or bare-plural subject is generic.  "Had", and
//! "have" before a participle (the perfect), are refused.

use super::super::grammar::{
    best, possession_relation, Cx, Head, Kind, Logic, Np, Scope, SentenceRule, Step,
};
use super::super::ir::{Drs, Term};
use super::super::refuse::{Construct, Refusal, Refused};
use super::super::token::{Closed, Det, Number, Tense};
use super::{force, frame};

pub struct Have;

impl SentenceRule for Have {
    fn name(&self) -> &'static str {
        "have"
    }

    fn pattern(&self) -> &'static str {
        "NP has/have [not] NP"
    }

    fn apply(&self, cx: &mut Cx, at: usize) -> Step<Logic> {
        let mut i = at;
        let mut aux_at: Option<usize> = None;
        if cx.question {
            if let Some(Closed::Aux(..)) = cx.tok(i).and_then(|t| t.closed()) {
                aux_at = Some(i);
                i += 1;
            }
        }
        let subjects = match cx.nps(i) {
            Ok(v) if v.is_empty() => return cx.miss(i, "a subject noun phrase"),
            Ok(v) => v,
            Err(r) => return Step::Refuse(r),
        };
        let mut attempts = Vec::new();
        for (subject, next) in subjects {
            let step = cx.attempt(|cx| self.with_subject(cx, aux_at, subject, next));
            let done = matches!(&step, Step::Hit(_, n) if cx.is_end(*n));
            attempts.push(step);
            if done {
                break;
            }
        }
        best(cx, attempts)
    }
}

impl Have {
    pub(super) fn with_subject(
        &self,
        cx: &mut Cx,
        mut aux_at: Option<usize>,
        subject: Np,
        mut i: usize,
    ) -> Step<Logic> {
        if let Some(Closed::Aux(..)) = cx.tok(i).and_then(|t| t.closed()) {
            if aux_at.is_some() {
                return cx.miss(i, "has/have");
            }
            aux_at = Some(i);
            i += 1;
        }
        let mut negated = false;
        if cx.tok(i).is_some_and(|t| t.is(Closed::Not)) {
            if aux_at.is_none() {
                return cx.miss(i, "does/do before not");
            }
            negated = true;
            i += 1;
        }
        let Some(have) = cx.tok(i).filter(|t| t.is(Closed::Have)) else {
            return cx.miss(i, "has/have");
        };
        let have_at = i;
        i += 1;
        let plural_form = have.lower == "have"
            && !matches!(
                aux_at.and_then(|a| cx.tok(a)).and_then(|t| t.closed()),
                Some(Closed::Aux(_, Number::Singular))
            );
        let subject = if plural_form {
            subject.pluralized(cx.lex)
        } else {
            subject
        };
        match (aux_at, have.lower.as_str()) {
            (Some(a), "have") => {
                if let Some(Closed::Aux(Tense::Present, number)) =
                    cx.tok(a).and_then(|t| t.closed())
                {
                    if !subject.agrees(cx, number) {
                        return cx.miss(a, "do/does agreeing with the subject in number");
                    }
                }
            }
            (Some(_), _) => return cx.miss(have_at, "have after do/does"),
            (None, "has") if subject.agrees(cx, Number::Singular) => {}
            (None, "have") if subject.agrees(cx, Number::Plural) => {}
            (None, "had") => {
                return cx.refuse(
                    Refusal::Unsupported {
                        construct: Construct::PastTense,
                        at: have.text.clone(),
                    },
                    have_at,
                )
            }
            (None, _) => return cx.miss(have_at, "has/have agreeing with the subject in number"),
        }
        // A participle after "have" is the perfect tense, not an object.
        if let Some(t) = cx.tok(i).filter(|t| t.is_open()) {
            if !cx.lex.is_nounish(&t.lower)
                && !cx.lex.is_adjectival(&t.lower)
                && cx.lex.is_verbal(&t.lower)
            {
                return cx.refuse(
                    Refusal::Unsupported {
                        construct: Construct::Perfect,
                        at: t.text.clone(),
                    },
                    i,
                );
            }
        }
        let objects = match cx.nps(i) {
            Ok(v) if v.is_empty() => return cx.miss(i, "an object noun phrase after has/have"),
            Ok(v) => v,
            Err(r) => return Step::Refuse(r),
        };
        let mut attempts = Vec::new();
        for (object, next) in objects {
            let subject = subject.clone();
            let step = cx.attempt(|cx| self.with_object(cx, subject, negated, object, next));
            let done = matches!(&step, Step::Hit(_, n) if cx.is_end(*n));
            attempts.push(step);
            if done {
                break;
            }
        }
        best(cx, attempts)
    }

    fn with_object(
        &self,
        cx: &mut Cx,
        subject: Np,
        negated: bool,
        mut object: Np,
        i: usize,
    ) -> Step<Logic> {
        let mut negated = negated;
        match object.det {
            Some(Det::Negative) => {
                negated = true;
                object.det = None;
            }
            Some(Det::Universal | Det::Demonstrative) => {
                return cx.refuse(
                    Refusal::Unsupported {
                        construct: Construct::QuantifiedPredicate,
                        at: cx.text(object.start, object.end),
                    },
                    object.start,
                )
            }
            _ => {}
        }
        let kind = if cx.question {
            Kind::Query
        } else {
            Kind::Assert
        };
        let subject_class: Option<String> = match &subject.head {
            Head::Noun { word, .. } => cx.lex.noun_class(word).ok().map(|c| c.term),
            Head::Name(n) => Some(n.clone()),
            Head::NewName(_) => Some("Human".into()),
            Head::Pronoun { .. } => cx.generic_you.as_ref().map(|(_, c)| c.clone()),
            Head::Gap => cx.gap.as_ref().and_then(|(_, c)| c.clone()),
        };
        let subject_force = force(&subject, true);
        let result = frame(cx, &subject, negated, subject_force, |cx, subj, scope| {
            Self::body(cx, subj, subject_class.as_deref(), &object, scope)
        });
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

    fn body(
        cx: &mut Cx,
        subj: &Term,
        subject_class: Option<&str>,
        object: &Np,
        scope: Scope,
    ) -> Result<Drs, Refused> {
        let unsupported = |cx: &Cx| {
            cx.refused(
                Refusal::Unsupported {
                    construct: Construct::Have,
                    at: cx.text(object.start, object.end),
                },
                object.start,
            )
        };
        // An attribute noun: "has a red color" -> (attribute X Red).
        if let Head::Noun { word, .. } = &object.head {
            if let Ok((choice, true)) = cx.lex.predicate_noun(word) {
                cx.record(&choice);
                let mut d = Drs::default();
                d.atom("attribute", vec![subj.clone(), Term::Const(choice.term)]);
                return Ok(d);
            }
        }
        let obj = object.realize(cx, scope)?;
        let obj_class = match (&obj.class, &obj.term) {
            (Some(c), _) => c.clone(),
            (None, Term::Const(n)) => n.clone(),
            _ => return Err(unsupported(cx)),
        };
        let Some((relation, part_of_subject)) = possession_relation(cx, subject_class, &obj_class)
        else {
            return Err(unsupported(cx));
        };
        let mut d = obj.drs;
        if part_of_subject {
            d.atom(relation, vec![obj.term, subj.clone()]);
        } else {
            d.atom(relation, vec![subj.clone(), obj.term]);
        }
        Ok(d)
    }
}
