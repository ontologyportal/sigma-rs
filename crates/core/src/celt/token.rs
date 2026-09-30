//! Sentence splitting and tokenization for the controlled language.
//!
//! The tokenizer knows only the CLOSED classes of English (determiners,
//! copula forms, auxiliaries, prepositions, pronouns, conjunctions, ...).
//! Every other word is [`Class::Open`] and is classified later, in position,
//! by the grammar rules asking the lexicon.  Punctuation is kept as tokens so
//! rules can see it; nothing is silently dropped.

/// Determiner subclasses that change the logical force of a noun phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Det {
    /// `a`, `an`
    Indefinite,
    /// `the`
    Definite,
    /// `some`
    Some,
    /// `every`, `all`, `each`
    Universal,
    /// `no`
    Negative,
    /// `this`, `that`, `these`, `those`
    Demonstrative,
}

/// Grammatical tense carried by a copula or auxiliary form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tense {
    Present,
    Past,
}

/// Grammatical number carried by a copula or auxiliary form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Number {
    Singular,
    Plural,
}

/// A closed-class word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closed {
    Det(Det),
    /// `is`, `are`, `was`, `were`
    Copula(Tense, Number),
    /// `do`, `does`, `did`
    Aux(Tense, Number),
    /// `not`, or the `n't` split off a contraction
    Not,
    Pronoun,
    Preposition,
    /// `and`, `or`, `but`
    Conjunction,
    /// `if`, `because`, `when`, `while`, `although`, ...
    Subordinator,
    /// `can`, `could`, `may`, `might`, `must`, `shall`, `should`, `will`, `would`
    Modal,
    /// `that`, `which`, `who`, `whom`, `whose` in relative position
    RelMarker,
    /// `has`, `have`, `had`
    Have,
    /// `there` (existential)
    There,
    /// `'s` possessive clitic
    Possessive,
}

/// Token class: closed-class word, open-class word, number, or punctuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Closed(Closed),
    Open,
    Number,
    Punct(char),
}

/// One token with its source span (char offsets into the sentence text).
#[derive(Debug, Clone)]
pub struct Token {
    pub text: String,
    pub lower: String,
    pub class: Class,
    pub start: usize,
    pub end: usize,
    /// Written with an initial capital letter.
    pub capitalized: bool,
    /// First token of its sentence (capitalization is not evidence there).
    pub sentence_initial: bool,
}

impl Token {
    pub fn is_open(&self) -> bool {
        self.class == Class::Open
    }
    pub fn closed(&self) -> Option<Closed> {
        match self.class {
            Class::Closed(c) => Some(c),
            _ => None,
        }
    }
    pub fn is(&self, c: Closed) -> bool {
        self.class == Class::Closed(c)
    }
    pub fn is_punct(&self, ch: char) -> bool {
        self.class == Class::Punct(ch)
    }
}

/// One sentence of the input: its text, char offset in the document, tokens
/// (terminal punctuation included), and whether it ended with `?`.
#[derive(Debug, Clone)]
pub struct Sentence {
    pub text: String,
    pub offset: usize,
    pub tokens: Vec<Token>,
    pub question: bool,
}

/// Split a document into sentences on `.`, `?`, `!` followed by whitespace
/// or end of input, then tokenize each.  No abbreviation handling: the
/// controlled language has none.
pub fn sentences(text: &str) -> Vec<Sentence> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let terminal =
            matches!(c, '.' | '?' | '!') && chars.get(i + 1).is_none_or(|n| n.is_whitespace());
        if terminal {
            push_sentence(&chars, start, i + 1, &mut out);
            start = i + 1;
        }
        i += 1;
    }
    push_sentence(&chars, start, chars.len(), &mut out);
    out
}

fn push_sentence(chars: &[char], start: usize, end: usize, out: &mut Vec<Sentence>) {
    let mut s = start;
    while s < end && chars[s].is_whitespace() {
        s += 1;
    }
    let mut e = end;
    while e > s && chars[e - 1].is_whitespace() {
        e -= 1;
    }
    if s >= e {
        return;
    }
    let text: String = chars[s..e].iter().collect();
    let tokens = tokenize(&text);
    let question = tokens.last().is_some_and(|t| t.is_punct('?'));
    out.push(Sentence {
        text,
        offset: s,
        tokens,
        question,
    });
}

/// Tokenize one sentence.  Words are maximal runs of letters, digits,
/// hyphens and apostrophes; `n't` splits into `not`; a trailing `'s` becomes
/// a [`Closed::Possessive`] token; everything else is one punctuation token.
pub fn tokenize(sentence: &str) -> Vec<Token> {
    let chars: Vec<char> = sentence.chars().collect();
    let mut out: Vec<Token> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_alphanumeric() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '-' || chars[i] == '\'')
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            push_word(word, start, &mut out);
            continue;
        }
        out.push(Token {
            text: c.to_string(),
            lower: c.to_string(),
            class: Class::Punct(c),
            start: i,
            end: i + 1,
            capitalized: false,
            sentence_initial: out.is_empty(),
        });
        i += 1;
    }
    out
}

