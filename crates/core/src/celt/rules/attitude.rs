//! `Attitude`: `NP VERB [not] to CLAUSE` and `NP VERB [not] that SENTENCE`
//! for a verb whose relation takes a formula.
//!
//! SUMO states propositional attitudes as a relation between an agent and
//! a formula: `(desires Mike (exists ...))`, `(believes Mike (instance Rex
//! Dog))`.  Which verbs do so is read from the KB, not from a list: a verb
//! is an attitude verb when one of its terms is a binary relation whose
//! second argument is a `Formula` (`believes`, `desires`, `knows`,
//! `hopes`, `fears`, `expects`, `doubts`, `considers`, `prefers`, `says`).
//!
//! An infinitive complement ("wants to read a book") is the ordinary
//! clause with the subject of the attitude as its own subject, parsed the
//! way a modal's clause is; a "that" complement is a sentence of its own.
//! Both are bound, not asserted: nothing inside a wish or a belief exists.
//! A generic subject keeps its universal outside ("Dogs want to eat":
//! every dog desires that it eats).  The same verb with a noun phrase
//! object ("wants an apple") is left to the event and stative rules.

use super::super::grammar::{best, Cx, Head, Kind, Logic, Np, SentenceRule, Step};
use super::super::ir::{Cond, Drs, Term};
use super::super::lookup::Inflection;
use super::super::refuse::{Construct, Refusal, Refused};
use super::super::token::{Closed, Number, Tense};
use super::{complement, force, frame};

pub struct Attitude;

impl SentenceRule for Attitude {
    fn name(&self) -> &'static str {
        "attitude"
    }

    fn pattern(&self) -> &'static str {
        "NP wants/believes [not] (to VERB... | that SENTENCE)"
    }

    fn apply(&self, cx: &mut Cx, at: usize) -> Step<Logic> {
        if cx.question {
            return Step::Miss;
        }
        let subjects = match cx.nps(at) {
            Ok(v) if v.is_empty() => return cx.miss(at, "a subject noun phrase"),
            Ok(v) => v,
            Err(r) => return Step::Refuse(r),
        };
        let mut attempts = Vec::new();
        for (subject, next) in subjects {
            let step = cx.attempt(|cx| self.with_subject(cx, subject, next));
            let done = matches!(&step, Step::Hit(_, n) if cx.is_end(*n));
            attempts.push(step);
            if done {
                break;
            }
        }
        best(cx, attempts)
    }
}

/// The complement's shape.
enum Complement {
    /// "to VERB ...": the attitude's subject is the clause's subject.
    Infinitive,
    /// "that SENTENCE".
    That,
}

impl Attitude {
    fn with_subject(&self, cx: &mut Cx, subject: Np, mut i: usize) -> Step<Logic> {
        let mut aux = None;
        if let Some(Closed::Aux(tense, number)) = cx.tok(i).and_then(|t| t.closed()) {
            if tense == Tense::Present && !subject.agrees(cx, number) {
                return cx.miss(i, "do/does agreeing with the subject in number");
            }
            aux = Some((tense, number));
            i += 1;
        }
        let negated = if cx.tok(i).is_some_and(|t| t.is(Closed::Not)) {
            if aux.is_none() {
                return cx.miss(i, "does/did before not");
            }
            i += 1;
            true
        } else {
            false
        };
        let Some(vt) = cx.tok(i).filter(|t| t.is_open()) else {
            return cx.miss(i, "a verb");
        };
        let verb_at = i;
        let Some((choice, infl)) = cx.lex.attitude_verb(&vt.lower) else {
            return cx.miss(i, "a verb");
        };
        i += 1;
        let shape = match cx.tok(i) {
            Some(t) if t.lower == "to" => Complement::Infinitive,
            Some(t) if t.lower == "that" => Complement::That,
            _ => return cx.miss(i, "\"to\" or \"that\" after the verb"),
        };
        i += 1;
        let plural_form =
            aux.is_some_and(|(_, n)| n == Number::Plural) || infl == Inflection::Present;
        let subject = if plural_form {
            subject.pluralized(cx.lex)
        } else {
            subject
        };
        let agrees = match (aux, infl) {
            (Some(_), Inflection::Present) => true,
            (Some(_), _) => false,
            (None, Inflection::Present) => subject.agrees(cx, Number::Plural),
            (None, Inflection::ThirdSingular) => subject.agrees(cx, Number::Singular),
            (None, Inflection::Past | Inflection::Progressive) => true,
        };
        if !agrees {
            return cx.miss(verb_at, "a verb agreeing with the subject in number");
        }
        let past =
            matches!(aux, Some((Tense::Past, _))) || (aux.is_none() && infl == Inflection::Past);
        if past || infl == Inflection::Progressive {
            let construct = if past {
                Construct::PastTense
            } else {
                Construct::Progressive
            };
            return cx.refuse(
                Refusal::Unsupported {
                    construct,
                    at: vt.text.clone(),
                },
                verb_at,
            );
        }
        if cx.is_end(i) {
            return cx.miss(i, "a clause after \"to\" or \"that\"");
        }
        cx.record(&choice);
        let relation = choice.term.clone();
        let subject_force = force(&subject, cx.generic_subject);
        let mut end = i;
        let result = frame(cx, &subject, negated, subject_force, |cx, subj, _| {
            Self::check_subject(cx, &subject, subj, &relation)?;
            let (body, next) = match shape {
                Complement::Infinitive => Self::infinitive(cx, &subject, subj, i)?,
                Complement::That => Self::that(cx, i)?,
            };
            end = next;
            let mut d = Drs::default();
            d.conds
                .push(Cond::Holds(relation.clone(), subj.clone(), body));
            Ok(d)
        });
        match result {
            Ok(drs) => Step::Hit(
                Logic {
                    kind: Kind::Assert,
                    drs,
                    choices: Vec::new(),
                    rule: self.name(),
                    declared: Drs::default(),
                },
                end,
            ),
            Err(r) => Step::Refuse(r),
        }
    }

