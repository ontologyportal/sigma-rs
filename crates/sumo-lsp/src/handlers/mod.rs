// crates/sumo-lsp/src/handlers/mod.rs
//
// LSP request / notification handlers.  Each handler takes the
// shared `GlobalState` and whatever payload the LSP message
// provides, and returns the response (or publishes a notification
// via the caller-provided sender).

pub mod diagnostics;

pub use diagnostics::publish_diagnostics;
