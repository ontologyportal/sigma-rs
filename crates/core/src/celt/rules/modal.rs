//! `Modal`: `NP MODAL [not] CLAUSE` and the question `MODAL NP CLAUSE?`.
//!
//! A modal scopes over the clause it introduces, and SUMO says so with
//! `modalAttribute`: `(modalAttribute FORMULA Possibility)`.  The clause
//! after the modal is parsed by the ordinary rules (copula with "be",
//! "have", an event with its object and prepositional phrases) with the
//! subject already in hand, under a scope that binds variables instead of
//! minting Skolem constants: nothing inside a possibility exists.  A
//! generic subject keeps its universal outside the modality ("Dogs can
//! swim": every dog, possibly swims); a particular one is modalized
//! directly.
//!
//! | modal                   | attribute     | negation                         |
//! |-------------------------|---------------|----------------------------------|
//! | can, could, may, might  | `Possibility` | `(not (modalAttribute F ...))`   |
//! | must                    | `Necessity`   | `(modalAttribute (not F) ...)`   |
//! | should, ought           | `Obligation`  | `(modalAttribute (not F) ...)`   |
//! | would                   | refused: counterfactual                          |
//! | shall, will             | refused: future                                  |
//!
//! "May not" is read as "not possible"; its permission reading
//! (`Prohibition`) is not distinguished.

use super::super::grammar::{best, Cx, Kind, Logic, Np, SentenceRule, Step};
use super::super::ir::{Cond, Drs};
use super::super::refuse::{Construct, Refusal};
use super::super::token::Closed;
use super::complement;

pub struct Modal;

impl SentenceRule for Modal {
    fn name(&self) -> &'static str {
        "modal"
    }

    fn pattern(&self) -> &'static str {
        "NP can/must/should [not] VERB..."
    }

    fn apply(&self, cx: &mut Cx, at: usize) -> Step<Logic> {
        let mut i = at;
        let mut modal_at: Option<usize> = None;
        if cx.question && cx.tok(i).is_some_and(|t| t.is(Closed::Modal)) {
            modal_at = Some(i);
            i += 1;
        }
        let subjects = match cx.nps(i) {
            Ok(v) if v.is_empty() => return cx.miss(i, "a subject noun phrase"),
            Ok(v) => v,
            Err(r) => return Step::Refuse(r),
        };
        let mut attempts = Vec::new();
        for (subject, next) in subjects {
            let step = cx.attempt(|cx| self.with_subject(cx, modal_at, subject, next));
            let done = matches!(&step, Step::Hit(_, n) if cx.is_end(*n));
            attempts.push(step);
            if done {
                break;
            }
        }
        best(cx, attempts)
    }
}

impl Modal {
    fn with_subject(
        &self,
        cx: &mut Cx,
        modal_at: Option<usize>,
        subject: Np,
        mut i: usize,
    ) -> Step<Logic> {
        let modal_at = match modal_at {
            Some(m) => m,
            None => {
                if !cx.tok(i).is_some_and(|t| t.is(Closed::Modal)) {
                    return cx.miss(i, "a modal");
                }
                i += 1;
                i - 1
            }
        };
        let modal = cx.tokens[modal_at].clone();
        let attribute = match modal.lower.as_str() {
            "can" | "could" | "may" | "might" => "Possibility",
            "must" => "Necessity",
            "should" | "ought" => "Obligation",
            other => {
                let construct = if other == "would" {
                    Construct::ModalCounterfactual
                } else {
                    Construct::Modal
                };
                return cx.refuse(
                    Refusal::Unsupported {
                        construct,
                        at: modal.text.clone(),
                    },
                    modal_at,
                );
            }
        };
        let negated = if cx.tok(i).is_some_and(|t| t.is(Closed::Not)) {
            i += 1;
            true
        } else {
            false
        };
        for term in ["modalAttribute", attribute] {
            if !cx.lex.defined(term) {
                return cx.refuse(
                    Refusal::TermNotInKb {
                        word: modal.text.clone(),
                        term: term.into(),
                    },
                    modal_at,
                );
            }
        }
        match complement(cx, subject, i) {
            Step::Hit(logic, next) => Step::Hit(
                Logic {
                    kind: if cx.question {
                        Kind::Query
                    } else {
                        Kind::Assert
                    },
                    drs: modalize(logic.drs, attribute, negated),
                    choices: Vec::new(),
                    rule: self.name(),
                    declared: Drs::default(),
                },
                next,
            ),
            other => other,
        }
    }
}

/// Put the clause under the modality.  A universal subject keeps its
/// quantifier outside: the consequent is what is possible or obligatory
/// for each instance.
fn modalize(mut drs: Drs, attribute: &str, negated: bool) -> Drs {
    let universal =
        drs.vars.is_empty() && drs.conds.len() == 1 && matches!(drs.conds[0], Cond::Implies(..));
    if universal {
        if let Some(Cond::Implies(ante, cons)) = drs.conds.pop() {
            drs.conds
                .push(Cond::Implies(ante, modalize_body(cons, attribute, negated)));
        }
        return drs;
    }
    modalize_body(drs, attribute, negated)
}

/// `(modalAttribute F A)`; a negated possibility denies the modality, a
/// negated necessity or obligation applies to the negated clause.
fn modalize_body(body: Drs, attribute: &str, negated: bool) -> Drs {
    let outer = negated && attribute == "Possibility";
    let inner = if negated && !outer {
        Drs {
            vars: Vec::new(),
            conds: vec![Cond::Not(body)],
        }
    } else {
        body
    };
    let modal = Cond::Modal(inner, attribute.to_string());
    let cond = if outer {
        Cond::Not(Drs {
            vars: Vec::new(),
            conds: vec![modal],
        })
    } else {
        modal
    };
    Drs {
        vars: Vec::new(),
        conds: vec![cond],
    }
}
