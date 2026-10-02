//! Browser external provers over the page-owned worker bridge.

use crate::config::{Backend, Config};
use sigmakee_rs_core::prover::external::backends::eprover;
use sigmakee_rs_sdk::{
    result_from_transcript, vampire_cli_args, ProverOpts, ProverResult, ProverRunner, ProverStatus,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};
use wasm_bindgen::prelude::*;

/// Backwards-compatible Vampire bridge name.
pub const BRIDGE_GLOBAL: &str = "__sigmaRunVampireSync";

/// External prover runner sharing transport and problem capture across E and Vampire.
#[derive(Debug, Default)]
pub struct WasmExternalRunner {
    backend: Backend,
    extra_args: String,
    keep_tptp: bool,
    axfilter: bool,
    subset_limit: u32,
    selection_budget: AtomicUsize,
    filter_seconds: u32,
    audit_limit: AtomicUsize,
    last_tptp: Mutex<Option<String>>,
}

impl WasmExternalRunner {
    pub(crate) fn new(config: &Config) -> Self {
        Self {
            backend: config.selected_backend(),
            extra_args: config.vampire_args().into(),
            keep_tptp: config.keep_tptp(),
            axfilter: config.audit_axfilter,
            subset_limit: config.audit_subset_limit,
            selection_budget: AtomicUsize::new(config.selection_budget as usize),
            filter_seconds: config.selection_time_limit_secs,
            audit_limit: AtomicUsize::new(5),
            last_tptp: Mutex::new(None),
        }
    }

    /// Set the maximum number of distinct contradictions for the next audit.
    pub fn set_audit_limit(&self, limit: usize) {
        self.audit_limit.store(limit.max(1), Ordering::Relaxed);
    }

    /// Set the resolved SInE target used by e_axfilter.
    pub fn set_selection_budget(&self, budget: usize) {
        self.selection_budget
            .store(budget.max(1), Ordering::Relaxed);
    }

    /// Exact input to the most recent external proving operation.
    pub fn last_tptp(&self) -> Option<String> {
        self.last_tptp.lock().ok().and_then(|g| g.clone())
    }

    fn bridge(
        &self,
        name: &str,
        tptp: &str,
        args: &str,
        timeout_ms: u32,
    ) -> Result<JsValue, String> {
        let global = js_sys::global();
        let value = js_sys::Reflect::get(&global, &JsValue::from_str(name))
            .map_err(|e| describe_js_error(&e))?;
        let function = value
            .dyn_ref::<js_sys::Function>()
            .ok_or_else(|| format!("The embedding page has not installed {name}"))?;
        function
            .call3(
                &global,
                &JsValue::from_str(tptp),
                &JsValue::from_str(args),
                &JsValue::from_f64(f64::from(timeout_ms)),
            )
            .map_err(|e| describe_js_error(&e))
    }

    fn run_once(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
        let (name, args) = if self.backend == Backend::E {
            // Browser deadlines belong to the worker bridge, not POSIX resource limits.
            (
                "__sigmaRunEproverSync",
                serde_json::to_string(&eprover::build_eprover_args(0)).unwrap_or_default(),
            )
        } else {
            (
                BRIDGE_GLOBAL,
                format!(
                    "{} {}",
                    vampire_cli_args(&opts.timeout().to_string()).join(" "),
                    self.extra_args.trim()
                ),
            )
        };
        let timeout_ms = if opts.timeout() == 0 {
            0
        } else {
            u32::try_from(opts.timeout().saturating_mul(1000))
                .unwrap_or(u32::MAX)
                .saturating_add(5000)
        };
        let started = js_sys::Date::now();
        let out = match self.bridge(name, tptp, &args, timeout_ms) {
            Ok(out) => out,
            Err(error) => {
                return ProverResult {
                    raw_output: error,
                    ..Default::default()
                }
            }
        };
        let elapsed =
            std::time::Duration::from_millis((js_sys::Date::now() - started).max(0.0) as u64);
        let stdout = string_field(&out, "stdout");
        let stderr = string_field(&out, "stderr");
        if self.backend == Backend::E {
            eprover::result_from_transcript(&stdout, &stderr, opts.mode, elapsed)
        } else {
            result_from_transcript(&stdout, &stderr, opts.mode, elapsed)
        }
    }

    fn audit_subsets(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
        let args = serde_json::json!({
            "budget": self.selection_budget.load(Ordering::Relaxed).clamp(1, 2_147_483_647),
            "subsetLimit": self.subset_limit,
        })
        .to_string();
        let out = match self.bridge(
            "__sigmaRunAxfilterSync",
            tptp,
            &args,
            self.filter_seconds.saturating_mul(1000),
        ) {
            Ok(out) => out,
            Err(error) => {
                return ProverResult {
                    raw_output: error,
                    ..Default::default()
                }
            }
        };
        let files =
            js_sys::Reflect::get(&out, &JsValue::from_str("subsets")).unwrap_or(JsValue::UNDEFINED);
        if !js_sys::Array::is_array(&files) {
            return eprover::result_from_transcript(
                &string_field(&out, "stdout"),
                &string_field(&out, "stderr"),
                opts.mode,
                std::time::Duration::ZERO,
            );
        }
        let files = js_sys::Array::from(&files);
        let mut results = Vec::new();
        for subset in files.iter() {
            let Some(text) = subset.as_string() else {
                continue;
            };
            let result = self.run_once(&text, opts);
            let stop = matches!(
                result.status,
                ProverStatus::Timeout | ProverStatus::InputError | ProverStatus::Unknown
            );
            results.push(result);
            let aggregate = eprover::aggregate_audits(&results, files.length() as usize);
            if stop
                || aggregate.contradiction_proofs.len() >= self.audit_limit.load(Ordering::Relaxed)
            {
                break;
            }
        }
        let mut result = eprover::aggregate_audits(&results, files.length() as usize);
        result
            .raw_output
            .push_str(&format!("\n{}", string_field(&out, "stderr")));
        result
    }
}

fn describe_js_error(e: &JsValue) -> String {
    e.dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| e.as_string())
        .unwrap_or_else(|| format!("{e:?}"))
}

fn string_field(obj: &JsValue, key: &str) -> String {
    js_sys::Reflect::get(obj, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}

impl ProverRunner for WasmExternalRunner {
    fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
        if self.keep_tptp {
            if let Ok(mut last) = self.last_tptp.lock() {
                *last = Some(tptp.into());
            }
        }
        if self.backend == Backend::E
            && self.axfilter
            && opts.mode == sigmakee_rs_sdk::ProverMode::CheckConsistency
        {
            self.audit_subsets(tptp, opts)
        } else {
            self.run_once(tptp, opts)
        }
    }

    fn name(&self) -> &str {
        if self.backend == Backend::E {
            "eprover-wasm"
        } else {
            "vampire-wasm"
        }
    }
}