    /// The subject must fit the relation's first argument (a
    /// `CognitiveAgent` for SUMO's attitudes).
    fn check_subject(cx: &Cx, subject: &Np, subj: &Term, relation: &str) -> Result<(), Refused> {
        let Some(dom) = cx.lex.domain_class(relation, 1) else {
            return Ok(());
        };
        let class: Option<String> = match (&subject.head, subj) {
            (Head::Noun { word, .. }, _) => cx.lex.noun_class(word).ok().map(|c| c.term),
            (Head::NewName(_), _) => Some("Human".into()),
            (Head::Pronoun { .. }, Term::Var(_)) => cx.generic_you.as_ref().map(|(_, c)| c.clone()),
            (Head::Gap, _) => cx.gap.as_ref().and_then(|(_, c)| c.clone()),
            (_, Term::Const(c)) => Some(c.clone()),
            _ => None,
        };
        match class {
            Some(c) if !cx.lex.has_ancestor(&c, &dom) => Err(cx.refused(
                Refusal::RoleMismatch {
                    word: cx.text(subject.start, subject.end),
                    term: c,
                    role: format!("subject of {relation}"),
                    expected: dom,
                },
                subject.start,
            )),
            _ => Ok(()),
        }
    }

    /// "to VERB ...": the clause with the attitude's subject standing in
    /// as a gap, so it is realized once, outside.
    fn infinitive(
        cx: &mut Cx,
        subject: &Np,
        subj: &Term,
        at: usize,
    ) -> Result<(Drs, usize), Refused> {
        let class = match &subject.head {
            Head::Noun { word, .. } => cx.lex.noun_class(word).ok().map(|c| c.term),
            Head::NewName(_) => Some("Human".into()),
            Head::Pronoun { .. } => cx.generic_you.as_ref().map(|(_, c)| c.clone()),
            Head::Gap => cx.gap.as_ref().and_then(|(_, c)| c.clone()),
            Head::Name(n) => Some(n.clone()),
        };
        let gap = Np {
            det: None,
            mods: Vec::new(),
            head: Head::Gap,
            possessor: None,
            relative: None,
            start: subject.start,
            end: subject.end,
        };
        let saved = cx.gap.replace((subj.clone(), class));
        let step = complement(cx, gap, at);
        cx.gap = saved;
        match step {
            Step::Hit(logic, next) if cx.is_end(next) => {
                cx.declared.extend(logic.declared);
                Ok((logic.drs, next))
            }
            Step::Hit(_, next) => Err(cx.refused(
                Refusal::TrailingTokens {
                    from: cx.text(next, next + 1),
                },
                next,
            )),
            Step::Refuse(r) => Err(r),
            Step::Miss => Err(cx.no_rule()),
        }
    }

    /// "that SENTENCE": the rest of the sentence parsed on its own, bound.
    fn that(cx: &mut Cx, at: usize) -> Result<(Drs, usize), Refused> {
        let mut toks = cx.tokens[at..cx.end()].to_vec();
        if let Some(t) = cx.tokens.get(cx.end()) {
            toks.push(t.clone());
        }
        let outcome = {
            let mut sub = Cx::new(
                &toks,
                cx.lex,
                cx.grammar,
                &mut *cx.discourse,
                cx.opts,
                false,
            );
            sub.under_modal = true;
            sub.generic_subject = cx.generic_subject;
            sub.generic_you = cx.generic_you.clone();
            sub.depth = cx.depth + 1;
            cx.grammar.parse(&mut sub)
        };
        let logic = outcome?;
        for c in &logic.choices {
            cx.record(c);
        }
        cx.declared.extend(logic.declared);
        Ok((logic.drs, cx.end()))
    }
}
