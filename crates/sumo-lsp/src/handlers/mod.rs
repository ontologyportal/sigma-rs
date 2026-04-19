// crates/sumo-lsp/src/handlers/mod.rs
//
// LSP request / notification handlers.  Each handler takes the
// shared `GlobalState` and whatever payload the LSP message
// provides, and returns the response (or publishes a notification
// via the caller-provided sender).

pub mod diagnostics;
pub mod hover;
pub mod goto;
pub mod symbols;

pub use diagnostics::publish_diagnostics;
pub use hover::handle_hover;
pub use goto::handle_goto_definition;
pub use symbols::handle_document_symbol;
