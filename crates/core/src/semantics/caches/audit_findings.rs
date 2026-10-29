//! `semantic::audit_findings` -- every contradiction a consistency audit has
//! found (or that was imported from a report), kept across runs and
//! backends.
//!
//! A refutation stays valid as long as every axiom it cites still exists:
//! FOL is monotone, so adding axioms cannot undo it, and removing or editing
//! an axiom it does not cite cannot affect it.  `SentenceId` is a content
//! hash, so editing an axiom is a `RootRemoved` for its old id.  Findings are
//! therefore keyed by their sorted, deduped cited sids and evicted exactly
//! when one of those sids is removed; a finding citing session assertions
//! is additionally dropped when that session is retracted.
//!
//! Populated by `ContradictionFound` events seeded from `kb/prove.rs` (the
//! only place that sees every backend's output), never computed on miss.

use std::collections::HashSet;
use std::sync::Arc;

use dashmap::DashMap;

use crate::cache::events::{Event, EventKind};
use crate::cache::{EagerMapBehavior, EntryCache};
use crate::prover::proof::KifProofStep;
use crate::semantics::types::{Scope, Scoped};
use crate::semantics::SemanticLayer;
use crate::syntactic::caches::session::session_id;
use crate::SentenceId;

/// Represents a single contradiction: the axioms it cites and the derivation
/// that refutes them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuditFinding {
    /// The cache key: Sorted, deduped source sids of every cited axiom
    pub culprits: Vec<SentenceId>,
    /// The proof which proves the contradiction
    pub steps: Vec<KifProofStep>,
    /// `true` when every leaf step cites a source sid, so the culprit set is
    /// the proof's whole axiom support.
    pub complete: bool,
    /// The session whose assertions the proof reasoned in, if any.
    pub session: Option<String>,
    /// Where the finding came from: a prover label (`"native"`, `"vampire"`,
    /// `"eprover"`) or an import label chosen by the caller.
    pub origin: String,
    /// Timestamp (Unix milliseconds) when the finding was recorded.
    pub found_at_ms: u64,
}

impl AuditFinding {
    /// The cache key: The source sids for the contradiction. It is scoped to
    /// a given session
    pub(crate) fn key(&self) -> Scoped<Vec<SentenceId>> {
        Scoped {
            scope: self
                .session
                .as_deref()
                .map_or(Scope::Base, |s| Scope::Session(session_id(s))),
            key: self.culprits.clone(),
        }
    }

    /// Build a finding from a proof transcript (output from an audit call).
    /// The `source_sid`s are the culprits, and the proof is complete when every
    /// premise-free step that is not the (negated) conjecture cites one.
    pub(crate) fn from_steps(
        steps: Vec<KifProofStep>,
        session: Option<String>,
        origin: String,
        found_at_ms: u64,
    ) -> Option<Self> {
        let mut culprits: Vec<SentenceId> = steps.iter().filter_map(|s| s.source_sid).collect();
        culprits.sort_unstable();
        culprits.dedup();
        if culprits.is_empty() {
            return None;
        }
        let complete = steps.iter().all(|s| {
            !s.premises.is_empty()
                || s.source_sid.is_some()
                || s.rule.eq_ignore_ascii_case("negated_conjecture")
                || s.rule.eq_ignore_ascii_case("conjecture")
        });
        Some(Self {
            culprits,
            steps,
            complete,
            session,
            origin,
            found_at_ms,
        })
    }
}

/// Reverse index: cited sid -> keys of the findings citing it, so a
/// `RootRemoved` evicts in O(cited).
#[derive(Debug, Default)]
pub(crate) struct AuditFindingsSide {
    pub(crate) by_sid: DashMap<SentenceId, HashSet<Scoped<Vec<SentenceId>>>>,
}

/// Behavior for the `semantic::audit_findings` eager keyed index.
#[derive(Debug, Default)]
pub(crate) struct AuditFindings;

impl EagerMapBehavior for AuditFindings {
    type Parent = SemanticLayer;
    type Key = Scoped<Vec<SentenceId>>;
    type Value = Arc<AuditFinding>;
    type Side = AuditFindingsSide;
    type SideSnapshot = Vec<(SentenceId, Vec<Scoped<Vec<SentenceId>>>)>;

    const NAME: &'static str = "semantic::audit_findings";

