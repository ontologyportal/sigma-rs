// crates/sumo-lsp/src/state.rs
//
// Per-server and per-document state.
//
// Design for MVP: a single shared `Arc<RwLock<KnowledgeBase>>` with
// `DashMap<Url, DocState>` for per-document data (rope, last parse,
// version).  No separate writer thread -- LSP requests arrive one at
// a time from the transport (lsp-server runs the reader/writer on
// internal threads but delivers messages single-file to the event
// loop), so the handler takes the write lock briefly for mutations
// and readers wait only a few milliseconds.  If contention becomes
// a bottleneck we'll split to an arc-swap + writer-thread pattern;
// this model is intentionally simple until then.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::sync::atomic::AtomicBool;

use lsp_types::Url;
use ropey::Rope;

use sumo_kb::{KnowledgeBase, ParsedDocument};

/// Per-document session state held by the server.
///
/// The rope is the authoritative text buffer -- LSP `didChange`
/// edits are applied to it incrementally.  `parsed` is regenerated
/// via `sumo_kb::parse_document` on every change.
pub struct DocState {
    /// Authoritative text buffer.  LSP incremental changes are
    /// applied to this rope; a full `String::from(&rope)` is fed
    /// into `parse_document` on reparse.
    pub rope:    Rope,
    /// The LSP client's last-seen version number for this document.
    /// Diagnostics carry the same version so stale results can be
    /// discarded.
    pub version: i32,
    /// Most recent parse.  Always corresponds to `rope` at version
    /// `version` (no stale parses).  `None` on freshly-opened docs
    /// until the first parse completes.
    pub parsed:  Option<ParsedDocument>,
}

impl DocState {
    pub fn new(text: &str, version: i32) -> Self {
        Self {
            rope:    Rope::from_str(text),
            version,
            parsed:  None,
        }
    }

    /// Snapshot the current text as a plain `String`.  Used to feed
    /// `parse_document`; avoids holding the rope alive in the parsed
    /// output.
    pub fn text_string(&self) -> String {
        String::from(&self.rope)
    }
}

/// Server-wide shared state.
///
/// Cloning `GlobalState` is cheap -- both fields are `Arc`s -- so
/// handlers receive their own handles without contention.
#[derive(Clone)]
pub struct GlobalState {
    /// The shared knowledge base.  Writers hold the write lock
    /// briefly during didChange / workspace-load mutations;
    /// readers hold the read lock during hover / definition /
    /// symbol queries (Phase 3+).
    pub kb:   Arc<RwLock<KnowledgeBase>>,
    /// Per-URI document state.  Wrapped in a `Mutex<HashMap>` for
    /// simplicity; the MVP traffic pattern (sequential LSP requests)
    /// doesn't benefit from finer locking.
    pub docs: Arc<RwLock<HashMap<Url, DocState>>>,
    /// Set to true once the client sends a `sumo/setActiveFiles`
    /// notification -- the client has taken authoritative control
    /// of KB membership.  While `true`, `didOpen` does **not**
    /// auto-add files to the shared KB; it only publishes
    /// diagnostics for whatever's already loaded.  Prevents drift
    /// between the extension's session model and the server's
    /// file_roots.
    pub client_manages_files: Arc<AtomicBool>,
    /// Semantic-error codes + names the client has opted out of
    /// (see the `sumo/setIgnoredDiagnostics` notification).
    /// Matched against both `SemanticError::code()` (e.g. `"E005"`)
    /// and `SemanticError::name()` (e.g. `"arity-mismatch"`) so
    /// the client can use whichever form it exposes in its UI.
    /// Empty by default -- the client sets this after it
    /// resolves its `sumo.diagnostics.ignoredCodes` config.
    pub ignored_diagnostic_codes: Arc<RwLock<HashSet<String>>>,
}

impl GlobalState {
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
