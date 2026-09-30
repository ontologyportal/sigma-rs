//! `BasicNp`: `DET? ADJ* NOUN` or a proper name.
//!
//! Every split of the open-word run into adjectives plus a head is a
//! candidate, longest head-position first; two- and three-word compounds
//! that WordNet or the KB knows as one lemma ("motor vehicle") are heads
//! too.  The sentence rule picks the reading under which the rest parses.

use crate::lexicon::Pos;

use super::super::grammar::{Cx, Head, Np, NpRule};
use super::super::refuse::{Construct, Refusal, Refused};
use super::super::token::{Closed, Det};

pub struct BasicNp;

impl NpRule for BasicNp {
    fn name(&self) -> &'static str {
        "np"
    }

    fn candidates(&self, cx: &mut Cx, at: usize) -> Result<Vec<(Np, usize)>, Refused> {
        let mut i = at;
        let det = match cx.tok(i).and_then(|t| t.closed()) {
            Some(Closed::Det(d)) => {
                i += 1;
                Some(d)
            }
            _ => None,
        };
        if !cx.tok(i).is_some_and(|t| t.is_open()) {
            cx.miss::<()>(i, "a noun");
            return Ok(Vec::new());
        }
        if let Some(name) = self.name(cx, at, i, det)? {
            return Ok(vec![name]);
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
        let mut out = Vec::new();
        // Head ending at position `end` (exclusive), preceded by adjectives.
        for end in (1..=words.len()).rev() {
            for len in [3usize, 2, 1] {
                if len > end {
                    continue;
                }
                let start = end - len;
                let joined = words[start..end].join("_");
                if !cx.lex.is_nounish(&joined) {
                    continue;
                }
                let mods = &words[..start];
                if !mods.iter().all(|m| cx.lex.is_adjectival(m)) {
                    continue;
                }
                let word = if len == 1 {
                    cx.tokens[i + start].text.clone()
                } else {
                    joined
                };
                let (lemma, plural) = cx.lex.noun_lemma(&word);
                if det == Some(Det::Indefinite) && plural {
                    cx.miss::<()>(i + start, "a singular noun after a/an");
                    continue;
                }
                out.push((
                    Np {
                        det,
                        mods: mods.to_vec(),
                        head: Head::Noun {
                            word,
                            lemma,
                            plural,
                        },
                        start: at,
                        end: i + end,
                    },
                    i + end,
                ));
            }
        }
        if out.is_empty() {
            // Say what the first unusable word is.
            for (k, w) in words.iter().enumerate() {
                if !cx.lex.is_nounish(w) && !cx.lex.is_adjectival(w) {
                    if cx.lex.has_sense(w, Pos::Adv) {
                        return Err(cx.refused(
                            Refusal::Unsupported {
                                construct: Construct::Adverb,
                                at: w.clone(),
                            },
                            i + k,
                        ));
                    }
                    if !cx.lex.is_verbal(w) {
                        return Err(cx.refused(Refusal::UnknownWord { word: w.clone() }, i + k));
                    }
                }
            }
            cx.miss::<()>(i, "a noun");
        }
        Ok(out)
    }
}

impl BasicNp {
    /// A proper name at `i` (after an optional determiner at `at`).
    ///
    /// Capitalization is the signal, except at the start of a sentence where
    /// it is not evidence: there a common noun wins.  Elsewhere a capitalized
    /// word reads as a common noun only when the KB labels a term with it or
    /// WordNet lists it capitalized ("Monday").  A capitalized word whose
    /// noun reading grounds to nothing is a name.
    fn name(
        &self,
        cx: &mut Cx,
        at: usize,
        i: usize,
        det: Option<Det>,
    ) -> Result<Option<(Np, usize)>, Refused> {
        let first = &cx.tokens[i];
        if !first.capitalized || det.is_some() {
            return Ok(None);
        }
        let known_name = cx.lex.name(&first.text).is_some();
        let listed = cx.opts.names.iter().any(|n| n == &first.text);
        let unusable_noun = !cx.lex.noun_lemma(&first.lower).1
            && matches!(
                cx.lex.noun_class(&first.lower),
                Err(Refusal::UnknownWord { .. }
                    | Refusal::NoSumoAnchor { .. }
                    | Refusal::TermNotInKb { .. })
            );
        let name_like = if first.sentence_initial {
            !cx.lex.is_nounish(&first.lower) || unusable_noun
        } else {
            (!cx.lex.has_label(&cx.lex.noun_lemma(&first.lower).0)
                && !cx.lex.has_capitalized_lemma(&first.text))
                || unusable_noun
        };
        if !(known_name || listed || name_like) {
            return Ok(None);
        }
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
            return Err(cx.refused(Refusal::UnknownName { name: spaced }, i));
        };
        Ok(Some((
            Np {
                det: None,
                mods: Vec::new(),
                head,
                start: at,
                end: j,
            },
            j,
        )))
    }
}
