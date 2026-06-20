// crates/core/src/prover/integrated.rs
//
// `IntegratedVampireRunner` — a `ProverRunner` that drives the embedded
// Vampire C++ library via FFI instead of spawning a subprocess.
//
// Input:  a TPTP string (same interface as `VampireRunner`).
// Output: `ProverResult` with `status`, `bindings` (empty — callers that
//         need bindings must use `KnowledgeBase::ask_embedded` directly),
//         `proof_kif`, `ir_proof`, and `proof_tptp` (empty, the embedded
//         path has no text transcript).
//
// Gated on `integrated-prover`.

#![cfg(feature = "integrated-prover")]

use std::time::Duration;

use vampire_prover::{Options, ProofRes, UnknownReason};


use super::super::{ProverMode, ProverOpts, ProverRunner};
use super::super::super::super::result::{
    ProverResult,
    ProverStatus,
    ProverTimings,
    TerminationReason
};

/// A [`ProverRunner`] that uses the embedded Vampire library (FFI) instead of
/// spawning a child process.
///
/// The runner parses the TPTP input string into an [`crate::trans::ir::Problem`],
/// lowers it to an FFI [`vampire_prover::SysProblem`], and calls
/// `solve_and_prove`.  Both `proof_kif` and `ir_proof` in the returned
/// [`ProverResult`] are populated when a proof is found.
///
/// # Binding extraction
///
/// `bindings` is always empty because the generic `ProverRunner` interface
/// doesn't carry a `QueryVarMap`.  Callers that need variable bindings should
/// use `KnowledgeBase::ask_embedded` directly, which has full access to the
/// variable map.
#[derive(Debug, Clone)]
pub struct IntegratedVampireRunner;

impl ProverRunner for IntegratedVampireRunner {
    /// Text-based compatibility entry: parse the TPTP back into an
    /// [`ir::Problem`](crate::trans::ir::Problem), then take the direct-IR
    /// path.  Callers that already hold the `Problem` should call
    /// [`ProverRunner::prove_ir`] and skip the round-trip entirely.
    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
        use std::time::Instant;

        let t_input = Instant::now();
        let ir_problem = match crate::trans::ir::parse_tptp(tptp) {
            Ok(p)  => p,
            Err(e) => {
                return ProverResult {
                    status:     ProverStatus::Unknown,
                    raw_output: format!("TPTP parse error: {e}"),
                    ..Default::default()
                };
            }
        };
        let parse_time = t_input.elapsed();

