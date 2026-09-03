//! Offline WordNet 3.0 lexicon with SUMO anchors -- the sidecar index behind
//! synonym-aware `search`, the WordNet section of `man`, and (eventually) the
//! lexical-grounding stage of a text front end.
//!
//! Data source: the `WordNetMappings30-{noun,verb,adj,adv}.txt` files shipped
//! with SUMO (annotated WordNet 3.0 `data.pos` files -- each synset record
//! carries a trailing `&%Term<kind>` SUMO anchor).  These four files are
//! self-contained: words, glosses, and anchors all come from them, so no
//! separate WordNet installation is required.  Two optional companions
//! sharpen results when present in the same directory:
//!
//!   - `index.sense`   -- orders each lemma's synsets by corpus frequency
//!                       (most-frequent sense first);
//!   - `noun.exc` / `verb.exc` -- irregular-inflection tables
//!                       (`children child`, `ran run`) for lookup fallback.
//!
//! This is deliberately **not** part of the [`crate::kb::KnowledgeBase`]
//! sentence store: ~117k synsets would become half a million ground atoms of
//! never-reasoning-relevant content flowing through SInE, sessions, and
//! translation.  The lexicon is static reference data with its own natural
//! shape (lemma -> synsets -> SUMO term); callers pass `&WordNet` to the APIs
//! that want it (see `SearchOpts::lexicon`).
//!
//! **No filesystem access here**: [`WordNet::from_texts`] is the only
//! constructor

pub mod parse;
pub mod wsd;

use std::collections::HashMap;
use std::fmt;

pub use wsd::*;

// -- Public types ------------------------------------------------------------

/// WordNet part of speech.  Adjective satellites (`s` records) fold into
/// [`Pos::Adj`]; sense keys with satellite type 5 fold the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Pos {
    Noun,
    Verb,
    Adj,
    Adv,
}

impl Pos {
    /// One-letter tag as used in sense labels (`dog#n#1`).
    pub fn as_char(self) -> char {
        match self {
            Pos::Noun => 'n',
            Pos::Verb => 'v',
            Pos::Adj => 'a',
            Pos::Adv => 'r',
        }
    }

    fn from_ss_type(c: char) -> Option<Pos> {
        match c {
            'n' => Some(Pos::Noun),
            'v' => Some(Pos::Verb),
            'a' | 's' => Some(Pos::Adj),
            'r' => Some(Pos::Adv),
            _ => None,
        }
    }
}

/// How a synset is anchored to its SUMO term, denoted as a single character
///  appended to the `&%Term<kind>` annotation.  `Equivalent` (`=`) means the
///  term *is* the concept; `Subsuming` (`+`) means the term is the nearest
///  SUMO ancestor; `Instance` (`@`) anchors the synset to an individual.  
///  The rare negated forms (`:`, `[`, `]`) and anything unrecognized land
///  in `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingKind {
    Equivalent,
    Subsuming,
    Instance,
    Other(char),
}

impl MappingKind {
    fn from_suffix(c: char) -> Option<MappingKind> {
        match c {
            '=' => Some(MappingKind::Equivalent),
            '+' => Some(MappingKind::Subsuming),
            '@' => Some(MappingKind::Instance),
            ':' | '[' | ']' => Some(MappingKind::Other(c)),
            _ => None,
        }
    }

    /// The annotation suffix character (`=`, `+`, `@`, ...) -- the same compact
    /// notation the mappings files use, suitable for terminal rendering.
    pub fn suffix(self) -> char {
        match self {
            MappingKind::Equivalent => '=',
            MappingKind::Subsuming => '+',
            MappingKind::Instance => '@',
            MappingKind::Other(c) => c,
        }
    }
}

/// One `&%Term<kind>` anchor on a synset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SumoAnchor {
    pub term: String,
    pub kind: MappingKind,
}

/// A synset: one WordNet concept
#[derive(Debug, Clone)]
pub struct Synset {
    /// Synset ID
    pub pos: Pos,
    /// Byte offset in the data file
    pub offset: u32,
    /// The surface words (multiword expressions preserved with '_')
    pub words: Vec<String>,
    /// The synset gloss (definition)
    pub gloss: String,
    /// The SUMO anchors corresponding to this Synset
    pub sumo: Vec<SumoAnchor>,
}

/// Stable synset identity: (part of speech, byte offset in the data file) --
/// the same key WordNet pointers use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SynsetId {
    pub pos: Pos,
    pub offset: u32,
}

