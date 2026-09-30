//! Word-to-term grounding over [`KnowledgeBase::search`].
//!
//! Every content word is resolved through the shared search API with the
//! WordNet lexicon attached, so the vocabulary is exactly what `sumo search`
//! finds: `termFormat` labels of the loaded KB first, then WordNet senses in
//! most-frequent-sense order, restricted to terms the KB defines.  A word
//! the KB has no label for and WordNet does not anchor is an unknown word;
//! the fix is a `termFormat` axiom, not a parser change.
//!
//! The typing gates ([`Lookup::noun_class`], [`Lookup::verb`],
//! [`Lookup::adjective`]) are the only place the KB's taxonomy is consulted
//! for grounding.  Each gate returns one chosen [`Choice`] or a [`Refusal`].

use std::cell::RefCell;
use std::collections::HashMap;

use crate::kb::search::{SearchOpts, SearchSource};
use crate::kb::KnowledgeBase;
use crate::layer::{Layer, TopLayer};
use crate::lexicon::{MappingKind, Pos, WordNet};
use crate::types::SymbolId;

use super::refuse::Refusal;

/// Where a candidate came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A `(termFormat EnglishLanguage Term "word")` label equal to the word.
    Label,
    /// A WordNet synset anchored (`=` or `+`) to the term.
    WordNet,
}

/// One possible SUMO term for a word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub term: String,
    /// 1 for labels and the most frequent sense; higher = rarer sense.
    pub rank: usize,
    /// `dog#n#1=` for WordNet candidates, empty for labels.
    pub sense: String,
    pub source: Source,
    /// Label or `=` anchor (the term IS the concept), vs a `+` anchor.
    pub exact: bool,
}

/// The chosen term for a word, with the alternatives that were passed over,
/// so the translation can explain its lexical decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub word: String,
    pub term: String,
    pub sense: String,
    pub alternatives: Vec<String>,
}

/// CELT argument types (paper, Figure 8), derived from SUMO class membership
/// and used to select preposition roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CeltType {
    Time,
    Person,
    Object,
}

impl CeltType {
    pub fn as_str(self) -> &'static str {
        match self {
            CeltType::Time => "time",
            CeltType::Person => "person",
            CeltType::Object => "object",
        }
    }
}

/// How a verb grounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    /// A Process class: the verb reifies as an event instance.
    Event { class: String, choice: Choice },
    /// A binary relation: the verb is a stative and relates subject to
    /// object directly (`owns` -> `possesses`).
    Stative { relation: String, choice: Choice },
}

/// Verb inflection recognized by the morphology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inflection {
    /// Base form or third-person singular present.
    Present,
    /// Regular `-ed` or an irregular past form.
    Past,
    /// `-ing`
    Progressive,
}

const ENGLISH: &str = "EnglishLanguage";
const GENERIC_TERMS: &[&str] = &["Entity", "Physical", "Abstract", "Process", "Object"];

/// Memoizing word-to-term resolver bound to one KB and lexicon.
pub struct Lookup<'a, L: TopLayer + Layer> {
    kb: &'a KnowledgeBase<L>,
    wn: &'a WordNet,
    cache: RefCell<HashMap<(String, Pos), Vec<Candidate>>>,
}

impl<'a, L: TopLayer + Layer> Lookup<'a, L> {
    pub fn new(kb: &'a KnowledgeBase<L>, wn: &'a WordNet) -> Self {
        Self {
            kb,
            wn,
            cache: RefCell::new(HashMap::new()),
        }
    }