    fn consumes(&self) -> &'static [EventKind] {
        &[
            EventKind::ContradictionFound,
            EventKind::RootRemoved,
            EventKind::SessionRetracted,
        ]
    }

    fn react(
        &self,
        _parent: &SemanticLayer,
        events: &[&Event],
        store: &EntryCache<Self::Key, Self::Value>,
        side: &AuditFindingsSide,
    ) -> Vec<Event> {
        for event in events {
            match event {
                Event::ContradictionFound { finding } => {
                    let key = finding.key();
                    for &sid in &finding.culprits {
                        side.by_sid.entry(sid).or_default().insert(key.clone());
                    }
                    store.update(key, Arc::clone(finding));
                }
                // When a given sentence is removed, evict any contradiction which was cause by it
                Event::RootRemoved { sid, .. } => {
                    let Some((_, keys)) = side.by_sid.remove(sid) else {
                        continue;
                    };
                    let keys: Vec<_> = keys.into_iter().collect();
                    for key in &keys {
                        for culprit in &key.key {
                            if culprit == sid {
                                continue;
                            }
                            if let Some(mut set) = side.by_sid.get_mut(culprit) {
                                set.remove(key);
                            }
                        }
                    }
                    store.evict_keys(&keys);
                }
                // If a session is removed, also remove any contradictions tied to it
                Event::SessionRetracted { session } => {
                    let scope = Scope::Session(session_id(session));
                    let mut doomed = Vec::new();
                    store.for_each(|(key, _)| {
                        if key.scope == scope {
                            doomed.push(key.clone());
                        }
                    });
                    for key in &doomed {
                        for culprit in &key.key {
                            if let Some(mut set) = side.by_sid.get_mut(culprit) {
                                set.remove(key);
                            }
                        }
                    }
                    store.evict_keys(&doomed);
                }
                _ => {}
            }
        }
        Vec::new()
    }

    fn snapshot_side(&self, side: &AuditFindingsSide) -> Self::SideSnapshot {
        side.by_sid
            .iter()
            .map(|e| (*e.key(), e.value().iter().cloned().collect()))
            .collect()
    }

    fn restore_side(&self, side: &AuditFindingsSide, snap: Self::SideSnapshot) {
        for (sid, keys) in snap {
            side.by_sid.entry(sid).or_default().extend(keys);
        }
    }
}

impl SemanticLayer {
    /// Every recorded finding, newest first.
    pub(crate) fn audit_findings(&self) -> Vec<Arc<AuditFinding>> {
        let mut out = Vec::with_capacity(self.audit_findings.len());
        self.audit_findings
            .for_each(|(_, v)| out.push(Arc::clone(v)));
        out.sort_by(|a, b| {
            b.found_at_ms
                .cmp(&a.found_at_ms)
                .then_with(|| a.culprits.cmp(&b.culprits))
        });
        out
    }

    /// The finding keyed by `culprits` (sorted, deduped) in `session`'s
    /// scope, if recorded.
    pub(crate) fn audit_finding(
        &self,
        culprits: &[SentenceId],
        session: Option<&str>,
    ) -> Option<Arc<AuditFinding>> {
        self.audit_findings.get(&Scoped {
            scope: session.map_or(Scope::Base, |s| Scope::Session(session_id(s))),
            key: culprits.to_vec(),
        })
    }

