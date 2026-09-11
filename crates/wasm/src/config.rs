//! WASM bindings for Prover Options
use sigmakee_rs_sdk::manager::NativeProverConfig;
use sigmakee_rs_sdk::{ExternalOpts, NativeOpts};
use wasm_bindgen::prelude::*;

// -- Config --------------------------------------------------------------------

/// Native-prover configuration exposed to JavaScript.
///
/// A wasm-bindgen property surface over the SDK's
/// [`NativeProverConfig`] (the serde-able subset of
/// [`NativeOpts`](sigmakee_rs_core::NativeOpts)); camelCase properties map
/// 1:1 to the `<prover type="native">` preference keys (`timeLimitSecs`,
/// `maxSteps`, `forwardClose`, `wantProof`, ...).  Per-query runtime fields
/// (`session`, `cancel`) are excluded.
///
/// On top of the wrapped config, one UI-facing selection knob
/// ([`selectionTolerancePct`](Self::selection_tolerance_pct)) expresses SInE
/// selection KB-relatively; it resolves to concrete
/// [`SineParams`](sigmakee_rs_core::SineParams) against the live KB's axiom
/// count at ask time (100 = the whole KB, no selection).
///
/// ```js
/// const cfg = new Config();
/// cfg.timeLimitSecs = 10;
/// cfg.wantProof = true;
/// prover.configure(cfg);
/// ```
#[wasm_bindgen]
#[derive(Clone)]
pub struct Config {
    inner: NativeProverConfig,
    selection_tolerance_pct: Option<f64>,
    backend: Backend,
    vampire_args: String,
    keep_tptp: bool,
}

/// Which prover a [`Session`](crate::Session) ask runs against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Backend {
    /// The in-process saturation prover.
    Native,
    /// The Emscripten Vampire, through the page-installed bridge (see
    /// [`crate::vampire`]).
    Vampire,
}

impl Config {
    pub(crate) fn selected_backend(&self) -> Backend {
        self.backend
    }

    pub(crate) fn vampire_args(&self) -> &str {
        &self.vampire_args
    }

    pub(crate) fn keep_tptp(&self) -> bool {
        self.keep_tptp
    }

    /// Build the external prover's [`ExternalOpts`] from these settings: the
    /// same time limit and selection budget the native backend reads.
    pub(crate) fn to_external_opts(&self, axiom_count: usize) -> ExternalOpts {
        let mut opts = ExternalOpts {
            timeout_secs: self.inner.time_limit_secs,
            ..ExternalOpts::default()
        };
        if let Some(pct) = self.selection_tolerance_pct {
            opts.selection = sigmakee_rs_core::SineParams::auto_pct(axiom_count, pct);
        }
        opts
    }

    /// Build a runtime [`NativeOpts`] seeded with these defaults; per-query
    /// `session` is layered on by the caller.
    ///
    /// `axiom_count` is the live KB's current
    /// (`KnowledgeBase::sine_axiom_count`) -- needed to turn
    /// [`Self::selection_tolerance_pct`] (a percentage) into the absolute
    /// SInE auto-budget the engine actually takes.
    pub(crate) fn to_native_opts(&self, axiom_count: usize) -> NativeOpts {
        let mut opts = self.inner.to_native_opts();
        if let Some(pct) = self.selection_tolerance_pct {
            opts.selection = sigmakee_rs_core::SineParams::auto_pct(axiom_count, pct);
        }
        opts
    }
}

