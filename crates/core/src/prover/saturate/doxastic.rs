// crates/core/src/prover/saturate/doxastic.rs
//
// Contexts-as-sessions: the doxastic PROJECTION driver (plan lever 2,
// the Konolige deduction model).  A belief context is a projected prover
// run, not a store mutation: the caller harvests the agent's ASSERTED
// belief contents (kb/doxastic.rs — the lint's collection logic), and
// this driver runs a fresh, isolated saturation over exactly those
// contents, fed to the clausifier AS TOP-LEVEL FORMULAS.
//
// Why that gives full consequence closure inside the modality: quoting
// happens at LIFT time (clausify.rs's quote walker), not at store time —
// the store holds real sub-sentence structure for every believed
// content.  Clausified as a root, a content is an ordinary first-order
// formula, so the entire calculus (resolution, equality, the theory
// oracle) applies inside the context — modus ponens, quantifier
// instantiation, everything the outer K-distribution schemata (which
// only rearrange quoted structure, see `clausify::modal_k_clauses`)
// structurally cannot do.  A belief-about-a-belief inside a content
// quotes ONE level down exactly as before: `(believes Mary P)` as an
// inner axiom is the inner FACT `believes(Mary, quote(P))` — nested
// contents stay opaque terms, one level per projection.
//
// GUARDRAIL (non-negotiable): the projection NEVER feeds inner
// conclusions back as outer facts.  Nothing here writes to the sentence
// store — no `believes(agent, X)` is asserted from an inner derivation;
// verdicts and proofs return to the CALLER only.  In-loop reflection
// (the DoxasticOracle, plan lever 5) is explicitly out of scope.
//
// Phase-1 scope decisions (documented extension points):
//
//   * BACKGROUND is EMPTY — a pure doxastic check: the inner problem is
//     the agent's belief base and nothing else.  Shared-world-knowledge
//     inclusion (projecting the outer KB, or a designated slice of it,
//     into the context alongside the beliefs) is a later parameter of
//     this driver: an extra `background: &[SentenceId]` loaded exactly
//     like `contents` below.  Note the theory ORACLE still reasons over
//     the outer semantic layer's taxonomy closures (it is part of the
//     prover apparatus, shared by every native run); a pure-attitude KB
//     has no taxonomy for it to say anything about.
//   * Asserted-belief harvest only.  DERIVED positive beliefs (a rule
//     concluding `(believes a P)` that the outer prover could derive)
//     are not harvested.  Design for the stretch: run a bounded outer
//     saturation with `set_audit`-style collection of derived positive
//     `attitude(agent, quote(...))` units, decode each quote back to a
//     store sentence (requires an unquote-to-store bridge — the inverse
//     of `lift_quote_sentence`, interning through the shared store, NOT
//     the prover-local atom table), and append those sids to `contents`.
//     The decode bridge does not exist yet and is the reason this is
//     deferred, not the outer run itself.
//   * No recursion into nested contexts (projecting John does not
//     project Mary), and no K-distribution injection INSIDE the inner
//     run: nested beliefs are inert quoted facts at this level; project
//     the inner agent explicitly to reason inside THEIR context.

use std::collections::HashSet;

use crate::SentenceId;
use crate::progress::ProveCtx;
use crate::prover::{ProverResult, ProverStatus};
use crate::semantics::types::Scope;

use super::{Conjecture, ProverLayer};
use super::clause::PClause;
use super::clausify::clausify_negated_conjunction_lossy;
use super::prover::{NativeOpts, NativeProver, RunVerdict};

