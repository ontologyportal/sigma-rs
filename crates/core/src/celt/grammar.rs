//! The rule engine: ordered-choice matching over tokens.
//!
//! A [`Grammar`] is an ordered list of rules per category.  Parsing tries
//! each rule in order and takes the first that matches the whole sentence
//! (PEG semantics), so every accepted sentence has exactly one reading.  A
//! rule that recognizes its pattern but cannot translate it returns a
//! [`Refused`], which stops the parse with that explanation; a rule whose
//! pattern does not apply returns [`Step::Miss`] after recording what it
//! expected, so a sentence no rule accepts is reported with the furthest
//! point reached and the alternatives that were tried there.
//!
//! Compound sentences are decomposed before the rules run
//! ([`Grammar::decompose`]): a coordination ("Birds have feathers and
//! wings") or a relative clause on the predicate nominal ("A bird is an
//! animal that flies") becomes two or more simple sentences, each parsed
//! on its own, and the sentence is accepted only when all of them are.
//! Decomposition nests at most [`MAX_DECOMPOSE_DEPTH`] deep and is not
//! attempted past [`MAX_CONJUNCTIONS`] conjunctions: each level multiplies
//! the split candidates, and a sentence that needs more is not controlled
//! English.
//!
//! Adding a construct = adding one rule file under `rules/` and registering
//! it in [`Grammar::standard`].

use crate::lexicon::Pos;

use super::ir::{Cond, Discourse, Drs, Term};
use super::lookup::{CeltType, Choice, Inflection, Lookup, Verb};
use super::refuse::{Construct, Refusal, Refused};
use super::token::{
    pronoun_info, Class, Closed, Det, Gender, Number, PronounCase, PronounInfo, Token,
};
use crate::layer::{Layer, TopLayer};

/// The lexical services a rule may use, object-safe so rules need not be
/// generic over the KB layer.
pub trait Lex {
    fn has_sense(&self, word: &str, pos: Pos) -> bool;
    fn is_nounish(&self, word: &str) -> bool;
    fn has_label(&self, word: &str) -> bool;
    fn has_capitalized_lemma(&self, word: &str) -> bool;
    fn is_adjectival(&self, word: &str) -> bool;
    fn is_verbal(&self, word: &str) -> bool;
    fn is_invariant_plural(&self, word: &str) -> bool;
    fn noun_lemma(&self, word: &str) -> (String, bool);
    fn noun_class(&self, word: &str) -> Result<Choice, Refusal>;
    fn predicate_noun(&self, word: &str) -> Result<(Choice, bool), Refusal>;
    fn named_instance(&self, word: &str) -> Option<Choice>;
    fn adjective(&self, word: &str) -> Result<Choice, Refusal>;
    fn verb(&self, word: &str) -> Result<(Verb, Inflection), Refusal>;
    fn name(&self, word: &str) -> Option<String>;
    fn defined(&self, term: &str) -> bool;
    fn has_ancestor(&self, term: &str, ancestor: &str) -> bool;
    fn domain_class(&self, relation: &str, arg: usize) -> Option<String>;
    fn celt_type(&self, term: &str) -> CeltType;
    fn cross_category(&self, sub: &str, sup: &str) -> Option<String>;
}

impl<L: TopLayer + Layer> Lex for Lookup<'_, L> {
    fn has_sense(&self, word: &str, pos: Pos) -> bool {
        Lookup::has_sense(self, word, pos)
    }
    fn is_nounish(&self, word: &str) -> bool {
        Lookup::is_nounish(self, word)
    }
    fn has_label(&self, word: &str) -> bool {
        Lookup::has_label(self, word)
    }
    fn has_capitalized_lemma(&self, word: &str) -> bool {
        Lookup::has_capitalized_lemma(self, word)
    }
    fn is_adjectival(&self, word: &str) -> bool {
        Lookup::is_adjectival(self, word)
    }
    fn is_verbal(&self, word: &str) -> bool {
        Lookup::is_verbal(self, word)
    }
    fn is_invariant_plural(&self, word: &str) -> bool {
        Lookup::is_invariant_plural(self, word)
    }
    fn noun_lemma(&self, word: &str) -> (String, bool) {
        Lookup::noun_lemma(self, word)
    }
    fn noun_class(&self, word: &str) -> Result<Choice, Refusal> {
        Lookup::noun_class(self, word)
    }
    fn predicate_noun(&self, word: &str) -> Result<(Choice, bool), Refusal> {
        Lookup::predicate_noun(self, word)
    }
    fn named_instance(&self, word: &str) -> Option<Choice> {
        Lookup::named_instance(self, word)
    }
    fn adjective(&self, word: &str) -> Result<Choice, Refusal> {
        Lookup::adjective(self, word)
    }
    fn verb(&self, word: &str) -> Result<(Verb, Inflection), Refusal> {
        Lookup::verb(self, word)
    }
    fn name(&self, word: &str) -> Option<String> {
        Lookup::name(self, word)
    }
    fn defined(&self, term: &str) -> bool {
        Lookup::defined(self, term)
    }
    fn has_ancestor(&self, term: &str, ancestor: &str) -> bool {
        Lookup::has_ancestor(self, term, ancestor)
    }
    fn domain_class(&self, relation: &str, arg: usize) -> Option<String> {
        Lookup::domain_class(self, relation, arg)
    }
    fn celt_type(&self, term: &str) -> CeltType {
        Lookup::celt_type(self, term)
    }
    fn cross_category(&self, sub: &str, sup: &str) -> Option<String> {
        Lookup::cross_category(self, sub, sup)
    }
}

/// Deepest nesting of [`Grammar::decompose`] (a coordination inside a
/// relative clause is depth 2).
pub const MAX_DECOMPOSE_DEPTH: usize = 2;
/// Most "and"/"but" tokens a sentence may carry and still be decomposed.
pub const MAX_CONJUNCTIONS: usize = 3;

