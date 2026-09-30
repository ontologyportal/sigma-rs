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
//! Adding a construct = adding one rule file under `rules/` and registering
//! it in [`Grammar::standard`].

use crate::lexicon::Pos;

use super::ir::{Cond, Discourse, Drs, Term};
use super::lookup::{CeltType, Choice, Inflection, Lookup, Verb};
use super::refuse::{Construct, Refusal, Refused};
use super::token::{Class, Closed, Det, Token};
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
    fn noun_lemma(&self, word: &str) -> (String, bool);
    fn noun_class(&self, word: &str) -> Result<Choice, Refusal>;
    fn predicate_noun(&self, word: &str) -> Result<(Choice, bool), Refusal>;
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
    fn noun_lemma(&self, word: &str) -> (String, bool) {
        Lookup::noun_lemma(self, word)
    }
    fn noun_class(&self, word: &str) -> Result<Choice, Refusal> {
        Lookup::noun_class(self, word)
    }
    fn predicate_noun(&self, word: &str) -> Result<(Choice, bool), Refusal> {
        Lookup::predicate_noun(self, word)
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

/// Translation options.
#[derive(Debug, Clone, Default)]
pub struct Opts {
    /// Treat an unknown capitalized word as the name of a Human instead of
    /// refusing it: emits `(instance Mike Human)` and `(names "Mike" Mike)`.
    pub assume_human_names: bool,
    /// Words that are always proper names of Humans (a first-name list),
    /// even where WordNet has a common-noun sense for them ("Mike").
    pub names: Vec<String>,
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
        Self {
            tokens,
            lex,
            grammar,
            discourse,
            opts,
            question,
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

    /// Parse a noun phrase at `at` with the grammar's NP rules.
    pub fn np(&mut self, at: usize) -> Step<Np> {
        for rule in &self.grammar.np {
            match rule.apply(self, at) {
                Step::Miss => continue,
                hit => return hit,
            }
        }
        Step::Miss
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
}

/// A parsed noun phrase (syntax only; grounding happens on realization).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Np {
    pub det: Option<Det>,
    /// Adjective words preceding the head.
    pub mods: Vec<String>,
    pub head: Head,
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
        match (&self.head, self.det) {
            (Head::Noun { plural, .. }, None) => *plural,
            (Head::Noun { .. }, Some(Det::Indefinite | Det::Universal)) => true,
            _ => false,
        }
    }

    pub fn is_referential(&self) -> bool {
        matches!(self.head, Head::Name(_) | Head::NewName(_))
            || matches!(self.det, Some(Det::Definite))
    }

    /// The word of the head, for messages.
    pub fn head_word(&self) -> &str {
        match &self.head {
            Head::Name(n) | Head::NewName(n) => n,
            Head::Noun { word, .. } => word,
        }
    }

    /// Ground the head as a class, refusing modifiers (a modified class
    /// phrase like "a red car" names no SUMO class).
    pub fn class(&self, cx: &mut Cx) -> Result<Choice, Refused> {
        if let Some(m) = self.mods.first() {
            return Err(cx.refused(Refusal::ModifiedClass { word: m.clone() }, self.start));
        }
        match &self.head {
            Head::Noun { word, .. } => {
                let c = cx
                    .lex
                    .noun_class(word)
                    .map_err(|r| cx.refused(r, self.end - 1))?;
                cx.record(&c);
                Ok(c)
            }
            Head::Name(n) | Head::NewName(n) => Err(cx.refused(
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
        let (term, class) = match &self.head {
            Head::Name(n) => (Term::Const(n.clone()), None),
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
                (Term::Const(n.clone()), Some("Human".to_string()))
            }
            Head::Noun { word, .. } => {
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
                (term, Some(class))
            }
        };
        for m in &self.mods {
            let a = cx.lex.adjective(m).map_err(|r| cx.refused(r, self.start))?;
            cx.record(&a);
            drs.atom("attribute", vec![term.clone(), Term::Const(a.term)]);
        }
        Ok(Realized { term, class, drs })
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

/// A noun-phrase rule.
pub trait NpRule: Send + Sync {
    fn name(&self) -> &'static str;
    fn apply(&self, cx: &mut Cx, at: usize) -> Step<Np>;
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
        g.sentence.push(Box::new(rules::event::Event));
        g.prepositions = standard_prepositions();
        g.prescan = vec![
            (Closed::Pronoun, Construct::Pronoun),
            (Closed::Conjunction, Construct::Coordination),
            (Closed::Subordinator, Construct::Subordination),
            (Closed::Modal, Construct::Modal),
            (Closed::RelMarker, Construct::RelativeClause),
            (Closed::Have, Construct::Have),
            (Closed::Possessive, Construct::Possessive),
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
                if let Some((_, construct)) = self.prescan.iter().find(|(k, _)| *k == c) {
                    return Err(cx.refused(
                        Refusal::Unsupported {
                            construct: construct.clone(),
                            at: t.text.clone(),
                        },
                        i,
                    ));
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
        Err(trailing.unwrap_or_else(|| cx.no_rule()))
    }
}

/// CELT's preposition table for the relations SUMO defines.
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
