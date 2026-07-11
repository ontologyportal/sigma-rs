//! Structural matching of patterns against stored sentences.

use std::collections::HashSet;

use crate::SymbolId;
use crate::types::{Element, SentenceId};

use super::super::SyntacticLayer;
use super::super::sentence::Sentence;
use super::types::{
    Bindings, PatternElement, SentencePattern,
    elements_eq_any_span_free, elements_eq_span_free,
};

// -- PatternMatcher -----------------------------------------------------------

/// Structural pattern matching over a [`SyntacticLayer`].
///
/// Construct via [`SyntacticLayer::patterns`], then call
/// `find_by_pattern` / `match_pattern`.
pub(crate) struct PatternMatcher<'a> {
    pub(super) store: &'a SyntacticLayer,
}

impl SyntacticLayer {
    /// A [`PatternMatcher`] borrowing this layer.
    pub(crate) fn patterns(&self) -> PatternMatcher<'_> {
        PatternMatcher { store: self }
    }
}

impl<'a> PatternMatcher<'a> {
    /// Return every root sentence whose elements match `pattern`, together
    /// with the resulting [`Bindings`].
    ///
    /// When `head` is `Some(name)`, only sentences whose first element is the
    /// named symbol are tested (uses [`by_head`](SyntacticLayer::by_head) for
    /// O(1) pre-filtering).  Pass `None` to scan all roots.
    /// 
    /// When `contains` is `Some(HashSet<SymbolId>)`, only roots containing ANY of
    /// the symbols are tests (uses [`axiom_index`](SyntacticLayer::axiom_index))
    /// for O(1) pre-filtering). Intersection with `head` if specified. Pass `None` 
    /// to scan all roots.
    ///
    /// `self` is passed through to [`match_pattern`] so that
    /// [`PatternElement::SubPattern`] positions can look up sub-sentences.
    pub(crate) fn find_by_pattern(
        &self,
        pattern:  &SentencePattern,
        head:     Option<&str>,
        contains: Option<HashSet<SymbolId>>
    ) -> Vec<(SentenceId, Bindings)> {
        let head_candidates: HashSet<SentenceId> = match head {
            Some(h) => self.store.by_head(h).iter().copied().collect(),
            None    => self.store.root_sids().into_iter().collect(),
        };
        let candidates: HashSet<SentenceId> = match contains {
            Some(c) => c.iter().flat_map(|s| self.store.axiom_sentences_of(*s).iter().copied().collect::<Vec<_>>()).collect(),
            None    => self.store.root_sids().into_iter().collect(),
        };

        let candidates: HashSet<SymbolId> = candidates.intersection(&head_candidates).map(|h| *h).collect();

        candidates
            .into_iter()
            .filter_map(|sid| {
                let sentence = self.store.sentence(sid)?;
                let bindings = self.match_pattern(pattern, &sentence)?;
                Some((sid, bindings))
            })
            .collect()
    }

    /// Return every root sentence whose elements match `pattern`, together
    /// with the resulting [`Bindings`]. Unlike [`find_by_pattern`](Self::find_by_pattern)
    /// this will recurse into subsentences to find a match
    ///
    /// When `contains` is `Some(HashSet<SymbolId>)`, only roots containing ANY of
    /// the symbols are tests (uses [`axiom_index`](SyntacticLayer::axiom_index))
    /// for O(1) pre-filtering). Pass `None` to scan all roots.
    ///
    /// `self` is passed through to [`match_pattern`] so that
    /// [`PatternElement::SubPattern`] positions can look up sub-sentences.
    pub(crate) fn find_by_pattern_sub(
        &self,
        pattern:  &SentencePattern,
        contains: Option<HashSet<SymbolId>>
    ) -> Vec<(SentenceId, Bindings)> {
        let roots: HashSet<SentenceId> = match contains {
            Some(c) => c.iter().flat_map(|s| self.store.axiom_sentences_of(*s).iter().copied().collect::<Vec<_>>()).collect(),
            None    => self.store.root_sids().into_iter().collect(),
        };
        self.find_by_pattern_sub_in_roots(pattern, roots)
    }

    /// Like [`find_by_pattern_sub`](Self::find_by_pattern_sub) but over an
    /// explicit, already-resolved set of *root* sentence ids rather than a
    /// `contains` symbol set.  Each root is expanded to its descendents and
    /// matched against `pattern`.
    pub(crate) fn find_by_pattern_sub_in_roots(
        &self,
        pattern: &SentencePattern,
        roots:   HashSet<SentenceId>,
    ) -> Vec<(SentenceId, Bindings)> {
        let candidates: HashSet<SentenceId> = roots.into_iter().flat_map(|c| {
            let Some(subs) = self.store.subs_of(c) else {
                return vec![c]
            };
            let mut out = vec![c];
            out.extend(subs);
            out
        }).collect();

        candidates
            .into_iter()
            .filter_map(|sid| {
                let sentence = self.store.sentence(sid)?;
                let bindings = self.match_pattern(pattern, &sentence)?;
                Some((sid, bindings))
            })
            .collect()
    }

