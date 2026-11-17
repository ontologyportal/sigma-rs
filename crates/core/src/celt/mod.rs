//! Controlled English to Logic Translation (CELT).
//!
//! A deliberately tiny fragment of English that translates to SUO-KIF
//! grounded in the loaded KB, after Pease & Li, "Controlled English to
//! Logic Translation".  The fragment is a formal language that happens to
//! look like English: every accepted sentence has exactly one reading, and
//! every rejected sentence is rejected with a [`Refusal`] naming the word or
//! construct at fault and how to rewrite it.
//!
//! # Pipeline
//!
//! 1. [`token`]: split into sentences and tokens, tag closed-class words.
//! 2. [`grammar`]: the [`Grammar`] tries its sentence [`rules`] in order
//!    (first full match wins).  Rules ask the [`lookup`] layer to classify
//!    open words and ground them to SUMO terms through `KnowledgeBase::search`
//!    with the WordNet lexicon: `termFormat` labels first, then WordNet
//!    anchors in sense order, only terms the KB defines.
//! 3. [`ir`]: rules build a small DRS; [`Logic::formula`] closes it into a
//!    KIF `AstNode`.  Assertions bind referents to Skolem constants
//!    (`Book-1`) so each sentence is independently tellable and a later
//!    definite phrase ("the book") refers back; questions bind variables and
//!    become conjectures.
//!
//! # The controlled language (baseline)
//!
//! ```text
//! Rex is a dog.                  (instance Rex Dog)
//! A dog is an animal.            (subclass Dog Animal)      also "Dogs are animals."
//! Every dog is an animal.        (subclass Dog Animal)
//! Rex is loud.                   (attribute Rex Loud)
//! Every dog is loud.             (forall (?X1) (=> (instance ?X1 Dog) (attribute ?X1 Loud)))
//! No dog is a cat.               (not (exists (?X1) (and (instance ?X1 Dog) (instance ?X1 Cat))))
//! Bob is a pianist.              (attribute Bob Musician)   when the noun maps to an Attribute
//! Rex is in the garden.          (instance Garden-1 Garden) (located Rex Garden-1)
//! Mike reads the book.           (instance Reading-1 Reading) (agent Reading-1 Mike)
//!                                (instance Book-1 Book) (patient Reading-1 Book-1)
//! Mike does not read a book.     (not (exists (?E1 ?X1) (and ...)))
//! Bob owns a car.                (instance Car-1 Automobile) (possesses Bob Car-1)   stative verb
//! Every boy likes fudge.         (forall (?X1) (=> (instance ?X1 Boy) (exists ...)))
//! A foal is a young horse.       (subclass Foal Horse) (forall (?X1) (=> (instance ?X1 Foal) (attribute ?X1 Young)))
//! Birds have feathers.           (forall (?X1) (=> (instance ?X1 Bird) (exists (?X2) (and (instance ?X2 Feather) (part ?X2 ?X1)))))
//! Rex has a book.                (instance Book-1 Book) (possesses Rex Book-1)
//! Dogs and cats are animals.     (subclass Dog Animal) (subclass Cat Animal)
//! Rex is red, young, and loud.   (attribute Rex Red) (attribute Rex Young) (attribute Rex Loud)
//! A dog is an animal that barks. (subclass Dog Animal) (forall (?X1) (=> (instance ?X1 Dog) (exists ...Barking...)))
//! Is Rex a dog?                  conjecture (instance Rex Dog)
//! Does Mike read the book?       conjecture (exists (?E1) (and ...))
//! ```
//!
//! Words are classified in position by the rules, with three part-of-speech
//! checks: subject-verb number agreement ("The dog bark" matches no rule),
//! no proper-noun sense for a lowercase or sentence-initial word ("jays"
//! is the bird, not the statesman), and the tag-count prior from
//! `index.sense` ([`lookup`]: "make" is a verb 1612 times and a noun once,
//! so it is never a noun).  Compound sentences are decomposed first
//! ([`Grammar::decompose`]): coordination with "and"/"but" and a relative
//! clause on the predicate nominal become simple sentences that must all
//! translate.
//!
//! Conventions: with the copula an indefinite or bare subject is generic
//! ("A dog is an animal" is about the class); with an action verb it is a
//! particular ("A dog barks" introduces one dog) and a bare plural is refused
//! as ambiguous.  A capitalized word is a proper name unless it starts the
//! sentence and is a common noun, or the KB labels a term with it, or
//! WordNet lists it capitalized (Monday).
//!
//! Pronouns are bound, never guessed ([`grammar::Np::realize`]).  First
//! and second person are deictic: they come from [`Opts::speaker`],
//! [`Opts::addressee`] or the generic class [`Opts::generic_you`] (one
//! universal over the sentence, every "you"/"your" the same variable), and
//! are refused otherwise.  Third person is anaphoric: the most recent
//! compatible referent of an earlier sentence or clause, never a
//! co-argument of the pronoun's own clause (a reflexive binds only there).
//! Compatibility is read from the KB through [`Opts::pronoun_classes`] and
//! from [`Opts::name_genders`]; two candidates is `ambiguous-reference`,
//! none is a refusal ("It is cold" is `expletive-it`).  Possessives ("your
//! tail", "Bob's car") use the same typing as "have"
//! ([`grammar::possession_relation`]).
//!
//! Everything else is refused with a reason: unbound pronouns, "or",
//! subordinate clauses, relative clauses on the subject, modals, passives,
//! numbers, progressive and perfect aspect, adverbs, bare-plural action
//! generics ("Dogs chase cats", unless [`Opts::generic_subjects`]), and any
//! word the KB has no term for.
//!
//! # Extending the language
//!
//! - **Vocabulary**: add `(termFormat EnglishLanguage Term "word")` to the
//!   KB.  Labels outrank WordNet senses, so this also overrides a wrong
//!   most-frequent sense.  Proper names are KB instances (by symbol or
//!   label); [`Opts::assume_human_names`] accepts unknown ones as Humans.
//! - **Prepositions**: add a [`PrepRule`] row (preposition x complement
//!   type -> relation) with [`Grammar::preposition`].
//! - **Constructs**: write a rule (one file under `rules/`, implementing
//!   [`SentenceRule`] or [`NpRule`]) and register it with
//!   [`Grammar::sentence_rule`] / [`Grammar::np_rule`].  Each
//!   [`refuse::Construct`] variant is a construct waiting for its rule.
//!
//! [`Translation::histogram`] counts refusals by code, which is how to find
//! the next rule worth writing on a corpus.

pub mod grammar;
pub mod ir;
pub mod lookup;
pub mod refuse;
pub mod rules;
pub mod token;

pub use grammar::{Grammar, Kind, Logic, NpRule, Opts, PrepRule, PronounClasses, SentenceRule};
pub use lookup::{english_languages, Choice, Lookup};
pub use refuse::{Construct, Refusal, Refused};
pub use token::Gender;

use std::collections::BTreeMap;

use crate::kb::KnowledgeBase;
use crate::layer::{Layer, TopLayer};
use crate::lexicon::WordNet;
use crate::parse::ast::AstNode;

impl Logic {
    /// The closed KIF formula.
    pub fn formula(&self) -> AstNode {
        self.drs.to_ast()
    }

    /// Separately tellable formulas: the side assertions, then the
    /// top-level conjuncts of an assertion or the query formula.
    pub fn formulas(&self) -> Vec<AstNode> {
        let mut out = self.declared.conjuncts();
        match self.kind {
            Kind::Assert => out.extend(self.drs.conjuncts()),
            Kind::Query => out.push(self.formula()),
        }
        out
    }
}

