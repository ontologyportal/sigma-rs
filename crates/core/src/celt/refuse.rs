//! Why a sentence was not translated.
//!
//! The controlled language never guesses.  Every failure is one
//! [`Refusal`], with a stable machine code ([`Refusal::code`]), a human
//! explanation (`Display`) and a rewrite hint ([`Refusal::hint`]).  The
//! taxonomy is the contract between the parser and its users: a new grammar
//! rule that covers a construct removes an [`Unsupported`] refusal; a new
//! `termFormat` label in the KB removes an `UnknownWord` one.

use std::fmt;

/// English constructs the parser recognizes but does not (yet) translate.
/// Each one is a candidate for a new rule in `celt::rules`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Construct {
    Pronoun,
    Passive,
    Modal,
    Coordination,
    Subordination,
    RelativeClause,
    Numeric,
    Possessive,
    Generic,
    Progressive,
    Perfect,
    Have,
    Adverb,
    Existential,
    Demonstrative,
    Imperative,
    PastTense,
    Identity,
    StativeAdjunct,
    QuantifiedPredicate,
    Other(String),
}

impl Construct {
    pub fn code(&self) -> &str {
        match self {
            Construct::Pronoun => "pronoun",
            Construct::Passive => "passive",
            Construct::Modal => "modal",
            Construct::Coordination => "coordination",
            Construct::Subordination => "subordination",
            Construct::RelativeClause => "relative-clause",
            Construct::Numeric => "numeric",
            Construct::Possessive => "possessive",
            Construct::Generic => "generic",
            Construct::Progressive => "progressive",
            Construct::Perfect => "perfect",
            Construct::Have => "have",
            Construct::Adverb => "adverb",
            Construct::Existential => "existential-there",
            Construct::Demonstrative => "demonstrative",
            Construct::Imperative => "imperative",
            Construct::PastTense => "past-tense",
            Construct::Identity => "identity",
            Construct::StativeAdjunct => "stative-adjunct",
            Construct::QuantifiedPredicate => "quantified-predicate",
            Construct::Other(s) => s,
        }
    }

    fn hint(&self) -> &str {
        match self {
            Construct::Pronoun => "repeat the noun phrase (\"the man\") instead of a pronoun",
            Construct::Passive => "use the active voice: \"Bob reads the book\"",
            Construct::Modal => {
                "state the plain fact; modals (can, must, should) are not translated"
            }
            Construct::Coordination => {
                "write one clause per sentence; \"and\"/\"or\" are not translated"
            }
            Construct::Subordination => {
                "write the main clause alone; conditions and reasons are not translated"
            }
            Construct::RelativeClause => "split the relative clause into its own sentence",
            Construct::Numeric => "counts and measures are not translated; use a determiner",
            Construct::Possessive => {
                "possessives are not translated; state ownership in its own sentence: \"Bob owns a book\""
            }
            Construct::Generic => {
                "bare plurals with an action verb are ambiguous; use \"every\" or \"some\""
            }
            Construct::Progressive => "use the simple present or past: \"Bob reads\", \"Bob read\"",
            Construct::Perfect => "use the simple past: \"Bob read the book\"",
            Construct::Have => "for possession write \"owns\"; the perfect tense is not translated",
            Construct::Adverb => "drop the adverb; only nouns, verbs and adjectives carry meaning",
            Construct::Existential => "write \"A dog is in the garden\" instead of \"There is\"",
            Construct::Demonstrative => "use \"the\" or a proper name instead of this/that",
            Construct::Imperative => "sentences need a subject",
            Construct::PastTense => {
                "define Now in the KB, e.g. (instance Now TimePoint), or use the present tense"
            }
            Construct::Identity => "\"X is the Y\" states identity; use \"a\" for class membership",
            Construct::StativeAdjunct => {
                "a stative verb (owns, likes) takes no time or place; state it in another sentence"
            }
            Construct::QuantifiedPredicate => {
                "the predicate noun phrase takes \"a\", \"an\" or no determiner"
            }
            Construct::Other(_) => "this construct has no grammar rule",
        }
    }
}

