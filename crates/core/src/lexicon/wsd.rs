//! Word-sense disambiguation auxillary module


use std::collections::HashMap;
use std::fmt;

use super::Pos;

/// Index to represent SUMO's WSD data (`wordFrequencies_combined.txt`)
/// per sense key
/// 
/// The WSD index helps to further decide a word's given sense / mapping
/// based on the co-occurence of other words with a given word sense word.
/// For example, the word "dog" corresponds to multiple senses (e.g. the 
/// animal, the action of persisting upon someone), the coincidence of the
/// word "woof" may indicate the term is its first sense and corresponds to
/// the SUMO term "DomesticDog".
pub struct WsdIndex {
    by_key: HashMap<(String, Pos), Vec<WsdSense>>,
}

struct WsdSense {
    term: String,
    bag: HashMap<String, u32>,
}

impl fmt::Debug for WsdIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WsdIndex")
            .field("keys", &self.by_key.len())
            .finish()
    }
}

impl WsdIndex {
    /// Parse the `Word: <lemma>_<POS>_<n> Values: w_c ...  SUMOTerm: <term>`
    /// line format.  Unparseable lines are skipped.
    pub fn from_text(text: &str) -> WsdIndex {
        let mut by_key: HashMap<(String, Pos), Vec<WsdSense>> = HashMap::new();
        for line in text.lines() {
            let Some(rest) = line.strip_prefix("Word: ") else {
                continue;
            };
            let Some((key, rest)) = rest.split_once(" Values: ") else {
                continue;
            };
            let Some(idx) = rest.find("SUMOTerm:") else {
                continue;
            };
            let values = &rest[..idx];
            let Some(term) = rest[idx + "SUMOTerm:".len()..].split_whitespace().next() else {
                continue;
            };

            // Key: lemma_POS_senseNo, split from the right (lemmas may
            // contain underscores).
            let mut it = key.rsplitn(3, '_');
            let (Some(_sense_no), Some(pos_tag), Some(lemma)) = (it.next(), it.next(), it.next())
            else {
                continue;
            };
            let pos = match pos_tag.as_bytes().first() {
                Some(b'N') => Pos::Noun,
                Some(b'V') => Pos::Verb,
                Some(b'J') => Pos::Adj,
                Some(b'R') => Pos::Adv,
                _ => continue,
            };

            let mut bag = HashMap::new();
            for tok in values.split_whitespace() {
                if let Some((w, c)) = tok.rsplit_once('_') {
                    if let Ok(c) = c.parse::<u32>() {
                        *bag.entry(w.to_lowercase()).or_insert(0) += c;
                    }
                }
            }
            by_key
                .entry((lemma.to_lowercase(), pos))
                .or_default()
                .push(WsdSense {
                    term: term.to_string(),
                    bag,
                });
        }
        WsdIndex { by_key }
    }

    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    /// Context-overlap score for reading `lemma`(`pos`) as SUMO `term`:
    /// the summed counts of `ctx` words in the bags of every sense of the
    /// lemma anchored to that term.  Zero when the lemma or term is
    /// unknown here.
    pub fn term_score(&self, lemma: &str, pos: Pos, term: &str, ctx: &[String]) -> u32 {
        let Some(senses) = self.by_key.get(&(lemma.to_lowercase(), pos)) else {
            return 0;
        };
        senses
            .iter()
            .filter(|s| s.term == term)
            .map(|s| ctx.iter().filter_map(|w| s.bag.get(w)).sum::<u32>())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn wsd_index_parses_and_scores() {
        let wsd = WsdIndex::from_text(
            "\
Word: run_VB_1 Values: track_2 fast_3  SUMOTerm: Running
Word: run_VB_2 Values: company_5 business_3  SUMOTerm: Managing
Word: junk line without markers
",
        );
        assert_eq!(wsd.len(), 1, "both senses share the (run, Verb) key");
        let ctx = vec!["mary".to_string(), "company".to_string()];
        assert_eq!(wsd.term_score("run", Pos::Verb, "Managing", &ctx), 5);
        assert_eq!(wsd.term_score("run", Pos::Verb, "Running", &ctx), 0);
        assert_eq!(
            wsd.term_score("run", Pos::Noun, "Managing", &ctx),
            0,
            "POS-keyed: noun `run` has no entries"
        );
        assert_eq!(wsd.term_score("walk", Pos::Verb, "Walking", &ctx), 0);
    }    
}