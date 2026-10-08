// core/src/semantics/validate/validators
//
//! W033 duplicate-axiom: one normalized axiom has several source occurrences.

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::FormulaValidator;
use crate::SentenceId;

#[derive(Debug, Clone, Error)]
#[error("duplicate axiom appears in {occurrences} source locations")]
pub struct DuplicateAxiom {
    pub sid: SentenceId,
    pub occurrences: usize,
}

semantic_error!(
    DuplicateAxiom,
    "W033",
    "duplicate-axiom",
    Warning,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], -1)
    },
);

pub(crate) struct DuplicateAxiomCheck;

impl FormulaValidator for DuplicateAxiomCheck {
    type Error = DuplicateAxiom;

    fn check(&self, cx: &Cx<'_>, root: SentenceId) -> Vec<Self::Error> {
        let occurrences = cx.layer.syntactic.source_spans(root).len();
        (occurrences > 1)
            .then_some(DuplicateAxiom {
                sid: root,
                occurrences,
            })
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::{KnowledgeBase, SourceFile};

    #[test]
    fn w033_fans_out_to_every_source_occurrence() {
        let mut kb = KnowledgeBase::new();
        let formula = "(subclass Dog Entity)".to_string();
        assert!(
            kb.load(
                SourceFile::kif(PathBuf::from("first.kif"), formula.clone()),
                "first"
            )
            .ok
        );
        assert!(
            kb.load(
                SourceFile::kif(PathBuf::from("second.kif"), formula),
                "second"
            )
            .ok
        );

        let mut duplicates: Vec<_> = kb
            .validate(crate::ValidationTarget::All, None)
            .into_iter()
            .filter(|d| d.code == "duplicate-axiom")
            .collect();
        duplicates.sort_by(|a, b| a.range.file.cmp(&b.range.file));

        assert_eq!(duplicates.len(), 2);
        assert_eq!(duplicates[0].range.file, "first.kif");
        assert_eq!(duplicates[1].range.file, "second.kif");
        assert!(duplicates
            .iter()
            .all(|d| d.message == "duplicate axiom appears in 2 source locations"));
    }

    #[test]
    fn w033_ignores_scratch_restatements_of_a_file_axiom() {
        let mut kb = KnowledgeBase::new();
        let formula = "(subclass Dog Entity)";
        assert!(
            kb.load(
                SourceFile::kif(PathBuf::from("first.kif"), formula.to_string()),
                "first"
            )
            .ok
        );
        kb.tell(formula, "scratch");

        let diags = kb.validate(crate::ValidationTarget::All, None);
        assert!(
            !diags.iter().any(|d| d.code == "duplicate-axiom"),
            "a tell restating a file axiom is not a duplicate; got {diags:?}"
        );
        assert!(
            diags.iter().all(|d| !d.range.file.starts_with("__")),
            "diagnostics must not be anchored at scratch sources; got {diags:?}"
        );
    }

    #[test]
    fn w033_ignores_unique_axioms() {
        let mut kb = KnowledgeBase::new();
        assert!(
            kb.load(
                SourceFile::kif(PathBuf::from("only.kif"), "(subclass Dog Entity)".into()),
                "only",
            )
            .ok
        );

        assert!(!kb
            .validate(crate::ValidationTarget::All, None)
            .iter()
            .any(|d| d.code == "duplicate-axiom"));
    }

    #[test]
    fn w033_tracks_source_reference_changes_with_validation_cached() {
        let mut kb = KnowledgeBase::new();
        let first = PathBuf::from("first.kif");
        let second = PathBuf::from("second.kif");
        let formula = "(subclass Dog Entity)".to_string();

        assert!(kb.load(SourceFile::kif(first, formula.clone()), "first").ok);
        assert!(!kb
            .validate(crate::ValidationTarget::All, None)
            .iter()
            .any(|d| d.code == "duplicate-axiom"));

        assert!(
            kb.load(SourceFile::kif(second.clone(), formula), "second")
                .ok
        );
        assert_eq!(
            kb.validate(crate::ValidationTarget::All, None)
                .iter()
                .filter(|d| d.code == "duplicate-axiom")
                .count(),
            2
        );

        assert!(kb.load(SourceFile::kif(second, String::new()), "second").ok);
        assert!(!kb
            .validate(crate::ValidationTarget::All, None)
            .iter()
            .any(|d| d.code == "duplicate-axiom"));
    }
}