/// The reason a sentence was refused.  See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The word has no WordNet sense and no `termFormat` label in the KB.
    UnknownWord { word: String },
    /// WordNet knows the word but none of its senses anchors to SUMO.
    NoSumoAnchor { word: String, pos: &'static str },
    /// The word anchors to a SUMO term the loaded KB does not define.
    TermNotInKb { word: String, term: String },
    /// A capitalized word that is neither a KB instance nor a common noun.
    UnknownName { name: String },
    /// A noun in class position grounds to something that is not a class.
    NotAClass { word: String, term: String },
    /// An adjective (or predicate noun) does not ground to an Attribute.
    NotAnAttribute { word: String, term: String },
    /// A verb grounds to neither a Process class nor a binary relation.
    NotAnEvent { verb: String, term: String },
    /// The verb's term is too generic to say anything (Process, Entity, ...).
    GenericTerm { word: String, term: String },
    /// An adjective modifies a noun phrase that names a class.
    ModifiedClass { word: String },
    /// Both sides of a taxonomic statement ground to the same term.
    TrivialFact { term: String },
    /// A taxonomic statement straddles a top-level SUMO partition.
    CrossCategory {
        sub: String,
        sup: String,
        category: String,
    },
    /// The subject cannot fill the case role the verb requires.
    RoleMismatch {
        word: String,
        term: String,
        role: String,
        expected: String,
    },
    /// No preposition-table entry for this preposition and complement type.
    UnmappedPreposition { prep: String, complement: String },
    /// A definite noun phrase matches more than one earlier referent.
    AmbiguousReference {
        phrase: String,
        candidates: Vec<String>,
    },
    /// A recognized construct outside the controlled language.
    Unsupported { construct: Construct, at: String },
    /// No sentence rule matched; `expected` lists what the rules wanted at
    /// the furthest point reached.
    NoRule { at: String, expected: Vec<String> },
    /// A rule matched a prefix but words were left over.
    TrailingTokens { from: String },
    /// A stative verb needs a direct object.
    MissingObject { verb: String, relation: String },
    /// The sentence has no words.
    Empty,
}

impl Refusal {
    /// Stable kebab-case code for histograms and tests.
    pub fn code(&self) -> String {
        match self {
            Refusal::UnknownWord { .. } => "unknown-word".into(),
            Refusal::NoSumoAnchor { .. } => "no-sumo-anchor".into(),
            Refusal::TermNotInKb { .. } => "term-not-in-kb".into(),
            Refusal::UnknownName { .. } => "unknown-name".into(),
            Refusal::NotAClass { .. } => "not-a-class".into(),
            Refusal::NotAnAttribute { .. } => "not-an-attribute".into(),
            Refusal::NotAnEvent { .. } => "not-an-event".into(),
            Refusal::GenericTerm { .. } => "generic-term".into(),
            Refusal::ModifiedClass { .. } => "modified-class".into(),
            Refusal::TrivialFact { .. } => "trivial-fact".into(),
            Refusal::CrossCategory { .. } => "cross-category".into(),
            Refusal::RoleMismatch { .. } => "role-mismatch".into(),
            Refusal::UnmappedPreposition { .. } => "unmapped-preposition".into(),
            Refusal::AmbiguousReference { .. } => "ambiguous-reference".into(),
            Refusal::Unsupported { construct, .. } => format!("unsupported/{}", construct.code()),
            Refusal::NoRule { .. } => "no-rule".into(),
            Refusal::TrailingTokens { .. } => "trailing-tokens".into(),
            Refusal::MissingObject { .. } => "missing-object".into(),
            Refusal::Empty => "empty".into(),
        }
    }

