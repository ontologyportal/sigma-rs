//! WordNet-to-KB mapping diagnostics -- a port of Java Sigma's
//! `WNdiagnostics`, which surfaces mismatches between a loaded WordNet
//! lexicon and a loaded KB rather than fixing them (see
//! `ontologyportal/sigma-rs#64`).
//!
//! Five reports, computed in one pass over the lexicon's synsets plus one
//! over the KB's symbols:
//!   1. [`WordNetDiagnostics::unmapped_synsets`] -- synsets with no `&%`
//!      SUMO anchor at all.
//!   2. [`WordNetDiagnostics::terms_without_synsets`] -- loaded KB terms
//!      (excluding functions, skolems, scope-qualified variables) with no
//!      synset mapped to them in either direction.
//!   3. [`WordNetDiagnostics::missing_terms`] -- synsets whose SUMO anchor
//!      names a term the loaded KB does not define (the flip side of
//!      `apply_wordnet`'s graceful degradation in `kb::search`: this report
//!      is where that gap becomes visible and actionable).
//!   4. [`WordNetDiagnostics::taxonomy_mismatches`] -- a noun synset's
//!      hypernym SUMO anchor is not an ancestor, in the KB's own subclass
//!      taxonomy, of the synset's own SUMO anchor.
//!   5. [`WordNetDiagnostics::counts`] -- mapping-kind x part-of-speech
//!      tallies across the whole lexicon.
//!
//! No caching: this is an on-demand report over ~117k synsets and the KB's
//! symbol table, not a value read on every KB mutation (see the cache-shape
//! table in `AGENTS.md` -- a one-shot linear scan doesn't fit any of the
//! four cache shapes and isn't reused often enough to be worth memoizing).

use crate::kb::man::ManKind;
use crate::kb::KnowledgeBase;
use crate::layer::{Layer, TopLayer};
use crate::parse::Span;

use super::{MappingKind, Pos, Synset, WordNet};

/// A capped list: `items` holds up to `limit` rows, `total` is the true
/// count -- so a caller can render "50 of 3,412" instead of silently
/// truncating (Java's equivalent instead appends a literal `"limited to 50
/// results."` string to the list).
#[derive(Debug, Clone)]
pub struct Capped<T> {
    pub items: Vec<T>,
    pub total: usize,
}

impl<T> Default for Capped<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            total: 0,
        }
    }
}

impl<T> Capped<T> {
    fn push(&mut self, item: T, limit: usize) {
        self.total += 1;
        if self.items.len() < limit {
            self.items.push(item);
        }
    }
}

/// A synset with no `&%SumoTerm` anchor at all.
#[derive(Debug, Clone)]
pub struct UnmappedSynset {
    pub pos: Pos,
    pub offset: u32,
    pub words: Vec<String>,
    /// Where this record lives in its source mapping file (see
    /// [`Synset::span`]).
    pub span: Span,
}

/// A synset whose SUMO anchor names a term the loaded KB does not define.
#[derive(Debug, Clone)]
pub struct MissingTerm {
    pub pos: Pos,
    pub offset: u32,
    pub words: Vec<String>,
    pub term: String,
    pub kind: MappingKind,
    /// Where this record lives in its source mapping file (see
    /// [`Synset::span`]).
    pub span: Span,
}

/// A loaded KB term with no WordNet synset mapped to it.
#[derive(Debug, Clone)]
pub struct UnsynsetTerm {
    pub symbol: String,
    pub kinds: Vec<ManKind>,
}

/// A noun synset's hypernym edge whose SUMO anchors disagree with the KB's
/// own subclass taxonomy.
#[derive(Debug, Clone)]
pub struct TaxonomyMismatch {
    pub word: String,
    pub term: String,
    pub hypernym_word: String,
    pub hypernym_term: String,
    /// Where `word`'s synset (the offending record) lives in its source
    /// mapping file (see [`Synset::span`]) -- not the hypernym's.
    pub span: Span,
}

/// Per-mapping-kind and per-part-of-speech counts across the whole lexicon.
#[derive(Debug, Clone, Default)]
pub struct MappingCounts {
    pub equivalent: usize,
    pub subsuming: usize,
    pub instance: usize,
    pub anti_subsuming: usize,
    pub anti_instance: usize,
    pub anti_equivalent: usize,
    pub nouns: usize,
    pub verbs: usize,
    pub adjectives: usize,
    pub adverbs: usize,
}

/// The full WordNet<->KB diagnostic report (see the module docs for what
/// each field covers).
#[derive(Debug, Clone, Default)]
pub struct WordNetDiagnostics {
    pub counts: MappingCounts,
    pub unmapped_synsets: Capped<UnmappedSynset>,
    pub missing_terms: Capped<MissingTerm>,
    pub terms_without_synsets: Capped<UnsynsetTerm>,
    pub taxonomy_mismatches: Capped<TaxonomyMismatch>,
}

