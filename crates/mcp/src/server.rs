//! The MCP tool surface: one long-lived `Session<ProverLayer>` behind a
//! mutex, exposed as `validate` / `ingest` / `ask` / `check_consistency` /
//! `translate` / `man` / `search` / `list_files` tools.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Mutex;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use sigmakee_rs_sdk::{
    ManKind, NativeOpts, Parser, ProverLayer, SearchOpts, Session, Source, TptpLang,
    TranslationLayer, parse_document,
};

use crate::render;

fn default_true() -> bool {
    true
}
fn default_timeout_secs() -> u32 {
    30
}
fn default_limit() -> usize {
    1
}
fn default_lang() -> String {
    "fof".to_string()
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ValidateParams {
    /// One or more KIF sentences to check, e.g. `(subclass Dog Mammal)`.
    /// Checked against the KB currently loaded in this session (see
    /// `ingest` / `list_files`) but NOT committed to it — safe to call
    /// repeatedly while drafting.
    pub kif: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ValidateKbParams {}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IngestParams {
    /// Local filesystem path to a `.kif` file or a directory of `.kif`
    /// files to load and commit as axioms. Mutually exclusive with `kif`.
    #[serde(default)]
    pub path: Option<String>,
    /// Inline KIF text to load and commit as axioms. Mutually exclusive
    /// with `path`.
    #[serde(default)]
    pub kif: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AskParams {
    /// The conjecture to prove, as a single KIF sentence,
    /// e.g. `(instance Rex Dog)`.
    pub query: String,
    /// Extra KIF sentences to assume as hypotheses for this query only —
    /// never committed to the KB. Use this to test "if I asserted X, would
    /// Y follow / would the KB stay consistent?" without calling `ingest`.
    #[serde(default)]
    pub hypotheses: Vec<String>,
    /// Wall-clock budget for the proof search, in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u32,
    /// Record and return a proof/countermodel transcript, not just the
    /// verdict. Costs some search overhead; default on.
    #[serde(default = "default_true")]
    pub want_proof: bool,
    /// Additionally render the proof as an English paragraph (on top of
    /// the step-wise KIF transcript). Ignored if `want_proof` is false.
    #[serde(default = "default_true")]
    pub prose: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CheckConsistencyParams {
    /// Wall-clock budget for the search, in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u32,
    /// How many distinct contradictions to look for before stopping.
    /// `1` (default) is the usual yes/no consistency check; raise it to
    /// enumerate more independent contradictions in one call.
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TranslateParams {
    /// One or more KIF sentences to translate to TPTP. This is a
    /// standalone syntactic translation — it does NOT run against the
    /// loaded KB (no relation/class lookups), so it does not require
    /// `ingest` first.
    pub kif: String,
    /// TPTP dialect: `"fof"` (untyped first-order, default) or `"tff"`
    /// (typed first-order).
    #[serde(default = "default_lang")]
    pub lang: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ManParams {
    /// Exact SUMO/KIF symbol name to look up, e.g. `"Dog"` or
    /// `"instance"`. Case-sensitive, no wildcards — use `search` to find
    /// a symbol name from a keyword first.
    pub symbol: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    /// Keyword or phrase to search for across every symbol's
    /// documentation, termFormat, and format strings (case-insensitive
    /// substring match).
    pub query: String,
    /// Restrict to one symbol kind: `"class"`, `"relation"`,
    /// `"function"`, `"predicate"`, `"instance"`, or `"individual"`.
    /// Omit to match any kind.
    #[serde(default)]
    pub kind: Option<String>,
    /// Cap on the number of hits returned.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListFilesParams {}

/// The MCP server: a mutex-guarded in-process `Session<ProverLayer>` plus
/// the generated tool router.
#[derive(Clone)]
pub struct SumoServer {
    session: std::sync::Arc<Mutex<Session<ProverLayer>>>,
    #[allow(dead_code, reason = "read by the #[tool_handler]-generated call_tool dispatch")]
    tool_router: ToolRouter<Self>,
}

impl SumoServer {
    pub fn new(session: Session<ProverLayer>) -> Self {
        Self {
            session: std::sync::Arc::new(Mutex::new(session)),
            tool_router: Self::tool_router(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Session<ProverLayer>> {
        // A poisoned mutex means a prior tool call panicked mid-mutation;
        // recovering the guard is the right call here (a long-lived server
        // shouldn't wedge itself over one bad request) — the KB's own
        // invariants (parse-then-commit) mean a panic can't leave it
        // half-written.
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn parse_lang(s: &str) -> TptpLang {
        match s.to_ascii_lowercase().as_str() {
            "tff" => TptpLang::Tff,
            "cnf" => TptpLang::Cnf,
            "auto" => TptpLang::Auto,
            _ => TptpLang::Fof,
        }
    }

    fn parse_kind(s: &str) -> Option<ManKind> {
        match s.to_ascii_lowercase().as_str() {
            "class" => Some(ManKind::Class),
            "relation" => Some(ManKind::Relation),
            "function" => Some(ManKind::Function),
            "predicate" => Some(ManKind::Predicate),
            "instance" => Some(ManKind::Instance),
            "individual" => Some(ManKind::Individual),
            _ => None,
        }
    }
}

#[tool_router]
impl SumoServer {
    #[tool(
        description = "Check one or more KIF sentences for syntax errors and semantic issues \
            (undeclared relations, arity/type mismatches, unresolved cross-references, etc.) \
            against the KB currently loaded in this session. Does NOT modify the KB — call this \
            before `ingest` to catch mistakes early, and after `ingest` is not necessary (ingest \
            reports the same diagnostics itself)."
    )]
    async fn validate(
        &self,
        Parameters(ValidateParams { kif }): Parameters<ValidateParams>,
    ) -> Result<String, ErrorData> {
        let mut session = self.lock();
        let diags = session
            .validate_formula(&kif)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        Ok(render::render_diagnostics(session.kb(), &diags))
    }

    #[tool(
        description = "Run full semantic validation over the whole KB currently loaded in this \
            session (every file ingested so far). Useful after several `ingest` calls to check \
            nothing upstream broke."
    )]
    async fn validate_kb(
        &self,
        Parameters(ValidateKbParams {}): Parameters<ValidateKbParams>,
    ) -> String {
        let session = self.lock();
        let diags = session.validate();
        render::render_diagnostics(session.kb(), &diags)
    }

    #[tool(
        description = "Load KIF from a local file/directory path or from inline text, and commit \
            it as axioms in this session's KB (persists for the rest of the conversation — later \
            `ask`/`validate`/`man` calls will see it). Exactly one of `path` or `kif` must be \
            given. Parse or semantic errors abort the commit (nothing partial is added); fix and \
            retry. Prefer validating with `validate` first for inline drafts you're unsure about."
    )]
    async fn ingest(
        &self,
        Parameters(IngestParams { path, kif }): Parameters<IngestParams>,
    ) -> Result<String, ErrorData> {
        let src = match (path, kif) {
            (Some(p), None) => Source::Local(vec![PathBuf::from(p)]),
            (None, Some(k)) => Source::Reader {
                name: "inline.kif".to_string(),
                reader: Box::new(Cursor::new(k.into_bytes())),
            },
            (Some(_), Some(_)) => {
                return Err(ErrorData::invalid_params(
                    "provide exactly one of `path` or `kif`, not both",
                    None,
                ));
            }
            (None, None) => {
                return Err(ErrorData::invalid_params("provide `path` or `kif`", None));
            }
        };
        let mut session = self.lock();
        let errs = session.ingest(src, true);
        Ok(render::render_sdk_errors(session.kb(), &errs))
    }

    #[tool(
        description = "Prove (or refute) a KIF conjecture against the KB loaded in this session, \
            via the in-process saturation prover. Returns the verdict (Proved / Disproved / \
            Timeout / ...), variable bindings if any, and — when `want_proof` is true — a \
            step-wise KIF proof transcript plus an English prose rendering. Use `hypotheses` to \
            test a conjecture under extra assumptions without committing them via `ingest`. This \
            is also the way to check whether a candidate axiom you're about to `ingest` would \
            make the KB inconsistent: pass the KB's own most-suspect consequence, or negate the \
            candidate axiom itself, as `query` with the candidate axiom as a `hypothesis`."
    )]
    async fn ask(
        &self,
        Parameters(AskParams { query, hypotheses, timeout_secs, want_proof, prose }): Parameters<
            AskParams,
        >,
    ) -> Result<String, ErrorData> {
        let opts = NativeOpts { time_limit_secs: timeout_secs as u64, want_proof, ..Default::default() };
        let mut session = self.lock();

        let result = if hypotheses.is_empty() {
            session.ask(&query, Some(opts))
        } else {
            hypotheses
                .iter()
                .try_fold(session.open_session(), |s, h| s.tell(h))
                .and_then(|open| open.ask(&query, Some(opts)))
        };
        let result = result.map_err(|errs| {
            ErrorData::invalid_params(
                errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; "),
                None,
            )
        })?;

        let goal_doc = parse_document("__mcp_ask_goal__", query.clone(), Parser::Kif);
        let goal_ast = goal_doc.ast.iter().find_map(|d| d.as_stmt());

        Ok(render::render_prover_result(
            session.kb(),
            goal_ast,
            &result,
            want_proof,
            prose && want_proof,
        ))
    }

    #[tool(
        description = "Saturate the KB loaded in this session looking for a logical \
            contradiction (an axiom set that entails both P and not-P). `Consistent` means the \
            search completed without finding one (a completeness certificate, not just \
            'ran out of time' — check the verdict); `Inconsistent` returns the offending \
            derivation(s) as KIF proof transcripts. Run this after a batch of `ingest` calls to \
            confirm you haven't broken the ontology."
    )]
    async fn check_consistency(
        &self,
        Parameters(CheckConsistencyParams { timeout_secs, limit }): Parameters<
            CheckConsistencyParams,
        >,
    ) -> Result<String, ErrorData> {
        let opts = NativeOpts { time_limit_secs: timeout_secs as u64, want_proof: true, ..Default::default() };
        let session = self.lock();
        let result = session
            .audit(opts, limit.max(1))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(render::render_prover_result(session.kb(), None, &result, true, false))
    }

    #[tool(
        description = "Translate KIF sentences to TPTP (fof/tff) syntax. Purely syntactic — does \
            not consult the loaded KB, so it works standalone without `ingest`. Mainly useful for \
            inspecting exactly how a formula will be handed to the prover, or for producing input \
            for an external TPTP-based tool."
    )]
    async fn translate(
        &self,
        Parameters(TranslateParams { kif, lang }): Parameters<TranslateParams>,
    ) -> Result<String, ErrorData> {
        let mut session = Session::<TranslationLayer>::new("mcp::translate".to_string());
        session
            .translate_formula(&kif, Self::parse_lang(&lang))
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))
    }

    #[tool(
        description = "Look up a SUMO/KIF symbol's man page: its kind (class/relation/function/…), \
            taxonomic parents, declared arity/domain/range, documentation, termFormat/format \
            natural-language renderings, and how many sentences in the loaded KB reference it. \
            ALWAYS check this (or `search`) before inventing a new relation or class name, to \
            avoid duplicating an existing one or misusing its arity/domains. Returns an error-free \
            'not found' text (not a protocol error) when the symbol isn't interned — that itself \
            is useful signal that the name is free to define."
    )]
    async fn man(&self, Parameters(ManParams { symbol }): Parameters<ManParams>) -> String {
        let session = self.lock();
        match session.manpage(&symbol) {
            Some(view) => render::render_manpage(&view),
            None => format!(
                "'{symbol}' has no man page in the loaded KB — either it isn't defined yet, or it \
                 has no (documentation/termFormat/format/subclass/instance/domain/range …) \
                 statements about it. Try `search` for a related keyword."
            ),
        }
    }

    #[tool(
        description = "Keyword search across every symbol's documentation, termFormat, and format \
            strings in the loaded KB (case-insensitive substring match). Use this to discover \
            existing vocabulary for a concept before coining a new symbol name — e.g. search \
            \"vehicle\" before defining a new class for cars."
    )]
    async fn search(
        &self,
        Parameters(SearchParams { query, kind, limit }): Parameters<SearchParams>,
    ) -> Result<String, ErrorData> {
        let opts = SearchOpts {
            kind: kind.as_deref().map(Self::parse_kind).flatten(),
            language: None,
            limit,
        };
        let session = self.lock();
        let hits = session
            .search(&query, &opts)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(render::render_search_hits(&hits))
    }

    #[tool(description = "List every file currently loaded in this session's KB, with each \
        file's sentence count. Use this to check what context is already available before \
        deciding what to `ingest`.")]
    async fn list_files(
        &self,
        Parameters(ListFilesParams {}): Parameters<ListFilesParams>,
    ) -> String {
        let session = self.lock();
        // `__`-prefixed tags are ephemeral internal sessions (scratch
        // validation/query buffers, e.g. `__inline(0)__`), not real ingests.
        let files: Vec<String> =
            session.kb().iter_files().into_iter().filter(|f| !f.starts_with("__")).collect();
        if files.is_empty() {
            return "no files loaded".to_string();
        }
        files
            .into_iter()
            .map(|f| {
                let n = session.kb().file_roots(&f).len();
                format!("{f}: {n} sentence(s)")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[tool_handler]
impl ServerHandler for SumoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("sumo-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(
            "Tools for authoring SUO-KIF SUMO ontology content that is syntactically valid, \
             semantically well-formed, and logically consistent with an in-memory knowledge \
             base. Suggested workflow: (1) `list_files` to see what's already loaded, `man` / \
             `search` to check existing vocabulary before naming a new symbol; (2) `validate` a \
             draft KIF snippet before committing it; (3) `ask` the negation of a candidate axiom \
             (or a suspect consequence) as a sanity check, optionally with the candidate as a \
             `hypothesis`, before `ingest`ing it for real; (4) `ingest` to commit; (5) \
             `check_consistency` after a batch of `ingest` calls. `translate` is a standalone \
             KIF-to-TPTP syntax helper.",
        )
    }
}
