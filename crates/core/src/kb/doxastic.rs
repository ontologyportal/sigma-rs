//! Belief contexts on `KnowledgeBase`: proving inside an agent's
//! propositional-attitude context (`believes`, `knows`, ...).
//!
//! Lever 2 of `docs/plans/doxastic-contexts.md` (contexts-as-sessions): full
//! consequence closure inside a context via a projected prover run -- the
//! Konolige deduction model. The semantic layer decides what an attitude is
//! (an instance of `PropositionalAttitude`) and collects an agent's asserted
//! contents (`SemanticLayer::attitude_facts_scoped`); exact `P` / `(not P)`
//! pairs are reported by the `attitude-conflict` validator. The projection
//! driver lives in `prover/saturate/doxastic.rs`.
//!
//! GUARDRAIL: these are `&self` queries -- the projection never asserts
//! anything. No `believes(agent, X)` is ever fed back from an inner
//! derivation; verdicts and proofs return to the caller only.

use super::KnowledgeBase;
use crate::layer::TopLayer;
use crate::prover::{Conjecture, ProverResult, ProverStatus};
use crate::semantics::consts::BELIEF_RELATION;
use crate::semantics::types::Scope;
use crate::types::SentenceId;

impl<S: TopLayer + 'static> KnowledgeBase<crate::prover::saturate::ProverLayer<S>> {
    /// Prove `query_kif` inside `agent`'s `attitude` context (`None` means
    /// `believes`): the agent's asserted contents under that attitude become
    /// the inner problem's axioms, the query its conjecture.
    ///
    /// `Proved` -- the contents entail the query under the full calculus.
    /// `Disproved` (saturation) -- the inner CounterSatisfiable analogue.
    /// `Inconsistent` -- the contents themselves derive a contradiction.
    /// `Unknown` / `Timeout` -- budget. `InputError` -- `attitude` is not a
    /// propositional attitude, or the query does not parse.
    pub fn doxastic_ask(
        &self,
        attitude: Option<&str>,
        agent: &str,
        query_kif: &str,
        opts: crate::NativeOpts,
    ) -> ProverResult {
        let contents = match self.attitude_contents(attitude, agent) {
            Ok(contents) => contents,
            Err(r) => return *r,
        };
        let asts = match Conjecture::parse(query_kif, crate::Parser::Kif { options: None }) {
            Ok(asts) => asts,
            Err(r) => return *r,
        };
        self.layer
            .doxastic_project(&contents, Some(asts), opts, &self.prove_ctx())
    }

    /// Is `agent`'s `attitude` context (`None` means `believes`) consistent
    /// under full consequence closure? Saturates the projected contents with
    /// no conjecture: `Consistent` / `Inconsistent` (cited transcripts in
    /// `contradiction_proofs`, the first also in `proof_kif`) / `Unknown` /
    /// `Timeout`, or `InputError` when `attitude` is not a propositional
    /// attitude. An empty context is trivially `Consistent`.
    pub fn doxastic_consistent(
        &self,
        attitude: Option<&str>,
        agent: &str,
        opts: crate::NativeOpts,
    ) -> ProverResult {
        let contents = match self.attitude_contents(attitude, agent) {
            Ok(contents) => contents,
            Err(r) => return *r,
        };
        self.layer
            .doxastic_project(&contents, None, opts, &self.prove_ctx())
    }

    /// The asserted content sentences of `agent`'s `attitude` context, or
    /// the `InputError` result when `attitude` is not a propositional
    /// attitude. An unknown agent has an empty context.
    fn attitude_contents(
        &self,
        attitude: Option<&str>,
        agent: &str,
    ) -> Result<Vec<SentenceId>, Box<ProverResult>> {
        let sem = self.layer.semantic();
        let attitude_id = match attitude {
            Some(name) => self.symbol_id(name),
            None => Some(BELIEF_RELATION.id()),
        };
        let Some(attitude_id) = attitude_id.filter(|&id| sem.is_attitude_scoped(id, Scope::Base))
        else {
            let name = attitude.map_or_else(|| BELIEF_RELATION.name().to_string(), str::to_string);
            return Err(Box::new(ProverResult {
                status: ProverStatus::InputError,
                raw_output: format!(
                    "'{name}' is not a propositional attitude in this KB \
                     (declare it with `(instance {name} PropositionalAttitude)`)"
                ),
                ..Default::default()
            }));
        };
        let Some(agent_id) = self.symbol_id(agent) else {
            return Ok(Vec::new());
        };
        let mut contents: Vec<SentenceId> = sem
            .attitude_facts_scoped(attitude_id, agent_id, Scope::Base)
            .into_iter()
            .map(|(content, _)| content)
            .collect();
        contents.dedup();
        Ok(contents)
    }
}

