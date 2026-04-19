// crates/sumo-lsp/src/handlers/workspace_symbols.rs
//
// `workspace/symbol` handler.  Iterates every interned symbol in
// the shared KB, filters by a case-insensitive substring match on
// the `query` string, and emits a `SymbolInformation` pointing at
// the symbol's defining sentence (first-declaration heuristic via
// `KnowledgeBase::defining_sentence`).
//
// Substring is the MVP ranking.  Phase 5+ can upgrade to a
// fuzzy-score match; until then, VSCode / Neovim / Helix all
// accept the raw list and do their own client-side filtering
// anyway, so the user experience is identical.

use lsp_types::{
    Location, SymbolInformation, SymbolKind, WorkspaceSymbolParams,
    WorkspaceSymbolResponse,
};
use ropey::Rope;

use crate::conv::{span_to_range, tag_to_uri};
use crate::state::GlobalState;

/// Hard cap on how many symbols we stream back per query.  The
/// spec permits more, but clients paginate poorly and a 20k-symbol
/// ontology is large enough to freeze a naive client on "empty
/// query".  Clients with stronger filtering can raise this by
/// issuing multiple narrower queries.
const MAX_RESULTS: usize = 500;

pub fn handle_workspace_symbols(
    state:  &GlobalState,
    params: WorkspaceSymbolParams,
) -> Option<WorkspaceSymbolResponse> {
    let query = params.query.to_lowercase();

    let docs = state.docs.read().ok()?;
    let kb   = state.kb.read().ok()?;

    let mut out: Vec<SymbolInformation> = Vec::new();
    for (_id, name) in kb.iter_symbols() {
        // Skolem symbols don't belong in a user-facing symbol list.
        if kb.symbol_is_skolem(name) { continue; }
        if !query.is_empty() && !name.to_lowercase().contains(&query) { continue; }

        let Some((_sid, span)) = kb.defining_sentence(name) else { continue; };
        let Some(uri)          = tag_to_uri(&span.file)     else { continue; };

        let range = if let Some(td) = docs.get(&uri) {
            span_to_range(&td.rope, &span)
        } else {
            let text = uri.to_file_path().ok()
                .and_then(|p| std::fs::read_to_string(&p).ok())
                .unwrap_or_default();
            let rope = Rope::from_str(&text);
            span_to_range(&rope, &span)
        };

        let kind = classify_symbol(&kb, name);

        #[allow(deprecated)]  // `deprecated` still required by the type
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

/// Map a sumo-kb symbol to an LSP `SymbolKind` heuristic.  Same
/// ordering priority as the document-symbol handler so identical
/// symbols present consistently in both views.
fn classify_symbol(kb: &sumo_kb::KnowledgeBase, name: &str) -> SymbolKind {
    let Some(id) = kb.symbol_id(name) else { return SymbolKind::NULL; };
    if kb.is_class(id)       { return SymbolKind::CLASS; }
    if kb.is_function(id)    { return SymbolKind::FUNCTION; }
    if kb.is_predicate(id)   { return SymbolKind::INTERFACE; }
    if kb.is_relation(id)    { return SymbolKind::INTERFACE; }
    if kb.is_instance(id)    { return SymbolKind::CONSTANT; }
    SymbolKind::VARIABLE
}