/// Translation options.
#[derive(Debug, Clone, Default)]
pub struct Opts {
    /// Treat an unknown capitalized word as the name of a Human instead of
    /// refusing it: emits `(instance Mike Human)` and `(names "Mike" Mike)`.
    pub assume_human_names: bool,
    /// Words that are always proper names of Humans (a first-name list),
    /// even where WordNet has a common-noun sense for them ("Mike").
    pub names: Vec<String>,
    /// Read a bare-plural or indefinite subject of an action verb as a
    /// universal ("Dogs chase cats": every dog chases some cat) instead of
    /// refusing it as ambiguous between a habit and a law.
    pub generic_subjects: bool,
    /// Gender of listed names (`FirstNames.csv`), for "he"/"she".
    pub name_genders: Vec<(String, Gender)>,
    /// The KB instance "I"/"me"/"my" denotes; without one, first person is
    /// refused.
    pub speaker: Option<String>,
    /// The KB instance "you"/"your" denotes, when the text addresses
    /// someone in particular.
    pub addressee: Option<String>,
    /// With no addressee, read "you" as an arbitrary instance of this class
    /// ("A flashlight helps you": every instance of it).  `None` refuses.
    pub generic_you: Option<String>,
    /// KB terms consulted when binding third-person pronouns.
    pub pronoun_classes: PronounClasses,
}

/// The KB terms third-person pronoun resolution consults; each is used
/// only when the loaded KB defines it, so a KB that encodes gender or
/// personhood differently supplies its own.  "He" needs a referent under
/// `male`, or under `person` and not under `female`; "it" one under none
/// of these; "they" one under `group`.
#[derive(Debug, Clone)]
pub struct PronounClasses {
    pub person: String,
    pub male: String,
    pub female: String,
    pub group: String,
}

impl Default for PronounClasses {
    fn default() -> Self {
        Self {
            person: "Human".into(),
            male: "Man".into(),
            female: "Woman".into(),
            group: "Collection".into(),
        }
    }
}

/// Whether a sentence asserts or asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Assert,
    Query,
}

/// The translation of one sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logic {
    pub kind: Kind,
    pub drs: Drs,
    /// Lexical decisions taken, with the alternatives passed over.
    pub choices: Vec<Choice>,
    /// The sentence rule that produced this reading.
    pub rule: &'static str,
    /// Side assertions the sentence relies on (an assumed name's
    /// `(instance Mike Human)`), kept out of a query's formula.
    pub declared: Drs,
}

/// Outcome of trying one rule at one position.
pub enum Step<T> {
    /// Matched; the value and the position after the match.
    Hit(T, usize),
    /// The pattern does not apply here.
    Miss,
    /// The pattern applies but the sentence is refused.
    Refuse(Refused),
}

/// Parse state for one sentence.
pub struct Cx<'a> {
    pub tokens: &'a [Token],
    pub lex: &'a dyn Lex,
    pub grammar: &'a Grammar,
    pub discourse: &'a mut Discourse,
    pub opts: &'a Opts,
    pub question: bool,
    /// Skip subject-verb number agreement: the sentence is one conjunct of
    /// a coordination whose verb agreed with the whole ("A dog and a cat
    /// are animals").
    pub relaxed_agreement: bool,
    /// Read an indefinite or bare-plural subject of an action verb as
    /// generic: the sentence is the relative clause of a class phrase
    /// ("an animal that flies").
    pub generic_subject: bool,
    /// Nesting level of decomposition (0 for the sentence as written).
    pub depth: usize,
    /// The variable and class a generic "you" binds to in this sentence
    /// ([`Opts::generic_you`]); set before the rules run, closed by a
    /// universal around the whole sentence.
    pub generic_you: Option<(String, String)>,
    /// The clause is the scope of a modal: referents bind variables (no
    /// Skolem constants inside a possibility), "be" is a copula, and a
    /// subject's number and genericity are not checked.
    pub under_modal: bool,
    /// Referents introduced by this clause start at this index of the
    /// discourse list: a plain pronoun may not corefer with one of them
    /// ("Rex sees it" is not "Rex sees Rex"), a reflexive must.
    clause_referents: usize,
    furthest: usize,
    expected: Vec<String>,
    pub choices: Vec<Choice>,
    pub declared: Drs,
}

impl<'a> Cx<'a> {
    pub fn new(
        tokens: &'a [Token],
        lex: &'a dyn Lex,
        grammar: &'a Grammar,
        discourse: &'a mut Discourse,
        opts: &'a Opts,
        question: bool,
    ) -> Self {
        let clause_referents = discourse.referents.len();
        Self {
            tokens,
            lex,
            grammar,
            discourse,
            opts,
            question,
            relaxed_agreement: false,
            generic_subject: false,
            depth: 0,
            generic_you: None,
            under_modal: false,
            clause_referents,
            furthest: 0,
            expected: Vec::new(),
            choices: Vec::new(),
            declared: Drs::default(),
        }
    }

