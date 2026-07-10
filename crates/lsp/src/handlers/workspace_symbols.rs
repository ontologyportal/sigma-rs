//! `workspace/symbol` handler. Iterates every interned symbol in the shared KB,
//! filters by a case-insensitive substring match on the `query` string, and
//! emits a `SymbolInformation` pointing at the symbol's defining sentence
//! (first-declaration heuristic via `KnowledgeBase::defining_sentence`).

use lsp_types::{
    Location, SymbolInformation, SymbolKind, WorkspaceSymbolParams,
    WorkspaceSymbolResponse,
};

use crate::conv::{span_to_range_with_fallback, tag_to_uri};
use crate::state::GlobalState;

/// Hard cap on how many symbols are streamed back per query.
const MAX_RESULTS: usize = 500;

/// Handle a `workspace/symbol` request, returning up to [`MAX_RESULTS`] symbols
/// whose names contain `query` (case-insensitive). Returns `None` if the shared
/// state locks cannot be acquired.
pub fn handle_workspace_symbols(
    state:  &GlobalState,
    params: WorkspaceSymbolParams,
) -> Option<WorkspaceSymbolResponse> {
    let query = params.query.to_lowercase();

    let docs    = state.docs.read().ok()?;
    let session = state.session.read().ok()?;
    let kb      = session.kb();

    let mut out: Vec<SymbolInformation> = Vec::new();
    for (_id, name) in kb.iter_symbols() {
        let name = name.as_str();
        // Skolem symbols don't belong in a user-facing symbol list.
        if kb.symbol_is_skolem(name) { continue; }
        if !query.is_empty() && !name.to_lowercase().contains(&query) { continue; }

        let Some((_sid, span)) = kb.defining_sentence(name) else { continue; };
        let Some(uri)          = tag_to_uri(&span.file)     else { continue; };

        let range = span_to_range_with_fallback(&docs, &uri, &span);

        let kind = classify_symbol(kb, name);

        #[allow(deprecated)]  // field required by the lsp_types struct
        out.push(SymbolInformation {
            name:          name.to_string(),
            kind,
            tags:          None,
            deprecated:    None,
            location:      Location { uri, range },
            container_name: None,
        });

        if out.len() >= MAX_RESULTS { break; }
    }

    Some(WorkspaceSymbolResponse::Flat(out))
}

/// Map a sigmakee-rs-core symbol to an LSP `SymbolKind` by taxonomy role.
fn classify_symbol(kb: &sigmakee_rs_sdk::KnowledgeBase, name: &str) -> SymbolKind {
    let Some(id) = kb.symbol_id(name) else { return SymbolKind::NULL; };
    if kb.is_class(id)       { return SymbolKind::CLASS; }
    if kb.is_function(id)    { return SymbolKind::FUNCTION; }
    if kb.is_predicate(id)   { return SymbolKind::INTERFACE; }
    if kb.is_relation(id)    { return SymbolKind::INTERFACE; }
    if kb.is_instance(id)    { return SymbolKind::CONSTANT; }
    SymbolKind::VARIABLE
}