#[cfg(all(test, feature = "native-prover"))]
mod projection_tests {
    use std::collections::HashSet;

    use super::KnowledgeBase;
    use crate::prover::{ProverLayer, ProverStatus, TerminationReason};
    use crate::types::SentenceId;
    use crate::{NativeOpts, SineParams};

    /// The spec's probe KB.
    const PROBE_KB: &str = "\
        (domain believes 2 Formula)\n\
        (believes John (bald Socrates))\n\
        (believes John (=> (bald Socrates) (old Socrates)))\n\
        (believes Mary (not (bald Socrates)))";

    /// A native KB over `kif`, with `believes` declared a propositional
    /// attitude as SUMO's Merge.kif does.
    fn kb_native(kif: &str) -> KnowledgeBase<ProverLayer> {
        kb_native_raw(&format!("(instance believes PropositionalAttitude)\n{kif}"))
    }

    fn kb_native_raw(kif: &str) -> KnowledgeBase<ProverLayer> {
        let mut kb = KnowledgeBase::new_native();
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("test.kif"), "test.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("test.kif").expect("promote");
        kb
    }

    fn fast() -> NativeOpts {
        NativeOpts {
            time_limit_secs: 10,
            ..Default::default()
        }
    }

    fn kif_of(kb: &KnowledgeBase<ProverLayer>, sid: SentenceId) -> String {
        crate::syntactic::display::sentence_to_plain_kif(sid, kb.store_for_testing())
    }

    #[test]
    fn harvest_collects_asserted_contents_only() {
        let kb = kb_native(PROBE_KB);
        let contents: Vec<String> = kb
            .attitude_contents(None, "John")
            .unwrap()
            .into_iter()
            .map(|s| kif_of(&kb, s))
            .collect();
        assert_eq!(contents.len(), 2, "got {contents:?}");
        assert!(contents.contains(&"(bald Socrates)".to_string()));
        assert!(contents.contains(&"(=> (bald Socrates) (old Socrates))".to_string()));
        assert_eq!(kb.attitude_contents(None, "Mary").unwrap().len(), 1);
        assert!(kb
            .attitude_contents(None, "NoSuchAgent")
            .unwrap()
            .is_empty());

        // A rule antecedent `(believes John …)` is not an asserted
        // belief — same exclusion the conflict lint applies.
        let kb2 = kb_native(
            "(=> (believes John (bald Socrates)) (confused John))\n\
             (believes John (wise Plato))",
        );
        let harvested: Vec<String> = kb2
            .attitude_contents(None, "John")
            .unwrap()
            .into_iter()
            .map(|s| kif_of(&kb2, s))
            .collect();
        assert_eq!(harvested, vec!["(wise Plato)".to_string()]);
    }

    /// THE decisive probe pair: modus ponens closes inside the projected
    /// context, while the outer ask for `(believes John Q)` stays
    /// unproven — the outer calculus has only the K-distribution
    /// schemata over quote constructors (conjunction rearrangement), so
    /// a quoted `impl_q` is inert there.  Level separation holds: the
    /// inner verdict is returned to the caller, never asserted.
    #[test]
    fn modus_ponens_closes_inside_context_but_not_outside() {
        let kb = kb_native(PROBE_KB);
        let roots_before = kb.store_for_testing().root_sids().len();

        let inner = kb.doxastic_ask(None, "John", "(old Socrates)", fast());
        assert_eq!(
            inner.status,
            ProverStatus::Proved,
            "modus ponens inside the context: {}",
            inner.raw_output
        );

        // GUARDRAIL: the projection asserted nothing.
        assert_eq!(
            kb.store_for_testing().root_sids().len(),
            roots_before,
            "projection must not mutate the store"
        );

        // Outer control: `(believes John (old Socrates))` is NOT
        // derivable outside the context.
        let outer = kb.ask_query(
            "(believes John (old Socrates))",
            None,
            SineParams::default(),
            fast(),
        );
        assert!(
            !matches!(
                outer.status,
                ProverStatus::Proved | ProverStatus::Inconsistent
            ),
            "outer ask must stay unproven (got {:?}): {}",
            outer.status,
            outer.raw_output
        );
    }

