// crates/core/src/prover/external/consistency.rs
//
// Consistency checking driver for external provers

use crate::{
    ProveCtx, ProverResult, ProverRunner, SentenceId,
    profile_span,
    prover::{
        ExternalProverLayer,
        ExternalOpts,
        external::backends::{ProverMode, ProverOpts}
    }
};

impl ExternalProverLayer {
    /// KB-wide satisfiability check (no conjecture): SInE-select from the
    /// session seed (or the whole axiom base), build, and saturate.
    pub(super) fn ext_check_consistency(&self, opts: &ExternalOpts, ctx: &ProveCtx)
        -> ProverResult
    {
        self.translation.ensure_rewrite_pass();

        let session_sids: Vec<SentenceId> = opts.session.as_deref()
            .map(|s| self.translation.semantic.syntactic.sessions.session_sentences(s))
            .unwrap_or_default();

        let mut sorted: Vec<SentenceId> = if session_sids.is_empty() {
            self.translation.semantic.syntactic.axiom_ids_set().into_iter().collect()
        } else {
            self.translation.semantic.syntactic
                .sine_select_for_sids(&session_sids, opts.selection, ctx)
                .into_iter().collect()
        };
        sorted.extend(session_sids.iter().copied());
        sorted.sort_unstable();
        sorted.dedup();
        let extra = self.translation.synthetic_replacements(&sorted);
        if !extra.is_empty() {
            sorted.extend(extra);
            sorted.sort_unstable();
            sorted.dedup();
        }

        let (problem, sid_map) = {
            profile_span!(ctx, "check.build_problem");
            self.translation.build_problem(&sorted, opts.mode)
        };
        let prover_opts = ProverOpts {
            timeout_secs: opts.timeout_secs,
            mode:         ProverMode::CheckConsistency,
        };
        // Structured hand-off: text backends serialise via the `prove_ir`
        // default; the embedded backend lowers the IR directly (no TPTP
        // round-trip).  No conjecture in a consistency check, so the
        // conjecture name is the assembler default.
        profile_span!(ctx, "check.prover_run");
        self.backend.prove_ir(&problem, &sid_map, "conjecture", &prover_opts)
    }
}