    /// Drop every recorded finding.
    pub(crate) fn clear_audit_findings(&self) {
        self.audit_findings.clear();
        self.audit_findings.side().by_sid.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheConfig;
    use crate::layer::Layer;
    use crate::parse::ast::AstNode;
    use crate::parse::Span;
    use crate::syntactic::SyntacticLayer;

    fn layer(kif: &str) -> SemanticLayer {
        let mut store = SyntacticLayer::default();
        store.load_kif(kif, "t");
        SemanticLayer::with_config(store, &CacheConfig::default())
    }

    fn axiom_step(index: usize, sid: SentenceId) -> KifProofStep {
        KifProofStep {
            index,
            rule: "axiom".into(),
            premises: Vec::new(),
            formula: AstNode::Symbol {
                name: format!("a{index}"),
                span: Span::synthetic(),
            },
            source_sid: Some(sid),
        }
    }

    fn false_step(index: usize, premises: Vec<usize>) -> KifProofStep {
        KifProofStep {
            index,
            rule: "resolution".into(),
            premises,
            formula: AstNode::Symbol {
                name: "FALSE".into(),
                span: Span::synthetic(),
            },
            source_sid: None,
        }
    }

    fn record(layer: &SemanticLayer, sids: &[SentenceId], session: Option<&str>) -> AuditFinding {
        let mut steps: Vec<KifProofStep> = sids
            .iter()
            .enumerate()
            .map(|(i, &sid)| axiom_step(i, sid))
            .collect();
        steps.push(false_step(steps.len(), (0..sids.len()).collect()));
        let finding =
            AuditFinding::from_steps(steps, session.map(str::to_string), "test".into(), 7).unwrap();
        let outcome = layer.cascade(vec![Event::ContradictionFound {
            finding: Arc::new(finding.clone()),
        }]);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        finding
    }

    #[test]
    fn insert_on_event_and_read_back() {
        let layer = layer("(subclass Foo Entity) (subclass Bar Entity) (subclass Baz Entity)");
        let foo = layer.syntactic.by_head("subclass")[0];
        let bar = layer.syntactic.by_head("subclass")[1];
        let finding = record(&layer, &[bar, foo], None);
        assert_eq!(finding.culprits, {
            let mut v = vec![foo, bar];
            v.sort_unstable();
            v
        });
        assert!(finding.complete);
        let all = layer.audit_findings();
        assert_eq!(all.len(), 1);
        assert_eq!(*all[0], finding);
        assert!(layer.audit_finding(&finding.culprits, None).is_some());
    }

    #[test]
    fn evicts_when_a_cited_axiom_is_removed_but_not_otherwise() {
        let layer = layer("(subclass Foo Entity) (subclass Bar Entity) (subclass Baz Entity)");
        let sids = layer.syntactic.by_head("subclass");
        let (foo, bar, baz) = (sids[0], sids[1], sids[2]);
        record(&layer, &[foo, bar], None);
        record(&layer, &[bar, baz], None);
        assert_eq!(layer.audit_findings().len(), 2);

        // Removing an uncited root leaves both findings.
        let _ = layer.cascade(vec![Event::RootRemoved {
            sid: 0xdead_beef,
            sentences: Vec::new(),
        }]);
        assert_eq!(layer.audit_findings().len(), 2);

        // Removing `bar` evicts both; `foo` and `baz` entries are cleaned up.
        let _ = layer.cascade(vec![Event::RootRemoved {
            sid: bar,
            sentences: Vec::new(),
        }]);
        assert!(layer.audit_findings().is_empty());
        let side = layer.audit_findings.side();
        assert!(side.by_sid.get(&foo).map_or(true, |s| s.is_empty()));
        assert!(side.by_sid.get(&baz).map_or(true, |s| s.is_empty()));
        assert!(side.by_sid.get(&bar).is_none());
    }

    #[test]
    fn survives_unchanged_reingest_of_cited_file() {
        let kif = "(subclass Foo Entity) (subclass Bar Entity)";
        let mut store = SyntacticLayer::default();
        store.load_kif(kif, "t");
        let mut layer = SemanticLayer::with_config(store, &CacheConfig::default());
        let sids = layer.syntactic.by_head("subclass");
        record(&layer, &sids, None);
        layer.syntactic.load_kif(kif, "t");
        assert_eq!(layer.audit_findings().len(), 1);
    }

    #[test]
    fn session_retraction_drops_session_findings_only() {
        let layer = layer("(subclass Foo Entity) (subclass Bar Entity)");
        let sids = layer.syntactic.by_head("subclass");
        record(&layer, &sids, Some("hyp"));
        record(&layer, &sids, None);
        assert_eq!(layer.audit_findings().len(), 2);
        let _ = layer.cascade(vec![Event::SessionRetracted {
            session: "hyp".into(),
        }]);
        let left = layer.audit_findings();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].session, None);
    }

    #[test]
    fn incomplete_provenance_is_flagged() {
        let layer = layer("(subclass Foo Entity)");
        let foo = layer.syntactic.by_head("subclass")[0];
        let mut leaf = axiom_step(1, foo);
        leaf.source_sid = None;
        let steps = vec![axiom_step(0, foo), leaf, false_step(2, vec![0, 1])];
        let finding = AuditFinding::from_steps(steps, None, "test".into(), 1).unwrap();
        assert!(!finding.complete);
        assert_eq!(finding.culprits, vec![foo]);
    }

    #[test]
    fn side_snapshot_round_trips() {
        let layer = layer("(subclass Foo Entity) (subclass Bar Entity)");
        let sids = layer.syntactic.by_head("subclass");
        let finding = record(&layer, &sids, None);
        let snap = layer.audit_findings.snapshot_side();
        let map = layer.audit_findings.snapshot();
        let fresh = SemanticLayer::with_config(SyntacticLayer::default(), &CacheConfig::default());
        fresh.audit_findings.restore(map);
        fresh.audit_findings.restore_side(snap);
        assert_eq!(*fresh.audit_findings()[0], finding);
        let _ = fresh.cascade(vec![Event::RootRemoved {
            sid: sids[0],
            sentences: Vec::new(),
        }]);
        assert!(fresh.audit_findings().is_empty());
    }
}