    pub fn tok(&self, at: usize) -> Option<&'a Token> {
        self.tokens.get(at)
    }

    /// Record that a rule wanted `what` at `at`, and miss.
    pub fn miss<T>(&mut self, at: usize, what: &str) -> Step<T> {
        if at > self.furthest {
            self.furthest = at;
            self.expected.clear();
        }
        if at == self.furthest && !self.expected.iter().any(|e| e == what) {
            self.expected.push(what.to_string());
        }
        Step::Miss
    }

    pub fn refuse<T>(&self, reason: Refusal, at: usize) -> Step<T> {
        Step::Refuse(self.refused(reason, at))
    }

    pub fn refused(&self, reason: Refusal, at: usize) -> Refused {
        let (s, e) = self.span(at, at + 1);
        Refused::new(reason, s, e)
    }

    /// Char span covering tokens `from..to`.
    pub fn span(&self, from: usize, to: usize) -> (usize, usize) {
        let s = self.tokens.get(from).map_or(0, |t| t.start);
        let e = self
            .tokens
            .get(to.saturating_sub(1).max(from))
            .map_or(s, |t| t.end);
        (s, e)
    }

    pub fn text(&self, from: usize, to: usize) -> String {
        self.tokens[from..to.min(self.tokens.len())]
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Index of the sentence-final punctuation (or the token count).
    pub fn end(&self) -> usize {
        match self.tokens.last() {
            Some(t) if matches!(t.class, Class::Punct('.' | '?' | '!')) => self.tokens.len() - 1,
            _ => self.tokens.len(),
        }
    }

    pub fn is_end(&self, at: usize) -> bool {
        at >= self.end()
    }

    pub fn record(&mut self, c: &Choice) {
        if !self.choices.iter().any(|x| x == c) {
            self.choices.push(c.clone());
        }
    }

    /// The refusal for "no rule matched", from the furthest miss.
    pub fn no_rule(&self) -> Refused {
        let at = self.furthest.min(self.tokens.len().saturating_sub(1));
        let text = self.tok(at).map_or_else(String::new, |t| t.text.clone());
        self.refused(
            Refusal::NoRule {
                at: text,
                expected: self.expected.clone(),
            },
            at,
        )
    }

    /// Every noun-phrase reading at `at`, longest first, from all NP rules.
    /// Empty means no rule applied (expectations are recorded).
    pub fn nps(&mut self, at: usize) -> Result<Vec<(Np, usize)>, Refused> {
        let mut out = Vec::new();
        for rule in &self.grammar.np {
            out.extend(rule.candidates(self, at)?);
        }
        Ok(out)
    }

    /// The longest noun-phrase reading at `at`.
    pub fn np(&mut self, at: usize) -> Step<Np> {
        match self.nps(at) {
            Ok(mut v) if !v.is_empty() => {
                let (np, next) = v.remove(0);
                Step::Hit(np, next)
            }
            Ok(_) => Step::Miss,
            Err(r) => Step::Refuse(r),
        }
    }

    /// Run one parse attempt and roll back its side effects unless it
    /// reached the end of the sentence.  Returns the attempt's outcome.
    pub fn attempt<T>(&mut self, f: impl FnOnce(&mut Cx) -> Step<T>) -> Step<T> {
        let mark = self.discourse.mark();
        let choices = self.choices.len();
        let declared = self.declared.conds.len();
        let step = f(self);
        let keep = matches!(&step, Step::Hit(_, next) if self.is_end(*next));
        if !keep {
            self.discourse.reset(mark);
            self.choices.truncate(choices);
            self.declared.conds.truncate(declared);
        }
        step
    }

    /// Parse `PREP NP` at `at`.
    pub fn pp(&mut self, at: usize) -> Step<Pp> {
        let Some(prep) = self.tok(at).filter(|t| t.is(Closed::Preposition)) else {
            return self.miss(at, "a preposition");
        };
        match self.np(at + 1) {
            Step::Hit(np, next) => Step::Hit(
                Pp {
                    prep: prep.lower.clone(),
                    at,
                    np,
                },
                next,
            ),
            Step::Miss => self.miss(at + 1, "a noun phrase after the preposition"),
            Step::Refuse(r) => Step::Refuse(r),
        }
    }
}

/// Head of a noun phrase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// A KB instance (by symbol or label).
    Name(String),
    /// A capitalized word not in the KB, accepted under
    /// [`Opts::assume_human_names`].
    NewName(String),
    /// A common noun; `lemma` is the base form.
    Noun {
        word: String,
        lemma: String,
        plural: bool,
    },
    /// A pronoun, bound on realization: deictic ones through [`Opts`],
    /// third-person ones to an earlier referent.
    Pronoun { word: String, info: PronounInfo },
}

/// A parsed noun phrase (syntax only; grounding happens on realization).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Np {
    pub det: Option<Det>,
    /// Adjective words preceding the head.
    pub mods: Vec<String>,
    pub head: Head,
    /// The owner in "your tail", "Bob's car", "the dog's bed".
    pub possessor: Option<Box<Np>>,
    pub start: usize,
    pub end: usize,
}

/// A parsed prepositional phrase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pp {
    pub prep: String,
    pub at: usize,
    pub np: Np,
}

/// How a noun phrase's referent is bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Assertion: new referents become Skolem constants.
    Assert,
    /// Query, negation, quantified: new referents become bound variables.
    Bound,
}

/// A realized noun phrase: its term plus the conditions introducing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Realized {
    pub term: Term,
    /// The class the referent was introduced under, when known.
    pub class: Option<String>,
    pub drs: Drs,
}

impl Np {
    pub fn is_generic(&self) -> bool {
        if self.possessor.is_some() {
            return false;
        }
        match (&self.head, self.det) {
            (Head::Noun { plural, .. }, None) => *plural,
            (Head::Noun { .. }, Some(Det::Indefinite | Det::Universal)) => true,
            _ => false,
        }
    }

    pub fn is_referential(&self) -> bool {
        matches!(
            self.head,
            Head::Name(_) | Head::NewName(_) | Head::Pronoun { .. }
        ) || matches!(self.det, Some(Det::Definite))
    }

    /// The word of the head, for messages.
    pub fn head_word(&self) -> &str {
        match &self.head {
            Head::Name(n) | Head::NewName(n) => n,
            Head::Noun { word, .. } | Head::Pronoun { word, .. } => word,
        }
    }

    /// Grammatical number of the phrase, `None` when the form does not fix
    /// it (a bare invariant plural: "deer", "people").
    pub fn number(&self, lex: &dyn Lex) -> Option<Number> {
        match &self.head {
            Head::Name(_) | Head::NewName(_) => Some(Number::Singular),
            // First and second person take the base verb form ("I read").
            Head::Pronoun { info, .. } if info.person == 3 => info.number,
            Head::Pronoun { .. } => None,
            Head::Noun { plural: true, .. } => Some(Number::Plural),
            Head::Noun { word, .. } => {
                let invariant = lex.is_invariant_plural(word);
                match self.det {
                    Some(Det::Indefinite) => Some(Number::Singular),
                    _ if invariant => None,
                    _ => Some(Number::Singular),
                }
            }
        }
    }