fn push_word(word: String, start: usize, out: &mut Vec<Token>) {
    let lower = word.to_lowercase();
    let len = word.chars().count();
    if let Some(base) = lower.strip_suffix("n't") {
        let base_text: String = word.chars().take(len - 3).collect();
        let base = match base {
            "ca" => "can".to_string(),
            "wo" => "will".to_string(),
            "sha" => "shall".to_string(),
            b => b.to_string(),
        };
        push_plain(base_text, base, start, start + len - 3, out);
        push_plain(
            "n't".into(),
            "not".into(),
            start + len - 3,
            start + len,
            out,
        );
        return;
    }
    if let Some(base) = lower.strip_suffix("'s") {
        let base_text: String = word.chars().take(len - 2).collect();
        push_plain(base_text, base.to_string(), start, start + len - 2, out);
        out.push(Token {
            text: "'s".into(),
            lower: "'s".into(),
            class: Class::Closed(Closed::Possessive),
            start: start + len - 2,
            end: start + len,
            capitalized: false,
            sentence_initial: false,
        });
        return;
    }
    push_plain(word, lower, start, start + len, out);
}

fn push_plain(text: String, lower: String, start: usize, end: usize, out: &mut Vec<Token>) {
    let capitalized = text.chars().next().is_some_and(|c| c.is_uppercase());
    let class = if lower
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
    {
        Class::Number
    } else {
        classify(&lower).map_or(Class::Open, Class::Closed)
    };
    out.push(Token {
        text,
        lower,
        class,
        start,
        end,
        capitalized,
        sentence_initial: out.is_empty(),
    });
}

/// Prepositions the grammar can see.  Whether a preposition has a SUMO role
/// mapping is decided by the preposition table in the grammar, not here.
pub const PREPOSITIONS: &[&str] = &[
    "at", "in", "on", "to", "into", "onto", "from", "with", "for", "through", "across", "within",
    "until", "after", "before", "of", "by", "near", "under", "over", "behind", "beside", "above",
    "below", "inside", "toward", "towards", "during", "about", "without",
];

fn classify(lower: &str) -> Option<Closed> {
    use Closed::*;
    Some(match lower {
        "a" | "an" => Det(self::Det::Indefinite),
        "the" => Det(self::Det::Definite),
        "some" => Det(self::Det::Some),
        "every" | "all" | "each" => Det(self::Det::Universal),
        "no" => Det(self::Det::Negative),
        "this" | "that" | "these" | "those" => Det(self::Det::Demonstrative),
        "is" => Copula(Tense::Present, Number::Singular),
        "are" => Copula(Tense::Present, Number::Plural),
        "was" => Copula(Tense::Past, Number::Singular),
        "were" => Copula(Tense::Past, Number::Plural),
        "does" => Aux(Tense::Present, Number::Singular),
        "do" => Aux(Tense::Present, Number::Plural),
        "did" => Aux(Tense::Past, Number::Plural),
        "not" => Not,
        "has" | "have" | "had" => Have,
        "there" => There,
        "i" | "me" | "my" | "mine" | "you" | "your" | "he" | "him" | "his" | "she" | "her"
        | "hers" | "it" | "its" | "we" | "us" | "our" | "they" | "them" | "their" | "myself"
        | "himself" | "herself" | "itself" | "themselves" => Pronoun,
        "and" | "or" | "but" | "nor" => Conjunction,
        "if" | "then" | "because" | "when" | "while" | "although" | "though" | "unless"
        | "since" | "so" | "whether" => Subordinator,
        "can" | "could" | "may" | "might" | "must" | "shall" | "should" | "will" | "would"
        | "ought" => Modal,
        "which" | "who" | "whom" | "whose" => RelMarker,
        w if PREPOSITIONS.contains(&w) => Preposition,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sentences_and_marks_questions() {
        let s = sentences("Mike reads the book. Is Rex a dog?  Bob runs.");
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].text, "Mike reads the book.");
        assert!(!s[0].question);
        assert!(s[1].question);
        assert_eq!(s[2].offset, 36);
    }

    #[test]
    fn tokenizes_closed_classes_and_punctuation() {
        let t = tokenize("The dog isn't a cat.");
        let classes: Vec<Class> = t.iter().map(|t| t.class).collect();
        assert_eq!(
            classes,
            vec![
                Class::Closed(Closed::Det(Det::Definite)),
                Class::Open,
                Class::Closed(Closed::Copula(Tense::Present, Number::Singular)),
                Class::Closed(Closed::Not),
                Class::Closed(Closed::Det(Det::Indefinite)),
                Class::Open,
                Class::Punct('.'),
            ]
        );
        assert_eq!(t[3].text, "n't");
        assert_eq!(t[3].start, 10);
        assert!(t[0].capitalized && t[0].sentence_initial);
        assert!(!t[1].sentence_initial);
    }

    #[test]
    fn possessive_and_numbers() {
        let t = tokenize("Bob's 5 rats");
        assert_eq!(t[0].text, "Bob");
        assert!(t[1].is(Closed::Possessive));
        assert_eq!(t[2].class, Class::Number);
        assert_eq!(t[3].lower, "rats");
    }

    #[test]
    fn empty_and_whitespace_only() {
        assert!(sentences("").is_empty());
        assert!(sentences("   \n ").is_empty());
        assert_eq!(sentences("...").len(), 1);
    }
}
