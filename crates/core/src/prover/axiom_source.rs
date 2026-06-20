//! Map proof-step formulas back to the source axioms that produced them.
//!
//! The prover renames variables on its way into CNF, so a proof step and its
//! source axiom can differ in variable spelling. The bridge is
//! [`canonical_sentence_fingerprint`]: an alpha-equivalent structural hash
//! that collapses both spellings onto the same 64-bit key. This module wraps
//! that hash in a pre-built index so a whole proof can be resolved without
//! re-scanning the KB per step.
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

use std::collections::HashMap;

use crate::parse::ast::AstNode;
use crate::parse::fingerprint::canonical_sentence_fingerprint;
use crate::types::SentenceId;

/// Where a matched axiom appears in its source file.
///
/// One proof-step hash can correspond to multiple `AxiomSource` entries when
/// the KB contains syntactically-identical axioms across different files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxiomSource {
    /// Id of the matched sentence in the KB.
    pub sid: SentenceId,
    /// File tag from `Sentence.file` — typically an absolute path, but for
    /// in-memory tests it can be any caller-chosen tag.
    pub file: String,
    /// 1-based start line from `Sentence.span.line`.
    pub line: u32,
}

/// Index from canonical fingerprint to the axiom(s) sharing that fingerprint,
/// plus a companion sid-keyed map for direct O(1) lookup when the proof
/// transcript preserved axiom names. Holds no reference to the KB.
///
/// Two complementary lookup paths:
///
/// - [`lookup_by_sid`](Self::lookup_by_sid) — O(1), takes a [`SentenceId`].
///   Use this first when the proof step carried a source sid; it is keyed on
///   the stable input identifier rather than the formula shape.
/// - [`lookup`](Self::lookup) — O(1) average case, takes an [`AstNode`].
///   Use as a fallback when the sid path returns `None`. Tolerates
///   alpha-equivalence.
#[derive(Debug, Clone, Default)]
pub struct AxiomSourceIndex {
    pub(crate) by_hash: HashMap<u64, Vec<AxiomSource>>,
    pub(crate) by_sid:  HashMap<SentenceId, AxiomSource>,
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

    /// Direct sid-keyed lookup. Returns the unique axiom with the given
    /// [`SentenceId`] if one exists in the KB, `None` otherwise.
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

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse_document, Parser};
    use crate::KnowledgeBase;

    fn parse_one(kif: &str) -> AstNode {
        let doc = parse_document("test", kif, Parser::Kif);
        assert!(!doc.has_errors(), "parse errors: {:?}", doc.parse_errors);
        doc.ast.into_iter().next().expect("at least one root").as_stmt().cloned().expect("doc stmt")
    }

    fn kb_with(kif: &str) -> KnowledgeBase {
        let mut kb: KnowledgeBase = KnowledgeBase::new();
        // Axiom-source provenance is about FILE origin, so ingest as a real file
        // (span.file == "test.kif"), then promote.
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("test.kif"), "test.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        let r = kb.make_session_axiomatic(
            "test.kif"
        );
        matches!(r, Ok(_));
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

    // TODO(migration): content-addressing dedups duplicate axioms to one sid; cross-file multi-source provenance no longer applies
    // #[test]
    // fn duplicate_axioms_yield_multiple_sources() {
    //     // Two files asserting the same axiom should both show up.
    //     let mut kb: KnowledgeBase = KnowledgeBase::new();
    //     let r1 = kb.tell("(instance Fido Dog)", "a.kif");
    //     assert!(r1.ok);
    //     let r2 = kb.tell("(instance Fido Dog)", "b.kif");
    //     assert!(r2.ok);
    //     let r = kb.make_session_axiomatic(
    //         "a.kif",
    //         Some(false),
    //         None,
    //         None
    //     );
    //     assert!(matches!(r, Ok(_)));
    //     let r = kb.make_session_axiomatic(
    //         "b.kif",
    //         Some(false),
    //         None,
    //         None
    //     );
    //     assert!(matches!(r, Ok(_)));
    //
    //     let idx = kb.build_axiom_source_index();
    //     let formula = parse_one("(instance Fido Dog)");
    //     let sources = idx.lookup(&formula);
    //     // Two matches — one per file — regardless of dedup status on
    //     // the KB side.  Both carry line 1.
    //     let files: std::collections::BTreeSet<&str> =
    //         sources.iter().map(|s| s.file.as_str()).collect();
    //     assert_eq!(files.len(), 2, "expected both files in sources: {:?}", sources);
    //     assert!(files.contains("a.kif") && files.contains("b.kif"),
    //         "got: {:?}", files);
    // }

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

        // `file_roots` order is unspecified, so verify the set of resolved
        // lines is exactly {1,2,3} rather than relying on enumeration order.
        let roots = kb.file_roots("test.kif");
        assert_eq!(roots.len(), 3);
        let mut lines: Vec<u32> = roots.iter().map(|&sid| {
            let got = idx.lookup_by_sid(sid)
                .unwrap_or_else(|| panic!("sid {} missing from sid index", sid));
            assert_eq!(got.sid, sid);
            assert_eq!(got.file, "test.kif");
            got.line
        }).collect();
        lines.sort_unstable();
        assert_eq!(lines, vec![1, 2, 3]);
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
        // 2 distinct alpha-classes (Dog/Mammal, Cat/Mammal).
        assert_eq!(idx.class_count(), 2);
        // Migration: SentenceIds are now content hashes, so the duplicate
        // `(subclass Dog Mammal)` on lines 1 and 3 dedups to a SINGLE sid —
        // hence a single source.  Total sources is now 2 (one per distinct
        // axiom), not 3 (one per textual occurrence) as under the old
        // sequential-id model.
        assert_eq!(idx.total_sources(), 2);
    }
}