/// Diagnose `wn` against `kb`: every report below is capped at `limit` rows
/// (with [`Capped::total`] carrying the true count).
pub fn diagnose<L: TopLayer + Layer>(
    wn: &WordNet,
    kb: &KnowledgeBase<L>,
    limit: usize,
) -> WordNetDiagnostics {
    let mut out = WordNetDiagnostics::default();

    for synset in wn.synsets.values() {
        count_mapping(&mut out.counts, synset);

        if synset.sumo.is_empty() {
            out.unmapped_synsets.push(
                UnmappedSynset {
                    pos: synset.pos,
                    offset: synset.offset,
                    words: synset.words.clone(),
                    span: synset.span.clone(),
                },
                limit,
            );
        }

        for anchor in &synset.sumo {
            if kb.symbol_id(&anchor.term).is_none() {
                out.missing_terms.push(
                    MissingTerm {
                        pos: synset.pos,
                        offset: synset.offset,
                        words: synset.words.clone(),
                        term: anchor.term.clone(),
                        kind: anchor.kind,
                        span: synset.span.clone(),
                    },
                    limit,
                );
            }
        }

        if synset.pos == Pos::Noun {
            check_taxonomy(wn, kb, synset, &mut out.taxonomy_mismatches, limit);
        }
    }

    for (sym_id, name) in kb.iter_symbols() {
        if !name.starts_with(|c: char| c.is_ascii_uppercase())
            || kb.symbol_is_variable(&name)
            || kb.symbol_is_skolem(&name)
            || kb.is_function(sym_id)
        {
            continue;
        }
        if wn.synsets_of_term(&name).is_empty() {
            let kinds = kb.kinds_of(sym_id);
            out.terms_without_synsets.push(
                UnsynsetTerm {
                    symbol: name,
                    kinds,
                },
                limit,
            );
        }
    }

    out
}

/// Tally one synset into the running mapping-kind/POS counts. Mirrors
/// Java's `countMappings`, which counts one bucket per synset *with a
/// mapping* (its `nounSUMOHash`/etc. hold exactly one mapping string per
/// synset key) -- so only the first anchor's kind is tallied when a synset
/// carries several.
fn count_mapping(counts: &mut MappingCounts, synset: &Synset) {
    match synset.pos {
        Pos::Noun => counts.nouns += 1,
        Pos::Verb => counts.verbs += 1,
        Pos::Adj => counts.adjectives += 1,
        Pos::Adv => counts.adverbs += 1,
    }
    if let Some(anchor) = synset.sumo.first() {
        match anchor.kind {
            MappingKind::Equivalent => counts.equivalent += 1,
            MappingKind::Subsuming => counts.subsuming += 1,
            MappingKind::Instance => counts.instance += 1,
            MappingKind::Other('[') => counts.anti_subsuming += 1,
            MappingKind::Other(']') => counts.anti_instance += 1,
            MappingKind::Other(':') => counts.anti_equivalent += 1,
            MappingKind::Other(_) => {}
        }
    }
}

