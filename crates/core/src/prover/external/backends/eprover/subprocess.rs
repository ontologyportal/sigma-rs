// crates/core/src/prover/eprover/subprocess.rs
//
// EproverRunner -- subprocess-based E (eprover) prover.
//
// Mirrors `vampire::subprocess::VampireRunner`: take a TPTP string, run the
// prover, return a backend-agnostic `ProverResult`.  The differences from the
// Vampire runner are all in E's surface dialect:
//
//   * stdin is fed via the filename `-` (E rejects `/dev/stdin` as "not a
//     regular file", unlike Vampire).
//   * verdict markers are `#`-prefixed and E declares time/memory exhaustion
//     with `# Failure: Resource limit exceeded (...)` rather than an SZS line.
//   * proof steps are named `c_0_N` / `i_0_N`, and the `inference(...)` terms
//     nest and carry a trailing `['proof']` annotation - so parent references
//     are resolved by name-membership over the whole annotation (see
//     `resolve_parents`) rather than Vampire's "last `[...]` bracket" rule.
//
// Proof-graph -> bindings / KIF / IR translation is shared with Vampire via
// `crate::prover::tptp_proof`.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use super::super::{ProverOpts, ProverRunner};
use super::build_eprover_args;
use crate::prover::result::{ProverResult, ProverStatus, ProverTimings};

// -- EproverRunner -------------------------------------------------------------

/// Spawns E (`eprover`) as a subprocess.
#[derive(Debug, Clone)]
pub struct EproverRunner {
    pub eprover_path: PathBuf,
    /// If set, write the generated TPTP to this path before running E.  When
    /// `None` the TPTP is piped directly to E's stdin with no intermediate file.
    pub tptp_dump_path: Option<PathBuf>,
}

impl Default for EproverRunner {
    fn default() -> Self {
        Self {
            eprover_path: PathBuf::from("eprover"),
            tptp_dump_path: None,
        }
    }
}

impl ProverRunner for EproverRunner {
    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
        if let Some(path) = &self.tptp_dump_path {
            if let Err(e) = write_file(path, tptp) {
                crate::log!(
                    Warn,
                    "sigmakee_rs_core::prover",
                    format!("failed to write TPTP dump to {}: {}", path.display(), e)
                );
            } else {
                crate::log!(
                    Info,
                    "sigmakee_rs_core::prover",
                    format!("wrote TPTP dump: {}", path.display())
                );
            }
        }

        // Per-call timeout from `opts` takes precedence (the autoscaling loop
        // varies it run-to-run); fall back to the runner's own field at 0.
        let secs = opts.timeout();
        let args = build_eprover_args(secs);

        crate::log!(
            Debug,
            "sigmakee_rs_core::prover",
            format!(
                "eprover: {} {} -",
                self.eprover_path.display(),
                args.join(" ")
            )
        );
        crate::log!(
            Info,
            "sigmakee_rs_core::prover",
            "starting eprover prover".to_string()
        );

        // E reads the problem from stdin when handed the filename `-`.
        let mut child = match Command::new(&self.eprover_path)
            .args(&args)
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                return ProverResult {
                    status: ProverStatus::Unknown,
                    raw_output: format!("Failed to spawn eprover: {}", e),
                    ..Default::default()
                }
            }
        };

        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(tptp.as_bytes()) {
                crate::log!(
                    Warn,
                    "sigmakee_rs_core::prover",
                    format!("failed to write to eprover stdin: {}", e)
                );
            }
        }

        let t_prover = Instant::now();
        let output = child.wait_with_output();
        let prover_run = t_prover.elapsed();

        match output {
            Err(e) => ProverResult {
                status: ProverStatus::Unknown,
                raw_output: format!("Failed to run eprover: {}", e),
                timings: ProverTimings {
                    prover_run,
                    ..Default::default()
                },
                ..Default::default()
            },
            Ok(out) => super::result_from_transcript(
                &String::from_utf8_lossy(&out.stdout),
                &String::from_utf8_lossy(&out.stderr),
                opts.mode,
                prover_run,
            ),
        }
    }
}

fn write_file(path: &Path, content: &str) -> std::io::Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(content.as_bytes())
}
