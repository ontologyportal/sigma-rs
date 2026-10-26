//! Recorded contradictions: reading the KB's audit-findings cache as views
//! and importing contradiction reports into it.
//!
//! Three report shapes are accepted, all identifying an axiom by its KIF
//! text plus an optional `file` / `line`:
//!
//! * a version 3 `sigma-proof-report` whose problem is an audit (what the
//!   web app downloads and embeds in `[Contradiction]` GitHub issues);
//! * the SUMO nightly workflow's `latest-contradictions.json`
//!   (`findings[].axioms[]`);
//! * `sumo audit --json` output (`contradictions[].axioms[]`).

#![cfg(any(feature = "external-prover", feature = "native-prover"))]

use std::sync::Arc;

use serde_json::Value;
use sigmakee_rs_core::{
    AuditFinding, CitedAxiom, ImportRejection, KnowledgeBase, ReportedStep, TopLayer,
};

use super::views::{AuditFocusView, ContradictionView};
use super::Session;
use crate::{SdkError, SdkResult};

/// One contradiction as a report states it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReportedContradiction {
    pub axioms: Vec<CitedAxiom>,
    /// The recorded derivation, empty when the report carries none.
    #[serde(default)]
    pub steps: Vec<ReportedStep>,
    /// When the report says the contradiction was first found (unix ms);
    /// the import time otherwise.
    #[serde(default)]
    pub found_at_ms: Option<u64>,
}

/// Parse a contradiction report in any accepted shape.
pub fn parse_contradiction_report(json: &str) -> SdkResult<Vec<ReportedContradiction>> {
    let root: Value =
        serde_json::from_str(json).map_err(|e| SdkError::Report(format!("invalid JSON: {e}")))?;
    let obj = root
        .as_object()
        .ok_or_else(|| SdkError::Report("the report is not a JSON object".into()))?;

    if obj.get("format").and_then(Value::as_str) == Some("sigma-proof-report") {
        let version = obj.get("version").and_then(Value::as_u64).unwrap_or(0);
        if version != 3 {
            return Err(SdkError::Report(format!(
                "unsupported sigma-proof-report version {version}; only version 3 carries cited axioms"
            )));
        }
        let command = obj
            .get("problem")
            .and_then(|p| p.get("command"))
            .and_then(Value::as_str);
        if command != Some("audit") {
            return Err(SdkError::Report(
                "this proof report is not a consistency audit".into(),
            ));
        }
        let axioms = cited_axioms(obj.get("axioms"))?;
        let steps = obj
            .get("result")
            .and_then(|r| r.get("proof"))
            .and_then(Value::as_array)
            .map(|proof| reported_steps(proof, &axioms))
            .unwrap_or_default();
        return Ok(vec![ReportedContradiction {
            axioms,
            steps,
            found_at_ms: None,
        }]);
    }

    if let Some(findings) = obj.get("findings").and_then(Value::as_array) {
        return findings
            .iter()
            .map(|f| {
                Ok(ReportedContradiction {
                    axioms: cited_axioms(f.get("axioms"))?,
                    steps: Vec::new(),
                    found_at_ms: f.get("found_at_ms").and_then(Value::as_u64),
                })
            })
            .collect();
    }

    if let Some(list) = obj.get("contradictions").and_then(Value::as_array) {
        return list
            .iter()
            .map(|c| {
                let axioms = cited_axioms(c.get("axioms"))?;
                let steps = c
                    .get("steps")
                    .and_then(Value::as_array)
                    .map(|proof| reported_steps(proof, &axioms))
                    .unwrap_or_default();
                Ok(ReportedContradiction {
                    axioms,
                    steps,
                    found_at_ms: c.get("found_at_ms").and_then(Value::as_u64),
                })
            })
            .collect();
    }

    Err(SdkError::Report(
        "no contradictions found: expected a sigma-proof-report (version 3, audit), \
         a workflow report with `findings`, or an audit summary with `contradictions`"
            .into(),
    ))
}

