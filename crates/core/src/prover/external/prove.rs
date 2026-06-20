// crates/core/src/prover/external/prove.rs
//
// Proving driver for external provers

use std::collections::HashSet;
use std::time::Instant;

use super::{
    ExternalProverLayer,
    ExternalOpts,
    Conjecture,
};
use super::super::ProverResult;
use super::backends::{ProverOpts, ProverMode, ProverRunner};

use crate::{SentenceId, SineParams, SymbolId, profile_span};
use crate::progress::ProveCtx;
use crate::semantics::types::Scope;

impl ExternalProverLayer {
    pub(super) fn ext_prove_once(
        &self,
        conj:     &Conjecture,
        params:   SineParams,
        slice:    u32,
        opts:     &ExternalOpts,
        ctx:      &ProveCtx,
    ) -> (ProverResult, usize) {
        // The conjecture roots are store sids (content hashes the cascade
        // interned in `intern_conjecture`) — exactly what `build_problem` /
        // `set_conjecture` resolve against.
        let query_sids_owned: Vec<SentenceId> = conj.sents.iter().map(|(_, sid)| *sid).collect();
        let query_sids = &query_sids_owned;
        let session    = opts.session.as_deref();
        let mode       = opts.mode;

        // Session assertions are force-included as hypotheses *and* seed SInE
        // alongside the conjecture, so axioms connecting an asserted fact to
        // the goal are reachable.
        let assertion_ids: HashSet<SentenceId> = session
            .map(|s| self.translation.semantic.syntactic.sessions.session_sentences(s)
                 .into_iter().collect())
            .unwrap_or_default();

        // Symbol seed = conjecture ∪ session assertions.  The external path now
        // seeds by SYMBOL (not sid) so it runs the same relevance pass as the
        // native engine — including the Liu & Xu structural rescue it had been
        // skipping.
        let mut seed: HashSet<SymbolId> = HashSet::new();
        for &sid in query_sids.iter().chain(assertion_ids.iter()) {
            seed.extend(self.translation.semantic.syntactic.sentence_symbols(sid));
        }

        // Shared relevance pass: SInE → head-filter → Liu rescue.  The external
        // backend always strips bookkeeping heads and (unlike before) takes the
        // rescue; `ProverOpts` carries no Liu knobs, so use the canonical
        // defaults.
        let sel = crate::syntactic::SelectionParams {
            head_filter: true,
            liu_rescue:  true,
            liu_rounds:  1,
            liu_top_k:   32,
        };
        let (selected, _frontier) =
            self.translation.semantic.syntactic.select_relevant(&seed, params, &sel, ctx);
        let raw_selected = selected.len();

        let mut axiom_sids: Vec<SentenceId> = selected.into_iter().collect();
        axiom_sids.extend(assertion_ids.iter().copied());
        axiom_sids.sort_unstable();
        axiom_sids.dedup();
        // (Synthetic replacements + predicate-variable instantiation now happen
        // inside `assemble_problem` — the translation layer scans the selected
        // axiom set for synthetic eligibility on demand.)
        // Taxonomy-closure injection: pull in the subclass/instance chain facts
        // connecting the conjecture's (and assertions') class symbols — the
        // same conjecture ∪ assertions symbol union already built as `seed`.
        let tax = self.translation.semantic
            .taxonomy_closure_facts_scoped(&seed, 4000, query_scope(session));
        if !tax.is_empty() {
            axiom_sids.extend(tax);
            axiom_sids.sort_unstable();
            axiom_sids.dedup();
        }

        let t_input = Instant::now();
        let prover_opts = ProverOpts { timeout_secs: slice as u64, mode: ProverMode::Prove };

        // Higher-order (THF) mode: assemble through the translation layer's
        // HO pipeline and hand the structured problem to the runner — text
        // backends serialise the 1-to-1 THF themselves (the `prove_ho`
        // default), the embedded backend lowers the HO IR straight into the
        // FFI solver's native structures.
        if opts.hol {
            let (problem, sid_map) = {
                profile_span!(ctx, "ask.build_problem");
                let seeds: Vec<SentenceId> = assertion_ids.iter().copied().collect();
                self.translation.assemble_problem_thf(
                    &axiom_sids, &seeds, query_sids, Some(query_scope(session)),
                )
            };
            let input_gen = t_input.elapsed();
            ctx.debug(format!(
                "ask(thf): {} selected + {} assertions, {} axiom rows",
                raw_selected, assertion_ids.len(), problem.axioms().len()));
            let mut result = {
                profile_span!(ctx, "ask.prover_run");
                self.backend.prove_ho(&problem, &sid_map, "query_0", &prover_opts)
            };
            result.timings.input_gen += input_gen;
            return (result, raw_selected);
        }

        // Translate through the translation layer: on-demand synthetic scan
        // over the selected axioms (replacements + predicate-variable
        // instantiation), cached axiom translation, and the conjecture install
        // (first convertible candidate; numbers hidden exactly as the axioms).
        let (problem, sid_map, _qvm) = {
            profile_span!(ctx, "ask.build_problem");
            let seeds: Vec<SentenceId> = assertion_ids.iter().copied().collect();
            self.translation.assemble_problem(
                &axiom_sids, &seeds, query_sids, mode,
                Some(query_scope(session)),
            )
        };
        let input_gen = t_input.elapsed();
        ctx.debug(format!(
            "ask({:?}): {} selected + {} assertions, {} axiom rows",
            mode, raw_selected, assertion_ids.len(), problem.axioms().len()));

        // Hand the structured problem to the runner.  Text backends serialise
        // it themselves (the `prove_ir` default → `assemble_tptp` → `prove`,
        // which also honours `--keep`); the embedded backend lowers the IR
        // straight into the FFI solver with no TPTP round-trip.
        let mut result = {
            profile_span!(ctx, "ask.prover_run");
            self.backend.prove_ir(&problem, &sid_map, "query_0", &prover_opts)
        };
        result.timings.input_gen += input_gen;
        (result, raw_selected)
    }
}

/// The semantic [`Scope`] a query reasons in: `Base` for a global ask, or the
/// named session's overlay.  (Local copy of the former `kb::prove::query_scope`.)
fn query_scope(session: Option<&str>) -> Scope {
    match session {
        Some(s) => Scope::Session(crate::syntactic::caches::session::session_id(s)),
        None    => Scope::Base,
    }
}