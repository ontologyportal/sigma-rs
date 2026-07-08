//! Doxastic lints: syntactic belief-contradiction probes over the
//! sentence store.
//!
//! Lever 1 of `docs/plans/doxastic-contexts.md`.  Content addressing
//! makes "is the exact negation of this belief also believed?" an O(1)
//! hash probe per belief: the store id of a sentence IS its content
//! hash, so the id of `(not P)` is computable from `P`'s id without the
//! store round-trip.
//!
//! This is deliberately a LINT, not a KB contradiction: `believes(a, P)
//! ∧ believes(a, (not P))` keeps the KB satisfiable (agents may be
//! inconsistent).  It flags exact syntactic `P` / `(not P)` pairs only;
//! contradictions under logical consequence are the contexts-as-sessions
//! phase's job (plan phase 2).

use std::collections::HashSet;

use super::KnowledgeBase;
use crate::parse::OpKind;
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, Sentence, SentenceId};

/// The store id the ingest-NORMALIZED negation of stored sentence `sid`
/// would have.  Ingest keeps every stored formula negation-normalized
/// (parse/macros/caf.rs `push_negation_inward`: `not` over `and`/`or`
/// pushed by De Morgan, double negations cancelled), so the negation of
/// a stored content must be computed in that same normal form for the
/// content-hash probe to find it:
///
///   * ¬(not X)    = X
///   * ¬(and A B…) = (or ¬A ¬B…)   (children negated recursively)
///   * ¬(or A B…)  = (and ¬A ¬B…)
///   * ¬other      = (not other)   (atoms, `=>`, `<=>`, quantifiers —
///     the fragments ingest leaves un-pushed)
///
/// Hash-only: nothing is interned or stored.  `None` for unresolvable
/// ids or element shapes ingest cannot produce in formula position.
fn normalized_negation_id(syn: &SyntacticLayer, sid: SentenceId) -> Option<SentenceId> {
    let wrap = |el: Element| -> SentenceId {
        Sentence {
            parent:   Vec::new(),
            elements: [Element::Op(OpKind::Not), el].into_iter().collect(),
        }
        .hash()
    };
    let s = syn.sentence(sid)?;
    match s.elements.first() {
        Some(Element::Op(OpKind::Not)) if s.elements.len() == 2 => match &s.elements[1] {
            Element::Sub(inner) => Some(*inner),
            // `(not <bare atom>)` — its negation is the bare element,
            // which is not a sentence; nothing believable matches.
            _ => None,
        },
        Some(Element::Op(op @ (OpKind::And | OpKind::Or))) => {
            let dual = if *op == OpKind::And { OpKind::Or } else { OpKind::And };
            let mut elements = Vec::with_capacity(s.elements.len());
            elements.push(Element::Op(dual));
            for el in s.elements.iter().skip(1) {
                elements.push(match el {
                    Element::Sub(c) => Element::Sub(normalized_negation_id(syn, *c)?),
                    // A bare propositional atom child: ingest negates it
                    // as the sub-sentence `(not <atom>)`.
                    Element::Symbol(_) | Element::Variable { .. } =>
                        Element::Sub(wrap(el.clone())),
                    _ => return None,
                });
            }
            Some(Sentence { parent: Vec::new(), elements: elements.into_iter().collect() }.hash())
        }
        _ => Some(wrap(Element::Sub(sid))),
    }
}

impl<L: crate::layer::TopLayer> KnowledgeBase<L> {
    /// The agent's ASSERTED believed-content sentence ids via `believes`
    /// — see [`doxastic_contents_via`](Self::doxastic_contents_via).
    pub fn doxastic_contents(&self, agent: &str) -> Vec<SentenceId> {
        self.doxastic_contents_via("believes", agent)
    }

