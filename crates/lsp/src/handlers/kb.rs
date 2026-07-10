//! Custom LSP notifications for KB file membership and diagnostic filtering.
//!
//! Clients hand the server the authoritative set of files that make up the
//! active knowledge base(s) via `sumo/setActiveFiles`. The server replaces
//! its in-memory file population to match, keeping the occurrence index,
//! fingerprints, and semantic caches consistent.
//!
//! The `KnowledgeBase` is a single shared store, so clients wanting multiple
//! KBs open at once send the union of every active KB's files.

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
/// Computes a symmetric difference against the KB's currently-loaded files,
/// then applies adds / removes directly via [`sigmakee_rs_core::KnowledgeBase::load`].
/// Returns the files that were added and those that were removed so callers
/// can republish diagnostics for each.
pub fn handle_set_active_files(
    state:  &GlobalState,
    params: SetActiveFilesParams,
) -> SetActiveFilesReport {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use sigmakee_rs_sdk::{FileOrigin, LocalProvenance, Severity, SourceFile};

    let requested: HashSet<String> = params.files.into_iter().collect();

    // Snapshot under the read lock so the delta is computed without holding
    // the lock across the mutation calls (which take the write lock).
    let currently_loaded: HashSet<String> = {
        let session = state.session.read().expect("kb lock not poisoned");
        session.kb().iter_files().into_iter().collect()
    };

    let to_add:    Vec<String> = requested.difference(&currently_loaded).cloned().collect();
    let to_remove: Vec<String> = currently_loaded.difference(&requested).cloned().collect();

    // `KnowledgeBase::remove_file` is O(total occurrences in the KB) per call,
    // so removing many files individually is quadratic. When more files are
    // being removed than kept, discard the KB and rebuild only the requested
    // files instead.
    let rebuild_is_cheaper = to_remove.len() >= requested.len();

    log::info!(target: "sumo_lsp::kb",
        "setActiveFiles: {} requested, {} to add, {} to remove{}",
        requested.len(), to_add.len(), to_remove.len(),
        if rebuild_is_cheaper { " (rebuild path)" } else { "" });

    let mut report = SetActiveFilesReport::default();
    let mut session = state.session.write().expect("kb lock not poisoned");

    let files_to_ingest: Vec<String> = if rebuild_is_cheaper {
        *session = sigmakee_rs_sdk::Session::<sigmakee_rs_sdk::TranslationLayer>::new(crate::state::LSP_SESSION.to_string());
        report.removed = currently_loaded.into_iter().collect();
        requested.into_iter().collect()
    } else {
        for tag in &to_remove {
            session.kb_mut().remove_file(tag);
            report.removed.push(tag.to_string());
        }
        to_add
    };

    // Drive ingestion file-by-file rather than as one batch — a single
    // bad-read file must not take down the whole set.
    for tag in files_to_ingest {
        let contents = match std::fs::read_to_string(&tag) {
            Ok(c) => c,
            Err(e) => {
                log::warn!(target: "sumo_lsp::kb",
                    "setActiveFiles: cannot read '{}': {}", tag, e);
                report.failed.push((tag, e.to_string()));
                continue;
            }
        };
        let Some(src) = SourceFile::from_file(
            PathBuf::from(&tag), contents, FileOrigin::Local(LocalProvenance::UNKNOWN),
        ) else {
            log::warn!(target: "sumo_lsp::kb",
                "setActiveFiles: cannot determine a parser for '{}'", tag);
            report.failed.push((tag, "no parser could be determined for this file".to_string()));
            continue;
        };

        let result = session.kb_mut().load(src, &tag);
        // Promote so man-page introspection (Base scope) sees the file.
        if result.ok { let _ = session.kb_mut().make_session_axiomatic(&tag); }
        let warnings = result.diagnostics.iter()
            .filter(|d| matches!(d.severity, Severity::Warning))
            .count();
        if warnings > 0 {
            log::warn!(target: "sumo_lsp::kb",
                "setActiveFiles: '{}' surfaced {} semantic warning(s)", tag, warnings);
        }
        // Parse failures (`!result.ok`) are recorded as added anyway so
        // diagnostics get republished and the editor sees the squiggle.
        if !result.ok {
            log::warn!(target: "sumo_lsp::kb",
                "setActiveFiles: load '{}' surfaced parse-level diagnostics", tag);
        }
        report.added.push(tag);
    }

    drop(session);
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

