//! `Event`: `NP [does/did not] VERB [NP] (PREP NP)*` and the question
//! `Does/Did NP VERB [NP] (PREP NP)* ?`.
//!
//! An action verb reifies as an event (CELT paper 4.1): `(instance ?E
//! Reading) (agent ?E Mike) (patient ?E ?B)`, with prepositional phrases
//! attached through the preposition table.  A verb that grounds to a binary
//! relation is a stative (4.2): `(possesses Mike ?B)`, no event.  The
//! subject is the `agent` when the verb's `agent` domain admits it, the
//! `experiencer` for perception and psychological processes, and the
//! `patient` of an intransitive verb otherwise.  Past tense adds
//! `(before (EndFn (WhenFn ?E)) NOW)`, the speech time being
//! [`Opts::now`](super::super::grammar::Opts::now) or the KB's `Now`.

use super::super::grammar::{attach_pp, best, Cx, Kind, Logic, Np, Pp, Scope, SentenceRule, Step};
use super::super::ir::{Drs, Term};
use super::super::lookup::{Inflection, Verb};
use super::super::refuse::{Construct, Refusal, Refused};
use super::super::token::{Closed, Number, Tense};
use super::{force, frame, generic_subject};
use crate::lexicon::Pos;

pub struct Event;

impl SentenceRule for Event {
    fn name(&self) -> &'static str {
        "event"
    }

    fn pattern(&self) -> &'static str {
        "NP [does not] VERB [NP] [PREPOSITION NP]..."
    }

    fn apply(&self, cx: &mut Cx, at: usize) -> Step<Logic> {
        let mut i = at;
        let mut aux: Option<usize> = None;
        if cx.question {
            if let Some(Closed::Aux(..)) = cx.tok(i).and_then(|t| t.closed()) {
                aux = Some(i);
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
            let step = cx.attempt(|cx| self.with_subject(cx, aux, subject, next));
            let done = matches!(&step, Step::Hit(_, n) if cx.is_end(*n));
            attempts.push(step);
            if done {
                break;
            }
        }
        best(cx, attempts)
    }
}

impl Event {
    pub(super) fn with_subject(
        &self,
        cx: &mut Cx,
        mut aux_at: Option<usize>,
        subject: Np,
        mut i: usize,
    ) -> Step<Logic> {
        let mut negated = false;
        if let Some(Closed::Aux(..)) = cx.tok(i).and_then(|t| t.closed()) {
            if aux_at.is_some() {
                return cx.miss(i, "a verb");
            }
            aux_at = Some(i);
            i += 1;
        }
        let aux = aux_at.and_then(|a| match cx.tok(a).and_then(|t| t.closed()) {
            Some(Closed::Aux(tense, number)) => Some((a, tense, number)),
            _ => None,
        });
        if let Some((a, Tense::Present, number)) = aux {
            if !subject.agrees(cx, number) {
                return cx.miss(a, "do/does agreeing with the subject in number");
            }
        }
        let aux_number = aux.map(|(_, _, number)| number);
        let aux = aux.map(|(_, tense, _)| tense);
        if cx.tok(i).is_some_and(|t| t.is(Closed::Not)) {
            if aux.is_none() {
                return cx.miss(i, "does/did before not");
            }
            negated = true;
            i += 1;
        }
        let Some(vt) = cx.tok(i).filter(|t| t.is_open()) else {
            return cx.miss(i, "a verb");
        };
        let verb_at = i;
        let (verb, infl) = match cx.lex.verb(&vt.lower) {
            Ok(v) => v,
            Err(Refusal::NoRule { .. }) => return cx.miss(i, "a verb"),
            Err(r) => return cx.refuse(r, i),
        };
        i += 1;
        let plural_form =
            aux_number == Some(Number::Plural) || (aux.is_none() && infl == Inflection::Present);
        let subject = if plural_form {
            subject.pluralized(cx.lex)
        } else {
            subject
        };
        let tense = match (aux, infl) {
            (_, Inflection::Progressive) => {
                return cx.refuse(
                    Refusal::Unsupported {
                        construct: Construct::Progressive,
                        at: vt.text.clone(),
                    },
                    verb_at,
                )
            }
            (Some(t), Inflection::Present) => t,
            (Some(_), _) => return cx.miss(verb_at, "the base form of the verb after do/does"),
            (None, Inflection::Past) => Tense::Past,
            (None, Inflection::Present) if subject.agrees(cx, Number::Plural) => Tense::Present,
            (None, Inflection::ThirdSingular) if subject.agrees(cx, Number::Singular) => {
                Tense::Present
            }
            (None, _) => return cx.miss(verb_at, "a verb agreeing with the subject in number"),
        };
        // Object readings, longest first, then no object.
        let mut objects: Vec<(Option<Np>, usize)> = match cx.nps(i) {
            Ok(v) => v.into_iter().map(|(np, n)| (Some(np), n)).collect(),
            Err(r) => return Step::Refuse(r),
        };
        objects.push((None, i));
        let mut attempts = Vec::new();
        for (object, next) in objects {
            let subject = subject.clone();
            let verb = verb.clone();
            let verb_word = vt.text.clone();
            let step = cx.attempt(|cx| {
                self.with_object(
                    cx, subject, negated, verb, verb_word, verb_at, tense, object, next,
                )
            });
            let done = matches!(&step, Step::Hit(_, n) if cx.is_end(*n));
            attempts.push(step);
            if done {
                break;
            }
        }
        best(cx, attempts)
    }

