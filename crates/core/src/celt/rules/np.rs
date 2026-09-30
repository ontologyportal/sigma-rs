//! `BasicNp`: `DET? ADJ* NOUN` or a proper name.
//!
//! Adjectives are open words with an adjective sense; the head is the first
//! noun-like word that ends the phrase (the last open word, or one followed
//! by a verb).  Two- and three-word compounds that WordNet or the KB knows
//! as one lemma ("motor vehicle") are taken as one head.

use crate::lexicon::Pos;

use super::super::grammar::{Cx, Head, Np, NpRule, Step};
use super::super::refuse::{Construct, Refusal};
use super::super::token::{Closed, Det};

pub struct BasicNp;

impl NpRule for BasicNp {
    fn name(&self) -> &'static str {
        "np"
    }

    fn apply(&self, cx: &mut Cx, at: usize) -> Step<Np> {
        let mut i = at;
        let det = match cx.tok(i).and_then(|t| t.closed()) {
            Some(Closed::Det(d)) => {
                i += 1;
                Some(d)
            }
            _ => None,
        };
        let Some(first) = cx.tok(i).filter(|t| t.is_open()) else {
            return cx.miss(i, "a noun");
        };
        // Proper name.  Capitalization is the signal, except at the start
        // of a sentence where it is not evidence: there a common noun wins.
        // Mid-sentence a capitalized word reads as a common noun only when
        // the KB labels a term with it or WordNet lists it capitalized
        // ("Monday"); a lowercase WordNet sense alone ("mike") does not.
        let known_name = first.capitalized && cx.lex.name(&first.text).is_some();
        let listed = first.capitalized && cx.opts.names.iter().any(|n| n == &first.text);
        // A common noun wins at the start of a sentence (capitalization is
        // not evidence there) and, elsewhere, when the KB labels a term with
        // the word or WordNet lists it capitalized ("Monday").  A word whose
        // noun reading grounds to nothing is reported as a name instead.
        let unusable_noun = |cx: &Cx| {
            matches!(
                cx.lex.noun_class(&first.lower),
                Err(Refusal::UnknownWord { .. }
                    | Refusal::NoSumoAnchor { .. }
                    | Refusal::TermNotInKb { .. })
            )
        };
        let name_like = first.capitalized
            && if first.sentence_initial {
                !cx.lex.is_nounish(&first.lower) || unusable_noun(cx)
            } else {
                (!cx.lex.has_label(&cx.lex.noun_lemma(&first.lower).0)
                    && !cx.lex.has_capitalized_lemma(&first.text))
                    || unusable_noun(cx)
            };
        if (known_name || listed || name_like) && det.is_none() {
            let mut j = i + 1;
            while cx.tok(j).is_some_and(|t| t.is_open() && t.capitalized) {
                j += 1;
            }
            let words: Vec<&str> = cx.tokens[i..j].iter().map(|t| t.text.as_str()).collect();
            let joined = words.concat();
            let spaced = words.join(" ");
            let head = if let Some(sym) = cx.lex.name(&joined).or_else(|| cx.lex.name(&spaced)) {
                Head::Name(sym)
            } else if cx.opts.assume_human_names || listed {
                Head::NewName(joined)
            } else {
                return cx.refuse(Refusal::UnknownName { name: spaced }, i);
            };
            return Step::Hit(
                Np {
                    det: None,
                    mods: Vec::new(),
                    head,
                    start: at,
                    end: j,
                },
                j,
            );
        }
        // Run of open words.
        let mut run_end = i;
        while cx.tok(run_end).is_some_and(|t| t.is_open()) {
            run_end += 1;
        }
        let words: Vec<String> = cx.tokens[i..run_end]
            .iter()
            .map(|t| t.lower.clone())
            .collect();
        // Head position: first noun-like word at the end of the run or
        // followed by a verb (that is not also an adjective).
        let mut head_len = 1;
        let mut head_end = None;
        'outer: for k in 0..words.len() {
            for len in [3usize, 2, 1] {
                if k + len > words.len() {
                    continue;
                }
                let joined = words[k..k + len].join("_");
                if !cx.lex.is_nounish(&joined) {
                    continue;
                }
                let after = words.get(k + len);
                let boundary = match after {
                    None => true,
                    Some(w) => cx.lex.is_verbal(w) && !cx.lex.is_adjectival(w),
                };
                if boundary {
                    head_len = len;
                    head_end = Some(k + len);
                    break 'outer;
                }
            }
        }
        let Some(hend) = head_end else {
            // Every word in the run: what is it?
            for (k, w) in words.iter().enumerate() {
                if !cx.lex.is_nounish(w) && !cx.lex.is_adjectival(w) {
                    if cx.lex.has_sense(w, Pos::Adv) {
                        return cx.refuse(
                            Refusal::Unsupported {
                                construct: Construct::Adverb,
                                at: w.clone(),
                            },
                            i + k,
                        );
                    }
                    if !cx.lex.is_verbal(w) {
                        return cx.refuse(Refusal::UnknownWord { word: w.clone() }, i + k);
                    }
                }
            }
            return cx.miss(i, "a noun");
        };
        let hstart = hend - head_len;
        let mods: Vec<String> = words[..hstart].to_vec();
        for (k, m) in mods.iter().enumerate() {
            if !cx.lex.is_adjectival(m) {
                return cx.miss(i + k, "an adjective");
            }
        }
        let word = if head_len == 1 {
            cx.tokens[i + hstart].text.clone()
        } else {
            words[hstart..hend].join("_")
        };
        let (lemma, plural) = cx.lex.noun_lemma(&word);
        if det == Some(Det::Indefinite) && plural {
            return cx.miss(i + hstart, "a singular noun after a/an");
        }
        Step::Hit(
            Np {
                det,
                mods,
                head: Head::Noun {
                    word,
                    lemma,
                    plural,
                },
                start: at,
                end: i + hend,
            },
            i + hend,
        )
    }
}