    /// Does a verb or copula form of `number` agree with this subject?
    pub fn agrees(&self, cx: &Cx, number: Number) -> bool {
        cx.relaxed_agreement || self.number(cx.lex).is_none_or(|n| n == number)
    }

    /// A bare noun whose form does not fix its number ("deer", "people"),
    /// read as plural because the verb or copula was plural.
    pub fn pluralized(mut self, lex: &dyn Lex) -> Self {
        if let Head::Noun { word, plural, .. } = &mut self.head {
            if self.det.is_none() && lex.is_invariant_plural(word) {
                *plural = true;
            }
        }
        self
    }

    /// Ground the head as a class, refusing modifiers (a modified class
    /// phrase like "a red car" names no SUMO class).
    pub fn class(&self, cx: &mut Cx) -> Result<Choice, Refused> {
        if let Some(m) = self.mods.first() {
            return Err(cx.refused(Refusal::ModifiedClass { word: m.clone() }, self.start));
        }
        self.head_class(cx)
    }

    /// The adjectives of the phrase, grounded as attributes.
    pub fn attributes(&self, cx: &mut Cx) -> Result<Vec<Choice>, Refused> {
        let mut out = Vec::new();
        for m in &self.mods {
            let a = cx.lex.adjective(m).map_err(|r| cx.refused(r, self.start))?;
            cx.record(&a);
            out.push(a);
        }
        Ok(out)
    }

    /// The conditions a variable must meet to be described by this phrase
    /// as a class term: `(instance ?X C)` plus one `attribute` atom per
    /// adjective ("red cars" -> cars that are red).
    pub fn restrict(&self, cx: &mut Cx, var: &Term) -> Result<Drs, Refused> {
        let class = self.head_class(cx)?;
        let mut drs = Drs::default();
        drs.atom("instance", vec![var.clone(), Term::Const(class.term)]);
        for a in self.attributes(cx)? {
            drs.atom("attribute", vec![var.clone(), Term::Const(a.term)]);
        }
        Ok(drs)
    }

    /// Ground the head noun as a class, ignoring modifiers.
    pub fn head_class(&self, cx: &mut Cx) -> Result<Choice, Refused> {
        match &self.head {
            Head::Noun { word, .. } => {
                let c = cx
                    .lex
                    .noun_class(word)
                    .map_err(|r| cx.refused(r, self.end - 1))?;
                cx.record(&c);
                Ok(c)
            }
            Head::Name(n) | Head::NewName(n) | Head::Pronoun { word: n, .. } => Err(cx.refused(
                Refusal::NotAClass {
                    word: n.clone(),
                    term: n.clone(),
                },
                self.start,
            )),
        }
    }

    /// Introduce the referent of this noun phrase.  Definites resolve to an
    /// earlier referent of the same class when exactly one exists.
    pub fn realize(&self, cx: &mut Cx, scope: Scope) -> Result<Realized, Refused> {
        let mut drs = Drs::default();
        let owner = match &self.possessor {
            Some(p) => Some(p.realize(cx, scope)?),
            None => None,
        };
        let (term, class) = match &self.head {
            Head::Name(n) => {
                cx.discourse.refer(n, "");
                (Term::Const(n.clone()), None)
            }
            Head::NewName(n) => {
                if !cx.discourse.names.contains(n) {
                    cx.discourse.names.push(n.clone());
                    cx.declared.atom(
                        "instance",
                        vec![Term::Const(n.clone()), Term::Const("Human".into())],
                    );
                    cx.declared
                        .atom("names", vec![Term::Str(n.clone()), Term::Const(n.clone())]);
                }
                cx.discourse.refer(n, "Human");
                (Term::Const(n.clone()), Some("Human".to_string()))
            }
            Head::Pronoun { word, info } => self.realize_pronoun(cx, word, *info)?,
            Head::Noun { word, .. }
                if matches!(self.det, None | Some(Det::Definite)) && self.mods.is_empty() =>
            {
                if let Some(c) = cx.lex.named_instance(word) {
                    cx.record(&c);
                    return Ok(Realized {
                        term: Term::Const(c.term),
                        class: None,
                        drs,
                    });
                }
                self.realize_noun(cx, word, scope, &mut drs)?
            }
            Head::Noun { word, .. } => self.realize_noun(cx, word, scope, &mut drs)?,
        };
        for a in self.attributes(cx)? {
            drs.atom("attribute", vec![term.clone(), Term::Const(a.term)]);
        }
        if let Some(owner) = owner {
            let owner_key = owner.class.clone().or_else(|| match &owner.term {
                Term::Const(c) => Some(c.clone()),
                _ => None,
            });
            let owned = class.clone().unwrap_or_default();
            let Some((relation, owned_is_part)) =
                possession_relation(cx, owner_key.as_deref(), &owned)
            else {
                return Err(cx.refused(
                    Refusal::Unsupported {
                        construct: Construct::Possessive,
                        at: cx.text(self.start, self.end),
                    },
                    self.start,
                ));
            };
            let mut d = owner.drs;
            d.extend(drs);
            drs = d;
            if owned_is_part {
                drs.atom(relation, vec![term.clone(), owner.term]);
            } else {
                drs.atom(relation, vec![owner.term, term.clone()]);
            }
        }
        Ok(Realized { term, class, drs })
    }