fn cited_axioms(value: Option<&Value>) -> SdkResult<Vec<CitedAxiom>> {
    let arr = value
        .and_then(Value::as_array)
        .ok_or_else(|| SdkError::Report("missing `axioms` array".into()))?;
    arr.iter()
        .map(|a| {
            let kif = a
                .get("kif")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| SdkError::Report("an axiom has no `kif` text".into()))?;
            Ok(CitedAxiom {
                file: a.get("file").and_then(Value::as_str).map(str::to_owned),
                line: a
                    .get("line")
                    .and_then(Value::as_u64)
                    .and_then(|l| u32::try_from(l).ok()),
                kif: kif.to_owned(),
            })
        })
        .collect()
}

/// Project a report's proof steps, tying each axiom-role step to the cited
/// axiom it restates (by text, else by file and line).
fn reported_steps(proof: &[Value], axioms: &[CitedAxiom]) -> Vec<ReportedStep> {
    let same = |a: &str, b: &str| a.split_whitespace().eq(b.split_whitespace());
    proof
        .iter()
        .filter_map(|s| {
            let kif = s.get("kif").and_then(Value::as_str)?;
            let file = s.get("file").and_then(Value::as_str);
            let line = s
                .get("line")
                .and_then(Value::as_u64)
                .and_then(|l| u32::try_from(l).ok());
            let premises: Vec<usize> = s
                .get("premises")
                .and_then(Value::as_array)
                .map(|p| {
                    p.iter()
                        .filter_map(Value::as_u64)
                        .filter_map(|i| usize::try_from(i).ok())
                        .collect()
                })
                .unwrap_or_default();
            let axiom = if premises.is_empty() {
                axioms.iter().position(|a| same(&a.kif, kif)).or_else(|| {
                    let (f, l) = (file?, line?);
                    axioms
                        .iter()
                        .position(|a| a.file.as_deref() == Some(f) && a.line == Some(l))
                })
            } else {
                None
            };
            Some(ReportedStep {
                rule: s
                    .get("rule")
                    .and_then(Value::as_str)
                    .unwrap_or("step")
                    .to_owned(),
                premises,
                kif: kif.to_owned(),
                axiom,
            })
        })
        .collect()
}

/// One cited axiom of a recorded finding.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditCulpritView {
    /// The sentence id, 16 hex digits (what `AuditRequest.culprits` takes).
    pub sid: String,
    pub kif: String,
    pub file: Option<String>,
    pub line: Option<u32>,
}

/// A recorded contradiction (see `KnowledgeBase::audit_findings`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditFindingView {
    /// Stable identity: the sorted culprit sids joined by `+`.
    pub key: String,
    pub culprits: Vec<AuditCulpritView>,
    pub complete: bool,
    pub session: Option<String>,
    pub origin: String,
    pub found_at_ms: u64,
    /// The derivation with its graphviz / prose / TPTP renderings.
    pub contradiction: ContradictionView,
}

impl AuditFindingView {
    /// Project `finding` against the KB holding it; `locations` is the KB's
    /// sentence location map, built once for a batch.
    pub fn project<L: TopLayer>(
        kb: &KnowledgeBase<L>,
        finding: &AuditFinding,
        locations: &std::collections::HashMap<sigmakee_rs_core::SentenceId, sigmakee_rs_core::Span>,
    ) -> Self {
        Self {
            key: finding_key(&finding.culprits),
            culprits: finding
                .culprits
                .iter()
                .map(|&sid| {
                    let focus = AuditFocusView::of(kb, sid);
                    AuditCulpritView {
                        sid: format!("{sid:016x}"),
                        kif: focus.kif,
                        file: focus.file,
                        line: focus.line,
                    }
                })
                .collect(),
            complete: finding.complete,
            session: finding.session.clone(),
            origin: finding.origin.clone(),
            found_at_ms: finding.found_at_ms,
            contradiction: ContradictionView::of(
                kb,
                &finding.steps,
                locations,
                &format!("finding-{}", finding_key(&finding.culprits)),
            ),
        }
    }
}

/// The stable identity of a culprit set: sorted sids as hex joined by `+`.
pub fn finding_key(culprits: &[sigmakee_rs_core::SentenceId]) -> String {
    let mut sids = culprits.to_vec();
    sids.sort_unstable();
    sids.dedup();
    sids.iter()
        .map(|s| format!("{s:016x}"))
        .collect::<Vec<_>>()
        .join("+")
}

