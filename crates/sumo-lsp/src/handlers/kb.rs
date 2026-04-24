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

    // `KnowledgeBase::remove_file` is O(total occurrences in the KB)
    // per call, so removing many files individually is quadratic.
    // When the remove cost would swamp a fresh load (rough heuristic:
    // more files are being removed than kept), throw the KB away and
    // rebuild only the requested files.  The threshold deliberately
    // errs on the side of rebuilding — the rebuild is cheap compared
    // to even a handful of remove_file calls on a large KB.
    let rebuild_is_cheaper = to_remove.len() >= requested.len();

    log::info!(target: "sumo_lsp::kb",
        "setActiveFiles: {} requested, {} to add, {} to remove{}",
        requested.len(), to_add.len(), to_remove.len(),
        if rebuild_is_cheaper { " (rebuild path)" } else { "" });

    let mut report = SetActiveFilesReport::default();

    {
        let mut kb = state.kb.write().expect("kb lock not poisoned");

        if rebuild_is_cheaper {
            // Wipe by replacing the KB; everything we care about
            // lives in the per-file stores, which a fresh KB
            // starts empty.  We then bulk-load the requested files.
            // The caller sees each previously-loaded file as
            // `removed` (for diagnostic republishing) and each
            // requested file as `added`.
            *kb = sumo_kb::KnowledgeBase::new();
            report.removed = currently_loaded.into_iter().collect();

            for tag in &requested {
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
        } else {
            // Incremental path.  Small delta, same-shape KB.
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

// -- sumo/setIgnoredDiagnostics ----------------------------------------------

/// Method name for the "set ignored diagnostic codes" notification.
pub const SET_IGNORED_DIAGNOSTICS_METHOD: &str = "sumo/setIgnoredDiagnostics";

/// Payload for [`SET_IGNORED_DIAGNOSTICS_METHOD`].  Each entry may
/// be either a `SemanticError::code()` (e.g. `"E005"`) or a
/// `SemanticError::name()` (e.g. `"arity-mismatch"`) -- the
/// handler matches both.  Anything not recognised is silently
/// kept in the set; the filter is lenient so a typo doesn't
/// crash the server.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetIgnoredDiagnosticsParams {
    /// Codes or names the client no longer wants published as
    /// diagnostics.  A fresh notification replaces the server-
    /// side set entirely; to clear the list, send `{"codes": []}`.
    pub codes: Vec<String>,
}

/// Apply a `sumo/setIgnoredDiagnostics` notification.
///
/// Replaces the server-side `ignored_diagnostic_codes` set with
/// the client's new list.  The caller is responsible for
/// re-publishing diagnostics for every open document so the UI
/// reflects the change immediately.
pub fn handle_set_ignored_diagnostics(
    state:  &crate::state::GlobalState,
    params: SetIgnoredDiagnosticsParams,
) {
    use std::collections::HashSet;
    let new: HashSet<String> = params.codes.into_iter().collect();
    log::info!(target: "sumo_lsp::kb",
        "setIgnoredDiagnostics: {} code(s) ignored", new.len());
    if let Ok(mut guard) = state.ignored_diagnostic_codes.write() {
        *guard = new;
    }
}