/// Proper names from a `FirstNames.csv` text (`"name","gender"` rows, as
/// shipped with the SUMO WordNet mappings), for [`Opts::names`].
pub fn names_from_csv(text: &str) -> Vec<String> {
    name_genders_from_csv(text)
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

/// Proper names with their gender (`M`/`F`, else `None`) from a
/// `FirstNames.csv` text, for [`Opts::names`] and [`Opts::name_genders`].
pub fn name_genders_from_csv(text: &str) -> Vec<(String, Option<token::Gender>)> {
    text.lines()
        .filter_map(|l| {
            let mut cols = l.split(',').map(|c| c.trim().trim_matches('"'));
            let name = cols.next()?;
            if name.is_empty() || name == "name" {
                return None;
            }
            let gender = match cols.next() {
                Some("M") | Some("m") => Some(token::Gender::Male),
                Some("F") | Some("f") => Some(token::Gender::Female),
                _ => None,
            };
            Some((name.to_string(), gender))
        })
        .collect()
}

/// The result for one input sentence.
#[derive(Debug, Clone)]
pub struct SentenceResult {
    pub text: String,
    /// Char offset of the sentence in the document.
    pub offset: usize,
    pub outcome: Result<Logic, Refused>,
}

/// The translation of a document.
#[derive(Debug, Clone, Default)]
pub struct Translation {
    pub sentences: Vec<SentenceResult>,
}

impl Translation {
    /// Every assertion formula, in order, one per top-level conjunct,
    /// including the side assertions of queries.
    pub fn assertions(&self) -> Vec<AstNode> {
        self.sentences
            .iter()
            .filter_map(|s| s.outcome.as_ref().ok())
            .flat_map(|l| match l.kind {
                Kind::Assert => l.formulas(),
                Kind::Query => l.declared.conjuncts(),
            })
            .collect()
    }

    /// Every query formula (conjecture), in order.
    pub fn queries(&self) -> Vec<AstNode> {
        self.sentences
            .iter()
            .filter_map(|s| s.outcome.as_ref().ok())
            .filter(|l| l.kind == Kind::Query)
            .map(Logic::formula)
            .collect()
    }

    pub fn refused(&self) -> impl Iterator<Item = (&SentenceResult, &Refused)> {
        self.sentences
            .iter()
            .filter_map(|s| s.outcome.as_ref().err().map(|r| (s, r)))
    }

    /// True when every sentence translated.
    pub fn complete(&self) -> bool {
        self.sentences.iter().all(|s| s.outcome.is_ok())
    }

    /// Refusal codes with counts, most frequent first.
    pub fn histogram(&self) -> Vec<(String, usize)> {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for (_, r) in self.refused() {
            *m.entry(r.reason.code()).or_default() += 1;
        }
        let mut v: Vec<(String, usize)> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }
}

/// Translate `text` with the standard grammar.
pub fn translate<L: TopLayer + Layer>(
    kb: &KnowledgeBase<L>,
    wn: &WordNet,
    text: &str,
    opts: &Opts,
) -> Translation {
    translate_with(kb, wn, &Grammar::standard(), text, opts)
}

/// Translate `text` with a custom grammar.
pub fn translate_with<L: TopLayer + Layer>(
    kb: &KnowledgeBase<L>,
    wn: &WordNet,
    grammar: &Grammar,
    text: &str,
    opts: &Opts,
) -> Translation {
    let lookup = Lookup::new(kb, wn);
    let mut discourse = ir::Discourse::default();
    let mut out = Translation::default();
    for (i, sentence) in token::sentences(text).into_iter().enumerate() {
        discourse.begin_sentence(i);
        let outcome = {
            let mut cx = grammar::Cx::new(
                &sentence.tokens,
                &lookup,
                grammar,
                &mut discourse,
                opts,
                sentence.question,
            );
            cx.generic_subject = opts.generic_subjects;
            grammar.parse(&mut cx)
        };
        out.sentences.push(SentenceResult {
            text: sentence.text,
            offset: sentence.offset,
            outcome,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::Pos;

    const KIF: &str = r#"
        (subclass Physical Entity)
        (subclass Abstract Entity)
        (subclass Object Physical)
        (subclass Process Physical)
        (subclass Attribute Abstract)
        (subclass Relation Abstract)
        (subclass Predicate Relation)
        (subclass BinaryRelation Relation)
        (subclass BinaryPredicate Predicate)
        (subclass BinaryPredicate BinaryRelation)
        (subclass Formula Abstract)
        (instance believes BinaryPredicate)
        (domain believes 1 AutonomousAgent)
        (domain believes 2 Formula)
        (subclass AutonomousAgent Object)
        (subclass Animal AutonomousAgent)
        (subclass Dog Animal)
        (subclass Cat Animal)
        (subclass Bird Animal)
        (subclass Human Animal)
        (subclass Man Human)
        (subclass Woman Human)
        (subclass Collection Object)
        (instance Mary Woman)
        (subclass BodyPart Object)
        (subclass Feather BodyPart)
        (subclass Wing BodyPart)
        (subclass Tail BodyPart)
        (subclass Artifact Object)
        (subclass Engine Artifact)
        (instance modalAttribute BinaryPredicate)
        (subclass NormativeAttribute Attribute)
        (subclass AlethicAttribute NormativeAttribute)
        (subclass DeonticAttribute NormativeAttribute)
        (instance Possibility AlethicAttribute)
        (instance Necessity AlethicAttribute)
        (instance Obligation DeonticAttribute)
        (instance part BinaryPredicate)
        (domain part 1 Object)
        (domain part 2 Object)
        (subclass Boy Human)
        (instance wants BinaryPredicate)
        (domain wants 1 AutonomousAgent)
        (domain wants 2 Object)
        (instance desires BinaryPredicate)
        (domain desires 1 AutonomousAgent)
        (domain desires 2 Formula)
        (termFormat EnglishLanguage desires "desires")
        (instance Teacher OccupationalRole)
        (subclass Planet Object)
        (instance PlanetEarth Planet)
        (subclass Book Object)
        (subclass Garden Object)
        (subclass Automobile Object)
        (subclass Rock Object)
        (subclass Horse Animal)
        (subclass Foal Horse)
        (subclass Living Process)
        (instance Young AgeAttribute)
        (subclass AgeAttribute Attribute)
        (subclass Microphone Object)
        (termFormat EnglishLanguage Tuesday "Tuesday")
        (subclass Tuesday Day)
        (subclass Substance Object)
        (subclass Water Substance)
        (subclass Ice Substance)
        (termFormat EnglishLanguage Automobile "car")
        (subclass IntentionalProcess Process)
        (subclass Reading IntentionalProcess)
        (subclass Pursuing IntentionalProcess)
        (subclass Barking Process)
        (subclass Falling Process)
        (subclass Snowing Process)
        (subclass SocialParty IntentionalProcess)
        (subclass PsychologicalProcess Process)
        (subclass Perception PsychologicalProcess)
        (subclass Seeing Perception)
        (subclass TimePosition Abstract)
        (subclass TimeInterval TimePosition)
        (subclass TimePoint TimePosition)
        (subclass Day TimeInterval)
        (subclass Monday Day)
        (instance Now TimePoint)
        (instance agent BinaryPredicate)
        (domain agent 1 Process)
        (domain agent 2 AutonomousAgent)
        (instance patient BinaryPredicate)
        (domain patient 1 Process)
        (domain patient 2 Entity)
        (instance experiencer BinaryPredicate)
        (domain experiencer 1 Process)
        (domain experiencer 2 AutonomousAgent)
        (instance located BinaryPredicate)
        (domain located 1 Physical)
        (domain located 2 Object)
        (subclass CaseRole BinaryPredicate)
        (instance eventLocated BinaryPredicate)
        (instance eventLocated CaseRole)
        (domain eventLocated 1 Process)
        (domain eventLocated 2 Object)
        (instance agent CaseRole)
        (instance patient CaseRole)
        (instance instrument CaseRole)
        (instance destination BinaryPredicate)
        (domain destination 1 Process)
        (domain destination 2 Entity)
        (instance instrument BinaryPredicate)
        (domain instrument 1 Process)
        (domain instrument 2 Object)
        (instance time BinaryPredicate)
        (domain time 1 Physical)
        (domain time 2 TimePosition)
        (instance possesses BinaryPredicate)
        (domain possesses 1 AutonomousAgent)
        (domain possesses 2 Object)
        (instance names BinaryPredicate)
        (domain names 1 SymbolicString)
        (domain names 2 Entity)
        (subclass SoundAttribute Attribute)
        (instance Loud SoundAttribute)
        (subclass ColorAttribute Attribute)
        (instance Red ColorAttribute)
        (instance Purple ColorAttribute)
        (subclass SubjectiveAssessmentAttribute Attribute)
        (subclass RelationalAttribute Attribute)
        (subclass SocialRole RelationalAttribute)
        (subclass OccupationalRole SocialRole)
        (instance Musician OccupationalRole)
        (instance Rex Dog)
        (subclass Star Object)
        (instance Sun Star)
        (subclass Sunday Day)
        (instance Mike Human)
        (instance Bob Human)
        (instance Mary Human)
        (termFormat EnglishLanguage Bob "Robert")
    "#;

    const NOUNS: &str = "\
00000001 05 n 01 dog 0 000 | a dog &%Dog=
00000002 05 n 01 cat 0 000 | a cat &%Cat=
00000003 05 n 01 animal 0 000 | an animal &%Animal=
00000004 05 n 01 book 0 000 | a book &%Book=
00000005 05 n 01 garden 0 000 | a garden &%Garden=
00000006 05 n 01 pianist 0 000 | a pianist &%Musician+
00000007 05 n 01 man 0 000 | a man &%Human+
00000008 05 n 01 party 0 000 | a party &%SocialParty=
00000009 05 n 01 rock 0 000 | a rock &%Rock=
00000010 05 n 01 water 0 000 | water &%Water=
00000011 05 n 01 snow 0 000 | snow &%Snowing=
00000012 05 n 01 ice 0 000 | ice &%Ice=
00000013 05 n 01 chase 0 000 | a chase &%Pursuing=
00000014 05 n 01 unicorn 0 000 | a unicorn &%Unicorn=
00000015 05 n 01 sky 0 000 | the sky
00000016 05 n 01 loud 0 000 | loudness (noun reading) &%SoundAttribute+
00000017 05 n 02 motor_vehicle 0 car 0 000 | a motor vehicle &%Automobile=
00000018 05 n 01 Monday 0 000 | monday &%Monday=
00000020 05 n 01 tuesday 0 000 | tuesday &%Tuesday=
00000021 05 n 01 boy 0 000 | a boy &%Boy=
00000022 05 n 01 like 0 000 | a like
00000023 05 n 01 pet 0 000 | a pet &%Pet=
00000024 05 n 01 pet 1 000 | a domestic animal &%Animal+
00000025 05 n 01 Regina 0 000 | a queen &%Queen=
00000026 05 n 01 sun 0 000 | the star &%Sun@
00000027 05 n 02 sun 1 Sunday 0 000 | first day &%Sunday=
00000028 05 n 01 foal 0 000 | a foal &%Foal=
00000029 05 n 01 horse 0 000 | a horse &%Horse=
00000030 05 n 01 young 0 000 | offspring &%Animal+
00000031 05 n 01 purple 0 000 | the color &%Purple=
00000019 05 n 01 mike 0 000 | a microphone &%Microphone=
00000032 05 n 01 bird 0 000 | a bird &%Bird=
00000033 05 n 01 feather 0 000 | a feather &%Feather=
00000034 05 n 01 wing 0 000 | a wing &%Wing=
00000035 05 n 01 tail 0 000 | a tail &%Tail=
00000036 05 n 01 engine 0 000 | an engine &%Engine=
00000037 05 n 01 Jay 0 000 | the statesman &%Human+
00000038 05 n 01 jay 0 000 | the bird &%Bird+
00000039 05 n 01 part 0 000 | a part &%Rock+
00000040 05 n 01 people 0 000 | people &%Human+
00000041 05 n 01 teacher 0 000 | a teacher &%Teacher=
00000042 05 n 01 earth 0 000 | the planet &%PlanetEarth=
00000043 05 n 01 saw 0 000 | a saw &%Artifact+
";
    const VERBS: &str = "\
00000101 30 v 01 read 0 000 00 | to read &%Reading=
00000102 30 v 01 chase 0 000 00 | to chase &%Pursuing=
00000103 30 v 01 bark 0 000 00 | to bark &%Barking=
00000104 30 v 01 own 0 000 00 | to own &%possesses+
00000105 30 v 01 see 0 000 00 | to see &%Seeing=
00000106 30 v 01 fall 0 000 00 | to fall &%Falling=
00000107 30 v 01 believe 0 000 00 | to believe &%believes=
00000108 30 v 01 exist 0 000 00 | to exist &%Entity+
00000109 30 v 01 like 0 000 00 | to like &%wants+
00000110 30 v 01 horse 0 000 00 | to provide with horses &%Process+
00000111 30 v 01 live 0 000 00 | to live &%Living=
00000112 30 v 01 part 0 000 00 | to part &%Falling+
00000113 30 v 02 desire 0 want 0 000 00 | to want &%IntentionalProcess+
";
    const ADJS: &str = "\
00000201 00 a 01 loud 0 000 | loud &%Loud=
00000202 00 a 01 red 0 000 | red &%Red=
00000203 00 a 01 big 0 000 | big &%SubjectiveAssessmentAttribute+
00000204 00 a 01 like 0 000 | similar
00000205 00 a 01 young 0 000 | young &%Young=
00000206 00 a 01 live 0 000 | alive
00000207 00 a 01 purple 0 000 | purple &%Purple=
";
    const ADVS: &str =
        "00000301 02 r 01 quickly 0 000 | quickly &%SubjectiveAssessmentAttribute+\n";
    const EXC: &str = "ran run\nsaw see\nfell fall\nfallen fall\nmen man\n";
    /// Tag counts: "part" is a noun 234 times and a verb 5 times.
    const INDEX: &str = "part%1:24:00:: 00000039 1 234\npart%2:35:00:: 00000112 1 5\n";

    fn kb() -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let r = kb.reload_kif(KIF, &std::path::PathBuf::from("test.kif"), "test.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("test.kif").expect("promote");
        kb
    }

    fn wn() -> WordNet {
        WordNet::from_texts(
            [
                (NOUNS, Pos::Noun, "noun"),
                (VERBS, Pos::Verb, "verb"),
                (ADJS, Pos::Adj, "adj"),
                (ADVS, Pos::Adv, "adv"),
            ],
            Some(INDEX),
            Some(EXC),
        )
    }

    fn run(text: &str) -> Translation {
        translate(&kb(), &wn(), text, &Opts::default())
    }

    /// The KIF of every sentence, or `[code]` for a refusal.
    fn kif(text: &str) -> Vec<String> {
        run(text)
            .sentences
            .iter()
            .map(|s| match &s.outcome {
                Ok(l) => l.formula().to_string(),
                Err(r) => format!("[{}]", r.reason.code()),
            })
            .collect()
    }

    fn one(text: &str) -> String {
        let v = kif(text);
        assert_eq!(v.len(), 1, "{v:?}");
        v.into_iter().next().unwrap_or_default()
    }

    fn refusal(text: &str) -> Refused {
        let t = run(text);
        t.sentences[0]
            .outcome
            .clone()
            .expect_err("expected a refusal")
    }

    // -- Copula -----------------------------------------------------------------

    #[test]
    fn copula_instance_subclass_and_plural() {
        assert_eq!(one("Rex is a dog."), "(instance Rex Dog)");
        assert_eq!(one("A dog is an animal."), "(subclass Dog Animal)");
        assert_eq!(one("Dogs are animals."), "(subclass Dog Animal)");
        assert_eq!(one("Every dog is an animal."), "(subclass Dog Animal)");
    }

    #[test]
    fn copula_attributes_and_roles() {
        assert_eq!(one("Rex is loud."), "(attribute Rex Loud)");
        assert_eq!(one("Bob is a pianist."), "(attribute Bob Musician)");
        assert_eq!(
            one("Every dog is loud."),
            "(forall (?X1) (=> (instance ?X1 Dog) (attribute ?X1 Loud)))"
        );
        assert_eq!(
            one("Rex is a red dog."),
            "(and (instance Rex Dog) (attribute Rex Red))"
        );
    }

    #[test]
    fn copula_negation_and_quantifiers() {
        assert_eq!(one("Rex is not a cat."), "(not (instance Rex Cat))");
        assert_eq!(
            one("No dog is a cat."),
            "(not (exists (?X1) (and (instance ?X1 Dog) (instance ?X1 Cat))))"
        );
        assert_eq!(
            one("Some dogs are loud."),
            "(and (instance Dog-1 Dog) (attribute Dog-1 Loud))"
        );
        assert_eq!(
            one("A dog is not a cat."),
            "(forall (?X1) (=> (instance ?X1 Dog) (not (instance ?X1 Cat))))"
        );
    }

    #[test]
    fn copula_prepositions_by_complement_type() {
        assert_eq!(
            one("Rex is in the garden."),
            "(and (instance Garden-1 Garden) (located Rex Garden-1))"
        );
        assert_eq!(
            one("The party is on Monday."),
            "(and (instance SocialParty-1 SocialParty) (instance Monday-1 Monday) (time SocialParty-1 Monday-1))"
        );
        assert_eq!(
            refusal("Rex is on Monday.").reason.code(),
            "unmapped-preposition"
        );
    }

    #[test]
    fn copula_questions_bind_variables() {
        assert_eq!(one("Is Rex a dog?"), "(instance Rex Dog)");
        assert_eq!(
            one("Is Rex in a garden?"),
            "(exists (?X1) (and (instance ?X1 Garden) (located Rex ?X1)))"
        );
        let t = run("Is Rex a dog?");
        assert_eq!(t.queries().len(), 1);
        assert!(t.assertions().is_empty());
    }

    #[test]
    fn taxonomic_guards() {
        assert_eq!(refusal("A dog is a dog.").reason.code(), "trivial-fact");
        assert_eq!(refusal("Snow is ice.").reason.code(), "cross-category");
        assert_eq!(
            one("A red dog is an animal."),
            "(forall (?X1) (=> (and (instance ?X1 Dog) (attribute ?X1 Red)) (instance ?X1 Animal)))"
        );
        assert_eq!(refusal("Rex is big.").reason.code(), "not-an-attribute");
        assert_eq!(
            refusal("Rex is the dog.").reason.code(),
            "unsupported/identity"
        );
    }

    // -- Events -------------------------------------------------------------------

    #[test]
    fn transitive_event_with_skolems() {
        assert_eq!(
            one("Mike reads a book."),
            "(and (instance Reading-1 Reading) (agent Reading-1 Mike) (instance Book-1 Book) (patient Reading-1 Book-1))"
        );
        let t = run("Mike reads a book.");
        assert_eq!(t.assertions().len(), 4);
    }

    #[test]
    fn intransitive_experiencer_and_patient_roles() {
        assert_eq!(
            one("Rex barks."),
            "(and (instance Barking-1 Barking) (agent Barking-1 Rex))"
        );
        assert_eq!(
            one("Mike sees a cat."),
            "(and (instance Seeing-1 Seeing) (experiencer Seeing-1 Mike) (instance Cat-1 Cat) (patient Seeing-1 Cat-1))"
        );
        assert_eq!(
            one("The rock falls."),
            "(and (instance Rock-1 Rock) (instance Falling-1 Falling) (patient Falling-1 Rock-1))"
        );
        assert_eq!(
            refusal("The rock reads a book.").reason.code(),
            "role-mismatch"
        );
    }

    #[test]
    fn prepositional_roles_on_events() {
        assert_eq!(
            one("Mike reads a book in the garden."),
            "(and (instance Reading-1 Reading) (agent Reading-1 Mike) (instance Book-1 Book) (patient Reading-1 Book-1) (instance Garden-1 Garden) (eventLocated Reading-1 Garden-1))"
        );
        assert_eq!(
            one("Mike reads with Mary."),
            "(and (instance Reading-1 Reading) (agent Reading-1 Mike) (agent Reading-1 Mary))"
        );
        assert_eq!(
            one("Mike reads with a book."),
            "(and (instance Reading-1 Reading) (agent Reading-1 Mike) (instance Book-1 Book) (instrument Reading-1 Book-1))"
        );
    }

    #[test]
    fn stative_verbs_relate_directly() {
        assert_eq!(
            one("Bob owns a car."),
            "(and (instance Automobile-1 Automobile) (possesses Bob Automobile-1))"
        );
        assert_eq!(refusal("Bob owns.").reason.code(), "missing-object");
        assert_eq!(
            refusal("Bob owns a car in the garden.").reason.code(),
            "unsupported/stative-adjunct"
        );
        assert_eq!(
            refusal("Bob believes a cat.").reason.code(),
            "role-mismatch"
        );
    }

    #[test]
    fn negation_scopes_over_indefinites() {
        assert_eq!(
            one("Mike does not read a book."),
            "(not (exists (?E1 ?X1) (and (instance ?E1 Reading) (agent ?E1 Mike) (instance ?X1 Book) (patient ?E1 ?X1))))"
        );
        assert_eq!(
            one("No dog reads a book."),
            "(not (exists (?X1 ?E1 ?X2) (and (instance ?X1 Dog) (instance ?E1 Reading) (agent ?E1 ?X1) (instance ?X2 Book) (patient ?E1 ?X2))))"
        );
    }

    #[test]
    fn universal_subject_with_event() {
        assert_eq!(
            one("Every dog chases a cat."),
            "(forall (?X1) (=> (instance ?X1 Dog) (exists (?E1 ?X2) (and (instance ?E1 Pursuing) (agent ?E1 ?X1) (instance ?X2 Cat) (patient ?E1 ?X2)))))"
        );
        assert_eq!(
            refusal("Dogs chase cats.").reason.code(),
            "unsupported/generic"
        );
    }

    #[test]
    fn event_questions_and_tense() {
        assert_eq!(
            one("Does Mike read a book?"),
            "(exists (?E1 ?X1) (and (instance ?E1 Reading) (agent ?E1 Mike) (instance ?X1 Book) (patient ?E1 ?X1)))"
        );
        assert_eq!(
            one("Rex chased a cat."),
            "(and (instance Pursuing-1 Pursuing) (agent Pursuing-1 Rex) (before (EndFn (WhenFn Pursuing-1)) Now) (instance Cat-1 Cat) (patient Pursuing-1 Cat-1))"
        );
        // "saw" is also a noun; the irregular past still reads as "see".
        assert!(one("Mike saw a cat.").contains("(before (EndFn (WhenFn Seeing-1)) Now)"));
        assert_eq!(refusal("Rex is chasing a cat.").reason.code(), "no-rule");
    }

    #[test]
    fn past_tense_needs_now_in_kb() {
        let mut kb = KnowledgeBase::new();
        let kif = KIF.replace("(instance Now TimePoint)", "");
        let r = kb.reload_kif(&kif, &std::path::PathBuf::from("t.kif"), "t.kif");
        assert!(r.ok);
        kb.make_session_axiomatic("t.kif").expect("promote");
        let t = translate(&kb, &wn(), "Rex chased a cat.", &Opts::default());
        let r = t.sentences[0].outcome.clone().expect_err("refused");
        assert_eq!(r.reason.code(), "unsupported/past-tense");
        assert!(r.reason.hint().contains("Now"));
        // A speech time from the options anchors it, declared when the
        // KB lacks the term.
        let opts = Opts {
            now: Some("Utterance-7".into()),
            ..Opts::default()
        };
        let t = translate(&kb, &wn(), "Rex chased a cat. Mike saw a cat.", &opts);
        let formulas: Vec<Vec<String>> = t
            .sentences
            .iter()
            .map(|s| {
                s.outcome
                    .as_ref()
                    .expect("accepted")
                    .formulas()
                    .iter()
                    .map(ToString::to_string)
                    .collect()
            })
            .collect();
        assert!(formulas[0].contains(&"(instance Utterance-7 TimePoint)".to_string()));
        assert!(
            formulas[0].contains(&"(before (EndFn (WhenFn Pursuing-1)) Utterance-7)".to_string())
        );
        assert!(!formulas[1].contains(&"(instance Utterance-7 TimePoint)".to_string()));
        assert!(formulas[1].contains(&"(before (EndFn (WhenFn Seeing-1)) Utterance-7)".to_string()));
    }

    #[test]
    fn attitude_verbs_take_clauses() {
        // "to" complement: the subject controls the clause, bound inside.
        assert_eq!(
            one("Mike wants to read a book."),
            "(desires Mike (exists (?E1 ?X1) (and (instance ?E1 Reading) (agent ?E1 Mike) (instance ?X1 Book) (patient ?E1 ?X1))))"
        );
        // "that" complement: a sentence of its own.
        assert_eq!(
            one("Mike believes that Rex is a dog."),
            "(believes Mike (instance Rex Dog))"
        );
        assert!(one("Mike does not want to read a book.").starts_with("(not (desires Mike (exists"));
        // A generic subject keeps its universal outside the attitude.
        let generic = Opts {
            generic_subjects: true,
            ..Opts::default()
        };
        let v = with("Dogs want to chase cats.", generic);
        assert!(
            v[0].starts_with("(forall (?X1) (=> (instance ?X1 Dog) (desires ?X1 (exists (?E1 ?X2)"),
            "{v:?}"
        );
        // The same verb with a noun phrase object is the relation `wants`
        // (its label outranks the WordNet sense).
        assert_eq!(
            one("Mike wants a book."),
            "(and (instance Book-1 Book) (wants Mike Book-1))"
        );
        // The subject must fit the relation's first argument.
        assert_eq!(
            refusal("A book wants to fall.").reason.code(),
            "role-mismatch"
        );
        assert_eq!(
            refusal("Mike wanted to read a book.").reason.code(),
            "unsupported/past-tense"
        );
        assert_eq!(refusal("Mike wants to.").reason.code(), "no-rule");
        assert_eq!(
            refusal("Mike wants to read a book quickly.").reason.code(),
            "unsupported/adverb"
        );
    }

    #[test]
    fn role_nouns_describe_their_bearers() {
        // A noun naming an Attribute instance: whatever bears it.
        assert_eq!(
            one("A teacher is a man."),
            "(forall (?X1) (=> (attribute ?X1 Teacher) (instance ?X1 Human)))"
        );
        let generic = Opts {
            generic_subjects: true,
            ..Opts::default()
        };
        let v = with("Teachers read books.", generic);
        assert!(
            v[0].starts_with("(forall (?X1) (=> (attribute ?X1 Teacher) (exists"),
            "{v:?}"
        );
        let k = one("The teacher reads a book.");
        assert!(
            k.contains("(attribute Teacher-1 Teacher)")
                && k.contains("(agent Reading-1 Teacher-1)"),
            "{k}"
        );
        assert_eq!(one("Mike is a teacher."), "(attribute Mike Teacher)");
        // A common noun whose term is a KB instance denotes it.
        assert_eq!(one("The earth is a rock."), "(instance PlanetEarth Rock)");
        assert_eq!(one("The earth is red."), "(attribute PlanetEarth Red)");
    }

    // -- Discourse ------------------------------------------------------------------

    #[test]
    fn definite_reference_across_sentences() {
        let v = kif("A man reads a book. The man owns the book.");
        assert_eq!(
            v[0],
            "(and (instance Human-1 Human) (instance Reading-1 Reading) (agent Reading-1 Human-1) (instance Book-1 Book) (patient Reading-1 Book-1))"
        );
        assert_eq!(v[1], "(possesses Human-1 Book-1)");
        let v = kif("A dog barks. A dog barks. The dog is loud.");
        assert_eq!(v[2], "[ambiguous-reference]");
        // A definite with no antecedent is accommodated as a new referent.
        assert_eq!(
            one("The dog is loud."),
            "(and (instance Dog-1 Dog) (attribute Dog-1 Loud))"
        );
    }

    #[test]
    fn capitalized_words_are_names_unless_labeled() {
        // "Mike" is a KB instance: the WordNet noun sense (microphone) loses.
        assert_eq!(
            one("Mary sees Mike."),
            "(and (instance Seeing-1 Seeing) (experiencer Seeing-1 Mary) (patient Seeing-1 Mike))"
        );
        // A capitalized word with only a WordNet sense is not a common noun.
        assert_eq!(refusal("Mary sees Zeke.").reason.code(), "unknown-name");
        // A capitalized WordNet lemma, or a KB label, makes a capitalized
        // word a common noun mid-sentence.
        assert!(one("The party is on Monday.").contains("(instance Monday-1 Monday)"));
        assert!(one("The party is on Tuesday.").contains("(instance Tuesday-1 Tuesday)"));
        // A first-name list forces the name reading.
        let t = translate(
            &kb(),
            &wn(),
            "Zeke is loud.",
            &Opts {
                assume_human_names: false,
                names: vec!["Zeke".into()],
                generic_subjects: false,
                ..Opts::default()
            },
        );
        let l = t.sentences[0].outcome.as_ref().expect("translated");
        assert_eq!(l.formula().to_string(), "(attribute Zeke Loud)");
        assert_eq!(l.declared.conds.len(), 2);
    }

    #[test]
    fn names_by_label_and_assumed_names() {
        assert_eq!(one("Robert is a pianist."), "(attribute Bob Musician)");
        assert_eq!(refusal("Alice is a pianist.").reason.code(), "unknown-name");
        let t = translate(
            &kb(),
            &wn(),
            "Alice reads a book. Alice is loud.",
            &Opts {
                assume_human_names: true,
                names: Vec::new(),
                generic_subjects: false,
                ..Opts::default()
            },
        );
        let v: Vec<String> = t
            .sentences
            .iter()
            .map(|s| {
                s.outcome
                    .as_ref()
                    .map(|l| l.formula().to_string())
                    .unwrap_or_default()
            })
            .collect();
        assert!(
            v[0].starts_with("(and (instance Reading-1 Reading) (agent Reading-1 Alice)"),
            "{}",
            v[0]
        );
        assert_eq!(v[1], "(attribute Alice Loud)");
        let first = t.sentences[0].outcome.as_ref().expect("ok");
        assert_eq!(
            first.declared.to_ast().to_string(),
            "(and (instance Alice Human) (names \"Alice\" Alice))"
        );
        assert_eq!(first.formulas().len(), 6);
        assert_eq!(t.assertions().len(), 7);
    }

    #[test]
    fn assumed_names_in_queries_become_side_assertions() {
        let t = translate(
            &kb(),
            &wn(),
            "Is Zeke a pianist?",
            &Opts {
                assume_human_names: true,
                names: Vec::new(),
                generic_subjects: false,
                ..Opts::default()
            },
        );
        let l = t.sentences[0].outcome.as_ref().expect("ok");
        assert_eq!(l.formula().to_string(), "(attribute Zeke Musician)");
        assert_eq!(t.queries().len(), 1);
        assert_eq!(t.assertions().len(), 2);
    }

    #[test]
    fn sentence_initial_name_colliding_with_a_noun() {
        // "mike" is a WordNet noun (microphone) the KB defines; Mike is an
        // instance, so the name wins.
        assert_eq!(
            one("Mike reads a book."),
            "(and (instance Reading-1 Reading) (agent Reading-1 Mike) (instance Book-1 Book) (patient Reading-1 Book-1))"
        );
        let names = names_from_csv("\"name\",\"gender\"\n\"Zeke\",\"M\"\n");
        assert_eq!(names, vec!["Zeke".to_string()]);
        let t = translate(
            &kb(),
            &wn(),
            "Zeke reads a book.",
            &Opts {
                assume_human_names: false,
                names,
                generic_subjects: false,
                ..Opts::default()
            },
        );
        assert!(t.complete(), "{:?}", t.sentences[0].outcome);
        // A sentence-initial word with a KB noun reading stays a noun; one
        // with no usable reading is an unknown name, accepted only under
        // assumed names.
        assert_eq!(
            refusal("Unicorn reads a book.").reason.code(),
            "unknown-name"
        );
        assert_eq!(refusal("Rock reads a book.").reason.code(), "role-mismatch");
        let t = translate(
            &kb(),
            &wn(),
            "Unicorn reads a book.",
            &Opts {
                assume_human_names: true,
                names: Vec::new(),
                generic_subjects: false,
                ..Opts::default()
            },
        );
        assert!(t.complete(), "{:?}", t.sentences[0].outcome);
    }

    // -- Lexicon -----------------------------------------------------------------------

    #[test]
    fn labels_outrank_wordnet_and_compounds_fold() {
        assert_eq!(one("A car is a rock."), "(subclass Automobile Rock)");
        assert_eq!(
            one("A motor vehicle is a rock."),
            "(subclass Automobile Rock)"
        );
        let t = run("Rex is a dog.");
        let l = t.sentences[0].outcome.as_ref().unwrap();
        assert_eq!(l.rule, "copula");
        assert_eq!(l.choices[0].term, "Dog");
        assert_eq!(l.choices[0].sense, "dog#n#1=");
        assert_eq!(
            l.choices[0].synset.map(|id| id.to_string()).as_deref(),
            Some("00000001-n")
        );
    }

    #[test]
    fn inflected_verb_is_not_an_adjective_boundary() {
        // "likes" strips to "like", which has an adjective sense; only the
        // exact form counts, so "boy" is the head and "likes" the verb.
        assert_eq!(
            one("Every boy likes a book."),
            "(forall (?X1) (=> (instance ?X1 Boy) (exists (?X2) (and (instance ?X2 Book) (wants ?X1 ?X2)))))"
        );
        // A binary relation's own name (or a label on it) names a verb.
        assert_eq!(
            one("Bob possesses a car."),
            "(and (instance Automobile-1 Automobile) (possesses Bob Automobile-1))"
        );
    }

    #[test]
    fn missing_mfs_anchor_is_reported_not_slid_past() {
        // pet#n#1 -> Pet (undefined); pet#n#2 -> Animal+ would be a guess.
        assert_eq!(refusal("Rex is a pet.").reason.code(), "term-not-in-kb");
        assert!(refusal("Rex is a pet.").reason.to_string().contains("Pet"));
        // Mid-sentence too, a capitalized word with a dead noun reading is
        // an unknown name, not a term-not-in-kb noun.
        assert_eq!(refusal("Mary sees Regina.").reason.code(), "unknown-name");
    }

    #[test]
    fn instance_anchors_and_no_sliding() {
        // sun#n#1 anchors to the instance Sun: "the sun" names it.
        assert_eq!(one("The sun is loud."), "(attribute Sun Loud)");
        assert_eq!(
            one("Rex sees the sun."),
            "(and (instance Seeing-1 Seeing) (experiencer Seeing-1 Rex) (patient Seeing-1 Sun))"
        );
        // In class position the instance is refused; the rarer Sunday sense
        // must not slide in.
        let r = refusal("A sun is a rock.");
        assert_eq!(r.reason.code(), "not-a-class");
        assert!(r.reason.to_string().contains("Sun"), "{r}");
    }

    #[test]
    fn cannot_and_relative_that_and_plurals() {
        assert!(one("Rex cannot bark.").starts_with("(not (modalAttribute"));
        assert_eq!(
            refusal("That dog barks.").reason.code(),
            "unsupported/demonstrative"
        );
        // A plural with no KB term is a missing term, not a name.
        assert_eq!(
            refusal("Unicorns are animals.").reason.code(),
            "term-not-in-kb"
        );
    }

    #[test]
    fn noun_phrase_readings_are_disambiguated_by_the_rest() {
        // "young" is also a noun and "horse" also a verb: the longest
        // predicate reading is the one under which the sentence completes;
        // the adjective on the class becomes a rule about the subject.
        assert_eq!(
            one("A foal is a young horse."),
            "(and (subclass Foal Horse) (forall (?X1) (=> (instance ?X1 Foal) (attribute ?X1 Young))))"
        );
        assert_eq!(
            one("Rex is a young horse."),
            "(and (instance Rex Horse) (attribute Rex Young))"
        );
        // "live" is also an adjective: the subject stops at "dogs".
        assert_eq!(
            one("Every dog lives in a garden."),
            "(forall (?X1) (=> (instance ?X1 Dog) (exists (?E1 ?X2) (and (instance ?E1 Living) (agent ?E1 ?X1) (instance ?X2 Garden) (eventLocated ?E1 ?X2)))))"
        );
        // Failed readings leave no trace: no gaps in Skolem numbering.
        assert_eq!(
            one("The foal sees a young horse."),
            "(and (instance Foal-1 Foal) (instance Seeing-1 Seeing) (experiencer Seeing-1 Foal-1) (instance Horse-1 Horse) (attribute Horse-1 Young) (patient Seeing-1 Horse-1))"
        );
    }

    #[test]
    fn predicate_adjective_never_slides_to_a_noun_sense() {
        // "loud" has a noun sense (SoundAttribute) but "big"'s refusal must
        // stand: the adjective reading is the only one offered.
        assert_eq!(refusal("Rex is big.").reason.code(), "not-an-attribute");
        assert_eq!(refusal("Dogs are big.").reason.code(), "not-an-attribute");
        // Instance statements are guarded like subclass statements.
        assert_eq!(
            refusal("The rock is a party.").reason.code(),
            "cross-category"
        );
        assert_eq!(refusal("Purple is purple.").reason.code(), "trivial-fact");
    }

    #[test]
    fn lexical_refusals() {
        assert_eq!(refusal("Rex is a florb.").reason.code(), "unknown-word");
        assert_eq!(refusal("Rex is a unicorn.").reason.code(), "term-not-in-kb");
        assert_eq!(refusal("Rex is a sky.").reason.code(), "no-sumo-anchor");
        assert_eq!(refusal("Rex is a loud.").reason.code(), "not-a-class");
        assert_eq!(refusal("Rex exists.").reason.code(), "not-an-event");
        assert_eq!(
            refusal("Rex is a big dog.").reason.code(),
            "not-an-attribute"
        );
    }

    // -- Part of speech: agreement, proper nouns, the tag-count prior ------------------

    #[test]
    fn subject_verb_agreement_filters_verb_readings() {
        assert_eq!(one("The dog bark."), "[no-rule]");
        assert_eq!(one("Dogs barks."), "[no-rule]");
        assert_eq!(one("Rex bark."), "[no-rule]");
        assert_eq!(one("A dog are an animal."), "[no-rule]");
        assert_eq!(one("Does the dog barks?"), "[no-rule]");
        assert_eq!(
            one("Does Rex bark?"),
            "(exists (?E1) (and (instance ?E1 Barking) (agent ?E1 Rex)))"
        );
        // A bare invariant plural takes its number from the verb.
        assert_eq!(one("People are animals."), "(subclass Human Animal)");
        assert_eq!(refusal("People bark.").reason.code(), "unsupported/generic");
    }

    #[test]
    fn proper_noun_senses_need_a_capital() {
        // "Jay" the statesman is listed before "jay" the bird, but a
        // lowercase (or sentence-initial) word skips capitalized-only senses.
        assert_eq!(one("Jays are animals."), "(subclass Bird Animal)");
        assert_eq!(one("Rex sees a jay."), "(and (instance Seeing-1 Seeing) (experiencer Seeing-1 Rex) (instance Bird-1 Bird) (patient Seeing-1 Bird-1))");
    }

    #[test]
    fn tag_count_prior_rejects_rare_parts_of_speech() {
        // "part" is tagged as a verb in 5 of 239 uses: below the floor, so
        // it is never read as one.
        assert_eq!(one("The dog parts."), "[no-rule]");
        assert_eq!(
            one("Rex sees a part."),
            "(and (instance Seeing-1 Seeing) (experiencer Seeing-1 Rex) (instance Rock-1 Rock) (patient Seeing-1 Rock-1))"
        );
        // Untagged words keep every WordNet part of speech.
        assert_eq!(
            one("Rex falls."),
            "(and (instance Falling-1 Falling) (agent Falling-1 Rex))"
        );
    }

    #[test]
    fn comma_separated_adjectives() {
        assert_eq!(
            one("Rex is a red, young horse."),
            "(and (instance Rex Horse) (attribute Rex Red) (attribute Rex Young))"
        );
        assert_eq!(one("Rex is a red, horse."), "[no-rule]");
    }

    // -- Have -----------------------------------------------------------------------------

    #[test]
    fn have_by_object_type() {
        assert_eq!(
            one("Birds have feathers."),
            "(forall (?X1) (=> (instance ?X1 Bird) (exists (?X2) (and (instance ?X2 Feather) (part ?X2 ?X1)))))"
        );
        assert_eq!(
            one("Rex has a tail."),
            "(and (instance Tail-1 Tail) (part Tail-1 Rex))"
        );
        assert_eq!(
            one("Bob has a book."),
            "(and (instance Book-1 Book) (possesses Bob Book-1))"
        );
        assert_eq!(
            one("A dog does not have wings."),
            "(forall (?X1) (=> (instance ?X1 Dog) (not (exists (?X2) (and (instance ?X2 Wing) (part ?X2 ?X1))))))"
        );
        assert_eq!(
            one("Rex has no tail."),
            "(not (exists (?X1) (and (instance ?X1 Tail) (part ?X1 Rex))))"
        );
        assert_eq!(
            one("Does Rex have a tail?"),
            "(exists (?X1) (and (instance ?X1 Tail) (part ?X1 Rex)))"
        );
        assert_eq!(
            refusal("Rex has fallen.").reason.code(),
            "unsupported/perfect"
        );
        assert_eq!(
            refusal("Rex had a book.").reason.code(),
            "unsupported/past-tense"
        );
        assert_eq!(one("Rex have a tail."), "[no-rule]");
    }

    // -- Decomposition: coordination and relative clauses ------------------------------

    #[test]
    fn coordination_distributes_over_the_sentence() {
        assert_eq!(
            one("Rex and Mike are loud."),
            "(and (attribute Rex Loud) (attribute Mike Loud))"
        );
        assert_eq!(
            one("Rex is loud and Mike is red."),
            "(and (attribute Rex Loud) (attribute Mike Red))"
        );
        assert_eq!(
            one("Rex is red, young, and loud."),
            "(and (attribute Rex Red) (attribute Rex Young) (attribute Rex Loud))"
        );
        assert_eq!(
            one("Birds have feathers and wings."),
            "(and (forall (?X1) (=> (instance ?X1 Bird) (exists (?X2) (and (instance ?X2 Feather) (part ?X2 ?X1))))) (forall (?X3) (=> (instance ?X3 Bird) (exists (?X4) (and (instance ?X4 Wing) (part ?X4 ?X3))))))"
        );
        // A conjunct that fails reports its own refusal, and nothing of the
        // sentence is kept.
        assert_eq!(
            refusal("Rex and Zeke are loud.").reason.code(),
            "unknown-name"
        );
        assert_eq!(
            refusal("Rex is big and loud.").reason.code(),
            "not-an-attribute"
        );
        // The refused sentence's referents are rolled back: the next book
        // is still Book-1.
        assert_eq!(
            kif("Mike reads a book and a unicorn. Mike reads a book."),
            vec![
                "[term-not-in-kb]".to_string(),
                "(and (instance Reading-1 Reading) (agent Reading-1 Mike) (instance Book-1 Book) (patient Reading-1 Book-1))".to_string()
            ]
        );
    }

    #[test]
    fn relative_clauses_restrict_the_noun_phrase() {
        // Predicate position: a second fact about the subject.
        assert_eq!(
            one("A dog is an animal that barks."),
            "(forall (?X1) (=> (instance ?X1 Dog) (exists (?E1) (and (instance ?X1 Animal) (instance ?E1 Barking) (agent ?E1 ?X1)))))"
        );
        assert_eq!(
            one("Rex is a dog which barks."),
            "(exists (?E1) (and (instance Rex Dog) (instance ?E1 Barking) (agent ?E1 Rex)))"
        );
        assert_eq!(
            one("A bird is an animal that has wings."),
            "(forall (?X1) (=> (instance ?X1 Bird) (exists (?X2) (and (instance ?X1 Animal) (instance ?X2 Wing) (part ?X2 ?X1)))))"
        );
        // Subject position: the clause restricts the universal.
        assert_eq!(
            one("Dogs that bark are loud."),
            "(forall (?E1 ?X1) (=> (and (instance ?X1 Dog) (instance ?E1 Barking) (agent ?E1 ?X1)) (attribute ?X1 Loud)))"
        );
        assert_eq!(
            one("Every dog that barks is loud."),
            "(forall (?E1 ?X1) (=> (and (instance ?X1 Dog) (instance ?E1 Barking) (agent ?E1 ?X1)) (attribute ?X1 Loud)))"
        );
        // Object position, an object gap, and a modal inside the clause.
        assert_eq!(
            one("Rex sees a dog that Mike chases."),
            "(exists (?E1) (and (instance Seeing-1 Seeing) (experiencer Seeing-1 Rex) (instance Dog-1 Dog) (instance ?E1 Pursuing) (agent ?E1 Mike) (patient ?E1 Dog-1) (patient Seeing-1 Dog-1)))"
        );
        assert_eq!(
            one("Rex sees a dog that can bark."),
            "(and (instance Seeing-1 Seeing) (experiencer Seeing-1 Rex) (instance Dog-1 Dog) (modalAttribute (exists (?E1) (and (instance ?E1 Barking) (agent ?E1 Dog-1))) Possibility) (patient Seeing-1 Dog-1))"
        );
        assert_eq!(
            one("Dogs that bark are animals that fall."),
            "(forall (?E1 ?X1) (=> (and (instance ?X1 Dog) (instance ?E1 Barking) (agent ?E1 ?X1)) (exists (?E2) (and (instance ?X1 Animal) (instance ?E2 Falling) (agent ?E2 ?X1)))))"
        );
        // A definite with a restriction is not resolved; the clause's own
        // refusals come through.
        assert_eq!(
            one("The dog that barks is loud."),
            "[unsupported/relative-clause]"
        );
        assert_eq!(
            refusal("A dog is an animal that barks quickly.")
                .reason
                .code(),
            "unsupported/adverb"
        );
    }

    // -- Pronouns and possessives ---------------------------------------------------------

    fn with(text: &str, opts: Opts) -> Vec<String> {
        translate(&kb(), &wn(), text, &opts)
            .sentences
            .iter()
            .map(|s| match &s.outcome {
                Ok(l) => l.formula().to_string(),
                Err(r) => format!("[{}]", r.reason.code()),
            })
            .collect()
    }

    #[test]
    fn third_person_pronouns_bind_to_discourse_referents() {
        // "it": the most recent non-person referent.
        assert_eq!(
            kif("Mike reads a book. Rex sees it."),
            vec![
                "(and (instance Reading-1 Reading) (agent Reading-1 Mike) (instance Book-1 Book) (patient Reading-1 Book-1))",
                "(and (instance Seeing-1 Seeing) (experiencer Seeing-1 Rex) (patient Seeing-1 Book-1))"
            ]
        );
        // "she": Mary is a Woman in the KB; "he": a Human not known female.
        assert_eq!(
            kif("Mary sees a book. She reads the book."),
            vec![
                "(and (instance Seeing-1 Seeing) (experiencer Seeing-1 Mary) (instance Book-1 Book) (patient Seeing-1 Book-1))",
                "(and (instance Reading-1 Reading) (agent Reading-1 Mary) (patient Reading-1 Book-1))"
            ]
        );
        assert_eq!(
            kif("Mary sees Mike. He reads a book.")[1],
            "(and (instance Reading-1 Reading) (agent Reading-1 Mike) (instance Book-1 Book) (patient Reading-1 Book-1))"
        );
        // Gender from the name list beats the KB's silence.
        let opts = Opts {
            name_genders: vec![("Bob".into(), token::Gender::Male)],
            ..Opts::default()
        };
        assert_eq!(
            with("Bob sees Mary. She sees a book.", opts)[1],
            "(and (instance Seeing-2 Seeing) (experiencer Seeing-2 Mary) (instance Book-1 Book) (patient Seeing-2 Book-1))"
        );
        // Two compatible referents is an ambiguity; none is a refusal.
        assert_eq!(
            kif("Mike reads a book. Rex sees a rock. Bob sees it.")[2],
            "[ambiguous-reference]"
        );
        assert_eq!(one("Rex sees it."), "[unsupported/pronoun]");
        assert_eq!(one("It is loud."), "[unsupported/expletive-it]");
        // A referent of an earlier clause of the same sentence binds too,
        // but never a co-argument of the pronoun's own clause.
        assert_eq!(
            one("Mike sees a book and it falls."),
            "(and (instance Seeing-1 Seeing) (experiencer Seeing-1 Mike) (instance Book-1 Book) (patient Seeing-1 Book-1) (instance Falling-1 Falling) (patient Falling-1 Book-1))"
        );
        assert_eq!(
            kif("Rex sees a rock. Rex sees it.")[1],
            "[ambiguous-reference]"
        );
        assert_eq!(
            kif("Mike reads a book. Rex sees a rock. Rex sees itself.")[2],
            "(and (instance Seeing-2 Seeing) (experiencer Seeing-2 Rex) (patient Seeing-2 Rex))"
        );
    }

    #[test]
    fn deictic_pronouns_come_from_the_options() {
        assert_eq!(one("I read a book."), "[unsupported/pronoun]");
        assert_eq!(one("You read a book."), "[unsupported/pronoun]");
        let speaker = Opts {
            speaker: Some("Mike".into()),
            addressee: Some("Bob".into()),
            ..Opts::default()
        };
        assert_eq!(
            with("I see you.", speaker),
            vec!["(and (instance Seeing-1 Seeing) (experiencer Seeing-1 Mike) (patient Seeing-1 Bob))"]
        );
        // Generic "you": one universal over the whole sentence, every
        // mention bound to the same variable, no Skolem constants inside.
        let generic = Opts {
            generic_you: Some("Human".into()),
            ..Opts::default()
        };
        assert_eq!(
            with("You read a book.", generic.clone()),
            vec!["(forall (?P1) (=> (instance ?P1 Human) (exists (?E1 ?X1) (and (instance ?E1 Reading) (agent ?E1 ?P1) (instance ?X1 Book) (patient ?E1 ?X1)))))"]
        );
        assert_eq!(
            with("A dog sees your book.", generic.clone()),
            vec!["(forall (?P1) (=> (instance ?P1 Human) (exists (?X1 ?E1 ?X2) (and (instance ?X1 Dog) (instance ?E1 Seeing) (experiencer ?E1 ?X1) (instance ?X2 Book) (possesses ?P1 ?X2) (patient ?E1 ?X2)))))"]
        );
        // The verb's domain still gates the generic class.
        let rock = Opts {
            generic_you: Some("Rock".into()),
            ..Opts::default()
        };
        assert_eq!(with("You read a book.", rock), vec!["[role-mismatch]"]);
        let missing = Opts {
            generic_you: Some("Martian".into()),
            ..Opts::default()
        };
        assert_eq!(with("You read a book.", missing), vec!["[term-not-in-kb]"]);
    }

    #[test]
    fn possessives_use_the_have_typing() {
        assert_eq!(
            one("Rex's tail is red."),
            "(and (instance Tail-1 Tail) (part Tail-1 Rex) (attribute Tail-1 Red))"
        );
        assert_eq!(
            one("Bob's book is red."),
            "(and (instance Book-1 Book) (possesses Bob Book-1) (attribute Book-1 Red))"
        );
        assert_eq!(
            one("Mike reads the dog's book."),
            "(and (instance Reading-1 Reading) (agent Reading-1 Mike) (instance Dog-1 Dog) (instance Book-1 Book) (possesses Dog-1 Book-1) (patient Reading-1 Book-1))"
        );
        assert_eq!(
            kif("Mary sees a book. Her book is red.")[1],
            "(and (instance Book-2 Book) (possesses Mary Book-2) (attribute Book-2 Red))"
        );
        assert_eq!(one("Rex's boy is loud."), "[unsupported/possessive]");
    }

    #[test]
    fn name_genders_csv() {
        let rows = name_genders_from_csv(
            "\"name\",\"gender\"\n\"Aaron\",\"M\"\n\"Abigail\",\"F\"\n\"Pat\",\"\"\n",
        );
        assert_eq!(rows[0], ("Aaron".to_string(), Some(token::Gender::Male)));
        assert_eq!(rows[1].1, Some(token::Gender::Female));
        assert_eq!(rows[2], ("Pat".to_string(), None));
        assert_eq!(
            names_from_csv("\"name\",\"gender\"\n\"Aaron\",\"M\"\n"),
            vec!["Aaron"]
        );
    }

    // -- Conditionals ---------------------------------------------------------------------

    #[test]
    fn conditionals_bind_antecedent_referents() {
        assert_eq!(
            one("If a dog barks, it is loud."),
            "(forall (?X1 ?E1) (=> (and (instance ?X1 Dog) (instance ?E1 Barking) (agent ?E1 ?X1)) (attribute ?X1 Loud)))"
        );
        assert_eq!(
            one("If a dog barks then the dog is loud."),
            "(forall (?X1 ?E1) (=> (and (instance ?X1 Dog) (instance ?E1 Barking) (agent ?E1 ?X1)) (attribute ?X1 Loud)))"
        );
        assert_eq!(
            one("Rex is loud if Rex barks."),
            "(forall (?E1) (=> (and (instance ?E1 Barking) (agent ?E1 Rex)) (attribute Rex Loud)))"
        );
        // No comma: the split is found by parsing.
        assert_eq!(
            one("When dogs bark cats fall."),
            "(forall (?X1 ?E1) (=> (and (instance ?X1 Dog) (instance ?E1 Barking) (agent ?E1 ?X1)) (exists (?X2 ?E2) (and (instance ?X2 Cat) (instance ?E2 Falling) (agent ?E2 ?X2)))))"
        );
        // A consequent indefinite is existential; "every" keeps its force.
        assert_eq!(
            one("If every dog barks, a cat falls."),
            "(=> (forall (?X1) (=> (instance ?X1 Dog) (exists (?E1) (and (instance ?E1 Barking) (agent ?E1 ?X1))))) (exists (?X2 ?E2) (and (instance ?X2 Cat) (instance ?E2 Falling) (agent ?E2 ?X2))))"
        );
        // Antecedent variables do not leak into later sentences.
        assert_eq!(
            kif("If a dog barks, it is loud. Rex sees it.")[1],
            "[unsupported/pronoun]"
        );
        let generic = Opts {
            generic_you: Some("Human".into()),
            ..Opts::default()
        };
        assert_eq!(
            with("If you read a book, you are loud.", generic),
            vec!["(forall (?P1) (=> (instance ?P1 Human) (forall (?E1 ?X1) (=> (and (instance ?E1 Reading) (agent ?E1 ?P1) (instance ?X1 Book) (patient ?E1 ?X1)) (attribute ?P1 Loud)))))"]
        );
        assert_eq!(
            refusal("Rex barks because Mike reads.").reason.code(),
            "unsupported/subordination"
        );
        assert_eq!(
            refusal("If Rex barks, Zeke reads.").reason.code(),
            "unknown-name"
        );
    }

    // -- Modals ---------------------------------------------------------------------------

    #[test]
    fn modals_scope_over_the_clause() {
        assert_eq!(
            one("Dogs can bark."),
            "(forall (?X1) (=> (instance ?X1 Dog) (modalAttribute (exists (?E1) (and (instance ?E1 Barking) (agent ?E1 ?X1))) Possibility)))"
        );
        assert_eq!(
            one("Rex cannot bark."),
            "(not (modalAttribute (exists (?E1) (and (instance ?E1 Barking) (agent ?E1 Rex))) Possibility))"
        );
        assert_eq!(
            one("Rex must bark."),
            "(modalAttribute (exists (?E1) (and (instance ?E1 Barking) (agent ?E1 Rex))) Necessity)"
        );
        assert_eq!(
            one("Rex should not bark."),
            "(modalAttribute (not (exists (?E1) (and (instance ?E1 Barking) (agent ?E1 Rex)))) Obligation)"
        );
        assert_eq!(
            one("Can Rex bark?"),
            "(modalAttribute (exists (?E1) (and (instance ?E1 Barking) (agent ?E1 Rex))) Possibility)"
        );
        // Objects, prepositional phrases, statives, "be" and "have" are the
        // ordinary clause rules under the modality; nothing is minted.
        assert_eq!(
            one("Dogs can chase cats."),
            "(forall (?X1) (=> (instance ?X1 Dog) (modalAttribute (exists (?E1 ?X2) (and (instance ?E1 Pursuing) (agent ?E1 ?X1) (instance ?X2 Cat) (patient ?E1 ?X2))) Possibility)))"
        );
        assert_eq!(
            one("Rex can fall in the garden."),
            "(modalAttribute (exists (?E1 ?X1) (and (instance ?E1 Falling) (agent ?E1 Rex) (instance ?X1 Garden) (eventLocated ?E1 ?X1))) Possibility)"
        );
        assert_eq!(
            one("Rex may own a car."),
            "(modalAttribute (exists (?X1) (and (instance ?X1 Automobile) (possesses Rex ?X1))) Possibility)"
        );
        assert_eq!(
            one("Rex can be loud."),
            "(modalAttribute (attribute Rex Loud) Possibility)"
        );
        assert_eq!(
            one("Rex must have a tail."),
            "(modalAttribute (exists (?X1) (and (instance ?X1 Tail) (part ?X1 Rex))) Necessity)"
        );
        assert_eq!(one("Rex be loud."), "[no-rule]");
        let generic = Opts {
            generic_you: Some("Human".into()),
            ..Opts::default()
        };
        assert_eq!(
            with("You can bark.", generic),
            vec!["(forall (?P1) (=> (instance ?P1 Human) (modalAttribute (exists (?E1) (and (instance ?E1 Barking) (agent ?E1 ?P1))) Possibility)))"]
        );
        // A modal no rule reaches is still reported as the construct.
        assert_eq!(one("Dogs are must."), "[unsupported/modal]");
    }

    // -- Refusals for recognized constructs ----------------------------------------------

    #[test]
    fn unsupported_constructs_name_the_construct() {
        let cases = [
            ("He is a dog.", "unsupported/pronoun"),
            ("Mike reads their book.", "unsupported/pronoun"),
            ("Rex is loud or red.", "unsupported/coordination"),
            ("Rex barks because Mike reads.", "unsupported/subordination"),
            ("Rex will bark.", "unsupported/modal"),
            ("Rex would bark.", "unsupported/modal-counterfactual"),
            ("The dog that barks is loud.", "unsupported/relative-clause"),
            ("Rex has a boy.", "unsupported/have"),
            ("Rex's boy is loud.", "unsupported/possessive"),
            ("It is loud.", "unsupported/expletive-it"),
            ("There is a dog.", "unsupported/existential-there"),
            ("That dog is loud.", "unsupported/demonstrative"),
            ("Mike reads 5 books.", "unsupported/numeric"),
            ("Rex barks quickly.", "unsupported/adverb"),
            ("Rex is every dog.", "unsupported/quantified-predicate"),
        ];
        for (text, code) in cases {
            let r = refusal(text);
            assert_eq!(r.reason.code(), code, "{text}: {r}");
            assert!(!r.reason.hint().is_empty());
        }
        assert_eq!(
            refusal("Mike reads their book.").reason.to_string(),
            "unsupported pronoun at \"their (third person, plural, possessive)\""
        );
    }

    #[test]
    fn no_rule_reports_furthest_expectation() {
        let r = refusal("Rex garden.");
        assert_eq!(r.reason.code(), "no-rule");
        match r.reason {
            Refusal::NoRule { at, expected } => {
                assert_eq!(at, "garden");
                assert!(
                    expected.iter().any(|e| e.contains("is/are")),
                    "{expected:?}"
                );
                assert!(expected.iter().any(|e| e.contains("verb")), "{expected:?}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(r.start, 4);
        assert_eq!(r.end, 10);
        assert_eq!(
            refusal("Mike reads a book garden.").reason.code(),
            "trailing-tokens"
        );
    }

    #[test]
    fn empty_input_and_histogram() {
        assert!(run("").sentences.is_empty());
        let t = run("He barks. She barks. Rex is a florb. Rex is a dog.");
        assert!(!t.complete());
        assert_eq!(
            t.histogram(),
            vec![
                ("unsupported/pronoun".to_string(), 2),
                ("unknown-word".to_string(), 1)
            ]
        );
        assert_eq!(t.assertions().len(), 1);
    }

    #[test]
    fn custom_grammar_extends_prepositions() {
        let g = Grammar::standard().preposition(PrepRule {
            prep: "near",
            complement: None,
            relation: "located",
            event_only: false,
        });
        let t = translate_with(
            &kb(),
            &wn(),
            &g,
            "Rex is near the garden.",
            &Opts::default(),
        );
        assert!(t.complete());
    }
}
