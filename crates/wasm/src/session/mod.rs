//! The wasm entry point: a [`Session`] pinned to the native-prover stack.
//!
//! Mirrors `sigmakee_rs_sdk::Session`, monomorphized for the browser --
//! wasm-bindgen cannot export generics, so this crate pins the one layer
//! stack the bundle ships ([`NativeStack`]) and wraps the SDK session in a
//! `#[wasm_bindgen]` class.  The module tree mirrors the SDK's
//! `session/` layout: `ingest` (loading / promotion), `ops` (validation /
//! translation / snapshot), `ask` (proving), `views` (query projections).

// Loading KIF into the KB and managing session assertions.
mod ingest;
// Non-proving ops: validate / translate / snapshot / threads.
mod ops;
// Proving ops: tell / ask / audit / clausify / Vampire transcript parsing.
mod ask;
// Query projections: search / manpage / taxonomy / stats / NL rendering.
mod views;

use std::sync::Arc;

use sigmakee_rs_sdk::{ExternalProverLayer, KnowledgeBase, Prover, ProverLayer, TranslationLayer};
use wasm_bindgen::prelude::*;

use crate::vampire::WasmExternalRunner;
use crate::Config;

/// The layer stack behind the wasm facade: the external layer (driving the
/// Vampire bridge) over the native prover over TPTP translation -- both
/// provers and TPTP export off one shared KB.
pub(crate) type NativeStack = ExternalProverLayer<ProverLayer<TranslationLayer>>;

/// Session name for the shared in-browser KB.
const WASM_SESSION: &str = "sumo-wasm";

/// A KIF knowledge base plus native saturation prover -- the browser
/// analogue of `sigmakee_rs_sdk::Session`.
#[wasm_bindgen]
pub struct Session {
    /// The knowledge base, held as a shared SDK `Session` so other facades
    /// over the SAME KB can be constructed from this one -- see [`WasmLsp`].
    /// wasm32 is single-threaded: the `RwLock` never contends, it is purely
    /// the sharing mechanism (`Arc` requires `Sync` access).
    ///
    /// [`WasmLsp`]: crate::WasmLsp
    pub(crate) session: std::sync::Arc<std::sync::RwLock<sigmakee_rs_sdk::Session<NativeStack>>>,
    config: Config,
    /// The runner installed in the external layer, kept here so the last
    /// problem text can be read back after an ask.
    pub(crate) runner: Arc<WasmExternalRunner>,
}

#[wasm_bindgen]
impl Session {
    /// Create an empty native-prover knowledge base with default [`Config`].
    ///
    /// Topped by [`ProverLayer<TranslationLayer>`] rather than a bare
    /// [`ProverLayer`] -- native proving AND TPTP export off one shared KB
    /// (see [`toTptpIndexed`](Session::to_tptp_indexed)), no dual KB.
    #[allow(
        clippy::new_without_default,
        reason = "wasm_bindgen constructor; a Default impl is unreachable from JS"
    )]
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        let config = Config::new();
        let runner = Arc::new(WasmExternalRunner::default());
        Self {
            // Explicit session name: `from_kb(.., None)` generates one from
            // `SystemTime::now()`, which panics on wasm32-unknown-unknown.
            session: std::sync::Arc::new(std::sync::RwLock::new(
                sigmakee_rs_sdk::Session::from_kb(
                    KnowledgeBase::new_external_native(Prover::Custom(runner.clone())),
                    Some(WASM_SESSION.to_string()),
                ),
            )),
            config,
            runner,
        }
    }

    /// Replace the active [`Config`] used by subsequent [`ask`](Session::ask) calls.
    #[wasm_bindgen]
    pub fn configure(&mut self, config: &Config) {
        self.config = config.clone();
        self.install_runner();
    }
}

impl Session {
    /// (Re)build the Vampire runner from the active config and install it in
    /// the external layer -- after a config change, and after a restore
    /// (which rebuilds the stack with no runner).
    pub(crate) fn install_runner(&mut self) {
        self.runner = Arc::new(WasmExternalRunner::new(&self.config));
        self.session
            .write()
            .expect("kb lock not poisoned")
            .set_runner(Prover::Custom(self.runner.clone()));
    }
}
