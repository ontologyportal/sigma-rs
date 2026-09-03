//! Module responsible for parsing WordNet data files

use std::collections::HashMap;

use super::{MappingKind, Pos, SumoAnchor, Synset, SynsetId, WordNet};

impl WordNet {
    /// Parse `(data text, pos)` pairs -- the contents of the
    /// `WordNetMappings30-*.txt` files -- plus the optional `index.sense`
    /// and exception-list (`noun.exc`/`verb.exc`, concatenated) contents
    pub fn from_texts<'a>(
        texts: impl IntoIterator<Item = (&'a str, Pos)>,
        index_sense: Option<&str>,
        exceptions: Option<&str>,
    ) -> WordNet {
        Self::build(texts.into_iter(), index_sense, exceptions)
    }

    fn build<'a>(
        texts: impl Iterator<Item = (&'a str, Pos)>,
        index_sense: Option<&str>,
        exceptions: Option<&str>,
    ) -> WordNet {
        let mut wn = WordNet {
            synsets: HashMap::new(),
            lemma_index: HashMap::new(),
            sumo_index: HashMap::new(),
            exceptions: HashMap::new(),
        };
        for (text, pos) in texts {
            for line in text.lines() {
                if let Some(s) = parse_data_line(line, Some(pos)) {
                    let id = SynsetId {
                        pos: s.pos,
                        offset: s.offset,
                    };
                    for w in &s.words {
                        wn.lemma_index.entry(w.to_lowercase()).or_default().push(id);
                    }
                    for a in &s.sumo {
                        wn.sumo_index.entry(a.term.clone()).or_default().push(id);
                    }
                    wn.synsets.insert(id, s);
                }
            }
        }
        // Sense-order each lemma bucket (most-frequent first) when
        // `index.sense` is available; unranked senses keep file order after
        // the ranked ones.  Also dedup: a lemma appearing twice in one
        // synset's word list (case variants) must not list the synset twice.
        let ranks = index_sense.map(parse_index_sense).unwrap_or_default();
        for (lemma, ids) in wn.lemma_index.iter_mut() {
            ids.dedup();
            ids.sort_by_key(|id| {
                ranks
                    .get(&(lemma.clone(), id.pos, id.offset))
                    .copied()
                    .unwrap_or(u32::MAX)
            });
        }
        for ids in wn.sumo_index.values_mut() {
            ids.sort();
            ids.dedup();
        }
        if let Some(exc) = exceptions {
            for line in exc.lines() {
                let mut it = line.split_whitespace();
                if let Some(inflected) = it.next() {
                    let bases: Vec<String> = it.map(str::to_string).collect();
                    if !bases.is_empty() {
                        wn.exceptions.insert(inflected.to_string(), bases);
                    }
                }
            }
        }
        wn
    }

    /// Extend the lexicon with local records in the same annotated
    /// `data.pos` format, each record's own `ss_type` deciding its POS --
    /// the user-extensible channel for domain vocabulary the shipped
    /// mappings lack (`stop_sign` -> a local `&%StopSign=` line).  Local
    /// senses append after the shipped ones (lowest priority) except for
    /// words the shipped lexicon lacks entirely.
    pub fn extend_mixed(&mut self, text: &str) {
        for line in text.lines() {
            if let Some(s) = parse_data_line(line, None) {
                let id = SynsetId {
                    pos: s.pos,
                    offset: s.offset,
                };
                for w in &s.words {
                    let bucket = self.lemma_index.entry(w.to_lowercase()).or_default();
                    if !bucket.contains(&id) {
                        bucket.push(id);
                    }
                }
                for a in &s.sumo {
                    let bucket = self.sumo_index.entry(a.term.clone()).or_default();
                    if !bucket.contains(&id) {
                        bucket.push(id);
                    }
                }
                self.synsets.insert(id, s);
            }
        }
    }
}

// -- Helper functions ------------------------------------------------------------