    /// How to rewrite the sentence, or how to extend the KB or grammar.
    pub fn hint(&self) -> String {
        match self {
            Refusal::UnknownWord { word } => format!(
                "add a (termFormat EnglishLanguage <Term> \"{word}\") label to the KB, or use a WordNet word"
            ),
            Refusal::NoSumoAnchor { word, .. } | Refusal::TermNotInKb { word, .. } => format!(
                "add a (termFormat EnglishLanguage <Term> \"{word}\") label for a term in the loaded KB"
            ),
            Refusal::UnknownName { name } => format!(
                "declare the name in the KB, e.g. (instance {name} Human), or enable assumed names"
            ),
            Refusal::NotAClass { word, .. } => format!("use a noun that names a class instead of \"{word}\""),
            Refusal::NotAnAttribute { word, .. } => {
                format!("no Attribute instance is labeled \"{word}\"; add one or use another adjective")
            }
            Refusal::NotAnEvent { verb, .. } => {
                format!("\"{verb}\" needs a Process class or a binary relation label in the KB")
            }
            Refusal::GenericTerm { word, .. } => format!("use a more specific verb or noun than \"{word}\""),
            Refusal::ModifiedClass { .. } => "state the adjective in its own sentence about an instance".into(),
            Refusal::TrivialFact { .. } => "both nouns mean the same term; nothing to say".into(),
            Refusal::CrossCategory { .. } => "likely a wrong word sense; pick a more specific noun".into(),
            Refusal::RoleMismatch { role, expected, .. } => {
                format!("the subject must be a {expected} to be the {role} of this verb")
            }
            Refusal::UnmappedPreposition { prep, .. } => {
                format!("add a preposition rule for \"{prep}\" with this complement type")
            }
            Refusal::AmbiguousReference { .. } => "name the referent or use a more specific noun".into(),
            Refusal::Unsupported { construct, .. } => construct.hint().into(),
            Refusal::NoRule { expected, .. } if expected.is_empty() => "no sentence rule applies".into(),
            Refusal::NoRule { expected, .. } => format!("expected {}", expected.join(" or ")),
            Refusal::TrailingTokens { .. } => "the sentence continues past the recognized pattern".into(),
            Refusal::MissingObject { .. } => "stative verbs need an object: \"Bob owns a book\"".into(),
            Refusal::Empty => "nothing to translate".into(),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::UnknownWord { word } => write!(f, "unknown word \"{word}\""),
            Refusal::NoSumoAnchor { word, pos } => {
                write!(f, "\"{word}\" ({pos}) has no SUMO anchor in the lexicon")
            }
            Refusal::TermNotInKb { word, term } => {
                write!(f, "\"{word}\" means {term}, which the loaded KB does not define")
            }
            Refusal::UnknownName { name } => write!(f, "unknown name \"{name}\""),
            Refusal::NotAClass { word, term } => write!(f, "\"{word}\" means {term}, which is not a class"),
            Refusal::NotAnAttribute { word, term } => {
                write!(f, "\"{word}\" means {term}, which is not an Attribute instance")
            }
            Refusal::NotAnEvent { verb, term } => {
                write!(f, "verb \"{verb}\" means {term}, which is neither a Process nor a binary relation")
            }
            Refusal::GenericTerm { word, term } => {
                write!(f, "\"{word}\" means only {term}, too generic to assert")
            }
            Refusal::ModifiedClass { word } => {
                write!(f, "adjective \"{word}\" modifies a noun phrase that names a class")
            }
            Refusal::TrivialFact { term } => write!(f, "both sides mean {term}; the fact is vacuous"),
            Refusal::CrossCategory { sub, sup, category } => write!(
                f,
                "{sub} and {sup} disagree on the {category} partition; refusing a cross-category fact"
            ),
            Refusal::RoleMismatch { word, term, role, expected } => write!(
                f,
                "\"{word}\" ({term}) cannot be the {role}: the verb requires a {expected}"
            ),
            Refusal::UnmappedPreposition { prep, complement } => {
                write!(f, "no role for preposition \"{prep}\" with a {complement} complement")
            }
            Refusal::AmbiguousReference { phrase, candidates } => write!(
                f,
                "\"{phrase}\" could refer to any of {}",
                candidates.join(", ")
            ),
            Refusal::Unsupported { construct, at } => {
                write!(f, "unsupported {} at \"{at}\"", construct.code())
            }
            Refusal::NoRule { at, .. } => write!(f, "no sentence pattern matched at \"{at}\""),
            Refusal::TrailingTokens { from } => write!(f, "words left over from \"{from}\""),
            Refusal::MissingObject { verb, relation } => {
                write!(f, "stative verb \"{verb}\" ({relation}) needs an object")
            }
            Refusal::Empty => write!(f, "empty sentence"),
        }
    }
}

/// A refusal anchored to a char span of the sentence text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub reason: Refusal,
    pub start: usize,
    pub end: usize,
}

impl Refused {
    pub fn new(reason: Refusal, start: usize, end: usize) -> Self {
        Self { reason, start, end }
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.reason.code(), self.reason)
    }
}
