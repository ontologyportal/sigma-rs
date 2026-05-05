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
/// loaded `file_roots`, then applies adds / removes via the SDK's
/// [`sigmakee_rs_sdk::IngestOp`].  Returns the files that were added + those
/// that were removed so callers can republish diagnostics for each.
///
/// # SDK migration
///
/// File reading and per-source dispatch (reconcile vs. fresh-load)
/// are delegated to `IngestOp`.  The handler keeps the bits the SDK
/// has no opinion on:
///
/// - The "rebuild is cheaper than incremental remove" heuristic
///   (KB-wide concern, not an ingest concern).
/// - The actual `kb.remove_file` calls when the incremental path is
///   chosen (the SDK is additive — ingest only).
/// - Translating SDK [`sigmakee_rs_sdk::SdkError::Io`] failures into the
///   per-tag `failed` vec the protocol returns.
pub fn handle_set_active_files(
    state:  &GlobalState,
    params: SetActiveFilesParams,
) -> SetActiveFilesReport {
    use std::collections::HashSet;
    use sigmakee_rs_sdk::{IngestOp, SdkError};

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
    let mut kb = state.kb.write().expect("kb lock not poisoned");

    // Decide which files the SDK should ingest, and whether to
    // discard the existing KB first.
    let files_to_ingest: Vec<String> = if rebuild_is_cheaper {
        *kb = sigmakee_rs_core::KnowledgeBase::new();
        report.removed = currently_loaded.into_iter().collect();
        requested.into_iter().collect()
    } else {
        for tag in &to_remove {
            kb.remove_file(tag);
            report.removed.push(tag.clone());
        }
        to_add
    };

    // Drive the SDK's IngestOp.  It handles the file reads and
    // dispatches each source to `reconcile_file` (already-known tag)
    // or `kb.load_kif` + axiomatic-promotion (fresh tag).  Per-source
    // I/O failures bubble out as `SdkError::Io` and we translate them
    // into the per-tag `failed` entries the protocol expects.
    //
    // `IngestOp::run` aborts on the FIRST failure, so we drive it
    // file-by-file rather than as one batch — that way a single
    // bad-read file doesn't take down the whole set.
    for tag in files_to_ingest {
        let op_result = IngestOp::new(&mut *kb).add_file(&tag).run();
        match op_result {
            Ok(ingest_report) => {
                for s in &ingest_report.sources {
                    if !s.semantic_warnings.is_empty() {
                        log::warn!(target: "sumo_lsp::kb",
                            "setActiveFiles: '{}' surfaced {} semantic warning(s)",
                            s.tag, s.semantic_warnings.len());
                    }
                }
                report.added.push(tag);
            }
            Err(SdkError::Io { source, .. }) => {
                log::warn!(target: "sumo_lsp::kb",
                    "setActiveFiles: cannot read '{}': {}", tag, source);
                report.failed.push((tag, source.to_string()));
            }
            Err(SdkError::Kb(e)) => {
                // Parse failures land here.  Surface as a load
                // warning (matches the legacy log shape) and still
                // record the file as added so diagnostics get
                // republished and the editor sees the squiggle.
                log::warn!(target: "sumo_lsp::kb",
                    "setActiveFiles: load '{}' surfaced KB error: {}", tag, e);
                report.added.push(tag);
            }
            Err(other) => {
                log::warn!(target: "sumo_lsp::kb",
                    "setActiveFiles: ingest of '{}' failed: {}", tag, other);
                report.failed.push((tag, other.to_string()));
            }
        }
    }

    drop(kb); // release the write lock before returning
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

