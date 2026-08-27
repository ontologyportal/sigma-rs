//! Per-server and per-document state.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};

use lsp_types::Url;
use ropey::Rope;

use sigmakee_rs_sdk::{ParsedDocument, SentenceId, Session, TopLayer, TranslationLayer};

/// Per-document session state held by the server.
pub struct DocState {
    /// Authoritative text buffer. LSP incremental changes are applied to this
    /// rope; `String::from(&rope)` is fed into `parse_document` on reparse.
    pub rope: Rope,
    /// The LSP client's last-seen version number for this document. Diagnostics
    /// carry the same version so stale results can be discarded.
    pub version: i32,
    /// Most recent parse, corresponding to `rope` at `version`. `None` on
    /// freshly-opened docs until the first parse completes.
    pub parsed: Option<ParsedDocument>,
}

impl DocState {
    /// Create a new document state from initial text and version.
    pub fn new(text: &str, version: i32) -> Self {
        Self {
            rope: Rope::from_str(text),
            version,
            parsed: None,
        }
    }

    /// Snapshot the current text as a plain `String`.
    pub fn text_string(&self) -> String {
        String::from(&self.rope)
    }
}

/// Session name for the server's shared KB.  The LSP is translation-only
/// (no prover), so the name only tags inline scratch sessions.
pub const LSP_SESSION: &str = "sumo-lsp";

/// Server-wide shared state. Cloning is cheap — all fields are `Arc`s.
///
/// Generic over the session's KB layer so one server can run against any
/// backend: the standalone `sumo-lsp` binary instantiates the default
/// [`TranslationLayer`], while an embedder that shares its KB with other
/// facades (the wasm build, a prover-capable editor host) constructs the
/// state via [`GlobalState::with_session`] around its own layer stack --
/// the LSP handlers only use layer-agnostic introspection.
pub struct GlobalState<L: TopLayer = TranslationLayer> {
    /// The shared knowledge base, held as an SDK [`Session`].  SDK-level ops
    /// (`manpage`, `validate`, …) are called on the session directly;
    /// introspection the SDK doesn't wrap goes through `Session::kb` /
    /// `Session::kb_mut`.
    pub session: Arc<RwLock<Session<L>>>,
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
    /// How to construct a fresh, empty session -- the capability behind
    /// `sumo/setActiveFiles`' discard-and-rebuild fast path (cheaper than
    /// per-file removal when most of the KB is being dropped).
    ///
    /// `None` for state built via [`GlobalState::with_session`]: a shared KB
    /// must never be discarded wholesale behind the embedder's back (it may
    /// hold content other facades loaded), so the slow per-file removal path
    /// is used instead.  The standalone server sets it in
    /// [`GlobalState::new`], where the LSP is the KB's sole owner.
    pub fresh_session: Option<fn() -> Session<L>>,
    /// Sentence -> 0-based output line of the last `sumo/toTptp` export,
    /// consulted by `sumo/tptpLineForPosition`.  Server-local cache, not KB
    /// state: stale after any KB mutation until the client re-exports.
    pub tptp_lines: Arc<RwLock<HashMap<SentenceId, u32>>>,
}

// Manual impl: `#[derive(Clone)]` would demand `L: Clone`, but every field is
// an `Arc` -- the layer itself is never cloned.
impl<L: TopLayer> Clone for GlobalState<L> {
    fn clone(&self) -> Self {
        Self {
            session: Arc::clone(&self.session),
            docs: Arc::clone(&self.docs),
            client_manages_files: Arc::clone(&self.client_manages_files),
            ignored_diagnostic_codes: Arc::clone(&self.ignored_diagnostic_codes),
            fresh_session: self.fresh_session,
            tptp_lines: Arc::clone(&self.tptp_lines),
        }
    }
}

impl<L: TopLayer> GlobalState<L> {
    /// Create server state around an existing shared session -- the seam that
    /// lets an embedder run the LSP and other facades (SDK calls, a prover)
    /// against the same underlying KB.  The caller keeps its own clone of the
    /// `Arc`; every mutation through either side is visible to both.
    pub fn with_session(session: Arc<RwLock<Session<L>>>) -> Self {
        Self {
            session,
            docs: Arc::new(RwLock::new(HashMap::new())),
            client_manages_files: Arc::new(AtomicBool::new(false)),
            ignored_diagnostic_codes: Arc::new(RwLock::new(HashSet::new())),
            fresh_session: None,
            tptp_lines: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl GlobalState {
    /// Create a new, empty server state owning its own translation-only KB --
    /// what the standalone `sumo-lsp` binary uses.
    pub fn new() -> Self {
        let mut state = Self::with_session(Arc::new(RwLock::new(
            Session::<TranslationLayer>::new(LSP_SESSION.to_string()),
        )));
        // Sole owner: the rebuild fast path may discard the KB freely.
        state.fresh_session = Some(|| Session::<TranslationLayer>::new(LSP_SESSION.to_string()));
        state
    }
}

impl Default for GlobalState {
    fn default() -> Self {
        Self::new()
    }
}
