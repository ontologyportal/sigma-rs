//! Map proof-step formulas back to the source axioms that produced
//! them.  The prover (Vampire) renames variables on its way into CNF,
//! so the proof transcript presents steps like
//!     `(=> (instance ?X0 Agent) (instance ?X0 Entity))`
//! where the source axiom originally read
//!     `(=> (instance ?A Agent) (instance ?A Entity))`
//! in `Merge.kif:17042`.
//!
//! The bridge is [`canonical_sentence_fingerprint`]: an alpha-equivalent
//! structural hash that collapses both spellings onto the same 64-bit
//! key.  This module wraps that hash in a pre-built index so the
//! `--proof` CLI can look up an entire proof's worth of steps without
//! re-scanning the whole KB per step.
//!
//! ## Usage
//!
//! ```ignore
//! let idx = kb.build_axiom_source_index();
//! for step in proof_kif.iter().filter(|s| s.rule == "axiom") {
//!     for src in idx.lookup(&step.formula) {
//!         println!("step {} ← {}:{}", step.index, src.file, src.line);
//!     }
//! }
//! ```
//!
//! Gated on `ask` because its only consumer is the proof-printing
//! path in the CLI.

#![cfg(feature = "ask")]

use std::collections::HashMap;

use crate::kb::KnowledgeBase;
use crate::parse::ast::AstNode;
use crate::parse::fingerprint::{
    canonical_sentence_fingerprint, sentence_canonical_fingerprint,
};
use crate::types::SentenceId;

/// Where a matched axiom appears in its source file.
///
/// One proof-step hash can correspond to multiple `AxiomSource`
/// entries when the KB contains syntactically-identical axioms
/// across different files (rare but possible — SUMO ships some
/// duplicates across ontology layers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxiomSource {
    /// Id of the matched sentence in the KB.
    pub sid: SentenceId,
    /// File tag from `Sentence.file` — typically an absolute path
    /// like `/Users/…/sumo/Merge.kif`, but for in-memory tests it
    /// can be any caller-chosen tag.
    pub file: String,
    /// 1-based start line from `Sentence.span.line`.
    pub line: u32,
}

/// Pre-built index from canonical fingerprint to the axiom (or
/// axioms) sharing that fingerprint, plus a companion sid-keyed map
/// for direct O(1) lookup when the proof transcript preserved axiom
/// names (Vampire's `--output_axiom_names on`).
///
/// Built once per proof-print pass via
/// [`KnowledgeBase::build_axiom_source_index`] and then queried
/// repeatedly — scanning the whole KB once is much cheaper than
/// rescanning per step.  Holds no reference to the KB; it's cheap
/// to keep around.
///
/// Two complementary lookup paths:
///
/// - [`lookup_by_sid`](Self::lookup_by_sid) — O(1), takes a
///   [`SentenceId`].  Use this first when the proof step carried a
///   source sid (via `--output_axiom_names on`).  Robust to every
///   structural transformation Vampire might apply (CNF, alpha-
///   renaming, quantifier flattening) because it's keyed on the
///   stable input identifier, not the formula shape.
/// - [`lookup`](Self::lookup) — O(1) average case, takes an
///   [`AstNode`].  Use as a fallback when the sid path returns
///   `None`.  Tolerates alpha-equivalence but (as of the recent
///   quantifier-collapse change in the TPTP→KIF translator) requires
///   the canonical fingerprint walker to normalise the same way; see
///   the caveat in the translator docstring.
#[derive(Debug, Clone, Default)]
pub struct AxiomSourceIndex {
    by_hash: HashMap<u64, Vec<AxiomSource>>,
    by_sid:  HashMap<SentenceId, AxiomSource>,
}

