// crates/core/src/syntactic/mod.rs
//
// Bottom layer of the KB stack.  The `SyntacticLayer` provides the
// syntactical construction structure and methods for KIF-based symbol
// tables.  This is the primary persistent layer and is constructed
// following parsing of input strings.
//
// Submodules split the impl across responsibilities:
//   intern.rs   -- symbol interning + name/id lookup
//   sentence.rs -- sentence allocation, AST -> Sentence build, ScopeCtx
//   index.rs    -- occurrence + head + axiom-symbol indices
//   remove.rs   -- sentence/file removal + orphaned-symbol pruning
//   lookup.rs   -- pattern-based sentence lookup (`by_head`, `lookup`)
//   display.rs  -- ANSI / plain KIF rendering
//   load.rs     -- top-level `load_kif` driver
//   persist.rs  -- LMDB persistence helpers (cfg `persist`)

use std::collections::HashMap;

use crate::layer::{Layer, NoLayer};
use crate::semantics::SemanticLayer;
use crate::types::{Occurrence, Sentence, SentenceId, Symbol, SymbolId};

pub mod intern;
pub mod sentence;
pub mod index;
pub mod remove;
pub mod lookup;
pub mod display;
pub mod load;
#[cfg(feature = "persist")]
pub mod persist;

pub(crate) use display::{SentenceDisplay, sentence_to_plain_kif};
pub(crate) use load::load_kif;

// SyntancticLayer
/// The parsed store containing parsed sentences, symbols, and literals
///
/// Populated incrementally by [`load_kif`].  Symbol and sentence IDs are
/// stable `u64` values driven by explicit atomic-style counters that can be
/// seeded from LMDB on `open()`, ensuring no ID collision between in-memory
/// and persisted data.
/// 
/// SyntacticLayer parsed a source file using the parse submodule and 
/// stores the symbols and sentences here
#[derive(Debug, Default)]
pub(crate) struct SyntacticLayer {
    /// A vector of sentences from the source knowledge bases
    pub sentences:    Vec<Sentence>,
    /// A hash map mapping the symbol name to its ID
    pub symbols:      HashMap<String, SymbolId>,
    /// A vector containing the symbol data structures
    pub symbol_data:  Vec<Symbol>,
    /// Root (top-level) sentence ids -- in insertion order.
    pub roots:        Vec<SentenceId>,
    /// All sub-sentence ids (nested inside root sentences).
    pub sub_sentences: Vec<SentenceId>,
    /// Root sentences grouped by file tag.
    pub file_roots:   HashMap<String, Vec<SentenceId>>,
    /// Per-root-sentence fingerprints for each file, positionally
    /// aligned with `file_roots[file]`.  Populated during `load`;
    /// used by incremental-reload workflows (file watchers,
    /// LSP didChange) to compute sentence-level diffs without
    /// re-consulting the AST.  Kept in lockstep with `file_roots`
    /// by every mutation path -- `remove_sentence`, `remove_file`,
    /// and `apply_file_diff` all update both tables.
    pub file_hashes:  HashMap<String, Vec<u64>>,
    /// Reverse index: SymbolId -> every occurrence in the KB.
    /// Populated during `index_sentence_occurrences` after each
    /// new root or sub-sentence is built; drained by
    /// `remove_sentence` when a sentence is dropped.  Synthetic
    /// spans (CNF output, rehydrated-from-LMDB elements) are
    /// excluded so the index only contains real source positions.
    ///
    /// Non-LSP uses: CLI `sumo find-refs`, coverage analysis,
    /// programmatic symbol-walk tools.
    pub occurrences:  HashMap<SymbolId, Vec<Occurrence>>,
    /// Root sentences indexed by head symbol name (e.g. "instance" -> [...]).
    /// TODO replace the key with the symbol id of the predicate rather than
    /// the string
    pub head_index:   HashMap<String, Vec<SentenceId>>,

