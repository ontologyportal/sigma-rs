//! Recorded contradictions: the `KnowledgeBase` face of the
//! `semantic::audit_findings` cache.
//!
//! Every consistency-audit path in `kb/prove.rs` records the contradictions
//! it finds here, and reports produced elsewhere (a nightly workflow, a
//! GitHub issue, a downloaded JSON file) are imported by resolving each
//! cited axiom back to a sentence in this KB.  A finding is only recorded
//! when every cited axiom is present, and the cache drops it as soon as one
//! of them is removed.

#![cfg(any(feature = "external-prover", feature = "native-prover"))]

use std::sync::Arc;

use crate::cache::events::Event;
use crate::clock::unix_millis;
use crate::layer::{Layer, TopLayer};
use crate::parse::ast::AstNode;
use crate::parse::kif::dis::AstKif;
use crate::parse::{Parser, Span};
use crate::prover::proof::KifProofStep;
use crate::prover::{ProverResult, ProverStatus};
use crate::types::SentenceId;
use crate::DocItem;

pub use crate::semantics::caches::audit_findings::AuditFinding;

use super::KnowledgeBase;

/// An axiom cited by a contradiction report: its KIF text plus, when the
/// report recorded one, the file and line it was read from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CitedAxiom {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    pub kif: String,
}

/// One step of a reported derivation, as a report carries it: KIF text
/// rather than an AST, and a cited-axiom index instead of a sentence id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReportedStep {
    pub rule: String,
    #[serde(default)]
    pub premises: Vec<usize>,
    pub kif: String,
    /// Index into the report's cited axioms when this step is one of them.
    #[serde(default)]
    pub axiom: Option<usize>,
}

/// Why an imported contradiction was not recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportRejection {
    /// The report cited no axioms at all.
    NoAxioms,
    /// These cited axioms are not in the KB (edited, unloaded, or never
    /// loaded), so the refutation cannot be assumed to hold here.
    Missing(Vec<CitedAxiom>),
}

impl std::fmt::Display for ImportRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAxioms => write!(f, "the report cites no axioms"),
            Self::Missing(axioms) => write!(
                f,
                "{} cited axiom(s) are not in the knowledge base",
                axioms.len()
            ),
        }
    }
}

/// Whitespace-insensitive comparison of two KIF renderings.
fn same_kif(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
}

impl<L: TopLayer + Layer> KnowledgeBase<L> {
    /// Every recorded contradiction, newest first.
    pub fn audit_findings(&self) -> Vec<Arc<AuditFinding>> {
        self.layer.semantic().audit_findings()
    }

    /// The recorded contradiction citing exactly `culprits` (any order) in
    /// `session`'s scope, if any.
    pub fn audit_finding(
        &self,
        culprits: &[SentenceId],
        session: Option<&str>,
    ) -> Option<Arc<AuditFinding>> {
        let mut key = culprits.to_vec();
        key.sort_unstable();
        key.dedup();
        self.layer.semantic().audit_finding(&key, session)
    }

    /// Forget every recorded contradiction.
    pub fn clear_audit_findings(&self) {
        self.layer.semantic().clear_audit_findings();
    }

    /// Record `finding` (cascading `ContradictionFound` down the stack) and
    /// return the shared copy the cache holds.
    pub fn record_finding(&self, finding: AuditFinding) -> Arc<AuditFinding> {
        let finding = Arc::new(finding);
        let outcome = self.layer.cascade(vec![Event::ContradictionFound {
            finding: Arc::clone(&finding),
        }]);
        for err in outcome.errors {
            self.log(
                crate::progress::LogLevel::Warn,
                format!("audit_findings: {}", err.message),
            );
        }
        finding
    }

    /// Record every contradiction proof `result` carries, under `session`'s
    /// scope and labelled `origin`.  Proofs citing no source axiom at all
    /// cannot be keyed and are skipped.
    pub(crate) fn record_contradictions(
        &self,
        result: &ProverResult,
        session: Option<&str>,
        origin: &str,
    ) -> Vec<Arc<AuditFinding>> {
        let mut proofs: Vec<&Vec<KifProofStep>> = result.contradiction_proofs.iter().collect();
        if proofs.is_empty()
            && result.status == ProverStatus::Inconsistent
            && !result.proof_kif.is_empty()
        {
            proofs.push(&result.proof_kif);
        }
        let now = unix_millis();
        proofs
            .into_iter()
            .filter_map(|steps| {
                AuditFinding::from_steps(
                    steps.clone(),
                    session.map(str::to_string),
                    origin.to_string(),
                    now,
                )
            })
            .map(|finding| self.record_finding(finding))
            .collect()
    }