    /// Closure genuinely beyond K: quantifier instantiation inside the
    /// context — no quote-level schema can instantiate under a
    /// `forall_q`.
    #[test]
    fn quantified_closure_beyond_k() {
        let kb = kb_native(
            "(domain believes 2 Formula)\n\
             (believes John (forall (?X) (=> (man ?X) (mortal ?X))))\n\
             (believes John (man Socrates))",
        );
        let inner = kb.doxastic_ask(None, "John", "(mortal Socrates)", fast());
        assert_eq!(inner.status, ProverStatus::Proved, "{}", inner.raw_output);

        let outer = kb.ask_query(
            "(believes John (mortal Socrates))",
            None,
            SineParams::default(),
            fast(),
        );
        assert!(
            !matches!(
                outer.status,
                ProverStatus::Proved | ProverStatus::Inconsistent
            ),
            "outer control (got {:?}): {}",
            outer.status,
            outer.raw_output
        );
    }

    #[test]
    fn negated_content_is_not_proved_inside() {
        let kb = kb_native(PROBE_KB);
        let r = kb.doxastic_ask(None, "John", "(not (bald Socrates))", fast());
        assert_eq!(
            r.status,
            ProverStatus::Disproved,
            "John's context saturates without ¬P: {}",
            r.raw_output
        );
    }

    #[test]
    fn consistency_flags_contradiction_with_cited_contents() {
        // Baseline: the probe KB's John is consistent.
        let kb = kb_native(PROBE_KB);
        let r = kb.doxastic_consistent(None, "John", fast());
        assert_eq!(r.status, ProverStatus::Consistent, "{}", r.raw_output);

        // Add `believes John (not Q)`: {P, P⇒Q, ¬Q} is inconsistent
        // under consequence — invisible to the syntactic lint, found by
        // the projection, with a transcript citing the three contents.
        let kb = kb_native(&format!("{PROBE_KB}\n(believes John (not (old Socrates)))"));
        assert!(
            !kb.validate(crate::ValidationTarget::All, None)
                .iter()
                .any(|d| d.code == "attitude-conflict"),
            "no syntactic P / (not P) pair -- this contradiction needs deduction"
        );
        let r = kb.doxastic_consistent(None, "John", fast());
        assert_eq!(r.status, ProverStatus::Inconsistent, "{}", r.raw_output);
        assert!(!r.proof_kif.is_empty(), "cited transcript expected");
        let cited: HashSet<String> = r
            .proof_kif
            .iter()
            .filter_map(|s| s.source_sid)
            .map(|sid| kif_of(&kb, sid))
            .collect();
        for content in [
            "(bald Socrates)",
            "(=> (bald Socrates) (old Socrates))",
            "(not (old Socrates))",
        ] {
            assert!(
                cited.contains(content),
                "proof must cite {content:?}, cited: {cited:?}"
            );
        }
    }

    #[test]
    fn agents_are_isolated() {
        let kb = kb_native(PROBE_KB);
        // Mary's ¬P never meets John's P: both contexts are consistent…
        let r = kb.doxastic_consistent(None, "Mary", fast());
        assert_eq!(r.status, ProverStatus::Consistent, "{}", r.raw_output);
        // …and Mary's context neither contains nor derives John's P.
        let r = kb.doxastic_ask(None, "Mary", "(bald Socrates)", fast());
        assert_eq!(r.status, ProverStatus::Disproved, "{}", r.raw_output);
        // Her own belief discharges directly.
        let r = kb.doxastic_ask(None, "Mary", "(not (bald Socrates))", fast());
        assert_eq!(r.status, ProverStatus::Proved, "{}", r.raw_output);
    }