    /// Try to match `pattern` against the elements of `sentence`.
    ///
    /// `syntactic` is required for [`PatternElement::SubPattern`] arms, which look
    /// up the referenced sub-sentence by `SentenceId`.  For patterns that contain
    /// no `SubPattern` positions a `SyntacticLayer::default()` is sufficient.
    ///
    /// Returns `Some(Bindings)` on success, `None` on any mismatch.
    ///
    /// Two engines: a [`PatternElement::Glob`]-free pattern matches its elements
    /// 1:1 against the sentence (strict zip — the common, allocation-light path);
    /// a glob-bearing pattern falls through to the variable-length
    /// [`match_seq`](Self::match_seq).
    pub(crate) fn match_pattern(
        &self,
        pattern:   &SentencePattern,
        sentence:  &Sentence
    ) -> Option<Bindings> {
        if pattern.0.iter().any(|p| matches!(p, PatternElement::Glob | PatternElement::GlobCapture(_))) {
            return self.match_seq(&pattern.0, &sentence.elements, Bindings::default());
        }

        if pattern.0.len() != sentence.elements.len() { return None; }
        let mut b = Bindings::default();
        for (pat, elem) in pattern.0.iter().zip(sentence.elements.iter()) {
            if !self.match_one(pat, elem, &mut b) { return None; }
        }
        Some(b)
    }

    /// Match a single (non-`Glob`) pattern element against one sentence element,
    /// committing any captures into `b`.  Returns `false` on mismatch.  `Glob`
    /// is handled by [`match_seq`](Self::match_seq) and returns `false` here.
    fn match_one(&self, pat: &PatternElement, elem: &Element, b: &mut Bindings) -> bool {
        match pat {
            PatternElement::Glob | PatternElement::GlobCapture(_) => false,
            PatternElement::Exact(key) => key.matches(elem),
            PatternElement::SubPattern(inner_pat) => {
                let Element::Sub(sid) = elem else { return false };
                let Some(sub_s) = self.store.sentence(*sid) else { return false };
                let Some(inner_b) = self.match_pattern(inner_pat, &sub_s) else { return false };
                // Callers must assign globally non-overlapping slot numbers.
                b.elements.extend(inner_b.elements);
                b.sub_sids.extend(inner_b.sub_sids);
                true
            }
            PatternElement::AnyCapture(idx) => {
                if let Some(prev) = b.elements.get(idx) {
                    elements_eq_span_free(prev, elem)
                } else {
                    b.elements.insert(*idx, elem.clone());
                    true
                }
            }
            PatternElement::AnySubSentence(idx) => {
                if let Element::Sub(sid) = elem {
                    b.sub_sids.insert(*idx, *sid);
                    true
                } else {
                    false
                }
            }
            PatternElement::AnyElement(idx) => {
                if let Some(prev) = b.elements.get(idx) {
                    elements_eq_any_span_free(prev, elem)
                } else {
                    b.elements.insert(*idx, elem.clone());
                    true
                }
            }
        }
    }

    /// Variable-length matcher: match the pattern slice `pats` against the
    /// element slice `elems`, threading `b` (cloned at each commit point so a
    /// failed branch never pollutes the accumulator).
    ///
    /// [`PatternElement::Glob`] consumes 0+ elements lazily: it advances the
    /// element cursor to the **first** position where the *remainder* of the
    /// pattern matches (a trailing glob matches the rest outright).  Every other
    /// element consumes exactly one element via [`match_one`](Self::match_one).
    fn match_seq(
        &self,
        pats:  &[PatternElement],
        elems: &[Element],
        b:     Bindings,
    ) -> Option<Bindings> {
        let Some((first, rest)) = pats.split_first() else {
            // Pattern exhausted: success iff the sentence is also exhausted.
            return elems.is_empty().then_some(b);
        };

        if let PatternElement::Glob | PatternElement::GlobCapture(_) = first {
            // A `GlobCapture(slot)` records how many elements it consumed.
            let slot = if let PatternElement::GlobCapture(s) = first { Some(*s) } else { None };
            for skip in 0..=elems.len() {
                let mut nb = b.clone();
                if let Some(s) = slot { nb.glob_lens.insert(s, skip); }
                if let Some(done) = self.match_seq(rest, &elems[skip..], nb) {
                    return Some(done);
                }
            }
            return None;
        }

        let (elem, rest_elems) = elems.split_first()?;
        let mut nb = b;
        if self.match_one(first, elem, &mut nb) {
            self.match_seq(rest, rest_elems, nb)
        } else {
            None
        }
    }
}