    pub fn kb(&self) -> &'a KnowledgeBase<L> {
        self.kb
    }

    // -- Part-of-speech oracles (syntax only, no KB typing) -------------------

    /// Does WordNet have a sense of `word` (surface form) as `pos`?
    pub fn has_sense(&self, word: &str, pos: Pos) -> bool {
        self.wn.senses(word).iter().any(|s| s.synset.pos == pos)
    }

    /// Does the KB carry an English label equal to `word`?
    pub fn has_label(&self, word: &str) -> bool {
        self.label_hits(word).next().is_some()
    }

    /// Does WordNet list `word` as a noun lemma with this exact
    /// capitalization ("Monday", "Europe")?  Such words are common nouns
    /// despite the capital letter.
    pub fn has_capitalized_lemma(&self, word: &str) -> bool {
        self.wn
            .senses(word)
            .iter()
            .any(|s| s.synset.pos == Pos::Noun && s.synset.words.iter().any(|w| w == word))
    }

    /// Is `word` usable as a noun: a noun sense or a KB label.
    pub fn is_nounish(&self, word: &str) -> bool {
        self.has_sense(word, Pos::Noun) || self.has_label(word)
    }

    /// Is `word` usable as an adjective: an adjective sense, or a label on an
    /// Attribute instance.
    pub fn is_adjectival(&self, word: &str) -> bool {
        self.senses_exact_pos(&word.to_lowercase(), Pos::Adj)
            || self
                .label_hits(word)
                .any(|sym| self.is_attribute_instance(&sym))
    }

    /// Is `word` (any inflection) usable as a verb.
    pub fn is_verbal(&self, word: &str) -> bool {
        self.verb_lemma(word).is_some()
    }

    // -- Morphology ------------------------------------------------------------

    /// Base form and inflection of a verb surface form, using WordNet's
    /// exception tables plus regular `-s`/`-es`/`-ies`/`-ed`/`-ing` rules.
    pub fn verb_lemma(&self, word: &str) -> Option<(String, Inflection)> {
        let w = word.to_lowercase();
        if self.senses_exact_pos(&w, Pos::Verb) {
            return Some((w, Inflection::Present));
        }
        let candidates: Vec<(String, Inflection)> = [
            (
                w.strip_suffix("ies").map(|s| format!("{s}y")),
                Inflection::Present,
            ),
            (
                w.strip_suffix("es").map(str::to_string),
                Inflection::Present,
            ),
            (w.strip_suffix('s').map(str::to_string), Inflection::Present),
            (
                w.strip_suffix("ied").map(|s| format!("{s}y")),
                Inflection::Past,
            ),
            (w.strip_suffix("ed").map(str::to_string), Inflection::Past),
            (w.strip_suffix('d').map(str::to_string), Inflection::Past),
            (doubled(&w, "ed"), Inflection::Past),
            (
                w.strip_suffix("ing").map(str::to_string),
                Inflection::Progressive,
            ),
            (
                w.strip_suffix("ing").map(|s| format!("{s}e")),
                Inflection::Progressive,
            ),
            (doubled(&w, "ing"), Inflection::Progressive),
        ]
        .into_iter()
        .filter_map(|(c, i)| c.map(|c| (c, i)))
        .collect();
        if let Some(hit) = candidates
            .into_iter()
            .find(|(c, _)| !c.is_empty() && self.senses_exact_pos(c, Pos::Verb))
        {
            return Some(hit);
        }
        // Irregular forms from verb.exc: past unless the form is a gerund.
        if let Some(lemma) = self.exception_base(&w, Pos::Verb) {
            let infl = if w.ends_with("ing") {
                Inflection::Progressive
            } else {
                Inflection::Past
            };
            return Some((lemma, infl));
        }
        // No WordNet verb: a KB label on a Process class or a binary
        // relation, or a binary relation's own name, still makes the word
        // a verb ("possesses").
        let labeled_verb = |form: &str| {
            self.verb_kind(form) == Some(false)
                || self
                    .label_hits(form)
                    .any(|sym| self.verb_kind(&sym).is_some())
        };
        if labeled_verb(&w) {
            return Some((w, Inflection::Present));
        }
        for base in [w.strip_suffix("es"), w.strip_suffix('s')]
            .into_iter()
            .flatten()
        {
            if labeled_verb(base) {
                return Some((base.to_string(), Inflection::Present));
            }
        }
        None
    }

    /// Base form of a noun surface form and whether it was plural.
    pub fn noun_lemma(&self, word: &str) -> (String, bool) {
        let w = word.to_lowercase();
        if let Some(base) = self.exception_base(&w, Pos::Noun) {
            return (base, true);
        }
        if self.senses_exact_pos(&w, Pos::Noun) || self.has_label(&w) {
            return (w, false);
        }
        for cand in [
            w.strip_suffix("ies").map(|s| format!("{s}y")),
            w.strip_suffix("es").map(str::to_string),
            w.strip_suffix('s').map(str::to_string),
        ]
        .into_iter()
        .flatten()
        {
            if !cand.is_empty()
                && (self.senses_exact_pos(&cand, Pos::Noun) || self.has_label(&cand))
            {
                return (cand, true);
            }
        }
        (w, false)
    }

    fn exception_base(&self, w: &str, pos: Pos) -> Option<String> {
        // `senses` consults noun.exc/verb.exc when the surface form itself
        // is not a lemma; the returned `lemma` is then the base form.
        if self.senses_exact_pos(w, pos) {
            return None;
        }
        self.wn
            .senses(w)
            .into_iter()
            .find(|s| s.synset.pos == pos && s.lemma != w)
            .map(|s| s.lemma)
    }

    fn senses_exact_pos(&self, w: &str, pos: Pos) -> bool {
        self.wn
            .senses(w)
            .iter()
            .any(|s| s.synset.pos == pos && s.lemma == w)
    }

    // -- Candidates --------------------------------------------------------------

    /// Ranked SUMO candidates for a base-form word as `pos`: KB labels first,
    /// then WordNet anchors in sense order.  Only terms the KB defines.
    pub fn candidates(&self, lemma: &str, pos: Pos) -> Vec<Candidate> {
        let key = (lemma.to_lowercase(), pos);
        if let Some(c) = self.cache.borrow().get(&key) {
            return c.clone();
        }
        let out = self.candidates_uncached(&key.0, pos);
        self.cache.borrow_mut().insert(key, out.clone());
        out
    }

    fn candidates_uncached(&self, lemma: &str, pos: Pos) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = Vec::new();
        for sym in self.label_hits(lemma) {
            if !out.iter().any(|c| c.term == sym) {
                out.push(Candidate {
                    term: sym,
                    rank: 1,
                    sense: String::new(),
                    source: Source::Label,
                    exact: true,
                });
            }
        }
        // The search API's WordNet pass dedups per term across parts of
        // speech, so the sense order is taken from the lexicon directly;
        // only terms the KB defines count, exactly as the search filters.
        let mut wn_hits: Vec<Candidate> = Vec::new();
        for sense in self.wn.senses(lemma) {
            if sense.synset.pos != pos || sense.lemma != lemma {
                continue;
            }
            for anchor in &sense.synset.sumo {
                let exact = match anchor.kind {
                    MappingKind::Equivalent => true,
                    MappingKind::Subsuming => false,
                    _ => continue,
                };
                if !self.defined(&anchor.term)
                    || out.iter().any(|c| c.term == anchor.term)
                    || wn_hits.iter().any(|c| c.term == anchor.term)
                {
                    continue;
                }
                wn_hits.push(Candidate {
                    term: anchor.term.clone(),
                    rank: sense.sense_no,
                    sense: format!("{}{}", sense.label(), anchor.kind.suffix()),
                    source: Source::WordNet,
                    exact,
                });
            }
        }
        out.extend(wn_hits);
        out
    }

    /// Symbols whose English `termFormat` equals `word` (case-insensitive).
    fn label_hits(&self, word: &str) -> impl Iterator<Item = String> + '_ {
        let query = word.replace('_', " ");
        let opts = SearchOpts {
            language: Some(ENGLISH),
            limit: None,
            ..SearchOpts::default()
        };
        let mut seen = std::collections::HashSet::new();
        self.kb
            .search(&query, &opts)
            .into_iter()
            .filter(move |h| {
                h.source == SearchSource::TermFormat && h.text.eq_ignore_ascii_case(&query)
            })
            .map(|h| h.symbol)
            .filter(move |s| seen.insert(s.clone()))
    }

    /// The no-sliding rule: when the most frequent sense (or a label) is not
    /// usable, a rarer subsuming anchor is a guess, not a translation.
    fn admissible(cands: Vec<Candidate>) -> Vec<Candidate> {
        if cands.iter().any(|c| c.rank == 1) {
            cands
        } else {
            cands.into_iter().filter(|c| c.exact).collect()
        }
    }

    /// Why a word has no usable candidates, in priority order.
    fn lexical_refusal(&self, word: &str, lemma: &str, pos: Pos) -> Refusal {
        let senses: Vec<_> = self
            .wn
            .senses(lemma)
            .into_iter()
            .filter(|s| s.synset.pos == pos)
            .collect();
        if senses.is_empty() {
            return Refusal::UnknownWord { word: word.into() };
        }
        let anchors: Vec<String> = senses
            .iter()
            .flat_map(|s| s.synset.sumo.iter())
            .filter(|a| matches!(a.kind, MappingKind::Equivalent | MappingKind::Subsuming))
            .map(|a| a.term.clone())
            .collect();
        match anchors.first() {
            None => Refusal::NoSumoAnchor {
                word: word.into(),
                pos: pos_name(pos),
            },
            Some(term) => Refusal::TermNotInKb {
                word: word.into(),
                term: term.clone(),
            },
        }
    }

    /// When the most frequent sense's anchor is missing from the KB, the
    /// no-sliding rule refuses the word for that reason rather than for a
    /// typing failure of some rarer sense.
    fn mfs_missing(&self, word: &str, lemma: &str, pos: Pos) -> Option<Refusal> {
        let sense = self
            .wn
            .senses(lemma)
            .into_iter()
            .find(|s| s.synset.pos == pos && s.sense_no == 1)?;
        let anchor = sense
            .synset
            .sumo
            .iter()
            .find(|a| matches!(a.kind, MappingKind::Equivalent | MappingKind::Subsuming))?;
        (!self.defined(&anchor.term)).then(|| Refusal::TermNotInKb {
            word: word.into(),
            term: anchor.term.clone(),
        })
    }

    fn choose(word: &str, mut cands: Vec<Candidate>) -> Choice {
        let first = cands.remove(0);
        Choice {
            word: word.into(),
            term: first.term,
            sense: first.sense,
            alternatives: cands.into_iter().map(|c| c.term).collect(),
        }
    }

    // -- Typing gates --------------------------------------------------------------

    /// A noun that must name a class (predicate nominal, generic subject).
    pub fn noun_class(&self, word: &str) -> Result<Choice, Refusal> {
        let (lemma, _) = self.noun_lemma(word);
        let all = self.candidates(&lemma, Pos::Noun);
        if all.is_empty() {
            return Err(self.lexical_refusal(word, &lemma, Pos::Noun));
        }
        let classes: Vec<Candidate> = all
            .iter()
            .filter(|c| self.sym(&c.term).is_some_and(|s| self.kb.is_class(s)))
            .cloned()
            .collect();
        let classes = Self::admissible(classes);
        if classes.is_empty() {
            return Err(self
                .mfs_missing(word, &lemma, Pos::Noun)
                .unwrap_or_else(|| Refusal::NotAClass {
                    word: word.into(),
                    term: all[0].term.clone(),
                }));
        }
        if GENERIC_TERMS.contains(&classes[0].term.as_str()) {
            return Err(Refusal::GenericTerm {
                word: word.into(),
                term: classes[0].term.clone(),
            });
        }
        Ok(Self::choose(word, classes))
    }

    /// A noun in predicate position after the copula: either a class
    /// (`a dog`) or an Attribute instance (`a pianist` -> Musician).
    pub fn predicate_noun(&self, word: &str) -> Result<(Choice, bool), Refusal> {
        let (lemma, _) = self.noun_lemma(word);
        let all = self.candidates(&lemma, Pos::Noun);
        if all.is_empty() {
            return Err(self.lexical_refusal(word, &lemma, Pos::Noun));
        }
        let usable: Vec<Candidate> = all
            .iter()
            .filter(|c| {
                self.is_attribute_instance(&c.term)
                    || self.sym(&c.term).is_some_and(|s| {
                        self.kb.is_class(s) && !self.kb.has_ancestor(s, "Attribute")
                    })
            })
            .cloned()
            .collect();
        let usable = Self::admissible(usable);
        match usable.first() {
            None => Err(self
                .mfs_missing(word, &lemma, Pos::Noun)
                .unwrap_or_else(|| Refusal::NotAClass {
                    word: word.into(),
                    term: all[0].term.clone(),
                })),
            Some(c) => {
                let is_attr = self.is_attribute_instance(&c.term);
                if !is_attr && GENERIC_TERMS.contains(&c.term.as_str()) {
                    return Err(Refusal::GenericTerm {
                        word: word.into(),
                        term: c.term.clone(),
                    });
                }
                Ok((Self::choose(word, usable), is_attr))
            }
        }
    }

    /// An adjective: must ground to an instance under `Attribute`.
    pub fn adjective(&self, word: &str) -> Result<Choice, Refusal> {
        let w = word.to_lowercase();
        let all = self.candidates(&w, Pos::Adj);
        if all.is_empty() {
            return Err(self.lexical_refusal(word, &w, Pos::Adj));
        }
        let attrs: Vec<Candidate> = all
            .iter()
            .filter(|c| self.is_attribute_instance(&c.term))
            .cloned()
            .collect();
        let attrs = Self::admissible(attrs);
        if attrs.is_empty() {
            return Err(self.mfs_missing(word, &w, Pos::Adj).unwrap_or_else(|| {
                Refusal::NotAnAttribute {
                    word: word.into(),
                    term: all[0].term.clone(),
                }
            }));
        }
        Ok(Self::choose(word, attrs))
    }

    /// A verb: a Process class (event) or a binary relation (stative).
    pub fn verb(&self, word: &str) -> Result<(Verb, Inflection), Refusal> {
        let Some((lemma, infl)) = self.verb_lemma(word) else {
            let w = word.to_lowercase();
            return Err(if self.wn.senses(&w).is_empty() && !self.has_label(&w) {
                Refusal::UnknownWord { word: word.into() }
            } else {
                Refusal::NoRule {
                    at: word.into(),
                    expected: vec!["a verb".into()],
                }
            });
        };
        let mut all = self.candidates(&lemma, Pos::Verb);
        // A label on the surface form ("possesses") or the gerund
        // ("reading"), or a binary relation named by the word itself, also
        // names the verb.
        for form in [word.to_lowercase(), format!("{lemma}ing"), lemma.clone()] {
            for c in self.candidates(&form, Pos::Verb) {
                if c.source == Source::Label && !all.iter().any(|a| a.term == c.term) {
                    all.push(c);
                }
            }
            if self.verb_kind(&form) == Some(false) && !all.iter().any(|a| a.term == form) {
                all.push(Candidate {
                    term: form.clone(),
                    rank: 1,
                    sense: String::new(),
                    source: Source::Label,
                    exact: true,
                });
            }
        }
        if all.is_empty() {
            return Err(self.lexical_refusal(word, &lemma, Pos::Verb));
        }
        let usable: Vec<Candidate> = all
            .iter()
            .filter(|c| self.verb_kind(&c.term).is_some())
            .cloned()
            .collect();
        let usable = Self::admissible(usable);
        let Some(first) = usable.first() else {
            return Err(self
                .mfs_missing(word, &lemma, Pos::Verb)
                .unwrap_or_else(|| Refusal::NotAnEvent {
                    verb: word.into(),
                    term: all[0].term.clone(),
                }));
        };
        if GENERIC_TERMS.contains(&first.term.as_str()) || first.term == "IntentionalProcess" {
            return Err(Refusal::GenericTerm {
                word: word.into(),
                term: first.term.clone(),
            });
        }
        let term = first.term.clone();
        let stative = self.verb_kind(&term) == Some(false);
        let choice = Self::choose(word, usable);
        let verb = if stative {
            Verb::Stative {
                relation: term,
                choice,
            }
        } else {
            Verb::Event {
                class: term,
                choice,
            }
        };
        Ok((verb, infl))
    }

    /// `Some(true)` for a Process class, `Some(false)` for a binary
    /// relation, `None` otherwise.
    fn verb_kind(&self, term: &str) -> Option<bool> {
        let s = self.sym(term)?;
        if self.kb.is_class(s) && self.kb.has_ancestor(s, "Process") {
            Some(true)
        } else if self.kb.is_relation(s) && !self.kb.is_function(s) && self.arity(s) == Some(2) {
            Some(false)
        } else {
            None
        }
    }

    /// A capitalized word as a proper name: the KB instance with that
    /// symbol name or English label.
    pub fn name(&self, word: &str) -> Option<String> {
        if let Some(s) = self.sym(word) {
            if self.kb.is_instance(s) {
                return Some(word.to_string());
            }
        }
        self.label_hits(word)
            .find(|sym| self.sym(sym).is_some_and(|s| self.kb.is_instance(s)))
    }

    // -- KB queries used by the rules ----------------------------------------------

    pub fn sym(&self, term: &str) -> Option<SymbolId> {
        self.kb.symbol_id(term)
    }

    pub fn defined(&self, term: &str) -> bool {
        self.sym(term).is_some()
    }

    pub fn has_ancestor(&self, term: &str, ancestor: &str) -> bool {
        self.sym(term)
            .is_some_and(|s| self.kb.has_ancestor(s, ancestor))
    }

    pub fn is_attribute_instance(&self, term: &str) -> bool {
        self.sym(term)
            .is_some_and(|s| self.kb.is_instance(s) && self.kb.has_ancestor(s, "Attribute"))
    }

    fn arity(&self, s: SymbolId) -> Option<i32> {
        self.kb.layer.semantic().arity(s)
    }

    /// Class of the instance-of domain of `relation` at 1-based `arg`.
    pub fn domain_class(&self, relation: &str, arg: usize) -> Option<String> {
        use crate::types::RelationDomain;
        match self.kb.domain(relation, arg)? {
            RelationDomain::Domain(id) | RelationDomain::DomainSubclass(id) => self.kb.sym_name(id),
            RelationDomain::Unknown => None,
        }
    }

    /// The CELT argument type of a class or instance (paper, Figure 8).
    pub fn celt_type(&self, term: &str) -> CeltType {
        if ["Human", "OccupationalRole", "SocialRole"]
            .iter()
            .any(|a| self.has_ancestor(term, a))
        {
            CeltType::Person
        } else if ["TimePosition", "TimeMeasure", "Process"]
            .iter()
            .any(|a| self.has_ancestor(term, a))
        {
            CeltType::Time
        } else {
            CeltType::Object
        }
    }

    /// Do two classes disagree on a top-level SUMO partition?  Catches wrong
    /// senses like `snow` -> Snowing (a Process) declared a subclass of Ice.
    pub fn cross_category(&self, sub: &str, sup: &str) -> Option<String> {
        ["Process", "Attribute", "Physical", "Abstract"]
            .iter()
            .find(|cat| self.has_ancestor(sub, cat) != self.has_ancestor(sup, cat))
            .map(|c| c.to_string())
    }
}

fn doubled(w: &str, suffix: &str) -> Option<String> {
    let stem = w.strip_suffix(suffix)?;
    let mut chars = stem.chars();
    let last = chars.next_back()?;
    let prev = chars.next_back()?;
    (last == prev && !"aeiou".contains(last)).then(|| stem[..stem.len() - 1].to_string())
}

fn pos_name(pos: Pos) -> &'static str {
    match pos {
        Pos::Noun => "noun",
        Pos::Verb => "verb",
        Pos::Adj => "adjective",
        Pos::Adv => "adverb",
    }
}
