// crates/core/src/prover/external/prove.rs
//
// Proving driver for external provers

use super::super::ProverResult;
use super::backends::{ProverMode, ProverOpts, ProverRunner};
use super::{Conjecture, ExternalOpts, ExternalProverLayer};

use crate::progress::ProveCtx;
use crate::{profile_span, SentenceId, SineParams};

impl<T: crate::trans::HasTranslation + 'static> ExternalProverLayer<T> {
    pub(super) fn ext_prove_once(
        &self,
        conj: &Conjecture,
        params: SineParams,
        slice: u32,
        opts: &ExternalOpts,
        ctx: &ProveCtx,
    ) -> (ProverResult, usize) {
        // The conjecture roots are store sids (content hashes the cascade
        // interned in `intern_conjecture`).
        let query_sids: Vec<SentenceId> = conj.sents.iter().map(|(_, sid)| *sid).collect();
        let built = self.translation().query_problem(
            &query_sids,
            params,
            opts.session.as_deref(),
            opts.mode,
            ctx,
        );
        let prover_opts = ProverOpts {
            timeout_secs: slice as u64,
            mode: ProverMode::Prove,
        };

        // Hand the structured problem to the runner.  Text backends serialise
        // it themselves (the `prove_ir` default -> `assemble_tptp_indexed` ->
        // `prove`, which also honours `--keep`); the embedded backend lowers
        // the IR straight into the FFI solver with no TPTP round-trip.
        let mut result = {
            profile_span!(ctx, "ask.prover_run");
            self.backend
                .prove_ir(&built.problem, &built.sid_map, "query_0", &prover_opts)
        };
        result.timings.input_gen += built.input_gen;
        (result, built.raw_selected)
    }
}