/// Report a hypernym edge whose SUMO anchors disagree with the KB's own
/// subclass taxonomy: `synset`'s SUMO term should have each hypernym's SUMO
/// term as an ancestor (a WordNet hypernym is the broader concept, so the
/// more specific SUMO term should subclass it). Silently skipped, not
/// reported, when either side's SUMO term isn't in the loaded KB -- that
/// gap is [`WordNetDiagnostics::missing_terms`]'s report to make, not this
/// one's.
fn check_taxonomy<L: TopLayer + Layer>(
    wn: &WordNet,
    kb: &KnowledgeBase<L>,
    synset: &Synset,
    out: &mut Capped<TaxonomyMismatch>,
    limit: usize,
) {
    let Some(anchor) = synset.sumo.first() else {
        return;
    };
    let Some(sym_id) = kb.symbol_id(&anchor.term) else {
        return;
    };
    for target_id in &synset.hypernyms {
        let Some(target) = wn.synsets.get(target_id) else {
            continue;
        };
        let Some(target_anchor) = target.sumo.first() else {
            continue;
        };
        if target_anchor.term == anchor.term || kb.symbol_id(&target_anchor.term).is_none() {
            continue;
        }
        if !kb.has_ancestor(sym_id, &target_anchor.term) {
            out.push(
                TaxonomyMismatch {
                    word: synset.words.first().cloned().unwrap_or_default(),
                    term: anchor.term.clone(),
                    hypernym_word: target.words.first().cloned().unwrap_or_default(),
                    hypernym_term: target_anchor.term.clone(),
                    span: synset.span.clone(),
                },
                limit,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kb::KnowledgeBase;
    use crate::TranslationLayer;

    fn kb_from(kif: &str) -> KnowledgeBase<TranslationLayer> {
        let mut kb = KnowledgeBase::new();
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("test.kif"), "test.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        let r = kb.make_session_axiomatic("test.kif");
        assert!(r.is_ok(), "promotion failed: {:?}", r.err());
        kb
    }

    fn wn(text: &str) -> WordNet {
        WordNet::from_texts(
            [(text, Pos::Noun, "WordNetMappings30-noun.txt")],
            None,
            None,
        )
    }

    #[test]
    fn unmapped_synset_is_reported() {
        let text = "02121620 05 n 01 cat 0 001 @ 02120997 n 0000 | feline mammal\n";
        let wn = wn(text);
        let kb = kb_from("(instance Foo Object)");
        let diag = diagnose(&wn, &kb, 10);
        assert_eq!(diag.unmapped_synsets.total, 1);
        assert_eq!(diag.unmapped_synsets.items[0].words, vec!["cat"]);
        assert_eq!(
            diag.unmapped_synsets.items[0].span.file,
            "WordNetMappings30-noun.txt"
        );
        assert_eq!(diag.unmapped_synsets.items[0].span.line, 1);
    }

    #[test]
    fn missing_term_is_reported_for_unloaded_anchor() {
        let text = "04257790 06 n 01 solar_panel 0 000 | electrical device &%SolarPanel=\n";
        let wn = wn(text);
        let kb = kb_from("(instance Foo Object)");
        let diag = diagnose(&wn, &kb, 10);
        assert_eq!(diag.missing_terms.total, 1);
        let hit = &diag.missing_terms.items[0];
        assert_eq!(hit.term, "SolarPanel");
        assert_eq!(hit.kind, MappingKind::Equivalent);

        // Loading SolarPanel makes it fall out of the report.
        let kb = kb_from("(subclass SolarPanel Device)");
        let diag = diagnose(&wn, &kb, 10);
        assert_eq!(diag.missing_terms.total, 0);
    }

    #[test]
    fn term_without_synset_is_reported_and_excludes_functions_and_skolems() {
        let text = "02121620 05 n 01 cat 0 001 @ 02120997 n 0000 | feline &%Feline+\n";
        let wn = wn(text);
        let kb = kb_from(
            r#"
            (subclass Feline Mammal)
            (subclass Canine Mammal)
            (instance AdditionFn Function)
            "#,
        );
        let diag = diagnose(&wn, &kb, 10);
        let names: Vec<&str> = diag
            .terms_without_synsets
            .items
            .iter()
            .map(|t| t.symbol.as_str())
            .collect();
        assert!(names.contains(&"Canine"), "{names:?}");
        assert!(!names.contains(&"Feline"), "Feline has a synset: {names:?}");
        assert!(
            !names.contains(&"AdditionFn"),
            "functions are excluded: {names:?}"
        );
    }

    #[test]
    fn taxonomy_mismatch_flags_disagreeing_hypernym() {
        // `dog` (hypernym `carnivore`) maps to Canine; `carnivore` maps to
        // Plant -- Canine is not a subclass of Plant, so this is a mismatch.
        let text = "\
02084071 05 n 01 dog 0 001 @ 02083346 n 0000 | a dog &%Canine+\n\
02083346 05 n 01 carnivore 0 000 | flesh-eating mammal &%Plant+\n";
        let wn = wn(text);
        let kb = kb_from(
            r#"
            (subclass Canine Mammal)
            (subclass Plant Organism)
            "#,
        );
        let diag = diagnose(&wn, &kb, 10);
        assert_eq!(diag.taxonomy_mismatches.total, 1);
        let m = &diag.taxonomy_mismatches.items[0];
        assert_eq!(m.term, "Canine");
        assert_eq!(m.hypernym_term, "Plant");
    }

    #[test]
    fn taxonomy_agreement_is_not_reported() {
        let text = "\
02084071 05 n 01 dog 0 001 @ 02083346 n 0000 | a dog &%Canine+\n\
02083346 05 n 01 carnivore 0 000 | flesh-eating mammal &%Carnivore+\n";
        let wn = wn(text);
        let kb = kb_from(
            r#"
            (subclass Canine Carnivore)
            (subclass Carnivore Mammal)
            "#,
        );
        let diag = diagnose(&wn, &kb, 10);
        assert_eq!(
            diag.taxonomy_mismatches.total, 0,
            "{:?}",
            diag.taxonomy_mismatches.items
        );
    }

    #[test]
    fn counts_tally_by_kind_and_pos() {
        let text = "\
02084071 05 n 01 dog 0 001 @ 02083346 n 0000 | a dog &%Canine+\n\
02121620 05 n 01 cat 0 000 | feline &%Feline=\n\
02083346 05 n 01 carnivore 0 000 | flesh-eating mammal\n";
        let wn = wn(text);
        let kb = kb_from("(instance Foo Object)");
        let diag = diagnose(&wn, &kb, 10);
        assert_eq!(diag.counts.nouns, 3);
        assert_eq!(diag.counts.subsuming, 1);
        assert_eq!(diag.counts.equivalent, 1);
        assert_eq!(diag.counts.instance, 0);
    }

    #[test]
    fn limit_caps_items_but_total_counts_everything() {
        let text = "\
02084071 05 n 01 dog 0 000 | a dog\n\
02121620 05 n 01 cat 0 000 | a cat\n\
02083346 05 n 01 carnivore 0 000 | a carnivore\n";
        let wn = wn(text);
        let kb = kb_from("(instance Foo Object)");
        let diag = diagnose(&wn, &kb, 2);
        assert_eq!(diag.unmapped_synsets.total, 3);
        assert_eq!(diag.unmapped_synsets.items.len(), 2);
    }
}
