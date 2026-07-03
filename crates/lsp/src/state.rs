//! Per-server and per-document state.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::sync::atomic::AtomicBool;

use lsp_types::Url;
use ropey::Rope;

use sigmakee_rs_core::{KnowledgeBase, ParsedDocument};

/// Per-document session state held by the server.
pub struct DocState {
    /// Authoritative text buffer. LSP incremental changes are applied to this
    /// rope; `String::from(&rope)` is fed into `parse_document` on reparse.
    pub rope:    Rope,
    /// The LSP client's last-seen version number for this document. Diagnostics
    /// carry the same version so stale results can be discarded.
    pub version: i32,
    /// Most recent parse, corresponding to `rope` at `version`. `None` on
    /// freshly-opened docs until the first parse completes.
    pub parsed:  Option<ParsedDocument>,
}

impl DocState {
    /// Create a new document state from initial text and version.
    pub fn new(text: &str, version: i32) -> Self {
        Self {
            rope:    Rope::from_str(text),
            version,
            parsed:  None,
        }
    }

    /// Snapshot the current text as a plain `String`.
    pub fn text_string(&self) -> String {
        String::from(&self.rope)
    }
}

/// Server-wide shared state. Cloning is cheap — all fields are `Arc`s.
#[derive(Clone)]
pub struct GlobalState {
    /// The shared knowledge base.
    pub kb:   Arc<RwLock<KnowledgeBase>>,
    /// Per-URI document state.
    pub docs: Arc<RwLock<HashMap<Url, DocState>>>,
    /// Set to true once the client sends a `sumo/setActiveFiles` notification,
    /// taking authoritative control of KB membership. While `true`, `didOpen`
    /// does not auto-add files to the shared KB; it only publishes diagnostics
    /// for whatever is already loaded.
    pub client_manages_files: Arc<AtomicBool>,
    /// Semantic-error codes + names the client has opted out of (see the
    /// `sumo/setIgnoredDiagnostics` notification). Matched against both
    /// `SemanticError::code()` (e.g. `"E005"`) and `SemanticError::name()`
    /// (e.g. `"arity-mismatch"`). Empty by default.
    pub ignored_diagnostic_codes: Arc<RwLock<HashSet<String>>>,
}

impl GlobalState {
    /// Create a new, empty server state.
    pub fn new() -> Self {
        Self {
            kb:   Arc::new(RwLock::new(KnowledgeBase::new())),
            docs: Arc::new(RwLock::new(HashMap::new())),
            client_manages_files:     Arc::new(AtomicBool::new(false)),
            ignored_diagnostic_codes: Arc::new(RwLock::new(HashSet::new())),
        }
    }
}

impl Default for GlobalState {
    fn default() -> Self { Self::new() }
}