impl ProverLayer {
    /// One projected prover run over a harvested belief base.
    ///
    /// `contents` are the agent's believed-content sentence ids (store
    /// sub-sentences; the caller harvests + sorts them — sorted input
    /// keeps clause registration order, and hence the run, deterministic).
    /// `query`:
    ///
    ///   * `Some(asts)` — doxastic ask: the query is clausified as the
    ///     conjecture against the contents-as-axioms.  `Proved` means the
    ///     agent's beliefs entail the query under full consequence
    ///     closure; `Disproved` (saturation) is the CounterSatisfiable
    ///     analogue; a refutation that never touches the conjecture means
    ///     the belief base itself is contradictory → `Inconsistent`.
    ///   * `None` — doxastic consistency check: full saturation with no
    ///     conjecture (the `check_consistency_driver` shape), contents as
    ///     set-of-support → `Consistent` / `Inconsistent` (+ cited
    ///     transcripts in `contradiction_proofs`) / `Unknown`-`Timeout`.
    ///
    /// `opts.session` is not consulted: the projection is scope-free by
    /// construction (its problem is exactly `contents`).  Budget knobs
    /// (`time_limit_secs`, `max_steps`) and search knobs apply as in any
    /// native run; on an exhausted budget the verdict is an honest
    /// `Unknown`/`Timeout`, never a hang (the loop's deadline checks).
    pub(crate) fn doxastic_project(
        &self,
        contents: &[SentenceId],
        query:    Option<Vec<crate::AstNode>>,
        opts:     NativeOpts,
        ctx:      &ProveCtx,
    ) -> ProverResult {
        let is_ask = query.is_some();

        // Conjecture prep (ask mode) — the same normalize + intern +
        // negated-conjunction clausification path `prove_native` uses.
        let conj: Option<Conjecture> = match query {
            None => None,
            Some(asts) => {
                let (normalized, norm_dropped) = Conjecture::normalize(asts);
                let seed_syms = Conjecture::seed(&normalized);
                let sents = self.intern_conjecture_native(&normalized);
                if sents.is_empty() {
                    return ProverResult {
                        status:     ProverStatus::Unknown,
                        raw_output: "No query sentence parsed".into(),
                        ..Default::default()
                    };
                }
                let dropped = norm_dropped + normalized.len().saturating_sub(sents.len());
                Some(Conjecture { sents, seed_syms, dropped })
            }
        };
        let (conjecture_clauses, conj_lossy): (Vec<PClause>, bool) = match &conj {
            Some(c) => clausify_negated_conjunction_lossy(
                &self.semantic.syntactic, &self.atoms, &c.sents),
            None => (Vec::new(), false),
        };

        // Fresh prover, Base scope (the projection carries no session
        // overlay; the inner problem is exactly `contents`).  Isolation
        // is per-problem already — nothing of this run outlives it.
        let mut prover = NativeProver::new(self, Scope::Base, opts);
        if !is_ask {
            // Consistency flavor: collect the first input contradiction
            // and stop (the cross-backend consistency contract).
            prover.set_audit(1);
        }

        // Theory pre-pass over the belief base (mirrors the prove /
        // consistency drivers): ground equalities, concrete subrelation
        // rules, FD declarations, and schema-stated relation properties
        // register before any clause is made.
        for sid in contents {
            let cls = self.clauses_for(*sid);
            prover.register_equalities(&cls);
            prover.synthesize_subrelation_rules(&cls);
            prover.mine_fd_relations(&cls, *sid);
            prover.mine_schema(&cls, *sid);
        }
        if is_ask {
            prover.register_equalities(&conjecture_clauses);
            prover.set_goal(&conjecture_clauses);
        }

        // Load the problem.  Ask: contents are the axiom base
        // (background), the negated query is the set of support.
        // Consistency: everything is support — without a conjecture the
        // SOS restriction would leave the queue empty and trivially
        // "saturate" (the `check_consistency_driver` rule).
        if is_ask {
            for sid in contents {
                prover.add_background_root(*sid);
            }
            prover.add_conjecture_clauses(
                &conjecture_clauses,
                conj.as_ref().and_then(|c| c.sents.first().map(|(_, sid)| *sid)),
            );
        } else {
            for sid in contents {
                prover.add_support_root(*sid);
            }
        }
        if prover.opts.strategy.bg_completion {
            prover.complete_background();
        }
        if prover.opts.forward_close {
            prover.forward_close();
        }

        let (verdict, steps) = prover.run();

        // Input-completeness: a content that failed to clausify
        // (unsupported shape / capacity) is a missing belief — any
        // confident "no"/"consistent" verdict below is withheld.
        let failed_roots = contents.iter().filter(|s| self.root_load_failed(**s)).count();
        let input_load_failures = failed_roots
            + conj.as_ref().map_or(0, |c| c.dropped)
            + usize::from(conj_lossy);

        // Harvest distinct input contradictions (deduped by implicated
        // source contents) — the cited transcripts an Inconsistent
        // verdict returns.
        let mut contradiction_proofs: Vec<Vec<crate::prover::proof::KifProofStep>> = Vec::new();
        let mut seen_culprits: HashSet<Vec<SentenceId>> = HashSet::new();
        for &cid in &prover.input_contradiction_ids {
            let steps = super::proof::extract_proof(&prover, cid);
            let mut culprits: Vec<SentenceId> =
                steps.iter().filter_map(|s| s.source_sid).collect();
            culprits.sort_unstable();
            culprits.dedup();
            if seen_culprits.insert(culprits) {
                contradiction_proofs.push(steps);
            }
        }
        let found = contradiction_proofs.len();

        let conjecture_used = match verdict {
            RunVerdict::Refutation(empty) if is_ask => prover.conjecture_rooted(empty),
            _ => false,
        };
        let proof_kif = match verdict {
            RunVerdict::Refutation(empty) if prover.opts.want_proof => {
                super::proof::extract_proof(&prover, empty)
            }
            _ => Vec::new(),
        };

        // A saturation verdict is complete only if no capacity cap
        // dropped a clause (mirrors `prove_one_driver`'s gate; the
        // strict-saturation regime is a TPTP-path concern, not this one).
        // `slot_lift_failures`: a stored clause that never entered the
        // run.  `input_contradictions`: only meaningful on the ask path —
        // the consistency arm runs in audit mode where contradictions are
        // COLLECTED (`found`), not suppressed, so the ask-path term uses
        // the suppressed count and the consistency arm relies on `found`.
        let complete_saturation = match verdict {
            RunVerdict::Saturated => Some(
                prover.stats.discarded_long == 0
                    && prover.stats.discarded_deep == 0
                    && prover.stats.slot_lift_failures == 0
                    && (!is_ask || prover.stats.input_contradictions == 0)
                    && input_load_failures == 0),
            _ => None,
        };

        // Shared ladder (`map_verdict`): the ask arm mirrors
        // `prove_one_driver` exactly (Disproved gated only under strict
        // saturation); the consistency arm ALWAYS gates Consistent on
        // `complete_saturation` — it is a certificate.
        let (status, termination) = if is_ask {
            super::prover::map_verdict(
                verdict, conjecture_used,
                prover.opts.strategy.strict_saturation, complete_saturation,
                super::prover::VerdictMode::Ask)
        } else if found > 0 {
            (ProverStatus::Inconsistent, None)
        } else {
            super::prover::map_verdict(
                verdict, false, false, complete_saturation,
                super::prover::VerdictMode::Consistency)
        };

        let raw = format!(
            "doxastic projection ({}; {} believed contents): {:?} after {} steps; \
             {} clauses, {} distinct contradiction(s) ({} total occurrences)",
            if is_ask { "ask" } else { "consistency" },
            contents.len(), verdict, steps, prover.clauses.len(),
            found, prover.stats.input_contradictions);
        ctx.debug(raw.clone());

        let mut result = ProverResult {
            status,
            termination,
            complete_saturation,
            given_steps: Some(steps),
            raw_output: raw,
            // Keep `proof_kif` populated for single-verdict consumers:
            // the refutation transcript when one was rendered, else the
            // first contradiction transcript (the consistency flavor).
            proof_kif: if proof_kif.is_empty() {
                contradiction_proofs.first().cloned().unwrap_or_default()
            } else {
                proof_kif
            },
            contradiction_proofs,
            ..Default::default()
        };
        result.withhold_countermodel(input_load_failures, "belief-content projection");
        result
    }
}