    #[test]
    fn nested_belief_stays_quoted_one_level() {
        let kb = kb_native("(believes John (believes Mary (not (bald Socrates))))");
        // John's projection holds `believes(Mary, ¬P)` as an inner FACT
        // (the content quotes one level down at inner clausification).
        let r = kb.doxastic_ask(
            None,
            "John",
            "(believes Mary (not (bald Socrates)))",
            fast(),
        );
        assert_eq!(r.status, ProverStatus::Proved, "{}", r.raw_output);
        // The nested content is never unquoted to John's assertion level.
        let r = kb.doxastic_ask(None, "John", "(not (bald Socrates))", fast());
        assert_eq!(
            r.status,
            ProverStatus::Disproved,
            "nested quote must stay opaque: {}",
            r.raw_output
        );
        // No recursion into Mary (phase 1): the nested belief is not an
        // ASSERTED root of Mary's, so her context is empty.
        assert!(kb.attitude_contents(None, "Mary").unwrap().is_empty());
        let r = kb.doxastic_consistent(None, "Mary", fast());
        assert_eq!(r.status, ProverStatus::Consistent, "{}", r.raw_output);
    }

    #[test]
    fn empty_belief_set_is_trivially_consistent() {
        let kb = kb_native("(instance John Human)");
        let r = kb.doxastic_consistent(None, "John", fast());
        assert_eq!(r.status, ProverStatus::Consistent, "{}", r.raw_output);
        let r = kb.doxastic_consistent(None, "NoSuchAgent", fast());
        assert_eq!(r.status, ProverStatus::Consistent, "{}", r.raw_output);
        // Nothing believed ⇒ nothing entailed.
        let r = kb.doxastic_ask(None, "John", "(bald Socrates)", fast());
        assert_eq!(r.status, ProverStatus::Disproved, "{}", r.raw_output);
    }

    #[test]
    fn tiny_budget_returns_unknown_without_hanging() {
        let kb = kb_native(PROBE_KB);
        let opts = NativeOpts {
            max_steps: 0,
            forward_close: false,
            time_limit_secs: 5,
            ..Default::default()
        };
        let r = kb.doxastic_ask(None, "John", "(old Socrates)", opts);
        assert_eq!(r.status, ProverStatus::Unknown, "{}", r.raw_output);
        assert_eq!(r.termination, Some(TerminationReason::GaveUp));
    }

    #[test]
    fn malformed_query_is_an_input_error() {
        let kb = kb_native(PROBE_KB);
        let r = kb.doxastic_ask(None, "John", "(((", fast());
        assert_eq!(r.status, ProverStatus::InputError);
    }

    #[test]
    fn an_undeclared_or_non_attitude_relation_is_an_input_error() {
        let kb = kb_native(PROBE_KB);
        let r = kb.doxastic_consistent(Some("subclass"), "John", fast());
        assert_eq!(r.status, ProverStatus::InputError, "{}", r.raw_output);
        let r = kb.doxastic_ask(Some("noSuchRelation"), "John", "(bald Socrates)", fast());
        assert_eq!(r.status, ProverStatus::InputError, "{}", r.raw_output);

        // Without the KB declaring it, even the default `believes` is not an
        // attitude.
        let bare = kb_native_raw("(believes John (bald Socrates))");
        let r = bare.doxastic_ask(None, "John", "(bald Socrates)", fast());
        assert_eq!(r.status, ProverStatus::InputError, "{}", r.raw_output);
    }

    #[test]
    fn a_named_attitude_selects_its_own_context() {
        let kb = kb_native(
            "(instance knows PropositionalAttitude)\n\
             (knows Ann (wise Plato))\n\
             (believes Ann (calm Sea))",
        );
        let r = kb.doxastic_ask(Some("knows"), "Ann", "(wise Plato)", fast());
        assert_eq!(r.status, ProverStatus::Proved, "{}", r.raw_output);
        let r = kb.doxastic_ask(Some("knows"), "Ann", "(calm Sea)", fast());
        assert_eq!(r.status, ProverStatus::Disproved, "{}", r.raw_output);
    }
}