/// Parse one WordNet `data.pos` record annotated with SUMO anchors:
///
/// ```text
/// offset lex_filenum ss_type w_cnt word lex_id [word lex_id ...] ... | gloss &%Term<kind>
/// ```
///
/// `w_cnt` is a **two-digit hexadecimal** count.  Pointer/frame fields
/// between the word list and the `|` are skipped; the gloss runs from `|` to
/// the first `&%` anchor.  Header/comment lines (anything not starting with
/// a digit) and records that fail to parse return `None`.
///
/// `file_pos` disambiguates adjective satellites: `s` records fold into the
/// file's own part of speech via [`Pos::from_ss_type`], but a corrupt
/// `ss_type` never silently reassigns a record to another file's POS.
fn parse_data_line(line: &str, file_pos: Option<Pos>) -> Option<Synset> {
    if !line.as_bytes().first()?.is_ascii_digit() {
        return None;
    }
    let (header, tail) = line.split_once(" | ")?;
    let mut toks = header.split_whitespace();

    let offset: u32 = toks.next()?.parse().ok()?;
    let _lex_filenum = toks.next()?;
    let pos = Pos::from_ss_type(toks.next()?.chars().next()?)?;
    if file_pos.is_some_and(|fp| pos != fp) {
        return None;
    }
    let w_cnt = usize::from_str_radix(toks.next()?, 16).ok()?;
    if w_cnt == 0 {
        return None;
    }

    let mut words = Vec::with_capacity(w_cnt);
    for _ in 0..w_cnt {
        let word = toks.next()?;
        let _lex_id = toks.next()?;
        words.push(strip_adj_marker(word).to_string());
    }

    let (gloss, sumo) = match tail.find("&%") {
        Some(i) => (tail[..i].trim(), parse_anchors(&tail[i..])),
        None => (tail.trim(), Vec::new()),
    };

    Some(Synset {
        pos,
        offset,
        words,
        gloss: gloss.to_string(),
        sumo,
    })
}

/// Parse the trailing `&%Term<kind>` anchor run (usually one, occasionally
/// several).  An anchor with an unrecognized suffix is dropped rather than
/// mis-kinded.
fn parse_anchors(s: &str) -> Vec<SumoAnchor> {
    let mut out = Vec::new();
    for chunk in s.split("&%").skip(1) {
        let end = chunk
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(chunk.len());
        if end == 0 {
            continue;
        }
        let Some(kind) = chunk[end..]
            .chars()
            .next()
            .and_then(MappingKind::from_suffix)
        else {
            continue;
        };
        out.push(SumoAnchor {
            term: chunk[..end].to_string(),
            kind,
        });
    }
    out
}

/// Strip WordNet adjective syntax markers: `abject(p)` -> `abject`.
fn strip_adj_marker(word: &str) -> &str {
    match word.find('(') {
        Some(i) if word.ends_with(')') => &word[..i],
        _ => word,
    }
}