#[wasm_bindgen]
impl Config {
    /// Construct a config with the native prover's defaults, except `wantProof`
    /// which is on (proofs are cheap to surface and useful in a UI).
    #[allow(
        clippy::new_without_default,
        reason = "wasm_bindgen constructor; a Default impl is unreachable from JS"
    )]
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: NativeProverConfig {
                want_proof: true,
                ..NativeProverConfig::default()
            },
            selection_tolerance_pct: None,
            backend: Backend::Native,
            vampire_args: String::new(),
            keep_tptp: false,
        }
    }

    /// Which prover `ask` / `auditConsistency` run: `"native"` (default) or
    /// `"vampire"` (the Emscripten Vampire behind the page's bridge).  Any
    /// other value is rejected.
    #[wasm_bindgen(getter)]
    pub fn backend(&self) -> String {
        match self.backend {
            Backend::Native => "native".into(),
            Backend::Vampire => "vampire".into(),
        }
    }
    #[wasm_bindgen(setter)]
    pub fn set_backend(&mut self, v: &str) -> Result<(), JsValue> {
        self.backend = match v {
            "native" => Backend::Native,
            "vampire" => Backend::Vampire,
            other => {
                return Err(JsValue::from_str(&format!(
                    "unknown backend {other:?}: expected \"native\" or \"vampire\""
                )))
            }
        };
        Ok(())
    }

    /// Extra Vampire CLI text appended after the fixed arguments (Vampire
    /// backend only; later flags win).
    #[wasm_bindgen(getter = vampireArgs)]
    pub fn vampire_args_js(&self) -> String {
        self.vampire_args.clone()
    }
    #[wasm_bindgen(setter = vampireArgs)]
    pub fn set_vampire_args(&mut self, v: String) {
        self.vampire_args = v;
    }

    /// Keep the exact problem text handed to Vampire on the last run and
    /// return it as the result's `input_tptp` (Vampire backend only).
    #[wasm_bindgen(getter = keepTptp)]
    pub fn keep_tptp_js(&self) -> bool {
        self.keep_tptp
    }
    #[wasm_bindgen(setter = keepTptp)]
    pub fn set_keep_tptp(&mut self, v: bool) {
        self.keep_tptp = v;
    }

    /// Wall-clock budget in seconds (0 = unlimited; the step cap still bounds it).
    #[wasm_bindgen(getter = timeLimitSecs)]
    pub fn time_limit_secs(&self) -> u32 {
        self.inner.time_limit_secs as u32
    }
    #[wasm_bindgen(setter = timeLimitSecs)]
    pub fn set_time_limit_secs(&mut self, v: u32) {
        self.inner.time_limit_secs = v as u64;
    }

    /// Maximum given-clause steps before the loop gives up.
    #[wasm_bindgen(getter = maxSteps)]
    pub fn max_steps(&self) -> u32 {
        self.inner.max_steps as u32
    }
    #[wasm_bindgen(setter = maxSteps)]
    pub fn set_max_steps(&mut self, v: u32) {
        self.inner.max_steps = v as usize;
    }

    /// Maximum literals per retained clause.
    #[wasm_bindgen(getter = maxLits)]
    pub fn max_lits(&self) -> u32 {
        self.inner.max_lits as u32
    }
    #[wasm_bindgen(setter = maxLits)]
    pub fn set_max_lits(&mut self, v: u32) {
        self.inner.max_lits = v as usize;
    }

    /// Run forward-closure over the theory before the given-clause loop.
    #[wasm_bindgen(getter = forwardClose)]
    pub fn forward_close(&self) -> bool {
        self.inner.forward_close
    }
    #[wasm_bindgen(setter = forwardClose)]
    pub fn set_forward_close(&mut self, v: bool) {
        self.inner.forward_close = v;
    }

    /// Populate the `proof` array on a `Proved` result.
    #[wasm_bindgen(getter = wantProof)]
    pub fn want_proof(&self) -> bool {
        self.inner.want_proof
    }
    #[wasm_bindgen(setter = wantProof)]
    pub fn set_want_proof(&mut self, v: bool) {
        self.inner.want_proof = v;
    }

    /// Emit phase-timing spans into `raw_output`.
    #[wasm_bindgen(getter)]
    pub fn profile(&self) -> bool {
        self.inner.profile
    }
    #[wasm_bindgen(setter)]
    pub fn set_profile(&mut self, v: bool) {
        self.inner.profile = v;
    }

    /// SInE selection budget, as a percentage (0-100) of the KB's total
    /// axiom count -- how much of the ontology a query-relevant selection is
    /// allowed to admit. `null`/`undefined` (the default) uses the engine's
    /// own default budget (a fixed axiom count, not a percentage -- see
    /// `SineParams::default`) instead of a KB-relative one; `100` emits the
    /// whole KB with no selection. Applies to BOTH the
    /// native backend (as the auto-tolerance loop's starting budget, which
    /// may still widen from there) and Vampire (as the final, one-shot
    /// budget -- see [`Session::to_tptp_for_ask`](crate::Session::to_tptp_for_ask)).
    #[wasm_bindgen(getter = selectionTolerancePct)]
    pub fn selection_tolerance_pct(&self) -> Option<f64> {
        self.selection_tolerance_pct
    }
    #[wasm_bindgen(setter = selectionTolerancePct)]
    pub fn set_selection_tolerance_pct(&mut self, v: Option<f64>) {
        self.selection_tolerance_pct = v;
    }
}
