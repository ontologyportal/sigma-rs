//! E015 disjoint-subclass: a term's subclass ancestors cannot be disjoint.

use std::collections::HashSet;

use thiserror::Error;

use crate::semantics::errors::semantic_error;
use crate::semantics::taxonomy::TaxRelation;
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::{SymbolPos, SymbolValidator};
use crate::types::Symbol;
use crate::{Element, SentenceId, SymbolId};

/// A symbol is a subclass of two disjoint classes.
#[derive(Debug, Clone, Error)]
#[error("'{sym}' is a subclass of disjoint classes ({class1} and {class2})")]
pub struct DisjointSubclass {
    pub sid: Vec<SentenceId>,
    pub sym: String,
    pub class1: String,
    pub class2: String,
}
semantic_error!(
    DisjointSubclass,
    "E015",
    "disjoint-subclass",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (self.sid.clone(), -1)
    },
);

pub(crate) struct DisjointParents;

impl SymbolValidator for DisjointParents {
    type Error = DisjointSubclass;

    fn check(&self, cx: &Cx<'_>, sym: SymbolId, pos: SymbolPos) -> Vec<DisjointSubclass> {
        if !cx.claim_symbol("E015", sym) {
            return Vec::new();
        }

        let mut ancestors = HashSet::new();
        let mut seen = HashSet::from([sym]);
        let mut stack = vec![sym];
        while let Some(child) = stack.pop() {
            for (parent, rel) in cx.parents_of(child) {
                if rel == TaxRelation::Subclass && seen.insert(parent) {
                    ancestors.insert(parent);
                    stack.push(parent);
                }
            }
        }
        if ancestors.len() < 2 {
            return Vec::new();
        }

        let mut parents: Vec<_> = ancestors.iter().copied().collect();
        parents.sort_unstable();
        let disjoint = Symbol::hash_name("disjoint");
        for parent in parents {
            let mut declarations = cx.subject_sids(disjoint, parent);
            declarations.sort_unstable();
            for sid in declarations {
                let Some(sentence) = cx.sentence(sid) else {
                    continue;
                };
                let [Element::Symbol(head), Element::Symbol(a), Element::Symbol(b)] =
                    sentence.elements.as_slice()
                else {
                    continue;
                };
                if head.id() != disjoint
                    || a.id() != parent
                    || a.id() == b.id()
                    || !ancestors.contains(&b.id())
                {
                    continue;
                }
                let mut classes = [cx.sym_name(a.id()), cx.sym_name(b.id())];
                classes.sort();
                let mut sids = vec![pos.sid];
                if sid != pos.sid {
                    sids.push(sid);
                }
                return vec![DisjointSubclass {
                    sid: sids,
                    sym: cx.sym_name(sym),
                    class1: classes[0].clone(),
                    class2: classes[1].clone(),
                }];
            }
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantics::types::Scope;
    use crate::semantics::validate::test_support::{kif_layer, root_by_head_with, root_by_op};
    use crate::semantics::SemanticLayer;
    use crate::{Diagnostic, KnowledgeBase, OpKind, Severity, SourceFile};

    fn findings(
        layer: &SemanticLayer,
        sym: &str,
        sid: SentenceId,
        scope: Scope,
    ) -> Vec<Diagnostic> {
        layer
            .validator_scoped(scope)
            .validate_sentence_collect(sid)
            .into_iter()
            .filter(|e| e.code() == "E015" && e.to_string().starts_with(&format!("'{sym}'")))
            .map(|e| e.diagnostic())
            .collect()
    }

    #[test]
    fn direct_parents_report_error_with_source_context_in_either_order() {
        for declaration in [
            "(disjoint Substance CorpuscularObject)",
            "(disjoint CorpuscularObject Substance)",
        ] {
            let layer = kif_layer(&format!(
                "(subclass BacterialAgent Substance)
                 (subclass BacterialAgent CorpuscularObject)
                 {declaration}"
            ));
            let sid = root_by_head_with(&layer, "subclass", "Substance");
            let errors = findings(&layer, "BacterialAgent", sid, Scope::Base);
            assert_eq!(errors.len(), 1);
            let error = &errors[0];
            assert_eq!(error.severity, Severity::Error);
            assert_eq!(error.code, "disjoint-subclass");
            assert_eq!(error.message,
                "'BacterialAgent' is a subclass of disjoint classes (CorpuscularObject and Substance)");
            assert_eq!(error.sids[0], sid);
            assert!(error.sids.contains(&layer.syntactic.by_head("disjoint")[0]));
        }
    }

    #[test]
    fn inherited_parents_and_descendants_are_checked() {
        let layer = kif_layer(
            "(subclass Substance Entity)
             (subclass CorpuscularObject Entity)
             (subclass Material Substance)
             (subclass Particle CorpuscularObject)
             (subclass BacterialAgent Material)
             (subclass BacterialAgent Particle)
             (subclass Pathogen BacterialAgent)
             (disjoint Substance CorpuscularObject)",
        );
        let sid = root_by_head_with(&layer, "subclass", "Pathogen");
        assert_eq!(findings(&layer, "Pathogen", sid, Scope::Base).len(), 1);
    }

    #[test]
    fn repeated_symbol_reports_one_error_per_formula() {
        let layer = kif_layer(
            "(subclass BacterialAgent Substance)
             (subclass BacterialAgent CorpuscularObject)
             (disjoint Substance CorpuscularObject)
             (=> (instance ?X BacterialAgent) (related BacterialAgent BacterialAgent))",
        );
        let sid = root_by_op(&layer, OpKind::Implies);
        assert_eq!(
            findings(&layer, "BacterialAgent", sid, Scope::Base).len(),
            1
        );
    }

    #[test]
    fn unrelated_disjointness_and_shared_ancestors_are_valid() {
        for declaration in ["", "(disjoint Substance Unrelated)"] {
            let layer = kif_layer(&format!(
                "(subclass BacterialAgent Substance)
                 (subclass BacterialAgent CorpuscularObject)
                 (subclass Substance Entity)
                 (subclass CorpuscularObject Entity)
                 {declaration}"
            ));
            let sid = layer.subject_sids_scoped(
                Symbol::hash_name("subclass"),
                Symbol::hash_name("BacterialAgent"),
                Scope::Base,
            )[0];
            assert!(findings(&layer, "BacterialAgent", sid, Scope::Base).is_empty());
        }
    }

    #[test]
    fn instance_edges_do_not_count_as_subclass_parents() {
        let layer = kif_layer(
            "(instance BacterialAgent Substance)
             (subclass BacterialAgent CorpuscularObject)
             (disjoint Substance CorpuscularObject)",
        );
        let sid = root_by_head_with(&layer, "subclass", "BacterialAgent");
        assert!(findings(&layer, "BacterialAgent", sid, Scope::Base).is_empty());
    }

    #[test]
    fn non_fact_or_malformed_disjoint_sentences_are_ignored() {
        for declaration in [
            "(not (disjoint Substance CorpuscularObject))",
            "(=> (related Substance CorpuscularObject) (disjoint Substance CorpuscularObject))",
            "(disjoint Substance)",
            "(disjoint Substance CorpuscularObject Entity)",
            "(disjoint Substance ?CLASS)",
            "(disjoint Substance (ClassFn CorpuscularObject))",
            "(disjoint Substance Substance)",
        ] {
            let layer = kif_layer(&format!(
                "(subclass BacterialAgent Substance)
                 (subclass BacterialAgent CorpuscularObject)
                 {declaration}"
            ));
            let sid = root_by_head_with(&layer, "subclass", "Substance");
            assert!(
                findings(&layer, "BacterialAgent", sid, Scope::Base).is_empty(),
                "must not treat {declaration} as a disjoint fact"
            );
        }
    }

    #[test]
    fn taxonomy_cycles_terminate_and_still_report_conflicts() {
        let layer = kif_layer(
            "(subclass BacterialAgent Substance)
             (subclass Substance BacterialAgent)
             (subclass Substance CorpuscularObject)
             (disjoint Substance CorpuscularObject)",
        );
        let sid = layer.subject_sids_scoped(
            Symbol::hash_name("subclass"),
            Symbol::hash_name("BacterialAgent"),
            Scope::Base,
        )[0];
        assert_eq!(
            findings(&layer, "BacterialAgent", sid, Scope::Base).len(),
            1
        );
    }

    #[test]
    fn public_validation_uses_session_scope_and_tracks_retraction() {
        let mut kb = KnowledgeBase::new();
        kb.load(
            SourceFile::kif(
                "base.kif".into(),
                "(subclass BacterialAgent Substance)
             (subclass BacterialAgent CorpuscularObject)"
                    .into(),
            ),
            "base.kif",
        );
        kb.make_session_axiomatic("base.kif").expect("promote base");
        assert!(!kb
            .validate_file("base.kif")
            .iter()
            .any(|d| d.code == "disjoint-subclass"));
        kb.tell("(disjoint Substance CorpuscularObject)", "conflict");
        let errors: Vec<_> = kb
            .validate_file_in_session("base.kif", "conflict")
            .into_iter()
            .filter(|d| d.code == "disjoint-subclass")
            .collect();
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().all(|d| d.range.file == "base.kif"));
        assert!(!kb
            .validate_file_in_session("base.kif", "other")
            .iter()
            .any(|d| d.code == "disjoint-subclass"));
        assert!(!kb
            .validate_file("base.kif")
            .iter()
            .any(|d| d.code == "disjoint-subclass"));
        kb.flush_session("conflict");
        assert!(!kb
            .validate_file_in_session("base.kif", "conflict")
            .iter()
            .any(|d| d.code == "disjoint-subclass"));
    }

    #[test]
    fn session_parent_can_conflict_with_a_base_parent() {
        let mut kb = KnowledgeBase::new();
        kb.load(
            SourceFile::kif(
                "base.kif".into(),
                "(subclass BacterialAgent Substance)
             (disjoint Substance CorpuscularObject)"
                    .into(),
            ),
            "base.kif",
        );
        kb.make_session_axiomatic("base.kif").expect("promote base");
        let result = kb.tell("(subclass BacterialAgent CorpuscularObject)", "edit");
        let errors = kb.validate_sentence_in_session(result.sids[0], "edit");
        assert!(errors.iter().any(|d| d.code == "disjoint-subclass"));
        assert!(!kb
            .validate_file("base.kif")
            .iter()
            .any(|d| d.code == "disjoint-subclass"));
    }

    #[test]
    fn empty_kb_has_no_disjoint_parent_errors() {
        assert!(KnowledgeBase::new().validate_all().is_empty());
    }
}