/// Parse `index.sense` (`sense_key offset sense_number tag_cnt`) into
/// `(lemma, pos, offset) -> sense_number`.  The sense key's synset type digit
/// (`lemma%D:...`) gives the POS: 1=n 2=v 3=a 4=r 5=satellite->a.
fn parse_index_sense(text: &str) -> HashMap<(String, Pos, u32), u32> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let mut toks = line.split_whitespace();
        let (Some(key), Some(offset), Some(sense_no)) = (toks.next(), toks.next(), toks.next())
        else {
            continue;
        };
        let Some((lemma, rest)) = key.split_once('%') else {
            continue;
        };
        let pos = match rest.as_bytes().first() {
            Some(b'1') => Pos::Noun,
            Some(b'2') => Pos::Verb,
            Some(b'3') | Some(b'5') => Pos::Adj,
            Some(b'4') => Pos::Adv,
            _ => continue,
        };
        let (Ok(offset), Ok(sense_no)) = (offset.parse::<u32>(), sense_no.parse::<u32>()) else {
            continue;
        };
        out.insert((lemma.to_lowercase(), pos, offset), sense_no);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOUN: &str = "\
;; annotated data.noun header comment
02084071 05 n 03 dog 0 domestic_dog 0 Canis_familiaris 0 013 @ 02083346 n 0000 | a member of the genus Canis (probably descended from the common wolf) that has been domesticated by man &%Canine+
02121620 05 n 01 cat 0 007 @ 02120997 n 0000 | feline mammal usually having thick soft fur &%Feline+
03791235 06 n 02 motor_vehicle 0 automotive_vehicle 0 008 @ 03100490 n 0000 | a self-propelled wheeled vehicle &%PoweredVehicle=
";

    const VERB: &str = "\
00005526 29 v 04 pant 0 puff 0 gasp 0 heave 1 007 @ 00007012 v 0000 01 + 02 00 | breathe noisily, as when one is exhausted &%Breathing+
";

    const ADJ: &str = "\
00001740 00 a 01 able 0 005 = 05200169 n 0000 | (usually followed by `to') having the necessary means &%capability=
00003356 00 s 02 abject 0 low(p) 0 002 & 00003131 a 0000 | of the most contemptible kind &%SubjectiveAssessmentAttribute+
";

    const ADV: &str = "\
00001740 02 r 01 a_cappella 0 000 | without musical accompaniment &%Singing+
";

    const INDEX_SENSE: &str = "\
cat%1:05:00:: 02121620 1 18
dog%1:05:00:: 02084071 1 42
";

    const EXC: &str = "\
children child
oxen ox
";

    fn wn() -> WordNet {
        WordNet::from_texts(
            [
                (NOUN, Pos::Noun),
                (VERB, Pos::Verb),
                (ADJ, Pos::Adj),
                (ADV, Pos::Adv),
            ],
            Some(INDEX_SENSE),
            Some(EXC),
        )
    }

    #[test]
    fn parses_all_fixture_synsets() {
        let wn = wn();
        assert_eq!(wn.len(), 7);
    }

    #[test]
    fn noun_record_fields() {
        let wn = wn();
        let senses = wn.senses("dog");
        assert_eq!(senses.len(), 1);
        let s = &senses[0];
        assert_eq!(s.synset.pos, Pos::Noun);
        assert_eq!(s.synset.offset, 2084071);
        assert_eq!(
            s.synset.words,
            vec!["dog", "domestic_dog", "Canis_familiaris"]
        );
        assert!(s.synset.gloss.starts_with("a member of the genus Canis"));
        assert_eq!(
            s.synset.sumo,
            vec![SumoAnchor {
                term: "Canine".to_string(),
                kind: MappingKind::Subsuming,
            }]
        );
        assert_eq!(s.label(), "dog#n#1");
    }

    #[test]
    fn multiword_lemma_lookup_normalizes_spaces() {
        let wn = wn();
        let senses = wn.senses("motor vehicle");
        assert_eq!(senses.len(), 1);
        assert_eq!(senses[0].synset.sumo[0].term, "PoweredVehicle");
        assert_eq!(senses[0].synset.sumo[0].kind, MappingKind::Equivalent);
    }

    #[test]
    fn adjective_marker_stripped_and_satellite_folds_into_adj() {
        let wn = wn();
        let senses = wn.senses("low");
        assert_eq!(senses.len(), 1);
        assert_eq!(senses[0].synset.pos, Pos::Adj);
        assert!(senses[0].synset.words.contains(&"low".to_string()));
    }

    #[test]
    fn synonym_reaches_same_synset() {
        let wn = wn();
        let via_syn = wn.senses("domestic dog");
        assert_eq!(via_syn.len(), 1);
        assert_eq!(via_syn[0].synset.offset, 2084071);
    }

    #[test]
    fn reverse_index_by_sumo_term() {
        let wn = wn();
        let hits = wn.synsets_of_term("Canine");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].words.contains(&"dog".to_string()));
        assert!(wn.synsets_of_term("NoSuchTerm").is_empty());
    }

    #[test]
    fn exception_table_fallback() {
        let wn = WordNet::from_texts(
            [(
                "02084071 05 n 01 child 0 000 | a young person &%HumanChild+\n",
                Pos::Noun,
            )],
            None,
            Some(EXC),
        );
        let senses = wn.senses("children");
        assert_eq!(senses.len(), 1);
        assert_eq!(senses[0].synset.sumo[0].term, "HumanChild");
    }

    #[test]
    fn plural_strip_fallback() {
        let wn = wn();
        let senses = wn.senses("dogs");
        assert_eq!(senses.len(), 1);
        assert_eq!(senses[0].lemma, "dog");
    }

    #[test]
    fn verb_frames_do_not_confuse_word_list() {
        let wn = wn();
        for w in ["pant", "puff", "gasp", "heave"] {
            let senses = wn.senses(w);
            assert_eq!(senses.len(), 1, "missing verb lemma {w}");
            assert_eq!(senses[0].synset.sumo[0].term, "Breathing");
        }
    }

    #[test]
    fn comment_lines_and_garbage_are_skipped() {
        let wn = WordNet::from_texts(
            [(";; header\nnot a record\n00000001 05 n 01\n", Pos::Noun)],
            None,
            None,
        );
        assert!(wn.is_empty());
    }

    #[test]
    fn extend_mixed_adds_local_vocabulary() {
        let mut wn = wn();
        assert!(wn.senses("stop sign").is_empty());
        wn.extend_mixed(
            "90000001 06 n 01 stop_sign 0 000 | a red traffic sign &%StopSign=\n\
             90000002 00 a 01 octagonal 0 000 | eight-sided &%Octagonal=\n",
        );
        let s = wn.senses("stop sign");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].synset.sumo[0].term, "StopSign");
        assert_eq!(
            wn.senses("octagonal")[0].synset.pos,
            Pos::Adj,
            "mixed file: each record's own ss_type decides POS"
        );
    }

    #[test]
    fn wrong_pos_record_rejected() {
        // A verb record inside the noun file must not be indexed as a noun.
        let wn = WordNet::from_texts([(VERB, Pos::Noun)], None, None);
        assert!(wn.is_empty());
    }
}
