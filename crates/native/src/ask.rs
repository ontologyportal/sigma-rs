use std::path::PathBuf;

use sumo_kb::{KnowledgeBase, VampireRunner, ProverStatus, TptpLang};

pub use sumo_kb::Binding;

// ── AskOptions / AskResult ────────────────────────────────────────────────────

/// Options for `ask()`.
#[derive(Debug, Default)]
pub struct AskOptions {
    /// Path to the Vampire executable. Defaults to `"vampire"` (PATH lookup).
    pub vampire_path: Option<PathBuf>,
    /// Timeout passed to Vampire in seconds. Defaults to 30.
    pub timeout_secs: Option<u32>,
    /// If true, keep the temporary TPTP file after the call.
    /// NOTE: no longer supported by the underlying VampireRunner; this field
    /// is accepted for source compatibility but has no effect.
    pub keep_tmp_file: bool,
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
}

// ── ask() ─────────────────────────────────────────────────────────────────────

/// Assert a conjecture and run Vampire to attempt a proof.
pub fn ask(kb: &mut KnowledgeBase, query_kif: &str, opts: AskOptions) -> AskResult {
    log::debug!("ask: {}", query_kif);

    let timeout_secs = opts.timeout_secs.unwrap_or(30);

    let result = match opts.backend.as_str() {
        #[cfg(feature = "integrated-prover")]
        "embedded" => {
            if matches!(opts.lang, TptpLang::Tff) {
                return AskResult {
                    proved:     false,
                    raw_output: String::new(),
                    errors:     vec!["TFF is not yet supported with the embedded prover backend".into()],
                    inference:  Vec::new(),
                };
            }
            kb.ask_embedded(query_kif, opts.session.as_deref(), timeout_secs)
        }
        _ => {
            let vampire_path = opts.vampire_path.unwrap_or_else(|| PathBuf::from("vampire"));
            let runner = VampireRunner { vampire_path, timeout_secs };
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
    }
}
