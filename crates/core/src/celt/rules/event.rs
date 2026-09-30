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
//! `(before (EndFn (WhenFn ?E)) Now)` when the KB defines `Now`.

use super::super::grammar::{attach_pp, Cx, Kind, Logic, Np, Pp, Scope, SentenceRule, Step};
use super::super::ir::{Drs, Term};
use super::super::lookup::{Inflection, Verb};
use super::super::refuse::{Construct, Refusal, Refused};
use super::super::token::{Closed, Tense};
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
        let mut aux: Option<Tense> = None;
        if cx.question {
            if let Some(Closed::Aux(t, _)) = cx.tok(i).and_then(|t| t.closed()) {
                aux = Some(t);
                i += 1;
            }
        }
        let subject = match cx.np(i) {
            Step::Hit(np, next) => {
                i = next;
                np
            }
            Step::Miss => return cx.miss(i, "a subject noun phrase"),
            Step::Refuse(r) => return Step::Refuse(r),
        };
        let mut negated = false;
        if let Some(Closed::Aux(t, _)) = cx.tok(i).and_then(|t| t.closed()) {
            if aux.is_some() {
                return cx.miss(i, "a verb");
            }
            aux = Some(t);
            i += 1;
        }
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
            (Some(t), _) => t,
            (None, Inflection::Past) => Tense::Past,
            (None, Inflection::Present) => Tense::Present,
        };
        let object = match cx.np(i) {
            Step::Hit(np, next) => {
                i = next;
                Some(np)
            }
            Step::Miss => None,
            Step::Refuse(r) => return Step::Refuse(r),
        };
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
        if let Some(r) = generic_subject(cx, &subject) {
            return Step::Refuse(r);
        }
        if tense == Tense::Past && !cx.lex.defined("Now") {
            return cx.refuse(
                Refusal::Unsupported {
                    construct: Construct::PastTense,
                    at: vt.text.clone(),
                },
                verb_at,
            );
        }
        let kind = if cx.question {
            Kind::Query
        } else {
            Kind::Assert
        };
        let subject_force = force(&subject, false);
        let verb_word = vt.text.clone();
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
                        tense,
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
}

impl Event {
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
        tense: Tense,
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
        if tense == Tense::Past {
            d.atom(
                "before",
                vec![
                    Term::Fn(
                        "EndFn".into(),
                        vec![Term::Fn("WhenFn".into(), vec![e.clone()])],
                    ),
                    Term::Const("Now".into()),
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
    fn subject_role(
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