    /// Bind a pronoun: first person to the speaker, second person to the
    /// addressee or the generic class, third person to the most recent
    /// compatible referent (gender from the name list or the KB).
    fn realize_pronoun(
        &self,
        cx: &mut Cx,
        word: &str,
        info: PronounInfo,
    ) -> Result<(Term, Option<String>), Refused> {
        let label = format!("{word} ({info})");
        let unsupported = |cx: &Cx, construct: Construct| {
            cx.refused(
                Refusal::Unsupported {
                    construct,
                    at: label.clone(),
                },
                self.start,
            )
        };
        match info.person {
            1 => match &cx.opts.speaker {
                Some(s) => Ok((Term::Const(s.clone()), None)),
                None => Err(unsupported(cx, Construct::Pronoun)),
            },
            2 => {
                if let Some(a) = &cx.opts.addressee {
                    return Ok((Term::Const(a.clone()), None));
                }
                if let Some((v, class)) = &cx.generic_you {
                    return Ok((Term::Var(v.clone()), Some(class.clone())));
                }
                Err(unsupported(cx, Construct::Pronoun))
            }
            _ => {
                if let Some((term, class)) = self.resolve_anaphor(cx, word, info)? {
                    return Ok((Term::Const(term), class));
                }
                let expletive = word.eq_ignore_ascii_case("it")
                    && self.start == 0
                    && (matches!(cx.tok(1).and_then(|t| t.closed()), Some(Closed::Copula(..)))
                        || cx.tok(2).is_some_and(|t| t.lower == "to"));
                Err(unsupported(
                    cx,
                    if expletive {
                        Construct::Expletive
                    } else {
                        Construct::Pronoun
                    },
                ))
            }
        }
    }

    /// The referent a third-person pronoun denotes: scanning sentences from
    /// the current one backwards, the first sentence with exactly one
    /// compatible referent binds it; several is an ambiguity; a reflexive
    /// looks only in its own sentence.
    fn resolve_anaphor(
        &self,
        cx: &mut Cx,
        word: &str,
        info: PronounInfo,
    ) -> Result<Option<(String, Option<String>)>, Refused> {
        let lex = cx.lex;
        let opts = cx.opts;
        let classes = &opts.pronoun_classes;
        let under = |key: &str, c: &str| lex.defined(c) && lex.has_ancestor(key, c);
        let compatible = |term: &str, key: &str| -> bool {
            let gender = opts
                .name_genders
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(term))
                .map(|(_, g)| *g)
                .or_else(|| {
                    if under(key, &classes.male) {
                        Some(Gender::Male)
                    } else if under(key, &classes.female) {
                        Some(Gender::Female)
                    } else {
                        None
                    }
                });
            let person = gender.is_some() || under(key, &classes.person);
            let group = under(key, &classes.group);
            match word.to_lowercase().as_str() {
                _ if info.number == Some(Number::Plural) => group,
                "he" | "him" | "his" | "himself" => person && gender != Some(Gender::Female),
                "she" | "her" | "hers" | "herself" => person && gender != Some(Gender::Male),
                _ => !person && !group,
            }
        };
        let current = cx.discourse.sentence;
        let reflexive = info.case == PronounCase::Reflexive;
        for s in (0..=current).rev() {
            let mut cands: Vec<(String, Option<String>)> = Vec::new();
            for (i, r) in cx.discourse.referents.iter().enumerate() {
                let own_clause = i >= cx.clause_referents;
                if r.sentence != s || own_clause != reflexive {
                    continue;
                }
                let key = if r.class.is_empty() {
                    &r.term
                } else {
                    &r.class
                };
                // Events are referents for "the reading", not for "it".
                if under(key, "Process") {
                    continue;
                }
                if compatible(&r.term, key) && !cands.iter().any(|(t, _)| *t == r.term) {
                    let class = (!r.class.is_empty()).then(|| r.class.clone());
                    cands.push((r.term.clone(), class));
                }
            }
            match cands.len() {
                0 if reflexive => return Ok(None),
                0 => continue,
                1 => return Ok(cands.pop()),
                _ => {
                    return Err(cx.refused(
                        Refusal::AmbiguousReference {
                            phrase: word.into(),
                            candidates: cands.into_iter().map(|(t, _)| t).collect(),
                        },
                        self.start,
                    ))
                }
            }
        }
        Ok(None)
    }

    fn realize_noun(
        &self,
        cx: &mut Cx,
        word: &str,
        scope: Scope,
        drs: &mut Drs,
    ) -> Result<(Term, Option<String>), Refused> {
        let choice = cx
            .lex
            .noun_class(word)
            .map_err(|r| cx.refused(r, self.end - 1))?;
        cx.record(&choice);
        let class = choice.term.clone();
        let resolved = if self.det == Some(Det::Definite) {
            self.resolve_definite(cx, &class)?
        } else {
            None
        };
        let term = match resolved {
            Some(t) => Term::Const(t),
            None => {
                let t = match scope {
                    Scope::Assert => {
                        let lex = cx.lex;
                        Term::Const(cx.discourse.skolem(&class, |n| lex.defined(n)))
                    }
                    Scope::Bound => {
                        let v = cx.discourse.var("X");
                        drs.bind(&v);
                        Term::Var(v)
                    }
                };
                drs.atom("instance", vec![t.clone(), Term::Const(class.clone())]);
                t
            }
        };
        Ok((term, Some(class)))
    }
    fn resolve_definite(&self, cx: &mut Cx, class: &str) -> Result<Option<String>, Refused> {
        let lex = cx.lex;
        let matches: Vec<String> = cx
            .discourse
            .matching(class, |sub, sup| lex.has_ancestor(sub, sup))
            .into_iter()
            .map(|r| r.term.clone())
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(Some(matches[0].clone())),
            _ => Err(cx.refused(
                Refusal::AmbiguousReference {
                    phrase: cx.text(self.start, self.end),
                    candidates: matches,
                },
                self.start,
            )),
        }
    }
}

/// A sentence-level rule: produces the logic of a whole sentence.
pub trait SentenceRule: Send + Sync {
    fn name(&self) -> &'static str;
    /// The pattern in human terms, for "expected" messages and docs.
    fn pattern(&self) -> &'static str;
    fn apply(&self, cx: &mut Cx, at: usize) -> Step<Logic>;
}

/// A noun-phrase rule: every reading it allows at a position, longest
/// first.  The sentence rule keeps the first reading under which the rest
/// of the sentence parses, so the KB and lexicon disambiguate.
pub trait NpRule: Send + Sync {
    fn name(&self) -> &'static str;
    fn candidates(&self, cx: &mut Cx, at: usize) -> Result<Vec<(Np, usize)>, Refused>;
}

