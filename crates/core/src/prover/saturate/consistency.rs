// crates/core/src/prover/saturate/consistency.rs
//
// Consistency checking / auditing with the native prover.
//
// One driver serves both the cross-backend `ProvingLayer::check_consistency`
// contract (stop at the first contradiction) and the enumerating `audit`
// surface (find up to N), parameterized by `limit`: the saturation loop's
// `set_audit(cap)` collects up to `cap` input contradictions and early-stops at
// the cap, so `limit == 1` *is* "stop at the first".

use std::collections::HashSet;

use crate::prover::{ProverResult, ProverStatus, TerminationReason};
use crate::progress::ProveCtx;
use crate::semantics::types::Scope;
use crate::syntactic::caches::session::session_id;
use crate::{SentenceId, SineParams, SymbolId};

use super::ProverLayer;
use super::prover::{NativeOpts, NativeProver, RunVerdict};

impl ProverLayer {
    /// Saturate the selected axiom base (everything as set-of-support, no
    /// conjecture) looking for input contradictions, harvesting up to `limit`
    /// distinct ones (deduped by the source axioms they implicate).
    ///
    /// - `limit == 1` → a plain "is this satisfiable?" decision (stop at the
    ///   first contradiction); larger `limit` enumerates (the audit surface).
    /// - `focus` extra-seeds **and** force-includes specific sids (a file's
    ///   sentences); empty `focus` + empty session ⇒ the whole promoted base.
    ///
    /// Status: `Inconsistent` (≥1 contradiction; transcripts in
    /// `contradiction_proofs`, the first also in `proof_kif`), `Consistent`
    /// (saturated, none found), or `Timeout` / `Unknown`.
    pub(crate) fn check_consistency_driver(
        &self,
        session:     Option<&str>,
        focus:       &[SentenceId],
        sine_params: SineParams,
        opts:        NativeOpts,
        ctx:         &ProveCtx,
        limit:       usize,
    ) -> ProverResult {
        let syn = &self.semantic.syntactic;
        let session_sids: Vec<SentenceId> = session
            .map(|s| syn.sessions.session_sentences(s))
            .unwrap_or_default();

        // Whole promoted base when there's nothing to anchor on; else SInE over
        // the (focus ∪ session) symbols, force-including `focus`.
        let selected: Vec<SentenceId> = if focus.is_empty() && session_sids.is_empty() {
            syn.axiom_ids_set().into_iter().collect()
        } else {
            let mut seed: HashSet<SymbolId> = HashSet::new();
            for sid in focus.iter().chain(session_sids.iter()) {
                seed.extend(syn.sentence_symbols(*sid));
            }
            let mut sel: Vec<SentenceId> =
                syn.sine_select_with_seed(seed, sine_params, ctx).into_iter().collect();
            sel.extend(focus.iter().copied());
            sel.sort_unstable();
            sel.dedup();
            sel
        };

        let scope = session.map(|s| Scope::Session(session_id(s))).unwrap_or(Scope::Base);

        let recognize_roles = opts.strategy.recognize_roles
            || std::env::var_os("SIGMA_RECOGNIZE_ROLES").is_some();

        let mut prover = NativeProver::new(self, scope, opts);
        // Audit mode: collect up to `limit` contradictions, then stop.
        prover.set_audit(limit.max(1));

        // Shape-recognize the taxonomy vocabulary before the pre-pass, so renamed
        // dialects engage the oracle (mirrors `prove_once`).
        if recognize_roles {
            let roots: Vec<SentenceId> =
                selected.iter().chain(session_sids.iter()).copied().collect();
            prover.recognize_roles(&roots);
        }

        // Theory pre-pass — the equality closure, concrete subrelation rules, and
        // FD / schema declarations are how type-level contradictions surface
        // fast.  (The pre-`ProvingLayer` `check_consistency_native` skipped this
        // and could therefore report `Consistent` on a KB an `ask`/`audit` would
        // find inconsistent — running it here is the fix.)
        for sid in selected.iter().chain(session_sids.iter()) {
            let cls = self.clauses_for(*sid);
            prover.register_equalities(&cls);
            prover.synthesize_subrelation_rules(&cls);
            prover.mine_fd_relations(&cls, *sid);
            prover.mine_schema(&cls, *sid);
        }

        // Everything is support: without a conjecture the SOS restriction would
        // otherwise leave the queue empty and trivially "saturate".
        for sid in selected.iter().chain(session_sids.iter()) {
            prover.add_support_root(*sid);
        }
        if prover.opts.strategy.bg_completion {
            prover.complete_background();
        }
        if prover.opts.forward_close {
            prover.forward_close();
        }

        let (verdict, steps) = prover.run();

        // Harvest distinct input contradictions, deduped by implicated source
        // axioms.
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
        let raw = format!(
            "native consistency: {:?} after {} steps over {} axioms (+{} session); \
             {} distinct contradiction(s) ({} total occurrences)",
            verdict, steps, selected.len(), session_sids.len(),
            found, prover.stats.input_contradictions);

        let (status, termination) = if found > 0 {
            (ProverStatus::Inconsistent, None)
        } else {
            match verdict {
                RunVerdict::Saturated =>
                    (ProverStatus::Consistent, Some(TerminationReason::Saturation)),
                RunVerdict::TimedOut =>
                    (ProverStatus::Timeout, Some(TerminationReason::TimeLimit)),
                _ =>
                    (ProverStatus::Unknown, Some(TerminationReason::GaveUp)),
            }
        };

        ProverResult {
            status,
            termination,
            raw_output: raw,
            // Keep `proof_kif` populated for single-verdict consumers.
            proof_kif: contradiction_proofs.first().cloned().unwrap_or_default(),
            contradiction_proofs,
            ..Default::default()
        }
    }
}
