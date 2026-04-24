// crates/sumo-lsp/src/handlers/mod.rs
//
// LSP request / notification handlers.  Each handler takes the
// shared `GlobalState` and whatever payload the LSP message
// provides, and returns the response (or publishes a notification
// via the caller-provided sender).

pub mod completion;
pub mod diagnostics;
pub mod format;
pub mod goto;
pub mod hover;
pub mod kb;
pub mod references;
pub mod rename;
pub mod semantic_tokens;
pub mod symbols;
pub mod taxonomy;
pub mod workspace_symbols;

pub use completion::handle_completion;
pub use diagnostics::publish_diagnostics;
pub use format::{handle_formatting, handle_range_formatting};
pub use goto::handle_goto_definition;
pub use hover::handle_hover;
pub use kb::{
    handle_set_active_files, handle_set_ignored_diagnostics,
    SetActiveFilesParams, SetActiveFilesReport, SetIgnoredDiagnosticsParams,
    METHOD as SET_ACTIVE_FILES_METHOD,
    SET_IGNORED_DIAGNOSTICS_METHOD,
};
pub use references::handle_references;
pub use rename::handle_rename;
pub use semantic_tokens::{handle_semantic_tokens_full, semantic_tokens_legend};
pub use symbols::handle_document_symbol;
pub use taxonomy::{
    handle_taxonomy, TaxonomyEdgeDto, TaxonomyParams, TaxonomyRequest, TaxonomyResponse,
};
pub use workspace_symbols::handle_workspace_symbols;