    #[allow(clippy::too_many_arguments)]
    fn with_object(
        &self,
        cx: &mut Cx,
        subject: Np,
        negated: bool,
        verb: Verb,
        verb_word: String,
        verb_at: usize,
        tense: Tense,
        object: Option<Np>,
        mut i: usize,
    ) -> Step<Logic> {
        let mut pps: Vec<Pp> = Vec::new();
        while cx.tok(i).is_some_and(|t| t.is(Closed::Preposition)) {
            match cx.pp(i) {
                Step::Hit(pp, next) => {
                    i = next;
                    pps.push(pp);
                }
                Step::Miss => return Step::Miss,
                Step::Refuse(r) => return Step::Refuse(r),
            }
        }
        if let Some(t) = cx.tok(i).filter(|t| t.is_open() && !cx.is_end(i)) {
            if cx.lex.has_sense(&t.lower, Pos::Adv) {
                return cx.refuse(
                    Refusal::Unsupported {
                        construct: Construct::Adverb,
                        at: t.text.clone(),
                    },
                    i,
                );
            }
        }
        if !cx.generic_subject && !cx.conditional {
            if let Some(r) = generic_subject(cx, &subject) {
                return Step::Refuse(r);
            }
        }
        let now = if tense == Tense::Past {
            match speech_time(cx) {
                Some(t) => Some(t),
                None => {
                    return cx.refuse(
                        Refusal::Unsupported {
                            construct: Construct::PastTense,
                            at: verb_word,
                        },
                        verb_at,
                    )
                }
            }
        } else {
            None
        };
        let kind = if cx.question {
            Kind::Query
        } else {
            Kind::Assert
        };
        let subject_force = force(&subject, cx.generic_subject);
        let result = frame(
            cx,
            &subject,
            negated,
            subject_force,
            |cx, subj, scope| match &verb {
                Verb::Stative { relation, choice } => {
                    cx.record(choice);
                    Self::stative(
                        cx,
                        subj,
                        relation,
                        &verb_word,
                        verb_at,
                        object.as_ref(),
                        &pps,
                        tense,
                        scope,
                    )
                }
                Verb::Event { class, choice } => {
                    cx.record(choice);
                    Self::event(
                        cx,
                        subj,
                        &subject,
                        class,
                        object.as_ref(),
                        &pps,
                        now.as_deref(),
                        scope,
                    )
                }
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

    #[allow(clippy::too_many_arguments)]
    fn stative(
        cx: &mut Cx,
        subj: &Term,
        relation: &str,
        verb: &str,
        verb_at: usize,
        object: Option<&Np>,
        pps: &[Pp],
        tense: Tense,
        scope: Scope,
    ) -> Result<Drs, Refused> {
        let Some(object) = object else {
            return Err(cx.refused(
                Refusal::MissingObject {
                    verb: verb.into(),
                    relation: relation.into(),
                },
                verb_at,
            ));
        };
        if let Some(pp) = pps.first() {
            return Err(cx.refused(
                Refusal::Unsupported {
                    construct: Construct::StativeAdjunct,
                    at: cx.text(pp.at, pp.np.end),
                },
                pp.at,
            ));
        }
        if tense == Tense::Past {
            return Err(cx.refused(
                Refusal::Unsupported {
                    construct: Construct::StativeAdjunct,
                    at: verb.into(),
                },
                verb_at,
            ));
        }
        let obj = object.realize(cx, scope)?;
        if let (Some(dom), Some(class)) = (cx.lex.domain_class(relation, 2), &obj.class) {
            if !cx.lex.has_ancestor(class, &dom) {
                return Err(cx.refused(
                    Refusal::RoleMismatch {
                        word: cx.text(object.start, object.end),
                        term: class.clone(),
                        role: format!("object of {relation}"),
                        expected: dom,
                    },
                    object.start,
                ));
            }
        }
        let mut d = obj.drs;
        d.atom(relation, vec![subj.clone(), obj.term]);
        Ok(d)
    }

    #[allow(clippy::too_many_arguments)]
    fn event(
        cx: &mut Cx,
        subj: &Term,
        subject: &Np,
        class: &str,
        object: Option<&Np>,
        pps: &[Pp],
        now: Option<&str>,
        scope: Scope,
    ) -> Result<Drs, Refused> {
        let mut d = Drs::default();
        let e = match scope {
            Scope::Assert => {
                let lex = cx.lex;
                Term::Const(cx.discourse.skolem(class, |n| lex.defined(n)))
            }
            Scope::Bound => {
                let v = cx.discourse.var("E");
                d.bind(&v);
                Term::Var(v)
            }
        };
        d.atom("instance", vec![e.clone(), Term::Const(class.into())]);
        let role = Self::subject_role(cx, subj, subject, class, object.is_some()).map_err(
            |(term, role, expected)| {
                cx.refused(
                    Refusal::RoleMismatch {
                        word: cx.text(subject.start, subject.end),
                        term,
                        role,
                        expected,
                    },
                    subject.start,
                )
            },
        )?;
        d.atom(role, vec![e.clone(), subj.clone()]);
        if let Some(now) = now {
            d.atom(
                "before",
                vec![
                    Term::Fn(
                        "EndFn".into(),
                        vec![Term::Fn("WhenFn".into(), vec![e.clone()])],
                    ),
                    Term::Const(now.into()),
                ],
            );
        }
        if let Some(object) = object {
            let obj = object.realize(cx, scope)?;
            d.extend(obj.drs);
            d.atom("patient", vec![e.clone(), obj.term]);
        }
        for pp in pps {
            let pd = attach_pp(cx, &e, pp, true, scope)?;
            d.extend(pd);
        }
        Ok(d)
    }

    /// `agent`, `experiencer` or `patient` for the subject, or the role and
    /// its required class when none fits.
    pub(super) fn subject_role(
        cx: &Cx,
        subj: &Term,
        subject: &Np,
        class: &str,
        transitive: bool,
    ) -> Result<&'static str, (String, String, String)> {
        // The subject's own class: a known constant's taxonomy, or the noun's.
        let subject_term: String = match (&subject.head, subj) {
            (super::super::grammar::Head::Noun { word, .. }, _) => {
                cx.lex.noun_class(word).map(|c| c.term).unwrap_or_default()
            }
            (super::super::grammar::Head::NewName(_), _) => "Human".into(),
            (super::super::grammar::Head::Pronoun { .. }, Term::Var(_)) => cx
                .generic_you
                .as_ref()
                .map(|(_, c)| c.clone())
                .unwrap_or_default(),
            (super::super::grammar::Head::Gap, _) => cx
                .gap
                .as_ref()
                .and_then(|(_, c)| c.clone())
                .unwrap_or_default(),
            (_, Term::Const(c)) => c.clone(),
            _ => String::new(),
        };
        let fits = |relation: &str| -> bool {
            match cx.lex.domain_class(relation, 2) {
                Some(dom) => subject_term.is_empty() || cx.lex.has_ancestor(&subject_term, &dom),
                None => true,
            }
        };
        let experiential = ["Perception", "PsychologicalProcess"]
            .iter()
            .any(|a| cx.lex.has_ancestor(class, a));
        if experiential && cx.lex.defined("experiencer") && fits("experiencer") {
            return Ok("experiencer");
        }
        if fits("agent") {
            return Ok("agent");
        }
        if !transitive {
            return Ok("patient");
        }
        Err((
            subject_term,
            "agent".into(),
            cx.lex
                .domain_class("agent", 2)
                .unwrap_or_else(|| "AutonomousAgent".into()),
        ))
    }
}

/// The term the past tense is anchored before: the speech time from the
/// options (declared a `TimePoint` once when the KB lacks it), else the
/// KB's own `Now`.
fn speech_time(cx: &mut Cx) -> Option<String> {
    if let Some(now) = cx.opts.now.clone() {
        if !cx.lex.defined(&now) && !cx.discourse.names.contains(&now) {
            cx.discourse.names.push(now.clone());
            cx.declared.atom(
                "instance",
                vec![Term::Const(now.clone()), Term::Const("TimePoint".into())],
            );
        }
        return Some(now);
    }
    cx.lex.defined("Now").then(|| "Now".to_string())
}