impl AxiomSourceIndex {
    /// Return every source entry matching `formula`'s canonical
    /// fingerprint.  An empty slice means the proof step didn't come
    /// directly from a source axiom — typical for CNF-transformed or
    /// resolution-derived steps (role != `"axiom"`) and for the
    /// negated conjecture.
    pub fn lookup(&self, formula: &AstNode) -> &[AxiomSource] {
        let h = canonical_sentence_fingerprint(formula);
        self.by_hash.get(&h).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Direct sid-keyed lookup.  Returns the unique axiom with the
    /// given [`SentenceId`] if one exists in the KB, `None`
    /// otherwise.
    ///
    /// Prefer this when the proof step preserved its source sid —
    /// it's exact (no possibility of cross-file duplicate collisions),
    /// O(1), and survives every structural transformation the prover
    /// might apply on the way to the proof transcript.
    pub fn lookup_by_sid(&self, sid: SentenceId) -> Option<&AxiomSource> {
        self.by_sid.get(&sid)
    }

    /// Number of distinct canonical hashes tracked (one entry per
    /// alpha-equivalence class).  Useful for diagnostics and tests.
    pub fn class_count(&self) -> usize {
        self.by_hash.len()
    }

    /// Total number of axiom sources indexed (counts duplicates as
    /// separate entries — the index preserves all matching source
    /// locations, not just one).
    pub fn total_sources(&self) -> usize {
        self.by_hash.values().map(|v| v.len()).sum()
    }
}

impl KnowledgeBase {
    /// Build a fresh [`AxiomSourceIndex`] by canonically hashing every
    /// root sentence in the KB.  O(N) in the total sentence count; for
    /// a 100 k-axiom SUMO load this takes ~30–80 ms.  Re-run if the KB
    /// mutates — the index is a snapshot, not a live view.
    ///
    /// Includes sentences from every loaded file, including ephemeral
    /// ones like `__query__` / `__sine_query__`.  Callers that want to
    /// surface only "real" source files typically filter by
    /// [`AxiomSource::file`] starting with `/` or by excluding the
    /// `__` prefix.
    pub fn build_axiom_source_index(&self) -> AxiomSourceIndex {
        let store = self.store_for_testing();
        let mut by_hash: HashMap<u64, Vec<AxiomSource>> = HashMap::new();
        let mut by_sid:  HashMap<SentenceId, AxiomSource> = HashMap::new();
        for (_file, roots) in store.file_roots.iter() {
            for &sid in roots {
                let h = sentence_canonical_fingerprint(sid, store);
                let s = &store.sentences[store.sent_idx(sid)];
                let entry = AxiomSource {
                    sid,
                    file: s.file.clone(),
                    line: s.span.line,
                };
                by_hash.entry(h).or_default().push(entry.clone());
                // A sid is unique in the KB — the only way a collision
                // could occur is if the same sid appeared under two
                // file tags, which `SyntacticLayer`'s invariants forbid.
                // Blind `insert` is correct here.
                by_sid.insert(sid, entry);
            }
        }
        AxiomSourceIndex { by_hash, by_sid }
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse_document, Parser};
    use crate::KnowledgeBase;

    fn parse_one(kif: &str) -> AstNode {
        let doc = parse_document("test", kif);
        assert!(!doc.has_errors(), "parse errors: {:?}", doc.diagnostics);
        doc.ast.into_iter().next().expect("at least one root")
    }

    fn kb_with(kif: &str) -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let r = kb.load_kif(kif, "test.kif", Some("test.kif"));
        assert!(r.ok, "load failed: {:?}", r.errors);
        kb.make_session_axiomatic("test.kif");
        kb
    }