    /// The root sentences a cited axiom denotes in this KB, or empty when it
    /// is not loaded.  Tried in order: the source formula's fingerprint (the
    /// report's KIF re-parsed), then the sentence at the recorded file and
    /// line whose source or normalized rendering matches, then every root
    /// with the same head symbol.
    pub fn resolve_cited_axiom(&self, axiom: &CitedAxiom) -> Vec<SentenceId> {
        let syn = &self.layer.semantic().syntactic;
        let node = parse_single(&axiom.kif);
        if let Some(node) = &node {
            let roots = syn.roots_of_fingerprint(node.fingerprint());
            if !roots.is_empty() {
                return roots;
            }
        }
        let matches = |sid: SentenceId| {
            syn.source_node_of(sid)
                .is_some_and(|n| same_kif(&n.format_plain(0), &axiom.kif))
                || same_kif(&syn.sentence_to_ast(sid).format_plain(0), &axiom.kif)
        };
        if let (Some(file), Some(line)) = (&axiom.file, axiom.line) {
            let at_line: Vec<SentenceId> = self
                .file_roots(file)
                .into_iter()
                .filter(|&sid| syn.source_span(sid).is_some_and(|s| s.line == line))
                .collect();
            let hit: Vec<SentenceId> = at_line.iter().copied().filter(|&s| matches(s)).collect();
            if !hit.is_empty() {
                return hit;
            }
        }
        let head = node.as_ref().and_then(|n| match n {
            AstNode::List { elements, .. } => match elements.first() {
                Some(AstNode::Symbol { name, .. }) => Some(name.clone()),
                _ => None,
            },
            _ => None,
        });
        if let Some(head) = head {
            let hit: Vec<SentenceId> = syn
                .by_head(&head)
                .into_iter()
                .filter(|&s| matches(s))
                .collect();
            if !hit.is_empty() {
                return hit;
            }
        }
        Vec::new()
    }

    /// Import one reported contradiction: resolve every cited axiom to this
    /// KB's sentences and record the finding under `origin`.  `steps` is the
    /// report's derivation when it carries one; without it the finding holds
    /// one axiom step per culprit and a closing contradiction step.  Rejected
    /// when any axiom is missing, so a stale report never records a
    /// refutation this KB does not contain.
    pub fn import_contradiction(
        &self,
        axioms: &[CitedAxiom],
        steps: &[ReportedStep],
        origin: &str,
    ) -> Result<Arc<AuditFinding>, ImportRejection> {
        self.import_contradiction_at(axioms, steps, origin, None)
    }

    /// [`import_contradiction`](Self::import_contradiction) stamped with the
    /// report's own `found_at_ms` when it records one, so a finding restored
    /// from a mirror keeps the time it was first found.
    pub fn import_contradiction_at(
        &self,
        axioms: &[CitedAxiom],
        steps: &[ReportedStep],
        origin: &str,
        found_at_ms: Option<u64>,
    ) -> Result<Arc<AuditFinding>, ImportRejection> {
        if axioms.is_empty() {
            return Err(ImportRejection::NoAxioms);
        }
        let resolved: Vec<Vec<SentenceId>> =
            axioms.iter().map(|a| self.resolve_cited_axiom(a)).collect();
        let missing: Vec<CitedAxiom> = axioms
            .iter()
            .zip(&resolved)
            .filter(|(_, sids)| sids.is_empty())
            .map(|(a, _)| a.clone())
            .collect();
        if !missing.is_empty() {
            return Err(ImportRejection::Missing(missing));
        }
        let syn = &self.layer.semantic().syntactic;
        let formula_of = |sid: SentenceId| {
            syn.source_node_of(sid)
                .unwrap_or_else(|| syn.sentence_to_ast(sid))
        };
        let mut out: Vec<KifProofStep> = Vec::new();
        if steps.is_empty() {
            for sids in &resolved {
                for &sid in sids {
                    out.push(KifProofStep {
                        index: out.len(),
                        rule: "axiom".into(),
                        premises: Vec::new(),
                        formula: formula_of(sid),
                        source_sid: Some(sid),
                    });
                }
            }
            let premises: Vec<usize> = (0..out.len()).collect();
            out.push(KifProofStep {
                index: out.len(),
                rule: "contradiction".into(),
                premises,
                formula: AstNode::Symbol {
                    name: "FALSE".into(),
                    span: Span::synthetic(),
                },
                source_sid: None,
            });
        } else {
            // Axiom steps take their cited sentence's own formula; a report
            // axiom that denotes several roots cites the first and the rest
            // join as extra axiom steps so the key still covers them.
            let mut extra: Vec<SentenceId> = Vec::new();
            for (i, step) in steps.iter().enumerate() {
                let sid = step.axiom.and_then(|a| resolved.get(a)).and_then(|sids| {
                    extra.extend(sids.iter().skip(1).copied());
                    sids.first().copied()
                });
                let formula = match sid {
                    Some(sid) => formula_of(sid),
                    None => parse_single(&step.kif).unwrap_or_else(|| AstNode::Symbol {
                        name: step.kif.clone(),
                        span: Span::synthetic(),
                    }),
                };
                out.push(KifProofStep {
                    index: i,
                    rule: step.rule.clone(),
                    premises: step.premises.clone(),
                    formula,
                    source_sid: sid,
                });
            }
            let cited: std::collections::HashSet<SentenceId> =
                out.iter().filter_map(|s| s.source_sid).collect();
            for sids in &resolved {
                extra.extend(sids.iter().copied());
            }
            extra.sort_unstable();
            extra.dedup();
            for sid in extra.into_iter().filter(|s| !cited.contains(s)) {
                out.push(KifProofStep {
                    index: out.len(),
                    rule: "axiom".into(),
                    premises: Vec::new(),
                    formula: formula_of(sid),
                    source_sid: Some(sid),
                });
            }
        }
        let finding = AuditFinding::from_steps(
            out,
            None,
            origin.to_string(),
            found_at_ms.unwrap_or_else(unix_millis),
        )
        .ok_or(ImportRejection::NoAxioms)?;
        Ok(self.record_finding(finding))
    }
}

