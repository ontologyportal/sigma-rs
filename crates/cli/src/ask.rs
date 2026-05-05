use std::path::PathBuf;

use sigmakee_rs_core::{KnowledgeBase, VampireRunner, ProverStatus, TptpLang};
pub use sigmakee_rs_core::prover::ProverTimings;

pub use sigmakee_rs_core::Binding;

// -- AskOptions / AskResult ----------------------------------------------------

/// Options for `ask()`.
#[derive(Debug, Default)]
pub struct AskOptions {
    /// Path to the Vampire executable. Defaults to `"vampire"` (PATH lookup).
    pub vampire_path: Option<PathBuf>,
    /// Timeout passed to Vampire in seconds. Defaults to 30.
    pub timeout_secs: Option<u32>,
    /// If set, write the generated TPTP to this path (subprocess backend only).
    /// When `None`, TPTP is piped directly to Vampire via stdin.
    pub tptp_dump_path: Option<PathBuf>,
    /// Session whose assertions are included as TPTP hypotheses.
    pub session: Option<String>,
    /// Prover backend: "subprocess" (default) or "embedded".
    pub backend: String,
    /// TPTP language to use when generating the problem file.
    pub lang: TptpLang,
}

/// Result from `ask()`.
#[derive(Debug)]
pub struct AskResult {
    /// True iff Vampire reported `SZS status Theorem`.
    pub proved: bool,
    /// Raw stdout + stderr from Vampire.
    pub raw_output: String,
    /// Any errors that prevented the call (parse, validation, I/O).
    pub errors: Vec<String>,
    /// Variable binding inferences extracted from the proof.
    pub inference: Vec<Binding>,
    /// Per-phase timing breakdown.
    pub timings: ProverTimings,
}

// -- ask() ---------------------------------------------------------------------

/// Assert a conjecture and run Vampire to attempt a proof.
pub fn ask(kb: &mut KnowledgeBase, query_kif: &str, opts: AskOptions) -> AskResult {
    log::debug!("ask: {}", query_kif);

    let timeout_secs = opts.timeout_secs.unwrap_or(30);

    let result = match opts.backend.as_str() {
        #[cfg(feature = "integrated-prover")]
        "embedded" => {
            kb.ask_embedded(query_kif, opts.session.as_deref(), timeout_secs, opts.lang)
        }
        _ => {
            let vampire_path = opts.vampire_path.unwrap_or_else(|| PathBuf::from("vampire"));
            let runner = VampireRunner { vampire_path, timeout_secs, tptp_dump_path: opts.tptp_dump_path };
            kb.ask(query_kif, opts.session.as_deref(), &runner, opts.lang)
        }
    };

    let proved = matches!(result.status, ProverStatus::Proved);

    // Surface prover-level failures (parse errors, I/O errors) as `errors`.
    let errors = if matches!(result.status, ProverStatus::Unknown) && !result.raw_output.is_empty() {
        vec![result.raw_output.clone()]
    } else {
        vec![]
    };

    AskResult {
        proved,
        raw_output: result.raw_output,
        errors,
        inference: result.bindings,
        timings: result.timings,
    }
}