/// Merge the outcomes of several parse attempts: a complete parse wins,
/// then the first refusal, then the first partial parse, then a miss.
pub fn best<T>(cx: &Cx, attempts: impl IntoIterator<Item = Step<T>>) -> Step<T> {
    let mut refused: Option<Refused> = None;
    let mut partial: Option<(T, usize)> = None;
    for step in attempts {
        match step {
            Step::Hit(v, next) if cx.is_end(next) => return Step::Hit(v, next),
            Step::Hit(v, next) => {
                if partial.is_none() {
                    partial = Some((v, next));
                }
            }
            Step::Refuse(r) => {
                if refused.is_none() {
                    refused = Some(r);
                }
            }
            Step::Miss => {}
        }
    }
    if let Some(r) = refused {
        return Step::Refuse(r);
    }
    if let Some((v, next)) = partial {
        return Step::Hit(v, next);
    }
    Step::Miss
}

/// One row of the preposition table (CELT paper, Table 1): a preposition
/// with a complement of the given type maps to a SUMO relation applied as
/// `(relation <head> <complement>)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepRule {
    pub prep: &'static str,
    /// `None` accepts any complement type.
    pub complement: Option<CeltType>,
    pub relation: &'static str,
    /// Only meaningful with an event as the head (not after the copula).
    pub event_only: bool,
}

/// The rule registry.
pub struct Grammar {
    pub sentence: Vec<Box<dyn SentenceRule>>,
    pub np: Vec<Box<dyn NpRule>>,
    pub prepositions: Vec<PrepRule>,
    /// Closed-class tokens that are recognized but refused before any rule
    /// runs, with the construct they signal.
    pub prescan: Vec<(Closed, Construct)>,
}

impl Grammar {
    /// An empty grammar; add rules with [`Grammar::sentence_rule`] etc.
    pub fn empty() -> Self {
        Self {
            sentence: Vec::new(),
            np: Vec::new(),
            prepositions: Vec::new(),
            prescan: Vec::new(),
        }
    }

    /// The baseline controlled language.
    pub fn standard() -> Self {
        use super::rules;
        let mut g = Self::empty();
        g.np.push(Box::new(rules::np::BasicNp));
        g.sentence.push(Box::new(rules::copula::Copula));
        g.sentence.push(Box::new(rules::have::Have));
        g.sentence.push(Box::new(rules::modal::Modal));
        g.sentence.push(Box::new(rules::event::Event));
        g.prepositions = standard_prepositions();
        g.prescan = vec![
            (Closed::Conjunction, Construct::Coordination),
            (Closed::Subordinator, Construct::Subordination),
            (Closed::RelMarker, Construct::RelativeClause),
            (Closed::There, Construct::Existential),
            (Closed::Det(Det::Demonstrative), Construct::Demonstrative),
        ];
        g
    }

    pub fn sentence_rule(mut self, r: impl SentenceRule + 'static) -> Self {
        self.sentence.push(Box::new(r));
        self
    }

    pub fn np_rule(mut self, r: impl NpRule + 'static) -> Self {
        self.np.push(Box::new(r));
        self
    }

    pub fn preposition(mut self, r: PrepRule) -> Self {
        self.prepositions.push(r);
        self
    }

    /// The relation for `prep` with a complement of `ctype`, if any.
    pub fn prep_relation(&self, prep: &str, ctype: CeltType, event: bool) -> Option<&PrepRule> {
        self.prepositions.iter().find(|r| {
            r.prep == prep && r.complement.is_none_or(|c| c == ctype) && (event || !r.event_only)
        })
    }

    /// Translate one tokenized sentence.
    pub fn parse(&self, cx: &mut Cx) -> Result<Logic, Refused> {
        if cx.end() == 0 {
            return Err(Refused::new(Refusal::Empty, 0, 0));
        }
        if let Some(outcome) = self.decompose(cx) {
            return outcome;
        }
        self.parse_simple(cx)
    }

    /// Split a compound sentence into simple ones and translate each.
    /// `None` when the sentence has no coordination or relative clause
    /// this understands; the simple parse then refuses it as usual.
    fn decompose(&self, cx: &mut Cx) -> Option<Result<Logic, Refused>> {
        let end = cx.end();
        let toks = cx.tokens;
        let conjunctions = toks[..end]
            .iter()
            .filter(|t| t.is(Closed::Conjunction))
            .count();
        if cx.depth >= MAX_DECOMPOSE_DEPTH || conjunctions > MAX_CONJUNCTIONS {
            return None;
        }
        let is_rel = |r: usize| {
            matches!(toks[r].lower.as_str(), "which" | "who")
                || (toks[r].lower == "that" && r > 0 && toks[r - 1].is_open())
        };
        // SUBJECT is NP that REST  ->  SUBJECT is NP. / SUBJECT REST.
        if let Some(r) = (1..end).find(|&r| is_rel(r)) {
            let copula = (1..r).find(|&c| matches!(toks[c].closed(), Some(Closed::Copula(..))));
            if let Some(c) = copula.filter(|&c| c + 1 < r && r + 1 < end) {
                let mut first = toks[..r].to_vec();
                first.extend(self.terminal(cx));
                let mut second = toks[..c].to_vec();
                second.extend_from_slice(&toks[r + 1..]);
                let fallback = cx.refused(
                    Refusal::Unsupported {
                        construct: Construct::RelativeClause,
                        at: toks[r].text.clone(),
                    },
                    r,
                );
                return Some(
                    self.combine(cx, &[first, second], "relative-clause", true, false)
                        .map_err(|r| r.unwrap_or(fallback)),
                );
            }
        }
        // Coordination with "and"/"but": a clause split, else a list of
        // like items distributed over the rest of the sentence.
        let k = (1..end).find(|&k| {
            toks[k].is(Closed::Conjunction) && matches!(toks[k].lower.as_str(), "and" | "but")
        })?;
        let fallback = cx.refused(
            Refusal::Unsupported {
                construct: Construct::Coordination,
                at: toks[k].text.clone(),
            },
            k,
        );
        let blocks = |t: &Token| {
            matches!(
                t.closed(),
                Some(Closed::Copula(..) | Closed::Aux(..) | Closed::Not | Closed::Conjunction)
            ) || matches!(t.class, Class::Punct(c) if c != ',')
        };
        let mut first_refusal: Option<Refused> = None;
        let mut note = |r: Option<Refused>| {
            if first_refusal.is_none() {
                first_refusal = r.filter(|r| {
                    !matches!(
                        r.reason,
                        Refusal::NoRule { .. } | Refusal::TrailingTokens { .. }
                    )
                });
            }
        };
        if k + 1 < end {
            let mut left = toks[..k].to_vec();
            left.extend(self.terminal(cx));
            let right = toks[k + 1..].to_vec();
            match self.combine(cx, &[left, right], "coordination", false, false) {
                Ok(l) => return Some(Ok(l)),
                Err(r) => note(r),
            }
        }
        for m in k + 2..=end {
            if m - (k + 1) > 5 || toks[k + 1..m].iter().any(blocks) {
                break;
            }
            for j in (0..k).rev() {
                if k - j > 12 || blocks(&toks[j]) {
                    break;
                }
                // Comma-separated items before the conjunction; a trailing
                // comma (", and") is allowed.
                let mut items: Vec<Vec<Token>> = toks[j..k]
                    .split(|t| t.is_punct(','))
                    .map(<[Token]>::to_vec)
                    .collect();
                if items.last().is_some_and(Vec::is_empty) {
                    items.pop();
                }
                if items.is_empty() || items.iter().any(Vec::is_empty) {
                    continue;
                }
                items.push(toks[k + 1..m].to_vec());
                let variants: Vec<Vec<Token>> = items
                    .into_iter()
                    .map(|item| {
                        let mut v = toks[..j].to_vec();
                        v.extend(item);
                        v.extend_from_slice(&toks[m..]);
                        v
                    })
                    .collect();
                match self.combine(cx, &variants, "coordination", false, true) {
                    Ok(l) => return Some(Ok(l)),
                    Err(r) => note(r),
                }
            }
        }
        Some(Err(first_refusal.unwrap_or(fallback)))
    }

