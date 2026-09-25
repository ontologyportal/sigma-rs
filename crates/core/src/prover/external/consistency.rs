// crates/core/src/prover/external/consistency.rs
//
// Consistency checking driver for external provers

use crate::{
    profile_span,
    prover::{
        external::backends::{ProverMode, ProverOpts},
        ExternalOpts, ExternalProverLayer,
    },
    ProveCtx, ProverResult, ProverRunner, SentenceId,
};

impl<T: crate::trans::HasTranslation + 'static> ExternalProverLayer<T> {
    /// KB-wide satisfiability check (no conjecture): SInE-select from the
    /// session seed (or the whole axiom base), build, and saturate.
    pub(super) fn ext_check_consistency(
        &self,
        focus: &[SentenceId],
        opts: &ExternalOpts,
        ctx: &ProveCtx,
    ) -> ProverResult {
        self.translation().ensure_rewrite_pass();

        let session_sids: Vec<SentenceId> = opts
            .session
            .as_deref()
            .map(|s| {
                self.translation()
                    .semantic
                    .syntactic
                    .sessions
                    .session_sentences(s)
            })
            .unwrap_or_default();

        let seeds: Vec<SentenceId> = focus.iter().chain(&session_sids).copied().collect();
        let mut sorted: Vec<SentenceId> = if seeds.is_empty() {
            self.translation()
                .semantic
                .syntactic
                .axiom_ids_set()
                .into_iter()
                .collect()
        } else {
            self.translation()
                .semantic
                .syntactic
                .sine_select_for_sids(&seeds, opts.selection, ctx)
                .into_iter()
                .collect()
        };
        sorted.extend(seeds);
        sorted.sort_unstable();
        sorted.dedup();
        let extra = self.translation().synthetic_replacements(&sorted);
        if !extra.is_empty() {
            sorted.extend(extra);
            sorted.sort_unstable();
            sorted.dedup();
        }

        let (problem, sid_map) = {
            profile_span!(ctx, "check.build_problem");
            self.translation().build_problem(&sorted, opts.mode)
        };
        let prover_opts = ProverOpts {
            timeout_secs: opts.timeout_secs,
            mode: ProverMode::CheckConsistency,
        };
        // Structured hand-off: text backends serialise via the `prove_ir`
        // default; the embedded backend lowers the IR directly (no TPTP
        // round-trip).  No conjecture in a consistency check, so the
        // conjecture name is the assembler default.
        profile_span!(ctx, "check.prover_run");
        let mut result = self.backend.prove_ir(
            &crate::trans::ir::ProblemIr::Fo(Box::new(problem)),
            &sid_map,
            "conjecture",
            &prover_opts,
        );
        if result.status == crate::ProverStatus::Inconsistent
            && result.contradiction_proofs.is_empty()
            && !result.proof_kif.is_empty()
        {
            result.contradiction_proofs.push(result.proof_kif.clone());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{KnowledgeBase, Prover, ProverStatus, SineParams};
    use std::sync::{Arc, Mutex};

    struct Capture(Mutex<String>);
    impl ProverRunner for Capture {
        fn prove(&self, tptp: &str, _: &ProverOpts) -> ProverResult {
            *self.0.lock().unwrap() = tptp.into();
            ProverResult {
                status: ProverStatus::Unknown,
                ..Default::default()
            }
        }
    }

    #[test]
    fn focus_and_session_are_included_without_unrelated_axioms() {
        let runner = Arc::new(Capture(Mutex::new(String::new())));
        let mut kb = KnowledgeBase::new_external(Prover::Custom(runner.clone()));
        for (file, text) in [
            ("focus.kif", "(focus A)"),
            ("unrelated.kif", "(unrelated B)"),
        ] {
            assert!(
                kb.reload_kif(text, &std::path::PathBuf::from(file), file)
                    .ok
            );
            kb.make_session_axiomatic(file).unwrap();
        }
        assert!(kb.tell("(support C)", "support").ok);
        let focus = kb.file_roots("focus.kif");
        let mut selection = SineParams::auto(1);
        selection.depth_limit = Some(0);
        let opts = ExternalOpts {
            selection,
            session: Some("support".into()),
            ..Default::default()
        };
        kb.audit_consistency(&focus, opts, 1);
        let text = runner.0.lock().unwrap().clone();
        assert!(text.contains("s__focus"), "{text}");
        assert!(text.contains("s__support"), "{text}");
        assert!(!text.contains("s__unrelated"), "{text}");
        kb.audit_consistency(&[], ExternalOpts::default(), 1);
        assert!(runner.0.lock().unwrap().contains("s__unrelated"));
    }
}