/// Parse `kif` as exactly one top-level formula.
fn parse_single(kif: &str) -> Option<AstNode> {
    let (items, errors) = Parser::Kif { options: None }.parse(kif, "report");
    if !errors.is_empty() {
        return None;
    }
    let mut stmts = items.into_iter().filter_map(|i| match i {
        DocItem::Stmt(n) => Some(n),
        DocItem::Meta(_) => None,
    });
    let first = stmts.next()?;
    if stmts.next().is_some() {
        return None;
    }
    Some(first.strip_annotation())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KnowledgeBase;

    fn reload(kb: &mut KnowledgeBase, kif: &str) {
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("t.kif"), "t.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("t.kif").expect("promote");
        // A staged reload defers removals to the recycle bin; commit them so
        // the retracted roots actually leave the store.
        kb.commit("t.kif");
    }

    fn kb(kif: &str) -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        reload(&mut kb, kif);
        kb
    }

    const KIF: &str = "(subclass Dog Animal)\n(subclass Cat Animal)\n(disjoint Dog Cat)\n(=> (instance ?X Dog) (instance ?X Cat))\n";

    #[test]
    fn resolves_by_reparsed_source_text() {
        let kb = kb(KIF);
        let sids = kb.resolve_cited_axiom(&CitedAxiom {
            file: None,
            line: None,
            kif: "(=> (instance ?X Dog) (instance ?X Cat))".into(),
        });
        assert_eq!(sids.len(), 1);
        assert!(same_kif(
            &kb.render_sentence(sids[0], crate::SentenceForm::Normalized, false, 0),
            "(=> (instance ?X Dog) (instance ?X Cat))"
        ));
    }

    #[test]
    fn resolves_whitespace_variants_by_file_line_and_head() {
        let kb = kb(KIF);
        // Different whitespace defeats the fingerprint only if the parser
        // keeps it; either way the line and head fallbacks find it.
        let by_line = kb.resolve_cited_axiom(&CitedAxiom {
            file: Some("t.kif".into()),
            line: Some(3),
            kif: "(disjoint   Dog\n  Cat)".into(),
        });
        assert_eq!(by_line.len(), 1);
        let by_head = kb.resolve_cited_axiom(&CitedAxiom {
            file: Some("other.kif".into()),
            line: Some(99),
            kif: "(disjoint Dog Cat)".into(),
        });
        assert_eq!(by_head, by_line);
    }

    #[test]
    fn missing_axiom_is_not_resolved_and_rejects_import() {
        let kb = kb(KIF);
        let missing = CitedAxiom {
            file: Some("t.kif".into()),
            line: Some(1),
            kif: "(subclass Dog Plant)".into(),
        };
        assert!(kb.resolve_cited_axiom(&missing).is_empty());
        let present = CitedAxiom {
            file: None,
            line: None,
            kif: "(subclass Dog Animal)".into(),
        };
        let err = kb
            .import_contradiction(&[present, missing.clone()], &[], "test")
            .unwrap_err();
        assert_eq!(err, ImportRejection::Missing(vec![missing]));
        assert!(kb.audit_findings().is_empty());
    }

    #[test]
    fn import_records_and_eviction_follows_cited_axiom() {
        let mut kb = kb(KIF);
        let axioms = vec![
            CitedAxiom {
                file: None,
                line: None,
                kif: "(disjoint Dog Cat)".into(),
            },
            CitedAxiom {
                file: None,
                line: None,
                kif: "(=> (instance ?X Dog) (instance ?X Cat))".into(),
            },
        ];
        let finding = kb.import_contradiction(&axioms, &[], "nightly").unwrap();
        assert_eq!(finding.culprits.len(), 2);
        assert!(finding.complete);
        assert_eq!(finding.origin, "nightly");
        assert_eq!(finding.steps.len(), 3);
        assert_eq!(finding.steps[2].rule, "contradiction");
        assert_eq!(kb.audit_findings().len(), 1);
        assert!(kb.audit_finding(&finding.culprits, None).is_some());

        // Re-importing the same report is idempotent.
        kb.import_contradiction(&axioms, &[], "nightly").unwrap();
        assert_eq!(kb.audit_findings().len(), 1);

        // Editing an uncited axiom keeps the finding; editing a cited one
        // drops it.
        reload(
            &mut kb,
            "(subclass Dog Mammal)\n(subclass Cat Animal)\n(disjoint Dog Cat)\n(=> (instance ?X Dog) (instance ?X Cat))\n",
        );
        assert_eq!(kb.audit_findings().len(), 1);
        reload(
            &mut kb,
            "(subclass Dog Mammal)\n(subclass Cat Animal)\n(=> (instance ?X Dog) (instance ?X Cat))\n",
        );
        assert!(kb.audit_findings().is_empty());
    }

    #[cfg(feature = "native-prover")]
    #[test]
    fn native_audit_records_its_contradictions() {
        use crate::prover::ProverLayer;
        let mut kb = KnowledgeBase::<ProverLayer>::new_native();
        let r = kb.reload_kif(
            "(=> (instance ?X Dog) (instance ?X Animal))\n(instance Rex Dog)\n(not (instance Rex Animal))\n",
            &std::path::PathBuf::from("t.kif"),
            "t.kif",
        );
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("t.kif").expect("promote");
        let focus = kb.file_roots("t.kif");
        let opts = crate::NativeOpts {
            time_limit_secs: 10,
            want_proof: true,
            ..Default::default()
        };
        let res = kb.audit_consistency(&focus, &opts, 4);
        assert_eq!(
            res.status,
            ProverStatus::Inconsistent,
            "raw: {}",
            res.raw_output
        );
        let findings = kb.audit_findings();
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].origin, "native");
        assert_eq!(findings[0].session, None);
        assert_eq!(findings[0].culprits.len(), 3);
        assert!(findings[0].complete);
        // Running the same audit again does not duplicate the finding.
        kb.audit_consistency(&focus, &opts, 4);
        assert_eq!(kb.audit_findings().len(), 1);
    }

    #[test]
    fn import_with_reported_steps_keeps_the_derivation() {
        let kb = kb(KIF);
        let axioms = vec![
            CitedAxiom {
                file: None,
                line: None,
                kif: "(disjoint Dog Cat)".into(),
            },
            CitedAxiom {
                file: None,
                line: None,
                kif: "(subclass Dog Animal)".into(),
            },
        ];
        let steps = vec![
            ReportedStep {
                rule: "axiom".into(),
                premises: vec![],
                kif: "(disjoint Dog Cat)".into(),
                axiom: Some(0),
            },
            ReportedStep {
                rule: "axiom".into(),
                premises: vec![],
                kif: "(subclass Dog Animal)".into(),
                axiom: Some(1),
            },
            ReportedStep {
                rule: "resolution".into(),
                premises: vec![0, 1],
                kif: "FALSE".into(),
                axiom: None,
            },
        ];
        let finding = kb.import_contradiction(&axioms, &steps, "issue#1").unwrap();
        assert_eq!(finding.steps.len(), 3);
        assert_eq!(finding.steps[2].premises, vec![0, 1]);
        assert!(finding.steps[0].source_sid.is_some());
        assert!(finding.steps[2].source_sid.is_none());
        assert!(finding.complete);
    }
}