        let mut result = self.prove_ir(&ir_problem, &[], "query_0", opts);
        result.timings.input_gen += parse_time;
        result
    }

    /// Direct HO-IR entry: lower the THF [`HoProblem`] straight into the FFI
    /// solver's native Kernel structures — no TPTP text round-trip
    /// (`lower_ho.rs`), mirroring the first-order [`ProverRunner::prove_ir`].
    fn prove_ho(
        &self,
        problem:          &crate::trans::ir::HoProblem,
        _sid_map:         &[crate::types::SentenceId],
        _conjecture_name: &str,
        opts:             &ProverOpts,
    ) -> ProverResult {
        use std::time::Instant;

        let mut vp_opts = Options::new();
        let timeout = opts.timeout();
        if timeout > 0 {
            vp_opts.timeout(Duration::from_secs(timeout));
        }
        vp_opts.set_option("mode", "vampire");
        vp_opts.set_option("sine_selection", "off");

        let t = Instant::now();
        let mut sys_problem = match super::lower_ho::lower_ho_problem(problem, vp_opts) {
            Ok(p) => p,
            Err(msg) => {
                return ProverResult {
                    status:     ProverStatus::Unknown,
                    raw_output: format!("HOL lowering error: {msg}"),
                    ..Default::default()
                };
            }
        };
        let res = sys_problem.solve();
        let status = match (&opts.mode, &res) {
            (ProverMode::Prove, ProofRes::Proved) => ProverStatus::Proved,
            (ProverMode::Prove, ProofRes::Unprovable) => ProverStatus::Disproved,
            (_, ProofRes::Unknown(UnknownReason::Timeout)) => ProverStatus::Timeout,
            (ProverMode::CheckConsistency, ProofRes::Proved) => ProverStatus::Inconsistent,
            (ProverMode::CheckConsistency, ProofRes::Unprovable) => ProverStatus::Consistent,
            _ => ProverStatus::Unknown,
        };
        let mut result = ProverResult {
            status,
            raw_output: format!("embedded vampire (thf, native ffi): {res:?}"),
            ..Default::default()
        };
        result.timings.prover_run = t.elapsed();
        result
    }

    /// Direct-IR entry: lower the [`ir::Problem`](crate::trans::ir::Problem)
    /// straight into the FFI solver — no TPTP serialisation, no re-parse.
    /// (`sid_map` / `conjecture_name` are text-path concerns: the embedded
    /// proof steps map back by formula content, not by axiom name.)
    fn prove_ir(
        &self,
        ir_problem:       &crate::trans::ir::Problem,
        _sid_map:         &[crate::types::SentenceId],
        _conjecture_name: &str,
        opts:             &ProverOpts,
    ) -> ProverResult {
        use std::time::Instant;

        let input_gen = std::time::Duration::ZERO;

        let mut vp_opts = Options::new();
        // Per-call timeout from `opts` takes precedence (the autoscaling
        // loop varies it run-to-run); fall back to the runner's own field.
        let timeout = opts.timeout();
        if timeout > 0 {
            vp_opts.timeout(Duration::from_secs(timeout as u64));
        }
        match opts.mode {
            ProverMode::Prove => {
                vp_opts.set_option("mode", "vampire");
                vp_opts.set_option("sine_selection", "off");
            }
            ProverMode::CheckConsistency => {
                vp_opts.set_option("mode", "vampire");
                vp_opts.set_option("sine_selection", "off");
            }
        }

        let t_prover = Instant::now();
        let mut sys_problem = super::lower::lower_problem(ir_problem, vp_opts);
        let (res, proof_opt) = sys_problem.solve_and_prove();
        let prover_run = t_prover.elapsed();

        // Status mapping that matches the subprocess backend's
        // semantics (see `prover/vampire/subprocess.rs::determine_status`).
        // Three differences from the previous mapping:
        //
        // 1. `Unknown(Timeout)` is surfaced as `ProverStatus::Timeout`
        //    rather than being collapsed into `Unknown`.  Without
        //    this, the SDK test harness's "Unknown + non-empty
        //    raw_output ⇒ ProverError" heuristic misclassified a
        //    real timeout as a prover error (e.g. TQG3).
        //
        // 2. `CheckConsistency` inverts the satisfiability mapping:
        //    `Proved` in this mode means "axioms alone are
        //    unsatisfiable" → `Inconsistent`; `Unprovable` means
        //    "axioms are saturable" → `Consistent`.  Previously the
        //    mode was ignored, which silently produced wrong
        //    `Proved`/`Disproved` verdicts on `debug`/check_consistency
        //    runs routed through this backend.
        //
        // 3. The `Unknown(reason)` payload is preserved for diagnostic
        //    routing — `MemoryLimit` / `Incomplete` / `Other` still
        //    flow as `ProverStatus::Unknown` but with a more
        //    informative `raw_output` (see below).
        let status = match (&opts.mode, &res) {
            (ProverMode::Prove, ProofRes::Proved)     => ProverStatus::Proved,
            (ProverMode::Prove, ProofRes::Unprovable) => ProverStatus::Disproved,
            (ProverMode::CheckConsistency, ProofRes::Proved)     => ProverStatus::Inconsistent,
            (ProverMode::CheckConsistency, ProofRes::Unprovable) => ProverStatus::Consistent,
            (_, ProofRes::Unknown(UnknownReason::Timeout))       => ProverStatus::Timeout,
            (_, ProofRes::Unknown(_))                            => ProverStatus::Unknown,
        };

        let t_output = Instant::now();
        let (proof_kif, ir_proof) = if let Some(proof) = proof_opt.as_ref() {
            let kif   = super::native_proof::native_proof_to_kif_steps(proof);
            let ir    = super::native_proof::native_proof_to_ir_steps(proof);
            (kif, ir)
        } else {
            (vec![], vec![])
        };
        let output_parse = t_output.elapsed();

        // Distinguish a genuine Theorem from ContradictoryAxioms.  Embedded
        // Vampire's FFI collapses both into `Proved`; a `Proved` whose
        // refutation never used the negated conjecture means the selected
        // axioms alone derive ⊥ → report `Inconsistent`, matching the
        // subprocess backend (and `KnowledgeBase::ask_embedded`).
        let status = if matches!(opts.mode, ProverMode::Prove)
            && matches!(status, ProverStatus::Proved)
            && !proof_kif.is_empty()
            && !proof_kif.iter().any(|s| s.rule == "negated_conjecture")
        {
            ProverStatus::Inconsistent
        } else {
            status
        };

        // raw_output: human-readable summary echoing the same
        // intent as the subprocess backend's stdout transcript
        // (subprocess: a multi-line `% SZS …` log; embedded: a
        // one-line summary because the FFI doesn't expose Vampire's
        // stderr).  Keep this string stable enough that scrapers can
        // recognise the verdict, but don't try to forge SZS lines —
        // the consumer should be reading `status`, not parsing
        // text.
        //
        // TODO: the embedded backend doesn't populate `bindings` or
        // `proof_tptp` — both are non-trivial because they require
        // walking the native `Proof` for ground-term bindings and
        // re-serialising clauses to TPTP.  `proof_kif` already works
        // via `native_proof_to_kif_steps`; the other two should
        // follow the same shape but are gated on more FFI work in
        // `vampire-prover`.
        let raw_output = match &res {
            ProofRes::Proved     => "Proved".to_string(),
            ProofRes::Unprovable => "Unprovable (saturation found a counter-model)".to_string(),
            ProofRes::Unknown(UnknownReason::Timeout)     => "Timeout".to_string(),
            ProofRes::Unknown(UnknownReason::MemoryLimit) => "Memory limit exceeded".to_string(),
            ProofRes::Unknown(UnknownReason::Incomplete)  => "Inconclusive (incomplete decision procedure)".to_string(),
            ProofRes::Unknown(UnknownReason::Unknown)     => "Inconclusive (no reason reported)".to_string(),
            ProofRes::Unknown(UnknownReason::Other(msg))  => format!("Internal prover error: {msg}"),
        };

        // Map the native termination reason onto the backend-agnostic
        // [`TerminationReason`] the autoscaling loop consumes.
        let termination = match &res {
            ProofRes::Proved                              => None,
            ProofRes::Unprovable                          => Some(TerminationReason::Saturation),
            ProofRes::Unknown(UnknownReason::Timeout)     => Some(TerminationReason::TimeLimit),
            ProofRes::Unknown(UnknownReason::MemoryLimit) => Some(TerminationReason::ResourceOut),
            ProofRes::Unknown(UnknownReason::Incomplete)  => Some(TerminationReason::GaveUp),
            ProofRes::Unknown(UnknownReason::Unknown)     => Some(TerminationReason::GaveUp),
            ProofRes::Unknown(UnknownReason::Other(_))    => Some(TerminationReason::Other),
        };

        ProverResult {
            status,
            raw_output,
            termination,
            proof_kif,
            ir_proof,
            timings:    ProverTimings { input_gen, prover_run, output_parse },
            ..Default::default()
        }
    }
}