    /// The sentence's terminal punctuation token, to close a split-off
    /// clause.
    fn terminal(&self, cx: &Cx) -> Option<Token> {
        cx.tokens.get(cx.end()).cloned()
    }

    /// Parse every variant as its own sentence and merge the results; a
    /// failure rolls back the discourse and reports the variant's
    /// refusal (`None` when it merely matched no rule).
    fn combine(
        &self,
        cx: &mut Cx,
        variants: &[Vec<Token>],
        rule: &'static str,
        generic: bool,
        relaxed: bool,
    ) -> Result<Logic, Option<Refused>> {
        let mark = cx.discourse.mark();
        let mut merged: Option<Logic> = None;
        for v in variants {
            let outcome = {
                let mut sub = Cx::new(v, cx.lex, self, &mut *cx.discourse, cx.opts, cx.question);
                sub.relaxed_agreement = relaxed || cx.relaxed_agreement;
                sub.generic_subject = generic || cx.generic_subject;
                sub.depth = cx.depth + 1;
                self.parse(&mut sub)
            };
            match outcome {
                Ok(logic) => match &mut merged {
                    None => merged = Some(Logic { rule, ..logic }),
                    Some(m) => {
                        m.drs.extend(logic.drs);
                        m.declared.extend(logic.declared);
                        for c in logic.choices {
                            if !m.choices.contains(&c) {
                                m.choices.push(c);
                            }
                        }
                    }
                },
                Err(r) => {
                    cx.discourse.reset(mark);
                    let informative = !matches!(
                        r.reason,
                        Refusal::NoRule { .. } | Refusal::TrailingTokens { .. }
                    );
                    return Err(informative.then_some(r));
                }
            }
        }
        merged.ok_or(None)
    }

    fn parse_simple(&self, cx: &mut Cx) -> Result<Logic, Refused> {
        for (i, t) in cx.tokens.iter().enumerate() {
            if t.class == Class::Number {
                return Err(cx.refused(
                    Refusal::Unsupported {
                        construct: Construct::Numeric,
                        at: t.text.clone(),
                    },
                    i,
                ));
            }
            if let Some(c) = t.closed() {
                let after_noun = t.lower == "that" && i > 0 && cx.tokens[i - 1].is_open();
                let construct = if after_noun {
                    Some(Construct::RelativeClause)
                } else {
                    self.prescan
                        .iter()
                        .find(|(k, _)| *k == c)
                        .map(|(_, c)| c.clone())
                };
                if let Some(construct) = construct {
                    return Err(cx.refused(
                        Refusal::Unsupported {
                            construct,
                            at: t.text.clone(),
                        },
                        i,
                    ));
                }
            }
        }
        cx.generic_you = None;
        if cx.opts.addressee.is_none() {
            if let Some(class) = &cx.opts.generic_you {
                let second_person = cx.tokens[..cx.end()]
                    .iter()
                    .any(|t| pronoun_info(&t.lower).is_some_and(|p| p.person == 2));
                if second_person {
                    if !cx.lex.defined(class) {
                        return Err(cx.refused(
                            Refusal::TermNotInKb {
                                word: "you".into(),
                                term: class.clone(),
                            },
                            0,
                        ));
                    }
                    let v = cx.discourse.var("P");
                    cx.generic_you = Some((v, class.clone()));
                }
            }
        }
        let mut trailing: Option<Refused> = None;
        for rule in &self.sentence {
            cx.choices.clear();
            cx.declared = Drs::default();
            let names_before = cx.discourse.names.len();
            match rule.apply(cx, 0) {
                Step::Hit(mut logic, next) => {
                    if cx.is_end(next) {
                        logic.choices = cx.choices.clone();
                        logic.declared = std::mem::take(&mut cx.declared);
                        if let Some((v, class)) = cx.generic_you.take() {
                            let mut ante = Drs::default();
                            ante.bind(&v);
                            ante.atom("instance", vec![Term::Var(v), Term::Const(class)]);
                            logic.drs = Drs {
                                vars: Vec::new(),
                                conds: vec![Cond::Implies(ante, logic.drs)],
                            };
                        }
                        return Ok(logic);
                    }
                    cx.discourse.names.truncate(names_before);
                    if trailing.is_none() {
                        trailing = Some(cx.refused(
                            Refusal::TrailingTokens {
                                from: cx.text(next, next + 1),
                            },
                            next,
                        ));
                    }
                }
                Step::Miss => {
                    cx.discourse.names.truncate(names_before);
                    continue;
                }
                Step::Refuse(r) => return Err(r),
            }
        }
        // A modal the modal rule never reached (not after the subject) is
        // still the construct at fault.
        if let Some((i, t)) = cx
            .tokens
            .iter()
            .enumerate()
            .find(|(_, t)| t.is(Closed::Modal))
        {
            if trailing.is_none() {
                return Err(cx.refused(
                    Refusal::Unsupported {
                        construct: Construct::Modal,
                        at: t.text.clone(),
                    },
                    i,
                ));
            }
        }
        Err(trailing.unwrap_or_else(|| cx.no_rule()))
    }
}

