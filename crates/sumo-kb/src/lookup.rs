// crates/sumo-kb/src/lookup.rs
//
// Position-based queries over the KB.  "Given a byte offset in a
// source file, which element is there?"  General-purpose: drives
// LSP hover / goto-definition / rename, but is also useful for
// CLI tools (`sumo explain --at file:L:C`), REPL inspection, and
// any offset-keyed analysis workflow.
//
// The core primitive is [`element_at_offset`] -- a linear walk
// down through root sentences and their sub-sentence chains.
// Spans marked [`crate::parse::ast::Span::synthetic`] (e.g. from
// CNF / rehydrated-from-LMDB elements) are invisible to the walk
// so position queries never surface spans with no real source.

use crate::error::Span;
use crate::kif_store::KifStore;
use crate::types::{Element, Sentence, SentenceId};

/// One element hit from a position query.
///
/// `sid` is the deepest sentence whose source range contains the
/// offset; `idx` is the index of the element within that sentence's
/// elements vector; `span` is the element's own source range.
#[derive(Debug, Clone)]
pub struct ElementHit {
    pub sid:  SentenceId,
    pub idx:  usize,
    pub span: Span,
}

/// Find the element at byte `offset` in `file`.
///
/// Walks `store.file_roots[file]`, selects the root whose span
/// contains the offset, and descends into sub-sentences.  Returns
/// the innermost non-synthetic element whose span contains the
/// offset.  `None` when no root covers the offset or when the
/// offset falls on whitespace between elements.
pub(crate) fn element_at_offset(store: &KifStore, file: &str, offset: usize) -> Option<ElementHit> {
    let root_sids = store.file_roots.get(file)?;
    for &root_sid in root_sids {
        let sentence = &store.sentences[store.sent_idx(root_sid)];
        if !span_contains(&sentence.span, offset) { continue; }
        if let Some(hit) = element_at_in_sentence(store, root_sid, sentence, offset) {
            return Some(hit);
        }
        // The offset hit the root's parens but not any element --
        // report the head (or first element) as a best-effort fallback
        // so callers can still resolve the sentence.
        if !sentence.elements.is_empty() {
            return Some(ElementHit {
                sid:  root_sid,
                idx:  0,
                span: sentence.elements[0].span().clone(),
            });
        }
    }
    None
}

/// True if `span` covers `offset` and is not a synthetic sentinel.
fn span_contains(span: &Span, offset: usize) -> bool {
    if span.is_synthetic() { return false; }
    offset >= span.offset && offset < span.end_offset
}

/// Descend into `sentence` looking for the innermost element
/// whose span contains `offset`.
fn element_at_in_sentence(
    store:    &KifStore,
    sid:      SentenceId,
    sentence: &Sentence,
    offset:   usize,
) -> Option<ElementHit> {
    for (i, el) in sentence.elements.iter().enumerate() {
        let span = el.span();
        if !span_contains(span, offset) { continue; }
        if let Element::Sub { sid: sub_sid, .. } = el {
            let sub = &store.sentences[store.sent_idx(*sub_sid)];
            if let Some(inner) = element_at_in_sentence(store, *sub_sid, sub, offset) {
                return Some(inner);
            }
            // Fell on the sub's opening paren but no deeper element
            // matched -- return the Sub itself.
            return Some(ElementHit { sid, idx: i, span: span.clone() });
        }
        return Some(ElementHit { sid, idx: i, span: span.clone() });
    }
    None
}

// -- Symbol resolution --------------------------------------------------------

/// If the element at `offset` is a [`Element::Symbol`], return the
/// symbol's name.  Convenience wrapper for hover / goto-definition
/// handlers that only care about symbol positions.
pub(crate) fn symbol_at_offset(store: &KifStore, file: &str, offset: usize) -> Option<String> {
    let hit = element_at_offset(store, file, offset)?;
    let sentence = &store.sentences[store.sent_idx(hit.sid)];
    match sentence.elements.get(hit.idx)? {
        Element::Symbol { id, .. } => Some(store.sym_name(*id).to_owned()),
        _ => None,
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kif_store::load_kif;

    fn store_with(text: &str, file: &str) -> KifStore {
        let mut store = KifStore::default();
        let errs = load_kif(&mut store, text, file);
        assert!(errs.is_empty(), "load errors: {:?}", errs);
        store
    }

    #[test]
    fn offset_on_head_symbol_returns_symbol() {
        //                               111111111122222
        //                     0123456789012345678901234
        let src  = "(subclass Human Animal)";
        let store = store_with(src, "t.kif");
        // Offset 1-8 covers `subclass`.
        let hit = element_at_offset(&store, "t.kif", 3).expect("hit");
        assert_eq!(hit.idx, 0);
        let name = symbol_at_offset(&store, "t.kif", 3).expect("name");
        assert_eq!(name, "subclass");
    }

    #[test]
    fn offset_on_second_symbol_returns_that_symbol() {
        //                     0123456789012345678901234
        let src  = "(subclass Human Animal)";
        let store = store_with(src, "t.kif");
        // 'Human' starts at 10.
        let name = symbol_at_offset(&store, "t.kif", 12).expect("name");
        assert_eq!(name, "Human");
        let name = symbol_at_offset(&store, "t.kif", 17).expect("name");
        assert_eq!(name, "Animal");
    }

    #[test]
    fn offset_in_nested_sub_sentence_finds_inner_symbol() {
        //             0         1         2
        //             0123456789012345678901234567890
        let src = "(=> (P ?X) (Q ?X))";
        let store = store_with(src, "t.kif");
        // 'Q' starts at 12.
        let name = symbol_at_offset(&store, "t.kif", 12).expect("hit Q");
        assert_eq!(name, "Q");
        // 'P' at 5.
        let name = symbol_at_offset(&store, "t.kif", 5).expect("hit P");
        assert_eq!(name, "P");
    }

    #[test]
    fn offset_outside_any_sentence_returns_none() {
        let src = "(subclass Human Animal)";
        let store = store_with(src, "t.kif");
        // Past the closing paren.
        assert!(element_at_offset(&store, "t.kif", 50).is_none());
    }

    #[test]
    fn offset_on_whitespace_between_elements_returns_root_fallback() {
        //                   "(subclass Human Animal)"
        //                     0123456789012345
        // Offset 9 is the space between `subclass` and `Human`.  The
        // walk descends into the root's paren range, finds no
        // matching element, falls back to index 0 (head).
        let src  = "(subclass Human Animal)";
        let store = store_with(src, "t.kif");
        let hit = element_at_offset(&store, "t.kif", 9).expect("hit");
        assert_eq!(hit.idx, 0);
    }

    #[test]
    fn unknown_file_returns_none() {
        let src = "(subclass Human Animal)";
        let store = store_with(src, "t.kif");
        assert!(element_at_offset(&store, "nope.kif", 3).is_none());
    }
}
