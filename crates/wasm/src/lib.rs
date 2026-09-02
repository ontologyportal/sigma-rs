// Threaded builds only: re-exports `initThreadPool`, the JS entry point that
// spins up the wasm-bindgen-rayon worker pool. Plain (non-`atomics`) wasm32
// builds never link this — `sigmakee-rs-core/parallel` itself is
// compile_error!-banned there, so the feature can only be on in a
// -Zbuild-std threads-enabled build. Nothing in this repo's build pipeline
// produces one; the feature is left available for external consumers.
#[cfg(feature = "parallel")]
pub use wasm_bindgen_rayon::init_thread_pool;

// -- Modules -------------------------------------------------------------------

pub mod config;
mod console_log;
pub mod lsp;
pub mod session;
pub mod types;

pub use config::*;
pub use lsp::*;
pub use session::*;