/// Parse the hex sids of a finding key or an `AuditRequest.culprits` list.
pub fn parse_sids<'a>(
    sids: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<sigmakee_rs_core::SentenceId>, String> {
    sids.into_iter()
        .flat_map(|s| s.split('+'))
        .filter(|s| !s.is_empty())
        .map(|s| {
            u64::from_str_radix(s.trim(), 16).map_err(|_| format!("invalid sentence id `{s}`"))
        })
        .collect()
}

/// A rejected report entry and why.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RejectedContradiction {
    pub axioms: Vec<CitedAxiom>,
    /// The report's derivation, as parsed (empty when it carried none).
    pub steps: Vec<ReportedStep>,
    pub reason: String,
    /// The cited axioms not present in the KB, when that was the reason.
    pub missing: Vec<CitedAxiom>,
}

/// What importing a report did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportOutcome {
    /// Findings recorded (or already present) for the report's entries.
    pub imported: Vec<AuditFindingView>,
    pub rejected: Vec<RejectedContradiction>,
}

impl<L: TopLayer> Session<L> {
    /// Every recorded contradiction, newest first.
    pub fn audit_findings_view(&self) -> Vec<AuditFindingView> {
        let findings = self.kb.audit_findings();
        if findings.is_empty() {
            return Vec::new();
        }
        let locations = sigmakee_rs_core::DiagnosticSource::sentence_locations(&self.kb);
        findings
            .iter()
            .map(|f| AuditFindingView::project(&self.kb, f, &locations))
            .collect()
    }

    /// The recorded finding with `key` (see [`finding_key`]), if any.
    pub fn audit_finding_view(&self, key: &str) -> Result<Option<AuditFindingView>, String> {
        let sids = parse_sids([key])?;
        Ok(self.kb.audit_finding(&sids, None).map(|f| {
            let locations = sigmakee_rs_core::DiagnosticSource::sentence_locations(&self.kb);
            AuditFindingView::project(&self.kb, &f, &locations)
        }))
    }

    /// Import `reports` under `origin`: each entry whose cited axioms are all
    /// present is recorded; the rest are returned with the reason.
    pub fn import_contradictions(
        &self,
        reports: &[ReportedContradiction],
        origin: &str,
    ) -> ImportOutcome {
        let mut imported: Vec<Arc<AuditFinding>> = Vec::new();
        let mut rejected = Vec::new();
        for report in reports {
            match self.kb.import_contradiction_at(
                &report.axioms,
                &report.steps,
                origin,
                report.found_at_ms,
            ) {
                Ok(finding) => imported.push(finding),
                Err(rejection) => rejected.push(RejectedContradiction {
                    axioms: report.axioms.clone(),
                    steps: report.steps.clone(),
                    reason: rejection.to_string(),
                    missing: match rejection {
                        ImportRejection::Missing(missing) => missing,
                        ImportRejection::NoAxioms => Vec::new(),
                    },
                }),
            }
        }
        let locations = if imported.is_empty() {
            Default::default()
        } else {
            sigmakee_rs_core::DiagnosticSource::sentence_locations(&self.kb)
        };
        ImportOutcome {
            imported: imported
                .iter()
                .map(|f| AuditFindingView::project(&self.kb, f, &locations))
                .collect(),
            rejected,
        }
    }

    /// Parse `json` (any accepted report shape) and import it.
    pub fn import_contradiction_report(
        &self,
        json: &str,
        origin: &str,
    ) -> SdkResult<ImportOutcome> {
        let reports = parse_contradiction_report(json)?;
        Ok(self.import_contradictions(&reports, origin))
    }