    /// Harvest the agent's belief base: the content sentence (third
    /// element) of every ASSERTED top-level `(attitude agent <content>)`
    /// root, by `by_head` — a rule antecedent `(believes ?A ?P)` is not
    /// an asserted root, so it never feeds the set (same exclusion as
    /// the conflict lint, which shares this harvest).  Only compound
    /// contents (`Element::Sub`) participate: they are real store
    /// sub-sentences the projection can clausify; a bare-symbol content
    /// has no store-resident structure.  Sorted + deduped (ids are
    /// content hashes), so downstream consumers are deterministic.
    ///
    /// Phase-1 scope: asserted beliefs only.  DERIVED beliefs (rule
    /// heads concluding `(believes a P)` the outer prover could reach)
    /// are a documented extension — see the design note in
    /// `prover/saturate/doxastic.rs`.
    pub fn doxastic_contents_via(&self, attitude: &str, agent: &str) -> Vec<SentenceId> {
        let syn = self.syntactic();
        let Some(agent_sym) = self.symbol_id(agent) else { return Vec::new() };
        let mut out: Vec<SentenceId> = Vec::new();
        for root in syn.by_head(attitude) {
            let Some(sent) = syn.sentence(root) else { continue };
            if sent.elements.len() != 3 { continue; }
            let Element::Symbol(a) = &sent.elements[1] else { continue };
            if a.id() != agent_sym { continue; }
            if let Element::Sub(content) = &sent.elements[2] {
                out.push(*content);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Syntactic belief-contradiction pairs for `agent`: every stored
    /// `(believes agent P)` whose exact negation is also believed —
    /// "exact" in the store's ingest normal form (see
    /// [`normalized_negation_id`]), so `(not (and …))` beliefs, stored
    /// as their De Morgan dual, are still found.
    ///
    /// Returns `(P, ¬P)` content-sentence id pairs, the plainly-negated
    /// side second when one side is `(not …)`-headed, deduplicated as
    /// unordered pairs, deterministically ordered (ids are content
    /// hashes).  Empty when the agent, the attitude relation, or any
    /// conflict is absent.  Only asserted top-level facts count — a rule
    /// antecedent `(believes ?A ?P)` is not a belief.
    pub fn doxastic_conflicts(&self, agent: &str) -> Vec<(SentenceId, SentenceId)> {
        self.doxastic_conflicts_via("believes", agent)
    }

    /// [`doxastic_conflicts`](Self::doxastic_conflicts) generalized over
    /// the attitude relation (`knows`, `desires`, …) — same contract.
    pub fn doxastic_conflicts_via(
        &self,
        attitude: &str,
        agent:    &str,
    ) -> Vec<(SentenceId, SentenceId)> {
        let syn = self.syntactic();

        // The agent's believed CONTENT sentences (shared harvest — the
        // contexts-as-sessions projection collects the same set).  Only
        // compound contents (`Element::Sub`) participate — a bare-symbol
        // content has no store-resident negation to probe for.
        let ids: Vec<SentenceId> = self.doxastic_contents_via(attitude, agent);
        let believed: HashSet<SentenceId> = ids.iter().copied().collect();

        // For each belief, probe the same agent's set for its
        // negation-normalized complement (O(1) per belief after the
        // complement hash is computed).
        let is_not_headed = |sid: SentenceId| -> bool {
            syn.sentence(sid).is_some_and(
                |s| matches!(s.elements.first(), Some(Element::Op(OpKind::Not))))
        };
        let mut seen: HashSet<(SentenceId, SentenceId)> = HashSet::new();
        let mut out: Vec<(SentenceId, SentenceId)> = Vec::new();
        for sid in ids {
            let Some(neg) = normalized_negation_id(syn, sid) else { continue };
            if neg == sid || !believed.contains(&neg) { continue; }
            if !seen.insert((sid.min(neg), sid.max(neg))) { continue; }
            // Orientation: the plainly-negated side second when
            // distinguishable, else the deterministic id order.
            match (is_not_headed(sid), is_not_headed(neg)) {
                (false, true) | (false, false) => out.push((sid, neg)),
                (true, false)                  => out.push((neg, sid)),
                (true, true)                   => out.push((sid.min(neg), sid.max(neg))),
            }
        }
        out.sort_unstable();
        out
    }
}

/// Contexts-as-sessions (plan lever 2): full consequence closure inside a
/// belief context via a PROJECTED prover run — the Konolige deduction
/// model.  The kb-layer surface; the projection driver itself lives in
/// `prover/saturate/doxastic.rs` (see its header for the architecture,
/// phase-1 scope decisions, and the stretch design note).
///
/// GUARDRAIL: these are `&self` queries — the projection never asserts
/// anything.  No `believes(agent, X)` is ever fed back from an inner
/// derivation; verdicts and proofs return to the caller only.
#[cfg(feature = "native-prover")]
impl KnowledgeBase<crate::prover::saturate::ProverLayer> {
    /// Prove `query_kif` inside `agent`'s belief context (`believes`):
    /// the agent's asserted belief contents become the inner problem's
    /// axioms (un-quoted naturally — contents are store sub-sentences
    /// clausified as top-level formulas), the query its conjecture.
    ///
    /// `Proved` — the beliefs entail the query under the FULL calculus
    /// (closure beyond the outer K-distribution schemata).  `Disproved`
    /// (saturation) — the inner run's CounterSatisfiable analogue.
    /// `Inconsistent` — the belief base itself derives ⊥ (the refutation
    /// never touched the query).  `Unknown`/`Timeout` — budget.
    pub fn doxastic_ask(
        &self,
        agent:     &str,
        query_kif: &str,
        opts:      crate::NativeOpts,
    ) -> crate::prover::ProverResult {
        self.doxastic_ask_via("believes", agent, query_kif, opts)
    }

    /// [`doxastic_ask`](Self::doxastic_ask) generalized over the attitude
    /// relation (`knows`, `desires`, …) — same contract.
    pub fn doxastic_ask_via(
        &self,
        attitude:  &str,
        agent:     &str,
        query_kif: &str,
        opts:      crate::NativeOpts,
    ) -> crate::prover::ProverResult {
        let doc = crate::parse_document(
            "doxastic_ask", query_kif.to_string(), crate::Parser::Kif);
        if doc.has_errors() {
            return crate::prover::ProverResult {
                status:     crate::prover::ProverStatus::InputError,
                raw_output: format!(
                    "query parse error ({} diagnostic(s))", doc.parse_errors.len()),
                ..Default::default()
            };
        }
        let asts: Vec<crate::AstNode> =
            doc.ast.into_iter().filter_map(|d| d.as_stmt().cloned()).collect();
        let contents = self.doxastic_contents_via(attitude, agent);
        self.layer.doxastic_project(&contents, Some(asts), opts, &self.prove_ctx())
    }

    /// Is `agent`'s belief base (via `believes`) consistent under full
    /// consequence closure?  Saturates the projected contents with no
    /// conjecture: `Consistent` / `Inconsistent` (cited transcripts in
    /// `contradiction_proofs`, the first also in `proof_kif`) /
    /// `Unknown`-`Timeout` (budget).  An EMPTY belief base is trivially
    /// `Consistent`.
    pub fn doxastic_consistent(
        &self,
        agent: &str,
        opts:  crate::NativeOpts,
    ) -> crate::prover::ProverResult {
        self.doxastic_consistent_via("believes", agent, opts)
    }

    /// [`doxastic_consistent`](Self::doxastic_consistent) generalized
    /// over the attitude relation — same contract.
    pub fn doxastic_consistent_via(
        &self,
        attitude: &str,
        agent:    &str,
        opts:     crate::NativeOpts,
    ) -> crate::prover::ProverResult {
        let contents = self.doxastic_contents_via(attitude, agent);
        self.layer.doxastic_project(&contents, None, opts, &self.prove_ctx())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kb_from(kif: &str) -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("test.kif"), "test.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        let r = kb.make_session_axiomatic("test.kif");
        assert!(r.is_ok(), "promotion failed: {:?}", r.err());
        kb
    }

    /// Render a content sentence back to KIF for readable asserts.
    fn kif_of(kb: &KnowledgeBase, sid: SentenceId) -> String {
        crate::syntactic::display::sentence_to_plain_kif(sid, kb.store_for_testing())
    }

    #[test]
    fn flags_believed_p_and_not_p() {
        let kb = kb_from(
            "(believes John (bald Socrates))\n\
             (believes John (not (bald Socrates)))",
        );
        let conflicts = kb.doxastic_conflicts("John");
        assert_eq!(conflicts.len(), 1, "exactly one P/¬P pair");
        let (p, n) = conflicts[0];
        assert_eq!(kif_of(&kb, p), "(bald Socrates)");
        assert_eq!(kif_of(&kb, n), "(not (bald Socrates))");
        assert_eq!(
            normalized_negation_id(kb.store_for_testing(), p), Some(n),
            "negation side is (not P) by content hash"
        );
    }

    #[test]
    fn distinct_agents_do_not_cross_flag() {
        let kb = kb_from(
            "(believes John (bald Socrates))\n\
             (believes Mary (not (bald Socrates)))",
        );
        assert!(kb.doxastic_conflicts("John").is_empty(), "no self-conflict for John");
        assert!(kb.doxastic_conflicts("Mary").is_empty(), "no self-conflict for Mary");
    }

    #[test]
    fn unrelated_beliefs_are_clean() {
        let kb = kb_from(
            "(believes John (bald Socrates))\n\
             (believes John (wise Plato))\n\
             (believes John (not (wise Aristotle)))",
        );
        assert!(kb.doxastic_conflicts("John").is_empty());
        assert!(kb.doxastic_conflicts("NoSuchAgent").is_empty(), "unknown agent is clean");
    }

    #[test]
    fn double_negation_cancels_at_ingest_and_still_flags() {
        // Ingest NNF-normalizes stored contents (caf.rs), so a believed
        // `(not (not Q))` is STORED as `Q` — and conflicts with the
        // believed `(not Q)` through the plain probe.
        let kb = kb_from(
            "(believes John (not (not (wise Plato))))\n\
             (believes John (not (wise Plato)))",
        );
        let conflicts = kb.doxastic_conflicts("John");
        assert_eq!(conflicts.len(), 1);
        let (p, n) = conflicts[0];
        assert_eq!(kif_of(&kb, p), "(wise Plato)");
        assert_eq!(kif_of(&kb, n), "(not (wise Plato))");
    }

    #[test]
    fn rule_antecedents_are_not_beliefs() {
        // The believes-atom inside a rule is not an asserted fact — it
        // must not feed the believed set.
        let kb = kb_from(
            "(=> (believes John (bald Socrates)) (confused John))\n\
             (believes John (not (bald Socrates)))",
        );
        assert!(kb.doxastic_conflicts("John").is_empty());
    }

    #[test]
    fn compound_contents_conflict_through_ingest_normal_form() {
        // Ingest pushes the believed `(not (and …))` to its De Morgan
        // dual `(or (not …) (not …))` before storing — the probe
        // computes the complement in the SAME normal form, so the pair
        // is still found.  The near-miss (Round vs Flat) must not flag.
        let kb = kb_from(
            "(believes John (and (shape Earth Flat) (orbits Earth Sun)))\n\
             (believes John (not (and (shape Earth Flat) (orbits Earth Sun))))\n\
             (believes John (not (and (shape Earth Round) (orbits Earth Sun))))",
        );
        let conflicts = kb.doxastic_conflicts("John");
        assert_eq!(conflicts.len(), 1, "only the exact-match pair flags");
        let (p, n) = conflicts[0];
        let pair = [kif_of(&kb, p), kif_of(&kb, n)];
        assert!(pair.contains(&"(and (shape Earth Flat) (orbits Earth Sun))".to_string()),
            "got {pair:?}");
        assert!(pair.contains(
            &"(or (not (shape Earth Flat)) (not (orbits Earth Sun)))".to_string()),
            "got {pair:?}");
    }

    #[test]
    fn attitude_relation_is_parameterizable() {
        let kb = kb_from(
            "(knows Ann (wise Plato))\n\
             (knows Ann (not (wise Plato)))\n\
             (believes Ann (calm Sea))",
        );
        assert_eq!(kb.doxastic_conflicts_via("knows", "Ann").len(), 1);
        assert!(kb.doxastic_conflicts("Ann").is_empty(), "believes set unaffected");
    }
}

// Contexts-as-sessions: the projected prover run (lever 2).  Probes per
// the spec: closure beyond K inside the context with the outer ask as a
// control, consistency with cited contents, agent isolation, one-level
// nesting, empty-set and budget behavior.
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

    fn kb_native(kif: &str) -> KnowledgeBase<ProverLayer> {
        let mut kb = KnowledgeBase::new_native();
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("test.kif"), "test.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("test.kif").expect("promote");
        kb
    }

    fn fast() -> NativeOpts {
        NativeOpts { time_limit_secs: 10, ..Default::default() }
    }

    fn kif_of(kb: &KnowledgeBase<ProverLayer>, sid: SentenceId) -> String {
        crate::syntactic::display::sentence_to_plain_kif(sid, kb.store_for_testing())
    }

    #[test]
    fn harvest_collects_asserted_contents_only() {
        let kb = kb_native(PROBE_KB);
        let contents: Vec<String> = kb.doxastic_contents("John")
            .into_iter().map(|s| kif_of(&kb, s)).collect();
        assert_eq!(contents.len(), 2, "got {contents:?}");
        assert!(contents.contains(&"(bald Socrates)".to_string()));
        assert!(contents.contains(&"(=> (bald Socrates) (old Socrates))".to_string()));
        assert_eq!(kb.doxastic_contents("Mary").len(), 1);
        assert!(kb.doxastic_contents("NoSuchAgent").is_empty());

        // A rule antecedent `(believes John …)` is not an asserted
        // belief — same exclusion the conflict lint applies.
        let kb2 = kb_native(
            "(=> (believes John (bald Socrates)) (confused John))\n\
             (believes John (wise Plato))");
        let harvested: Vec<String> = kb2.doxastic_contents("John")
            .into_iter().map(|s| kif_of(&kb2, s)).collect();
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

        let inner = kb.doxastic_ask("John", "(old Socrates)", fast());
        assert_eq!(inner.status, ProverStatus::Proved,
            "modus ponens inside the context: {}", inner.raw_output);

        // GUARDRAIL: the projection asserted nothing.
        assert_eq!(kb.store_for_testing().root_sids().len(), roots_before,
            "projection must not mutate the store");

        // Outer control: `(believes John (old Socrates))` is NOT
        // derivable outside the context.
        let outer = kb.ask_query(
            "(believes John (old Socrates))", None, SineParams::default(), fast());
        assert!(
            !matches!(outer.status, ProverStatus::Proved | ProverStatus::Inconsistent),
            "outer ask must stay unproven (got {:?}): {}",
            outer.status, outer.raw_output);
    }

    /// Closure genuinely beyond K: quantifier instantiation inside the
    /// context — no quote-level schema can instantiate under a
    /// `forall_q`.
    #[test]
    fn quantified_closure_beyond_k() {
        let kb = kb_native(
            "(domain believes 2 Formula)\n\
             (believes John (forall (?X) (=> (man ?X) (mortal ?X))))\n\
             (believes John (man Socrates))");
        let inner = kb.doxastic_ask("John", "(mortal Socrates)", fast());
        assert_eq!(inner.status, ProverStatus::Proved, "{}", inner.raw_output);

        let outer = kb.ask_query(
            "(believes John (mortal Socrates))", None, SineParams::default(), fast());
        assert!(
            !matches!(outer.status, ProverStatus::Proved | ProverStatus::Inconsistent),
            "outer control (got {:?}): {}", outer.status, outer.raw_output);
    }

    #[test]
    fn negated_content_is_not_proved_inside() {
        let kb = kb_native(PROBE_KB);
        let r = kb.doxastic_ask("John", "(not (bald Socrates))", fast());
        assert_eq!(r.status, ProverStatus::Disproved,
            "John's context saturates without ¬P: {}", r.raw_output);
    }

    #[test]
    fn consistency_flags_contradiction_with_cited_contents() {
        // Baseline: the probe KB's John is consistent.
        let kb = kb_native(PROBE_KB);
        let r = kb.doxastic_consistent("John", fast());
        assert_eq!(r.status, ProverStatus::Consistent, "{}", r.raw_output);

        // Add `believes John (not Q)`: {P, P⇒Q, ¬Q} is inconsistent
        // under consequence — invisible to the syntactic lint, found by
        // the projection, with a transcript citing the three contents.
        let kb = kb_native(&format!(
            "{PROBE_KB}\n(believes John (not (old Socrates)))"));
        assert!(kb.doxastic_conflicts("John").is_empty(),
            "no syntactic P/¬P pair — this contradiction needs deduction");
        let r = kb.doxastic_consistent("John", fast());
        assert_eq!(r.status, ProverStatus::Inconsistent, "{}", r.raw_output);
        assert!(!r.proof_kif.is_empty(), "cited transcript expected");
        let cited: HashSet<String> = r.proof_kif.iter()
            .filter_map(|s| s.source_sid)
            .map(|sid| kif_of(&kb, sid))
            .collect();
        for content in [
            "(bald Socrates)",
            "(=> (bald Socrates) (old Socrates))",
            "(not (old Socrates))",
        ] {
            assert!(cited.contains(content),
                "proof must cite {content:?}, cited: {cited:?}");
        }
    }

    #[test]
    fn agents_are_isolated() {
        let kb = kb_native(PROBE_KB);
        // Mary's ¬P never meets John's P: both contexts are consistent…
        let r = kb.doxastic_consistent("Mary", fast());
        assert_eq!(r.status, ProverStatus::Consistent, "{}", r.raw_output);
        // …and Mary's context neither contains nor derives John's P.
        let r = kb.doxastic_ask("Mary", "(bald Socrates)", fast());
        assert_eq!(r.status, ProverStatus::Disproved, "{}", r.raw_output);
        // Her own belief discharges directly.
        let r = kb.doxastic_ask("Mary", "(not (bald Socrates))", fast());
        assert_eq!(r.status, ProverStatus::Proved, "{}", r.raw_output);
    }

    #[test]
    fn nested_belief_stays_quoted_one_level() {
        let kb = kb_native(
            "(believes John (believes Mary (not (bald Socrates))))");
        // John's projection holds `believes(Mary, ¬P)` as an inner FACT
        // (the content quotes one level down at inner clausification).
        let r = kb.doxastic_ask("John", "(believes Mary (not (bald Socrates)))", fast());
        assert_eq!(r.status, ProverStatus::Proved, "{}", r.raw_output);
        // The nested content is never unquoted to John's assertion level.
        let r = kb.doxastic_ask("John", "(not (bald Socrates))", fast());
        assert_eq!(r.status, ProverStatus::Disproved,
            "nested quote must stay opaque: {}", r.raw_output);
        // No recursion into Mary (phase 1): the nested belief is not an
        // ASSERTED root of Mary's, so her context is empty.
        assert!(kb.doxastic_contents("Mary").is_empty());
        let r = kb.doxastic_consistent("Mary", fast());
        assert_eq!(r.status, ProverStatus::Consistent, "{}", r.raw_output);
    }

    #[test]
    fn empty_belief_set_is_trivially_consistent() {
        let kb = kb_native("(instance John Human)");
        let r = kb.doxastic_consistent("John", fast());
        assert_eq!(r.status, ProverStatus::Consistent, "{}", r.raw_output);
        let r = kb.doxastic_consistent("NoSuchAgent", fast());
        assert_eq!(r.status, ProverStatus::Consistent, "{}", r.raw_output);
        // Nothing believed ⇒ nothing entailed.
        let r = kb.doxastic_ask("John", "(bald Socrates)", fast());
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
        let r = kb.doxastic_ask("John", "(old Socrates)", opts);
        assert_eq!(r.status, ProverStatus::Unknown, "{}", r.raw_output);
        assert_eq!(r.termination, Some(TerminationReason::GaveUp));
    }

    #[test]
    fn malformed_query_is_an_input_error() {
        let kb = kb_native(PROBE_KB);
        let r = kb.doxastic_ask("John", "(((", fast());
        assert_eq!(r.status, ProverStatus::InputError);
    }
}
