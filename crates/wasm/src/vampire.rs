//! The browser's Vampire runner: a [`ProverRunner`] over a synchronous JS
//! bridge to the Emscripten Vampire build.
//!
//! The Rust side is deliberately transport-free.  `prove` builds the same
//! command line the subprocess runner uses, calls
//! `globalThis.__sigmaRunVampireSync(tptp, args, timeoutMs)` -- which the
//! embedding page installs (the web demo parks the sigma worker on
//! `Atomics.wait` while a dedicated Vampire worker runs; Node can wrap
//! `spawnSync`) -- and turns the captured `{ stdout, stderr }` into a
//! [`ProverResult`] with the shared transcript parser, so a browser run and
//! a CLI run yield identical results for identical transcripts.

use std::sync::Mutex;

use sigmakee_rs_sdk::{
    result_from_transcript, vampire_cli_args, ProverOpts, ProverResult, ProverRunner, ProverStatus,
};
use wasm_bindgen::prelude::*;

/// The global the embedder installs.  Signature:
/// `(tptp: string, args: string, timeoutMs: number) => { stdout: string,
/// stderr: string, code?: number }`; throwing reports a bridge failure.
pub const BRIDGE_GLOBAL: &str = "__sigmaRunVampireSync";

/// Grace the bridge gets past Vampire's own `-t` budget before it is told
/// to give up on the run (module instantiation, proof printing).
const BRIDGE_GRACE_MS: u32 = 5_000;

/// A [`ProverRunner`] that drives Vampire through the JS bridge.
#[derive(Debug, Default)]
pub struct WasmVampireRunner {
    /// Raw extra CLI text appended after the fixed arguments (the settings
    /// panel's "extra CLI args"); later flags win in Vampire.
    pub extra_args: String,
    /// Retain the problem text of the most recent run (see
    /// [`last_tptp`](Self::last_tptp)).
    pub keep_tptp: bool,
    last_tptp: Mutex<Option<String>>,
}

impl WasmVampireRunner {
    pub fn new(extra_args: String, keep_tptp: bool) -> Self {
        Self {
            extra_args,
            keep_tptp,
            last_tptp: Mutex::new(None),
        }
    }

    /// The problem text of the most recent run, when `keep_tptp` is set.
    /// Under autoscaling this is the final attempt's problem.
    pub fn last_tptp(&self) -> Option<String> {
        self.last_tptp.lock().ok().and_then(|g| g.clone())
    }

    /// The full argument line for one run at `timeout_secs`.
    fn args(&self, timeout_secs: u64) -> String {
        let mut args = vampire_cli_args(&timeout_secs.to_string()).join(" ");
        let extra = self.extra_args.trim();
        if !extra.is_empty() {
            args.push(' ');
            args.push_str(extra);
        }
        args
    }

    fn call_bridge(&self, tptp: &str, args: &str, timeout_ms: u32) -> Result<JsValue, String> {
        let global = js_sys::global();
        let f = js_sys::Reflect::get(&global, &JsValue::from_str(BRIDGE_GLOBAL))
            .map_err(|e| format!("{e:?}"))?;
        let f = f.dyn_ref::<js_sys::Function>().ok_or_else(|| {
            format!(
                "globalThis.{BRIDGE_GLOBAL} is not installed: the Vampire (WASM) \
                 backend needs the embedding page to provide it"
            )
        })?;
        let out = f
            .call3(
                &global,
                &JsValue::from_str(tptp),
                &JsValue::from_str(args),
                &JsValue::from_f64(f64::from(timeout_ms)),
            )
            .map_err(|e| describe_js_error(&e))?;
        Ok(out)
    }
}

/// Render a thrown JS value as its message when it is an `Error`.
fn describe_js_error(e: &JsValue) -> String {
    e.dyn_ref::<js_sys::Error>()
        .map(|err| String::from(err.message()))
        .or_else(|| e.as_string())
        .unwrap_or_else(|| format!("{e:?}"))
}

fn string_field(obj: &JsValue, key: &str) -> String {
    js_sys::Reflect::get(obj, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}

impl ProverRunner for WasmVampireRunner {
    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
        if self.keep_tptp {
            if let Ok(mut g) = self.last_tptp.lock() {
                *g = Some(tptp.to_string());
            }
        }
        let secs = opts.timeout();
        let args = self.args(secs);
        // `-t 0` is unbounded for Vampire; the bridge gets no deadline either.
        let timeout_ms = if secs == 0 {
            0
        } else {
            u32::try_from(secs.saturating_mul(1_000))
                .unwrap_or(u32::MAX)
                .saturating_add(BRIDGE_GRACE_MS)
        };
        let started = js_sys::Date::now();
        let out = match self.call_bridge(tptp, &args, timeout_ms) {
            Ok(v) => v,
            Err(msg) => {
                return ProverResult {
                    status: ProverStatus::Unknown,
                    raw_output: format!("Vampire bridge error: {msg}"),
                    ..Default::default()
                }
            }
        };
        let prover_run =
            std::time::Duration::from_millis((js_sys::Date::now() - started).max(0.0) as u64);
        let stdout = string_field(&out, "stdout");
        let stderr = string_field(&out, "stderr");
        result_from_transcript(&stdout, &stderr, opts.mode, prover_run)
    }

    fn name(&self) -> &str {
        "vampire-wasm"
    }
}