    #[test]
    fn exact_match_finds_source() {
        let kb = kb_with("(subclass Dog Mammal)");
        let idx = kb.build_axiom_source_index();
        let formula = parse_one("(subclass Dog Mammal)");
        let sources = idx.lookup(&formula);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].file, "test.kif");
        assert_eq!(sources[0].line, 1);
    }

    #[test]
    fn alpha_variant_matches_source() {
        // The proof step's variable name differs from the source —
        // canonical hashing must still match them.
        let kb = kb_with("(=> (instance ?AGT Agent) (instance ?AGT Entity))");
        let idx = kb.build_axiom_source_index();
        let proof_step = parse_one("(=> (instance ?X0 Agent) (instance ?X0 Entity))");
        let sources = idx.lookup(&proof_step);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].line, 1);
    }

    #[test]
    fn vampire_style_leading_forall_matches_implicit_source() {
        // Vampire always makes top-level universals explicit — even
        // nested, one variable per `forall`.  The source axiom leaves
        // them implicit (KIF convention).  The forall-strip layer in
        // the canonical fingerprint means these match.
        let kb = kb_with(
            "(=> (equal ?P (equilibriumPriceFn ?THING ?M)) (member ?THING ?M))"
        );
        let idx = kb.build_axiom_source_index();
        let vampire_style = parse_one(
            "(forall (?X1) (forall (?X2) (forall (?X3) \
             (=> (equal ?X3 (equilibriumPriceFn ?X1 ?X2)) (member ?X1 ?X2)))))"
        );
        let sources = idx.lookup(&vampire_style);
        assert_eq!(
            sources.len(), 1,
            "nested-forall Vampire output must match implicit-universal source: {:?}",
            sources
        );
    }

    #[test]
    fn multi_variable_alpha_equivalence() {
        let kb = kb_with("(=> (and (P ?X) (Q ?Y)) (R ?X ?Y))");
        let idx = kb.build_axiom_source_index();
        let proof_step = parse_one("(=> (and (P ?X0) (Q ?X1)) (R ?X0 ?X1))");
        let sources = idx.lookup(&proof_step);
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn non_matching_formula_returns_empty() {
        let kb = kb_with("(subclass Dog Mammal)");
        let idx = kb.build_axiom_source_index();
        let unrelated = parse_one("(subclass Cat Feline)");
        assert_eq!(idx.lookup(&unrelated).len(), 0);
    }

    #[test]
    fn duplicate_axioms_yield_multiple_sources() {
        // Two files asserting the same axiom should both show up.
        let mut kb = KnowledgeBase::new();
        let r1 = kb.load_kif("(instance Fido Dog)", "a.kif", Some("a.kif"));
        assert!(r1.ok);
        let r2 = kb.load_kif("(instance Fido Dog)", "b.kif", Some("b.kif"));
        assert!(r2.ok);
        kb.make_session_axiomatic("a.kif");
        kb.make_session_axiomatic("b.kif");

        let idx = kb.build_axiom_source_index();
        let formula = parse_one("(instance Fido Dog)");
        let sources = idx.lookup(&formula);
        // Two matches — one per file — regardless of dedup status on
        // the KB side.  Both carry line 1.
        let files: std::collections::BTreeSet<&str> =
            sources.iter().map(|s| s.file.as_str()).collect();
        assert_eq!(files.len(), 2, "expected both files in sources: {:?}", sources);
        assert!(files.contains("a.kif") && files.contains("b.kif"),
            "got: {:?}", files);
    }

    #[test]
    fn line_number_is_source_relative() {
        // Multiple axioms in one file should carry correct line
        // numbers — ensures we're reading `span.line` and not some
        // accidental global counter.
        let src = "\
            (subclass Dog Mammal)\n\
            (subclass Cat Mammal)\n\
            (subclass Mammal Animal)\n\
        ";
        let kb = kb_with(src);
        let idx = kb.build_axiom_source_index();

        for (kif, expected_line) in [
            ("(subclass Dog Mammal)", 1),
            ("(subclass Cat Mammal)", 2),
            ("(subclass Mammal Animal)", 3),
        ] {
            let formula = parse_one(kif);
            let sources = idx.lookup(&formula);
            assert_eq!(sources.len(), 1, "for {}: {:?}", kif, sources);
            assert_eq!(sources[0].line, expected_line, "for {}", kif);
        }
    }

    #[test]
    fn lookup_by_sid_hits_every_loaded_axiom() {
        // Populate the index with three axioms, then verify every
        // sid returned by `file_roots` resolves via `lookup_by_sid`
        // to exactly the matching source entry.
        let src = "\
            (subclass Dog Mammal)\n\
            (subclass Cat Mammal)\n\
            (subclass Mammal Animal)\n\
        ";
        let kb = kb_with(src);
        let idx = kb.build_axiom_source_index();

        for (i, &sid) in kb.syntactic().file_roots["test.kif"].iter().enumerate() {
            let got = idx.lookup_by_sid(sid)
                .unwrap_or_else(|| panic!("sid {} missing from sid index", sid));
            assert_eq!(got.sid, sid);
            assert_eq!(got.file, "test.kif");
            assert_eq!(got.line, (i + 1) as u32);
        }
    }

    #[test]
    fn lookup_by_sid_returns_none_for_unknown_sid() {
        let kb = kb_with("(subclass Dog Mammal)");
        let idx = kb.build_axiom_source_index();
        assert!(idx.lookup_by_sid(999_999).is_none());
    }

    #[test]
    fn index_stats_are_reasonable() {
        let src = "\
            (subclass Dog Mammal)\n\
            (subclass Cat Mammal)\n\
            (subclass Dog Mammal)\n\
        ";
        // First and third are dup — canonical hash is the same.
        let _p = Parser::Kif;  // silences unused-import warnings if the parse path changes
        let kb = kb_with(src);
        let idx = kb.build_axiom_source_index();
        // 2 distinct alpha-classes (Dog/Mammal, Cat/Mammal) but 3 sources total.
        assert_eq!(idx.class_count(), 2);
        assert_eq!(idx.total_sources(), 3);
    }
}