/// One sense of a query word: the synset plus which surface lemma matched
/// and its 1-based rank in the lemma's frequency-ordered sense list.
#[derive(Debug, Clone)]
pub struct Sense<'a> {
    pub synset: &'a Synset,
    pub lemma: String,
    pub sense_no: usize,
}

impl Sense<'_> {
    /// Compact sense label: `dog#n#1`.
    pub fn label(&self) -> String {
        format!(
            "{}#{}#{}",
            self.lemma,
            self.synset.pos.as_char(),
            self.sense_no
        )
    }
}

/// The loaded lexicon.  Construct with [`WordNet::from_texts`]; filesystem
/// callers go through `sigmakee_rs_sdk::lexicon::load_dir`.
pub struct WordNet {
    /// All the loaded synsets
    synsets: HashMap<SynsetId, Synset>,
    /// lowercase lemma (underscores intact) -> sense-ordered synsets.
    lemma_index: HashMap<String, Vec<SynsetId>>,
    /// SUMO term -> every synset anchored to it.
    sumo_index: HashMap<String, Vec<SynsetId>>,
    /// irregular inflection -> base form(s), from `noun.exc` / `verb.exc`.
    exceptions: HashMap<String, Vec<String>>,
}

impl fmt::Debug for WordNet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WordNet")
            .field("synsets", &self.synsets.len())
            .field("lemmas", &self.lemma_index.len())
            .field("sumo_terms", &self.sumo_index.len())
            .finish()
    }
}

// -- Loading -----------------------------------------------------------------

impl WordNet {
    // -- Lookup --------------------------------------------------------------

    /// Number of loaded synsets.
    pub fn len(&self) -> usize {
        self.synsets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.synsets.is_empty()
    }

    /// All senses of `word` (case-insensitive; spaces normalize to the
    /// underscores multi-word lemmas are stored with), most-frequent sense
    /// first per part of speech.  Falls back through the irregular-inflection
    /// tables and a light plural strip when the surface form itself is not a
    /// lemma -- so `children` and `dogs` both resolve.
    pub fn senses(&self, word: &str) -> Vec<Sense<'_>> {
        let key = word.trim().to_lowercase().replace(' ', "_");
        if key.is_empty() {
            return Vec::new();
        }
        let mut out = self.senses_exact(&key);
        if out.is_empty() {
            for base in self.exceptions.get(&key).into_iter().flatten() {
                out.extend(self.senses_exact(&base.to_lowercase()));
            }
        }
        if out.is_empty() {
            for cand in plural_candidates(&key) {
                out = self.senses_exact(&cand);
                if !out.is_empty() {
                    break;
                }
            }
        }
        out
    }

    fn senses_exact(&self, key: &str) -> Vec<Sense<'_>> {
        let Some(ids) = self.lemma_index.get(key) else {
            return Vec::new();
        };
        // sense_no is per (lemma, pos): dog#n#1, dog#n#2, ... restart at
        // dog#v#1 -- matching WordNet's own per-POS sense numbering.
        let mut per_pos: HashMap<Pos, usize> = HashMap::new();
        ids.iter()
            .filter_map(|id| self.synsets.get(id))
            .map(|synset| {
                let n = per_pos.entry(synset.pos).or_insert(0);
                *n += 1;
                Sense {
                    synset,
                    lemma: key.to_string(),
                    sense_no: *n,
                }
            })
            .collect()
    }

    /// Every synset anchored to SUMO term `term` -- the reverse direction,
    /// for man-page style "which words map here" listings.
    pub fn synsets_of_term(&self, term: &str) -> Vec<&Synset> {
        self.sumo_index
            .get(term)
            .into_iter()
            .flatten()
            .filter_map(|id| self.synsets.get(id))
            .collect()
    }
}

/// Naive plural-strip candidates for a failed lookup, cheapest first:
/// `dogs`->`dog`, `boxes`->`box`, `ladies`->`lady`.  Irregulars are handled by
/// the exception tables before this runs.
fn plural_candidates(key: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(stem) = key.strip_suffix("ies") {
        if !stem.is_empty() {
            out.push(format!("{stem}y"));
        }
    }
    if let Some(stem) = key.strip_suffix("es") {
        if !stem.is_empty() {
            out.push(stem.to_string());
        }
    }
    if let Some(stem) = key.strip_suffix('s') {
        if !stem.is_empty() {
            out.push(stem.to_string());
        }
    }
    out
}