    /// SentenceId -> Vec index.  Maps stable IDs from position of the sentence
    /// in the sentences vector so that seeded counters (e.g. starting at 1000
    /// after an LMDB load) do not cause out-of-bounds accesses.
    pub(in crate::syntactic) sent_idx:         HashMap<SentenceId, usize>,
    /// SymbolId -> Vec index. Maps stable IDs from position of the symbol
    /// in the symbol vector so that seeded counters (e.g. starting at 1000
    /// after an LMDB load) do not cause out-of-bounds accesses.
    pub(in crate::syntactic) sym_idx:          HashMap<SymbolId, usize>,

    /// Explicit counter for next SymbolId -- seeded from LMDB max on open().
    pub(in crate::syntactic) next_symbol_id:   u64,
    /// Explicit counter for next SentenceId -- seeded from LMDB max on open().
    pub(in crate::syntactic) next_sentence_id: u64,
    /// Internal counter for variable scope disambiguation (not persisted).
    pub(in crate::syntactic) scope_counter:    u64,
}

impl Layer for SyntacticLayer {
    type Inner = NoLayer;
    type Outer = SemanticLayer;

    fn inner(&self) -> Option<&NoLayer> { None }
    fn outer(&self) -> Option<&SemanticLayer> { None }
}

// -- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::OccurrenceKind;
    use load::load_kif;

    fn store_from(kif: &str) -> SyntacticLayer {
        let mut store = SyntacticLayer::default();
        let errors = load_kif(&mut store, kif, "test");
        assert!(errors.is_empty(), "load errors: {:?}", errors);
        store
    }

    #[test]
    fn basic_load() {
        let store = store_from("(subclass Human Animal)");
        assert_eq!(store.roots.len(), 1);
        assert!(store.symbols.contains_key("subclass"));
        assert!(store.symbols.contains_key("Human"));
        assert!(store.symbols.contains_key("Animal"));
    }

    #[test]
    fn stable_ids_incremental() {
        let mut store = SyntacticLayer::default();
        load_kif(&mut store, "(subclass Human Animal)", "a");
        let n_sym_before  = store.next_symbol_id;
        let n_sent_before = store.next_sentence_id;
        load_kif(&mut store, "(instance Fido Dog)", "b");
        assert!(store.next_symbol_id   > n_sym_before,  "symbol counter did not advance");
        assert!(store.next_sentence_id > n_sent_before, "sentence counter did not advance");
    }

    #[test]
    #[cfg(feature = "persist")]
    fn seed_counters() {
        let mut store = SyntacticLayer::default();
        store.seed_counters(1000, 500);
        load_kif(&mut store, "(subclass Dog Animal)", "test");
        // First new symbol should get id >= 1000
        let dog_id = store.sym_id("Dog").unwrap();
        assert!(dog_id >= 1000, "symbol id {} < seeded base 1000", dog_id);
    }

    #[test]
    fn head_index() {
        let store = store_from("(subclass Human Animal)\n(subclass Dog Animal)");
        assert_eq!(store.by_head("subclass").len(), 2);
    }

    #[test]
    fn parse_error_preserves_recovered_sentences() {
        // The parser is error-recovering: a file with one bad
        // sentence should still surface the well-formed ones.
        let mut store = SyntacticLayer::default();
        let errors = load_kif(&mut store,
            "(subclass Human Animal)\n(\"bad\" head)\n(subclass Dog Animal)",
            "mixed");
        assert!(!errors.is_empty(), "expected a parse error");
        assert_eq!(store.by_head("subclass").len(), 2,
            "recovered sentences should be committed despite the parse error");
        assert!(store.symbols.contains_key("Human"));
        assert!(store.symbols.contains_key("Dog"));
    }

    #[test]
    fn parse_error_leaves_earlier_files_intact() {
        let mut store = SyntacticLayer::default();
        let ok = load_kif(&mut store, "(subclass Human Animal)", "good");
        assert!(ok.is_empty());
        assert_eq!(store.by_head("subclass").len(), 1);

        let errs = load_kif(&mut store, "(\"broken\"", "bad");
        assert!(!errs.is_empty());

        assert_eq!(store.by_head("subclass").len(), 1,
            "good file's roots disturbed by bad file's parse failure");
    }

    #[test]
    fn pattern_lookup() {
        let store = store_from(
            "(instance subclass BinaryRelation)\n(instance instance BinaryPredicate)");
        assert_eq!(store.lookup("instance _ BinaryRelation").len(), 1);
        assert_eq!(store.lookup("instance _ _").len(), 2);
    }

    #[test]
    fn remove_file() {
        let mut store = SyntacticLayer::default();
        load_kif(&mut store, "(subclass Human Animal)", "base");
        load_kif(&mut store, "(subclass Cat Animal)",   "delta");
        assert_eq!(store.roots.len(), 2);
        store.remove_file("delta");
        assert_eq!(store.roots.len(), 1);
        assert!(!store.symbols.contains_key("Cat"));
        assert!(store.symbols.contains_key("Human"));
    }

    // -- Occurrence index ---------------------------------------------------

    #[test]
    fn occurrences_indexed_for_root_symbols() {
        let mut store = SyntacticLayer::default();
        load_kif(&mut store, "(subclass Human Animal)", "t.kif");
        let human_id = store.sym_id("Human").expect("Human interned");
        let occs = store.occurrences.get(&human_id).expect("Human has occurrences");
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0].idx,  1);
        assert_eq!(occs[0].kind, OccurrenceKind::Arg);

        let sub_id = store.sym_id("subclass").unwrap();
        let sub_occs = &store.occurrences[&sub_id];
        assert_eq!(sub_occs[0].kind, OccurrenceKind::Head);
    }

    #[test]
    fn occurrences_indexed_through_sub_sentences() {
        let mut store = SyntacticLayer::default();
        load_kif(&mut store, "(=> (P ?X) (Q ?X))", "t.kif");
        let p_id = store.sym_id("P").expect("P interned");
        let q_id = store.sym_id("Q").expect("Q interned");
        assert_eq!(store.occurrences[&p_id].len(), 1);
        assert_eq!(store.occurrences[&q_id].len(), 1);
        assert_eq!(store.occurrences[&p_id][0].kind, OccurrenceKind::Head);
        assert_eq!(store.occurrences[&q_id][0].kind, OccurrenceKind::Head);
    }

    #[test]
    fn occurrences_cleared_on_remove_file() {
        let mut store = SyntacticLayer::default();
        load_kif(&mut store, "(subclass Human Animal)", "a.kif");
        load_kif(&mut store, "(subclass Human Mammal)", "b.kif");
        let human_id = store.sym_id("Human").unwrap();
        assert_eq!(store.occurrences[&human_id].len(), 2);

        store.remove_file("b.kif");
        let remaining = store.occurrences.get(&human_id)
            .map(|v| v.len()).unwrap_or(0);
        assert_eq!(remaining, 1);
    }

    #[test]
    fn occurrences_cleared_on_remove_sentence() {
        let mut store = SyntacticLayer::default();
        load_kif(&mut store, "(subclass Human Animal)\n(subclass Human Mammal)", "t.kif");
        let human_id = store.sym_id("Human").unwrap();
        assert_eq!(store.occurrences[&human_id].len(), 2);

        let first_sid = store.file_roots["t.kif"][0];
        store.remove_sentence(first_sid);
        assert_eq!(store.occurrences.get(&human_id).map(|v| v.len()).unwrap_or(0), 1);
    }

    #[test]
    fn variables_have_scope_qualified_occurrences() {
        let mut store = SyntacticLayer::default();
        load_kif(&mut store, "(forall (?X) (P ?X))\n(forall (?X) (Q ?X))", "t.kif");
        let xs: Vec<&str> = store.symbols.keys()
            .filter(|k| k.starts_with("X__"))
            .map(|s| s.as_str())
            .collect();
        assert!(xs.len() >= 2, "expected distinct X__<scope> ids, got {:?}", xs);
    }
}