    /// Forget every recorded contradiction.
    pub fn clear_audit_findings(&self) {
        self.kb.clear_audit_findings();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_3_audit_report() {
        let json = r#"{
          "format": "sigma-proof-report", "version": 3,
          "problem": { "command": "audit", "request": {} },
          "axioms": [
            { "file": "Merge.kif", "line": 10, "kif": "(subclass Dog Animal)" },
            { "file": null, "line": null, "kif": "(disjoint Dog Animal)" }
          ],
          "result": { "status": "Inconsistent", "proof": [
            { "index": 0, "rule": "axiom", "premises": [], "kif": "(subclass Dog Animal)", "tptp": null, "file": "Merge.kif", "line": 10 },
            { "index": 1, "rule": "axiom", "premises": [], "kif": "(disjoint   Dog Animal)", "tptp": null, "file": null, "line": null },
            { "index": 2, "rule": "resolution", "premises": [0, 1], "kif": "FALSE", "tptp": null, "file": null, "line": null }
          ] }
        }"#;
        let reports = parse_contradiction_report(json).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].axioms.len(), 2);
        assert_eq!(reports[0].steps.len(), 3);
        assert_eq!(reports[0].steps[0].axiom, Some(0));
        assert_eq!(reports[0].steps[1].axiom, Some(1));
        assert_eq!(reports[0].steps[2].axiom, None);
        assert_eq!(reports[0].steps[2].premises, vec![0, 1]);
    }

    #[test]
    fn rejects_non_audit_and_old_reports() {
        let prove = r#"{"format":"sigma-proof-report","version":3,"problem":{"command":"prove","query":"x"},"axioms":[]}"#;
        assert!(matches!(
            parse_contradiction_report(prove),
            Err(SdkError::Report(_))
        ));
        let v1 = r#"{"format":"sigma-proof-report","version":1}"#;
        assert!(matches!(
            parse_contradiction_report(v1),
            Err(SdkError::Report(_))
        ));
        assert!(matches!(
            parse_contradiction_report("{}"),
            Err(SdkError::Report(_))
        ));
        assert!(matches!(
            parse_contradiction_report("not json"),
            Err(SdkError::Report(_))
        ));
    }

    #[test]
    fn parses_workflow_and_cli_shapes() {
        let workflow = r#"{"version":1,"findings":[
          {"seed":0,"step":3,"axioms":[{"file":"Merge.kif","line":1,"kif":"(a)"},{"file":null,"line":null,"kif":"(b)"}]},
          {"seed":0,"step":9,"axioms":[{"file":"Mid.kif","line":2,"kif":"(c)"}]}
        ]}"#;
        let reports = parse_contradiction_report(workflow).unwrap();
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].axioms[1].kif, "(b)");
        assert!(reports[1].steps.is_empty());

        let cli = r#"{"inconsistent":true,"contradictions":[{"axioms":[{"file":"t.kif","line":1,"kif":"(a)"}],"steps":4}]}"#;
        let reports = parse_contradiction_report(cli).unwrap();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].steps.is_empty());
    }

    #[test]
    fn finding_keys_round_trip() {
        let key = finding_key(&[0xbeef, 0x1]);
        assert_eq!(key, "0000000000000001+000000000000beef");
        assert_eq!(parse_sids([key.as_str()]).unwrap(), vec![1, 0xbeef]);
        assert!(parse_sids(["zz"]).is_err());
    }

    #[cfg(feature = "native-prover")]
    #[test]
    fn imports_into_a_native_session_and_reads_views() {
        use crate::Source;
        let mut s = Session::<sigmakee_rs_core::ProverLayer>::new("findings-test".into());
        s.ingest(Source::Reader {
            name: "t.kif".into(),
            reader: Box::new(std::io::Cursor::new(Vec::from(
                "(subclass Dog Animal)\n(disjoint Dog Animal)\n",
            ))),
        });
        let json = r#"{"contradictions":[
          {"axioms":[{"file":"t.kif","line":1,"kif":"(subclass Dog Animal)"},{"file":"t.kif","line":2,"kif":"(disjoint Dog Animal)"}]},
          {"axioms":[{"file":"t.kif","line":9,"kif":"(subclass Cat Animal)"}]}
        ]}"#;
        let outcome = s.import_contradiction_report(json, "test").unwrap();
        assert_eq!(outcome.imported.len(), 1, "{outcome:?}");
        assert_eq!(outcome.rejected.len(), 1);
        assert_eq!(outcome.rejected[0].missing.len(), 1);
        let views = s.audit_findings_view();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].culprits.len(), 2);
        assert_eq!(views[0].origin, "test");
        assert_eq!(views[0].culprits[0].file.as_deref(), Some("t.kif"));
        assert_eq!(views[0].contradiction.steps.len(), 3);
        let again = s.audit_finding_view(&views[0].key).unwrap().unwrap();
        assert_eq!(again.key, views[0].key);
        s.clear_audit_findings();
        assert!(s.audit_findings_view().is_empty());
    }
}