/// CELT's preposition table for the relations SUMO defines.
///
/// This table is the one place English prepositions are mapped to SUMO
/// relations, and it is grammar data, not lexicon data: WordNet has no
/// prepositions, and SUMO has no relation that names them.  It is kept in
/// code for now.  The intended successor is a KB relation of the shape
/// `(prepositionRole EnglishLanguage "on" eventLocated Process Object)`
/// (language, preposition, relation, head class, complement class),
/// declared and populated in a constituent file beside `english_format.kif`,
/// so that prepositions extend the way vocabulary does (`termFormat`), the
/// complement test becomes any KB class rather than [`CeltType`], and a
/// different language or ontology supplies its own rows.  Rows found in
/// the KB would take precedence over this table, as labels outrank senses.
///
/// For an event head the location rows give `eventLocated`, SUMO's case
/// role for where a process happens; `located` is for objects.
pub fn standard_prepositions() -> Vec<PrepRule> {
    use CeltType::*;
    let row = |prep, complement, relation, event_only| PrepRule {
        prep,
        complement,
        relation,
        event_only,
    };
    vec![
        row("at", Some(Time), "time", true),
        row("in", Some(Time), "time", true),
        row("on", Some(Time), "time", true),
        row("during", Some(Time), "time", true),
        row("at", Some(Object), "eventLocated", true),
        row("in", Some(Object), "eventLocated", true),
        row("on", Some(Object), "eventLocated", true),
        row("near", Some(Object), "eventLocated", true),
        row("inside", Some(Object), "eventLocated", true),
        row("at", Some(Object), "located", false),
        row("in", Some(Object), "located", false),
        row("on", Some(Object), "located", false),
        row("near", Some(Object), "located", false),
        row("inside", Some(Object), "located", false),
        row("to", None, "destination", true),
        row("into", None, "destination", true),
        row("onto", None, "destination", true),
        row("toward", None, "destination", true),
        row("towards", None, "destination", true),
        row("from", None, "origin", true),
        row("with", Some(Person), "agent", true),
        row("with", Some(Object), "instrument", true),
        row("for", Some(Person), "destination", true),
        row("through", Some(Object), "traverses", true),
        row("across", Some(Object), "traverses", true),
    ]
}

/// The relation "have" or a possessive expresses between an owner (a
/// class, or an instance term) and an owned class, by the owned thing's
/// type: an anatomical structure or an artifact part is a `part` of the
/// owner (`true` = owned is the first argument), anything else an agent
/// owns is `possesses`.  `None` when no reading applies (a person as the
/// owned thing, "a dog has an owner", names a role, not a possession).
pub fn possession_relation(
    cx: &Cx,
    owner: Option<&str>,
    owned: &str,
) -> Option<(&'static str, bool)> {
    let under =
        |term: &str, ancestors: &[&str]| ancestors.iter().any(|a| cx.lex.has_ancestor(term, a));
    let anatomical = under(owned, &["AnatomicalStructure", "BodyPart", "BodySubstance"]);
    let (relation, owned_is_part) = if anatomical {
        ("part", true)
    } else if under(owned, &["Human", "SocialRole", "OccupationalRole"]) {
        return None;
    } else if owner.is_some_and(|o| under(o, &["Artifact"])) && under(owned, &["Artifact"]) {
        ("part", true)
    } else if owner.is_some_and(|o| under(o, &["AutonomousAgent"])) && under(owned, &["Object"]) {
        ("possesses", false)
    } else {
        return None;
    };
    cx.lex
        .defined(relation)
        .then_some((relation, owned_is_part))
}

/// Helper for rules: the role atom for a prepositional phrase attached to
/// `head` (an event term or the copula subject).
pub fn attach_pp(
    cx: &mut Cx,
    head: &Term,
    pp: &Pp,
    event: bool,
    scope: Scope,
) -> Result<Drs, Refused> {
    let real = pp.np.realize(cx, scope)?;
    let ctype = match (&real.class, &real.term) {
        (Some(c), _) => cx.lex.celt_type(c),
        (None, Term::Const(n)) => cx.lex.celt_type(n),
        _ => CeltType::Object,
    };
    let Some(rule) = cx.grammar.prep_relation(&pp.prep, ctype, event) else {
        return Err(cx.refused(
            Refusal::UnmappedPreposition {
                prep: pp.prep.clone(),
                complement: ctype.as_str().into(),
            },
            pp.at,
        ));
    };
    if !cx.lex.defined(rule.relation) {
        return Err(cx.refused(
            Refusal::TermNotInKb {
                word: pp.prep.clone(),
                term: rule.relation.into(),
            },
            pp.at,
        ));
    }
    let mut drs = real.drs;
    drs.conds.push(Cond::Atom(
        rule.relation.to_string(),
        vec![head.clone(), real.term],
    ));
    Ok(drs)
}
