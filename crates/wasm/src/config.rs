//! WASM bindings for Prover Options
use sigmakee_rs_sdk::manager::NativeProverConfig;
use sigmakee_rs_sdk::NativeOpts;
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
/// On top of the wrapped config, two UI-facing selection knobs
/// ([`selectAll`](Self::select_all) /
/// [`selectionTolerancePct`](Self::selection_tolerance_pct)) express SInE
/// selection KB-relatively; they resolve to concrete
/// [`SineParams`](sigmakee_rs_core::SineParams) against the live KB's axiom
/// count at ask time.
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
    select_all: bool,
    selection_tolerance_pct: Option<f64>,
}

impl Config {
    /// Build a runtime [`NativeOpts`] seeded with these defaults; per-query
    /// `session` is layered on by the caller.
    ///
    /// `axiom_count` is the live KB's current
    /// (`KnowledgeBase::sine_axiom_count`) -- needed to turn
    /// [`Self::selection_tolerance_pct`] (a percentage) into the absolute
    /// SInE auto-budget the engine actually takes.
    pub(crate) fn to_native_opts(&self, axiom_count: usize) -> NativeOpts {
        let mut opts = self.inner.to_native_opts();
        if self.select_all {
            opts.selection = sigmakee_rs_core::SineParams::whole_kb();
        } else if let Some(pct) = self.selection_tolerance_pct {
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
            select_all: false,
            selection_tolerance_pct: None,
        }
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

    /// Disable SInE axiom selection -- search the WHOLE promoted KB instead of
    /// a query-relevant subset. Off (`false`) by default, matching the
    /// engine's own default (`SineParams::default()`, auto-budget SInE on);
    /// `true` uses `SineParams::whole_kb()`. Slower and more memory-hungry,
    /// but sidesteps selection ever excluding an axiom the query actually
    /// needs -- useful for debugging a query that fails under selection.
    #[wasm_bindgen(getter = selectAll)]
    pub fn select_all(&self) -> bool {
        self.select_all
    }
    #[wasm_bindgen(setter = selectAll)]
    pub fn set_select_all(&mut self, v: bool) {
        self.select_all = v;
    }

    /// SInE selection budget, as a percentage (0-100) of the KB's total
    /// axiom count -- how much of the ontology a query-relevant selection is
    /// allowed to admit. `null`/`undefined` (the default) uses the engine's
    /// own default budget (a fixed axiom count, not a percentage -- see
    /// `SineParams::default`) instead of a KB-relative one. Ignored when
    /// [`Self::selectAll`](Self::select_all) is set. Applies to BOTH the
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
