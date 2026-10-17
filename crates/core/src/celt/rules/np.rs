//! `BasicNp`: `DET? ADJ* NOUN` or a proper name.
//!
//! Every split of the open-word run into adjectives plus a head is a
//! candidate, longest head-position first; two- and three-word compounds
//! that WordNet or the KB knows as one lemma ("motor vehicle") are heads
//! too.  Adjectives may be comma-separated ("a small, simple shelter").
//! A pronoun is a phrase of its own; a possessive pronoun or a phrase with
//! `'s` owns the noun run that follows ("your tail", "Bob's car").  The
//! sentence rule picks the reading under which the rest parses.

use crate::lexicon::Pos;

use super::super::grammar::{Cx, Head, Np, NpRule};
use super::super::refuse::{Construct, Refusal, Refused};
use super::super::token::{pronoun_info, Closed, Det};

pub struct BasicNp;

impl NpRule for BasicNp {
    fn name(&self) -> &'static str {
        "np"
    }

    fn candidates(&self, cx: &mut Cx, at: usize) -> Result<Vec<(Np, usize)>, Refused> {
        if let Some(pronoun) = cx.tok(at).filter(|t| t.is(Closed::Pronoun)) {
            let Some(info) = pronoun_info(&pronoun.lower) else {
                return Ok(Vec::new());
            };
            let np = Np {
                det: None,
                mods: Vec::new(),
                head: Head::Pronoun {
                    word: pronoun.text.clone(),
                    info,
                },
                possessor: None,
                start: at,
                end: at + 1,
            };
            let determiner = matches!(
                pronoun.lower.as_str(),
                "my" | "your" | "his" | "her" | "its" | "our" | "their"
            );
            let mut out = Vec::new();
            if determiner && cx.tok(at + 1).is_some_and(|t| t.is_open()) {
                out = self.possessed(cx, np.clone(), at + 1)?;
            }
            out.push((np, at + 1));
            return Ok(out);
        }
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
        if let Some((name, j)) = self.name(cx, at, i, det)? {
            // The name reads first; a KB instance that is also an adjective
            // ("Red" the attribute) may instead modify a following noun
            // ("Red dogs"), tried when the name reading fails.
            let mut out = Vec::new();
            if cx.tok(j).is_some_and(|t| t.is(Closed::Possessive)) {
                out.extend(self.possessed(cx, name.clone(), j + 1)?);
            }
            out.push((name.clone(), j));
            let first = &cx.tokens[i];
            let modifier = j == i + 1
                && cx.tok(j).is_some_and(|t| t.is_open())
                && cx.lex.is_adjectival(&first.lower);
            if modifier {
                out.extend(self.noun_run(cx, at, i, det)?);
            }
            return Ok(out);
        }
        let plain = self.noun_run(cx, at, i, det)?;
        let mut out = Vec::new();
        for (np, end) in &plain {
            if cx.tok(*end).is_some_and(|t| t.is(Closed::Possessive)) {
                out.extend(self.possessed(cx, np.clone(), end + 1)?);
            }
        }
        out.extend(plain);
        Ok(out)
    }
}

impl BasicNp {
    /// The noun run after an owner: every reading of `owner 's NOUN...`.
    fn possessed(&self, cx: &mut Cx, owner: Np, at: usize) -> Result<Vec<(Np, usize)>, Refused> {
        if !cx.tok(at).is_some_and(|t| t.is_open()) {
            cx.miss::<()>(at, "a noun after the possessive");
            return Ok(Vec::new());
        }
        let mut out = self.noun_run(cx, owner.start, at, None)?;
        for (np, _) in &mut out {
            np.possessor = Some(Box::new(owner.clone()));
        }
        Ok(out)
    }

    /// `ADJ* NOUN` readings of the open-word run starting at `i`, for a
    /// phrase that began at `at` with determiner `det`.
    fn noun_run(
        &self,
        cx: &mut Cx,
        at: usize,
        i: usize,
        det: Option<Det>,
    ) -> Result<Vec<(Np, usize)>, Refused> {
        // Run of open words; a comma between two open words continues it
        // ("small, simple shelter") and marks the word after it.
        let mut positions: Vec<usize> = vec![i];
        let mut after_comma: Vec<bool> = vec![false];
        let mut run_end = i + 1;
        loop {
            if cx.tok(run_end).is_some_and(|t| t.is_open()) {
                positions.push(run_end);
                after_comma.push(false);
                run_end += 1;
            } else if cx.tok(run_end).is_some_and(|t| t.is_punct(','))
                && cx.tok(run_end + 1).is_some_and(|t| t.is_open())
            {
                positions.push(run_end + 1);
                after_comma.push(true);
                run_end += 2;
            } else {
                break;
            }
        }
        let words: Vec<String> = positions
            .iter()
            .map(|&p| cx.tokens[p].lower.clone())
            .collect();
        let mut out = Vec::new();
        // Head ending at position `end` (exclusive), preceded by adjectives.
        for end in (1..=words.len()).rev() {
            for len in [3usize, 2, 1] {
                if len > end {
                    continue;
                }
                let start = end - len;
                // No comma inside the head or right before it.
                if after_comma[start..end].iter().any(|&c| c) {
                    continue;
                }
                let joined = words[start..end].join("_");
                if !cx.lex.is_nounish(&joined) {
                    continue;
                }
                let mods = &words[..start];
                if !mods.iter().all(|m| cx.lex.is_adjectival(m)) {
                    continue;
                }
                let head_tok = &cx.tokens[positions[start]];
                let word = if len > 1 {
                    joined
                } else if head_tok.sentence_initial {
                    head_tok.lower.clone()
                } else {
                    head_tok.text.clone()
                };
                let (lemma, plural) = cx.lex.noun_lemma(&word);
                if det == Some(Det::Indefinite) && plural {
                    cx.miss::<()>(positions[start], "a singular noun after a/an");
                    continue;
                }
                let next = positions[end - 1] + 1;
                out.push((
                    Np {
                        det,
                        mods: mods.to_vec(),
                        head: Head::Noun {
                            word,
                            lemma,
                            plural,
                        },
                        possessor: None,
                        start: at,
                        end: next,
                    },
                    next,
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
                            positions[k],
                        ));
                    }
                    if !cx.lex.is_verbal(w) {
                        return Err(
                            cx.refused(Refusal::UnknownWord { word: w.clone() }, positions[k])
                        );
                    }
                }
            }
            cx.miss::<()>(i, "a noun");
        }
        Ok(out)
    }

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
        // Mid-sentence the capital admits proper-noun senses ("Monday");
        // sentence-initially it is no evidence and the word reads lowercase.
        let as_noun = if first.sentence_initial {
            &first.lower
        } else {
            &first.text
        };
        let unusable_noun = !cx.lex.noun_lemma(&first.lower).1
            && matches!(
                cx.lex.noun_class(as_noun),
                Err(Refusal::UnknownWord { .. }
                    | Refusal::NoSumoAnchor { .. }
                    | Refusal::TermNotInKb { .. })
            );
        let name_like = if first.sentence_initial {
            (!cx.lex.is_nounish(&first.lower) && !cx.lex.is_adjectival(&first.lower))
                || unusable_noun
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
                possessor: None,
                start: at,
                end: j,
            },
            j,
        )))
    }
}
