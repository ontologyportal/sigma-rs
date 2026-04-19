// crates/sumo-lsp/src/handlers/kb.rs
//
// Custom LSP notification: `sumo/setActiveFiles`.
//
// Clients that manage their own KB-membership model (e.g. the
// VSCode extension reading a SigmaKEE `config.xml`) hand the
// server the authoritative set of files that make up the active
// knowledge base(s).  The server replaces its in-memory file
// population to match, running the existing per-file load /
// remove primitives so that the rest of the state (occurrence
// index, fingerprints, semantic caches) stays consistent.
//
// This is a soft-merge: the server's `KnowledgeBase` is a single
// shared store.  Clients that want "multiple KBs open at once"
// send the UNION of every active KB's files; separating them
// visually (tree views, status-bar badges) is a client-side
// concern.

use serde::{Deserialize, Serialize};

use crate::state::GlobalState;

/// `sumo/setActiveFiles` notification payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetActiveFilesParams {
    /// Absolute canonical filesystem paths that should be loaded
    /// into the shared KB.  Files currently loaded but not in this
    /// list are removed; files in this list but not loaded are
    /// read from disk and ingested.
    pub files: Vec<String>,
}

/// Method name for the custom notification.
pub const METHOD: &str = "sumo/setActiveFiles";

/// Apply a `sumo/setActiveFiles` notification.
///
/// Computes a symmetric difference against the KB's currently-
/// loaded `file_roots`, then applies adds / removes via the
/// existing mutation surface.  Returns the files that were added
/// + those that were removed so callers can republish
/// diagnostics for each.
pub fn handle_set_active_files(
    state:  &GlobalState,
    params: SetActiveFilesParams,
) -> SetActiveFilesReport {
    use std::collections::HashSet;

    let requested: HashSet<String> = params.files.into_iter().collect();

    // Snapshot the current per-file population under the read
    // lock so we can compute the delta without holding the lock
    // across the mutation calls (which take the write lock).
    let currently_loaded: HashSet<String> = {
        let kb = state.kb.read().expect("kb lock not poisoned");
        kb.iter_files().map(|s| s.to_owned()).collect()
    };

    let to_add:    Vec<String> = requested.difference(&currently_loaded).cloned().collect();
    let to_remove: Vec<String> = currently_loaded.difference(&requested).cloned().collect();

    log::info!(target: "sumo_lsp::kb",
        "setActiveFiles: {} requested, {} to add, {} to remove",
        requested.len(), to_add.len(), to_remove.len());

    let mut report = SetActiveFilesReport::default();

    {
        let mut kb = state.kb.write().expect("kb lock not poisoned");

        // Remove first so stale cache entries aren't quoted by
        // the load pass.
        for tag in &to_remove {
            kb.remove_file(tag);
            report.removed.push(tag.clone());
        }

        for tag in &to_add {
            match std::fs::read_to_string(tag) {
                Ok(text) => {
                    let r = kb.load_kif(&text, tag, None);
                    if !r.ok {
                        log::warn!(target: "sumo_lsp::kb",
                            "setActiveFiles: load '{}' surfaced {} errors",
                            tag, r.errors.len());
                    }
                    report.added.push(tag.clone());
                }
                Err(e) => {
                    log::warn!(target: "sumo_lsp::kb",
                        "setActiveFiles: cannot read '{}': {}", tag, e);
                    report.failed.push((tag.clone(), e.to_string()));
                }
            }
        }
    }

    report
}

/// Snapshot of a `sumo/setActiveFiles` application.  Useful for
/// the caller that wants to republish diagnostics for the
/// affected files without holding any KB lock.
#[derive(Debug, Default, Clone)]
pub struct SetActiveFilesReport {
    pub added:   Vec<String>,
    pub removed: Vec<String>,
    pub failed:  Vec<(String, String)>,
}

