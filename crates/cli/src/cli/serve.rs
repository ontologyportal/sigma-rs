// crates/native/src/cli/serve.rs
//
// `sumo serve` -- a persistent daemon that exposes the ask / tell /
// KB-maintenance primitives over a line-delimited JSON wire format
// on stdio.
//
// Motivation: the VSCode extension needs interactive querying, but
// `sumo-lsp` is the wrong place for it (the prover takes seconds,
// blocking the language server would ruin hover latency).  This
// daemon is the analogue of a Jupyter kernel: spawned once per
// editor window, lives as long as the window, owns a long-lived
// `KnowledgeBase` in memory so every query amortises the load cost
// over many asks.
//
// ## DB lifecycle
//
// Default: the kernel opens an LMDB at `--db` (creating if absent)
// and reconciles every `-f` file against the DB at boot.  Subsequent
// spawns with the same `--db` and file set skip re-ingestion
// entirely -- `reconcile_file`'s fast path detects no-ops via the
// per-file hash manifest.  This is the "persistent kernel" mode the
// VSCode extension uses.
//
// `--no-db`: everything is in-memory.  Files are loaded as session
// axioms and vanish when the process exits.  Use this for one-off
// scratch work or when the user has explicitly disabled caching.
//
// ## Wire format
//
// Each message is a single JSON object on its own line (`\n`-
// delimited, no framing headers).  Fields follow JSON-RPC 2.0
// conventions loosely -- `id` / `method` / `params` on requests and
// `id` / `result` | `error` on responses -- but we omit the
// `jsonrpc: "2.0"` preamble and don't implement the full spec.
// This is a pragmatic MVP; upgrade to real JSON-RPC 2.0 framing
// (Content-Length headers, progress notifications) when streaming /
// cancellation lands.
//
// ## Methods
//
// | Method              | Purpose                                          |
// |---------------------|--------------------------------------------------|
// | `tell`              | Session-local assertion (ephemeral)              |
// | `ask`               | Run a conjecture through the prover              |
// | `debug`             | Consistency-check a loaded file via SInE + prover |
// | `test`              | Run `.kif.tq` test files and report pass/fail    |
// | `kb.reconcileFile`  | Sync one file from disk into the DB              |
// | `kb.removeFile`     | Drop one file from the in-memory KB + DB         |
// | `kb.flush`          | Wipe all persisted files (in-memory + DB)        |
// | `kb.listFiles`      | Return loaded files + sentence counts            |
// | `kb.generateTptp`   | Emit the loaded KB as TPTP (fof/tff)             |
// | `shutdown`          | Clean exit                                       |

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Arc;

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use sigmakee_rs_core::{
    parse_test_content, KnowledgeBase, ProverStatus, SentenceId, SineParams,
    TptpLang, TptpOptions, VampireRunner,
};

// `crate::ask` (the inline `native_ask` shim) is no longer used
// from serve.rs — `handle_test` and `handle_ask` both go through
// `sigmakee_rs_sdk::TestOp` / `AskOp` now.  The shim is kept in
// `crates/cli/src/ask.rs` as a re-exported public API + for the
// `ask_parse_error` unit test in lib.rs; deleting it is a follow-up.
use crate::cli::args::KbArgs;
use crate::cli::debug::resolve_file_tag;
use crate::cli::util::{collect_kif_files, parse_lang, read_kif_file, resolve_vampire_path};

// -- Wire-format types --------------------------------------------------------

/// Incoming message.  `params` is an arbitrary JSON value that the
/// per-method handler deserialises into its own typed struct.
#[derive(Debug, Deserialize)]
struct Request {
    /// Correlates response with request.  Missing/null = notification
    /// (we reply with nothing; useful for `shutdown` etc.).
    id:     Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

/// Outgoing message.  Always echoes the request's `id`.  Exactly one
/// of `result` / `error` is present.
#[derive(Debug, Serialize)]
struct Response {
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error:  Option<ResponseError>,
}

#[derive(Debug, Serialize)]
struct ResponseError {
    code:    i32,
    message: String,
}

// JSON-RPC-flavoured error codes.  We only use the handful listed
// here; keeping them as `const` means typos can't escape into the
// wire format.
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS:   i32 = -32602;
const INTERNAL_ERROR:   i32 = -32603;

// -- Method payloads ----------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TellParams {
    /// Session the assertion goes into.  Defaults to `"default"`.
    #[serde(default = "default_session")]
    session: String,
    /// KIF source to ingest.
    kif:     String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TellResponse {
    ok:       bool,
    errors:   Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AskParams {
    /// Session the query runs in -- scoped by `--tell`-style assertions.
    #[serde(default = "default_session")]
    session:    String,
    /// Conjecture as a single KIF sentence.
    query:      String,
    /// Vampire proof-search timeout in seconds.
    #[serde(default = "default_timeout_secs")]
    timeout_secs: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AskResponse {
    /// `"Proved"`, `"Disproved"`, `"Consistent"`, `"Inconsistent"`,
    /// `"Timeout"`, `"Unknown"`.  Mirrors `sigmakee_rs_core::ProverStatus`.
    status:    String,
    /// Variable bindings returned by the prover (may be empty).
    bindings:  Vec<String>,
    /// Proof steps in KIF form, one formula per step.  Empty if the
    /// prover didn't emit a proof section or if the conjecture was
    /// disproved / timed out.
    proof_kif: Vec<String>,
    /// Raw Vampire transcript, unparsed.  Useful for debugging or
    /// for clients that want to render the full proof object.
    raw:       String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReconcileFileParams {
    /// Absolute path (used both as file-tag for the KB and to
    /// read from disk when `text` is omitted).
    path: String,
    /// Optional inline text.  When set, the KB reconciles against
    /// this text (buffer-based update).  When omitted, the kernel
    /// reads from `path`.  The VSCode extension uses the omitted
    /// form for save-triggered syncs -- disk is the source of
    /// truth.
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReconcileFileResponse {
    /// File tag the reconcile operated on (echo of `path`).
    path:            String,
    /// Number of sentences added since the last reconcile.
    added:           usize,
    /// Number of sentences removed since the last reconcile.
    removed:         usize,
    /// Number of sentences retained unchanged.
    retained:        usize,
    /// Number of sentences in the selected SInE neighbourhood that
    /// were re-validated after the edit.  Diagnostic only -- the
    /// LSP surfaces the actual findings via `publish_diagnostics`.
    revalidated:     usize,
    /// Hard parse errors, one per unrecoverable sentence.  Empty
    /// on clean files.  Populated errors abort the commit for
    /// this file (the recovered sentences are *not* persisted).
    parse_errors:    Vec<String>,
    /// Validation hard-errors.  Only populated when the caller
    /// spawned the kernel under `-W` / `-Wall` promotions; in the
    /// default configuration every semantic issue is a warning and
    /// this list is empty.
    semantic_errors: Vec<String>,
    /// True iff the reconcile's delta was persisted.  False on
    /// parse error (delta discarded) or in `--no-db` mode (nothing
    /// to persist).
    persisted:       bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveFileParams {
    path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoveFileResponse {
    /// Number of root sentences that were dropped from the KB.
    /// Zero when the file wasn't loaded.
    removed:   usize,
    /// True iff the deletion was committed to LMDB.  False in
    /// `--no-db` mode.
    persisted: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FlushResponse {
    /// Number of files that were cleared.
    files_removed:  usize,
    /// Total sentences removed across all files.
    sentences_removed: usize,
    /// True iff the wipe touched LMDB.  False in `--no-db` mode.
    persisted:      bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListFilesResponse {
    files: Vec<FileEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileEntry {
    path:           String,
    sentence_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateTptpParams {
    /// TPTP dialect: `"fof"` or `"tff"`.  Unknown values fall back
    /// to fof with a warning logged to stderr; we don't reject the
    /// request outright because that would bubble up as a scary
    /// error to the VSCode user when the right thing is to just
    /// emit *something* useful.
    #[serde(default = "default_tptp_lang")]
    lang: String,
    /// Optional KB session whose assertions get rendered as
    /// `hypothesis` next to the axioms.  The VSCode extension
    /// doesn't currently use this -- generateTPTP is a snapshot
    /// of the persisted KB, not a session dump -- but the wire
    /// slot is here so a REPL that tells into a session can
    /// export the full context later.
    #[serde(default)]
    session: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerateTptpResponse {
    /// The full TPTP document (no trailing newline stripped).
    tptp:          String,
    /// Count of emitted top-level formulae.  Derived by scanning
    /// the output for `^(fof|tff|cnf)\(` line prefixes.  Ballpark
    /// only -- a precise count would need the converter to
    /// surface it, which is more work than the value justifies.
    formula_count: usize,
    /// Echo of the resolved dialect so the client can confirm
    /// what the kernel actually emitted.
    lang:          String,
}

// -- `debug` method payloads --------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DebugParams {
    /// Path or filename of a KIF file already loaded into the KB.
    /// Resolved against loaded file-tags by exact match, then
    /// canonicalized-absolute-path match, then basename-suffix
    /// match (same resolution rules as `sumo debug`).
    file:         String,
    /// Fraction of the file's root sentences to sample for the
    /// consistency check, in (0.0, 1.0].  Defaults to 1.0 (check
    /// every sentence).
    #[serde(default = "default_thoroughness")]
    thoroughness: f32,
    /// SInE tolerance factor for the relevance expansion.  `None`
    /// uses the crate default (typically 2.0 unless overridden at
    /// build time via `SINE_TOLERANCE`).
    #[serde(default)]
    scope:        Option<f32>,
    /// Vampire proof-search timeout in seconds.  Defaults to 60.
    #[serde(default = "default_debug_timeout_secs")]
    timeout_secs: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DebugResponse {
    /// Resolved file tag (may differ from the requested `file`
    /// when basename matching resolved to a loaded absolute path).
    file:             String,
    /// Number of root sentences in the file.
    root_sentences:   usize,
    /// Number of sentences actually sampled (`thoroughness` rounded
    /// up).
    sampled:          usize,
    /// Number of additional axioms pulled in by SInE relevance
    /// expansion.
    sine_expanded:    usize,
    /// Total sentences Vampire saw (`sampled + sine_expanded`).
    total_checked:    usize,
    /// SInE tolerance used for this run.
    tolerance:        f32,
    /// Other KB files from which SInE pulled axioms (ephemeral tags
    /// excluded).  Sorted for stability.
    files_pulled:     Vec<String>,
    /// Vampire's verdict: `"Consistent"`, `"Inconsistent"`,
    /// `"Timeout"`, or `"Unknown"`.
    status:           String,
    /// Axioms that appear in the refutation (populated only when
    /// `status == "Inconsistent"` AND Vampire emitted a proof
    /// transcript).  Same two-tier resolution (sid-first, canonical-
    /// hash fallback) as the CLI's contradiction summary.
    contradictions:   Vec<ContradictionEntry>,
    /// Full KIF proof transcript, same shape as
    /// `AskResponse.proofKif` on the `ask` method.  Empty when no
    /// refutation was produced.
    proof_kif:        Vec<ProofStepEntry>,
    /// Raw prover transcript — useful for debugging when the
    /// structured paths above return nothing.
    raw:              String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ContradictionEntry {
    /// Sentence id of the contributing axiom.
    sid:  SentenceId,
    /// Source file tag (as stored in the KB).
    file: String,
    /// 1-based line number in the source file.
    line: u32,
    /// Plain-KIF rendering of the sentence (re-parseable).
    kif:  String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProofStepEntry {
    /// 0-based position in the proof.
    index:       usize,
    /// Role/rule label (`"axiom"`, `"cnf_transformation"`,
    /// `"resolution"`, etc.).  Opaque to the client except for the
    /// literal `"axiom"` which signals "this step came from a KB
    /// sentence".
    rule:        String,
    /// Indices of premise steps (indices into this same list).
    premises:    Vec<usize>,
    /// Step formula as flat KIF.
    formula:     String,
    /// When the step is an `"axiom"` AND its source resolved to a
    /// loaded KB sentence, these three are populated.  Resolution
    /// tries the preserved-name sid-direct path first, then falls
    /// back to canonical-fingerprint hashing.  Non-axiom steps and
    /// unresolvable axioms leave these as `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_sid:  Option<SentenceId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_line: Option<u32>,
}

// -- `test` method payloads ---------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestParams {
    /// `.kif.tq` test files or directories containing them.
    /// Directories are walked non-recursively and sorted.
    paths:        Vec<String>,
    /// Per-test timeout override in seconds.  When `None`, each
    /// test uses its own `(time N)` directive or 30s default.
    #[serde(default)]
    timeout_secs: Option<u32>,
    /// Prover backend — `"subprocess"` (default) or `"embedded"`.
    #[serde(default = "default_test_backend")]
    backend:      String,
    /// TPTP dialect — `"fof"` (default) or `"tff"`.
    #[serde(default = "default_tptp_lang")]
    lang:         String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestResponse {
    /// Number of test files that ran.
    total:   usize,
    /// Number that reported `"Passed"`.
    passed:  usize,
    /// Number that reported anything other than `"Passed"`.
    failed:  usize,
    /// Per-file results in input order.
    results: Vec<TestCaseResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestCaseResult {
    /// Path passed in, preserved for correlation.
    file:             String,
    /// The `(note "…")` string from the test file, or the file name
    /// if no note was present.
    note:             String,
    /// Outcome tag — one of:
    /// - `"Passed"`       — expected verdict met and all expected answers found.
    /// - `"Incomplete"`   — verdict met, but at least one expected answer missing.
    /// - `"Failed"`       — verdict mismatch (expected proved, got disproved or vice-versa).
    /// - `"Error"`        — parse / load / prover error; `error` is populated.
    outcome:          String,
    /// `true` if the test file's `(expected-proof yes)` was present
    /// (or defaulted to yes — the KIF-tq default).
    expected_proof:   bool,
    /// What Vampire actually concluded.
    actual_proved:    bool,
    /// Expected variable bindings (if the test specified any).
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_answers: Option<Vec<String>>,
    /// Variable bindings the prover returned.
    found_answers:    Vec<String>,
    /// Subset of `expected_answers` that weren't found.
    missing_answers:  Vec<String>,
    /// Error message when `outcome == "Error"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    error:            Option<String>,
}

fn default_session() -> String { "default".to_string() }
fn default_timeout_secs() -> u32 { 30 }
fn default_tptp_lang() -> String { "fof".to_string() }
fn default_thoroughness() -> f32 { 1.0 }
fn default_debug_timeout_secs() -> u32 { 60 }
fn default_test_backend() -> String { "subprocess".to_string() }

// -- Entry point --------------------------------------------------------------

/// Run the kernel server loop.
///
/// Opens (or creates) the LMDB at `kb_args.db` unless `--no-db` is
/// set, then reconciles every `-f` / `-d` file against the DB so
/// the kernel's in-memory view matches the on-disk truth.  Reads
/// requests from stdin one line at a time and writes responses to
/// stdout.  Exits cleanly on EOF or on a `shutdown` request.
///
/// Returns `true` on clean exit, `false` on a startup failure that
/// should propagate a non-zero CLI exit code.  Per-request errors
/// are always reported back to the client as JSON-RPC errors --
/// the loop itself never returns `false` mid-conversation.
pub fn run_serve(kb_args: KbArgs) -> bool {
    log::info!(target: "sumo_native::serve",
        "sumo-kernel starting (db={}, no_db={})",
        kb_args.db.display(), kb_args.no_db);

    let mut kb = match boot_kb(&kb_args) {
        Ok(k)   => k,
        Err(()) => {
            log::error!(target: "sumo_native::serve",
                "kernel startup failed: could not open KB");
            return false;
        }
    };
    let persistent  = !kb_args.no_db;
    let db_path     = kb_args.db.clone();

    // Resolve the Vampire binary once at startup.  A missing binary
    // is NOT a fatal startup error -- the client might never send an
    // `ask` and be perfectly happy to `tell` only -- so we cache the
    // resolution and surface any failure per-ask instead.
    let vampire_candidate = kb_args.vampire.clone()
        .unwrap_or_else(|| PathBuf::from("vampire"));
    let vampire_path: Option<Arc<PathBuf>> =
        resolve_vampire_path(&vampire_candidate).ok().map(Arc::new);
    if vampire_path.is_none() {
        log::warn!(target: "sumo_native::serve",
            "vampire binary not found at startup ('{}'); `ask` requests will fail \
             until a valid path is available",
            vampire_candidate.display());
    }

    let stdin  = std::io::stdin();
    let stdout = std::io::stdout();
    let reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    log::info!(target: "sumo_native::serve", "kernel ready; awaiting requests on stdin");

    for line in reader.lines() {
        let line = match line {
            Ok(l)  => l,
            Err(e) => {
                log::error!(target: "sumo_native::serve", "stdin read error: {}", e);
                break;
            }
        };
        let line = line.trim();
        if line.is_empty() { continue; }

        let req: Request = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                log::warn!(target: "sumo_native::serve",
                    "malformed request: {} ({})", e, line);
                let resp = Response {
                    id:     Value::Null,
                    result: None,
                    error:  Some(ResponseError {
                        code:    -32700,   // PARSE_ERROR
                        message: format!("parse error: {}", e),
                    }),
                };
                write_response(&mut writer, &resp);
                continue;
            }
        };

        if req.method == "shutdown" {
            log::info!(target: "sumo_native::serve", "shutdown received");
            let resp = Response {
                id: req.id.unwrap_or(Value::Null),
                result: Some(Value::Null),
                error: None,
            };
            write_response(&mut writer, &resp);
            break;
        }

        let resp = dispatch(&mut kb, persistent, &db_path,
                            vampire_path.as_deref(), &req);
        // Notifications (no id) get no reply.  The spec says
        // notifications are one-way; silently dropping the reply
        // keeps clients that send them happy.
        if let Some(id) = req.id {
            let resp = Response { id, ..resp };
            write_response(&mut writer, &resp);
        }
    }

    log::info!(target: "sumo_native::serve", "sumo-kernel shutting down");
    true
}

// -- Boot: open + reconcile ---------------------------------------------------

/// Open (or create) the KB and reconcile the requested `-f`/`-d`
/// files against it.  In `--no-db` mode this is an in-memory KB
/// with files loaded as session axioms (legacy behaviour); in the
/// default `--db` mode it's an LMDB-backed KB where each file is
/// diff-merged via `reconcile_file` + `persist_reconcile_diff`.
fn boot_kb(kb_args: &KbArgs) -> Result<KnowledgeBase, ()> {
    if kb_args.no_db {
        // Legacy in-memory flow -- files load as session axioms
        // and vanish with the process.
        return crate::cli::util::open_or_build_kb(kb_args);
    }

    // Persistent mode: open the LMDB (creating if absent) and
    // reconcile every requested file.  `reconcile_file` is a no-op
    // when the file hasn't changed since the last reconcile, so
    // repeated kernel spawns on an unchanged KB start in ~200 ms.
    let mut kb = KnowledgeBase::open(&kb_args.db).map_err(|e| {
        log::error!(target: "sumo_native::serve",
            "failed to open LMDB at '{}': {}", kb_args.db.display(), e);
    })?;

    let has_files = !kb_args.files.is_empty() || !kb_args.dirs.is_empty();
    if !has_files {
        log::info!(target: "sumo_native::serve",
            "opened LMDB at '{}' with no `-f`/`-d` files; serving existing axioms",
            kb_args.db.display());
        return Ok(kb);
    }

    let all_files = collect_kif_files(kb_args)?;

    // Read every file up-front — `reconcile_files` takes an
    // `IntoIterator<Item = (&str, impl AsRef<str>)>`, so we need
    // owned strings to survive the call.  Unreadable files log a
    // warning and drop out of the batch; readable files continue.
    let mut readable: Vec<(String, String)> = Vec::with_capacity(all_files.len());
    for path in &all_files {
        match read_kif_file(path) {
            Ok(text) => readable.push((path.display().to_string(), text)),
            Err(())  => log::warn!(target: "sumo_native::serve",
                "skipping unreadable file '{}' during boot", path.display()),
        }
    }

    // Batched reconcile.  `reconcile_files` folds the expensive
    // phases (SInE promotion, taxonomy rebuild, smart revalidation)
    // into one pass across the whole batch instead of N per-file
    // passes — on cold SUMO this drops boot from ~60s to ~2–4s.
    let reports = kb.reconcile_files(
        readable.iter().map(|(tag, text)| (tag.as_str(), text.as_str())),
    );

    // Per-file logging + persistence.  The commit happens as N
    // separate `persist_reconcile_diff` calls today for clarity;
    // batching LMDB writes into one transaction is a follow-up
    // that'd shave a bit more wall time but requires a public-API
    // change.
    let mut total_added   = 0usize;
    let mut total_removed = 0usize;
    for report in &reports {
        let tag = report.file.as_str();

        // Parse errors abort *this file only*.  Other files in the
        // batch already completed — their deltas are in memory and
        // will be committed below.  Matches the LSP's
        // "one broken file shouldn't break the KB" invariant.
        if !report.parse_errors.is_empty() {
            for e in &report.parse_errors {
                log::warn!(target: "sumo_native::serve",
                    "boot reconcile: {} has {} parse error(s): {}",
                    tag, report.parse_errors.len(), e);
            }
            continue;
        }
        if !report.semantic_errors.is_empty() {
            for e in &report.semantic_errors {
                log::warn!(target: "sumo_native::serve",
                    "boot reconcile: {} has semantic error: {}", tag, e);
            }
            continue;
        }

        if report.is_noop() {
            log::debug!(target: "sumo_native::serve",
                "boot reconcile: {} unchanged ({} retained)", tag, report.retained);
            continue;
        }

        if let Err(e) = kb.persist_reconcile_diff(&report.removed_sids, &report.added_sids) {
            log::error!(target: "sumo_native::serve",
                "boot reconcile: failed to commit delta for {}: {}", tag, e);
            continue;
        }
        total_added   += report.added();
        total_removed += report.removed();
        log::info!(target: "sumo_native::serve",
            "boot reconcile: {} +{} -{}", tag, report.added(), report.removed());
    }
    log::info!(target: "sumo_native::serve",
        "boot reconcile complete: {} file(s) processed (+{} added, -{} removed)",
        reports.len(), total_added, total_removed);

    Ok(kb)
}

// -- Dispatch -----------------------------------------------------------------

fn dispatch(
    kb:           &mut KnowledgeBase,
    persistent:   bool,
    db_path:      &PathBuf,
    vampire_path: Option<&PathBuf>,
    req:          &Request,
) -> Response {
    match req.method.as_str() {
        "tell"              => handle_tell(kb, &req.params),
        "ask"               => handle_ask(kb, vampire_path, &req.params),
        "debug"             => handle_debug(kb, vampire_path, &req.params),
        "test"              => handle_test(kb, vampire_path, &req.params),
        "kb.reconcileFile"  => handle_reconcile_file(kb, persistent, &req.params),
        "kb.removeFile"     => handle_remove_file(kb, persistent, &req.params),
        "kb.flush"          => handle_flush(kb, persistent, db_path),
        "kb.listFiles"      => handle_list_files(kb),
        "kb.generateTptp"   => handle_generate_tptp(kb, &req.params),
        other => Response {
            id:     Value::Null,
            result: None,
            error:  Some(ResponseError {
                code:    METHOD_NOT_FOUND,
                message: format!("method '{}' is not implemented", other),
            }),
        },
    }
}

fn handle_tell(kb: &mut KnowledgeBase, params: &Value) -> Response {
    let params: TellParams = match serde_json::from_value(params.clone()) {
        Ok(p)  => p,
        Err(e) => return err_response(INVALID_PARAMS, format!("invalid tell params: {}", e)),
    };

    log::debug!(target: "sumo_native::serve",
        "tell: session={:?} kif-bytes={}", params.session, params.kif.len());

    let result = kb.tell(&params.session, &params.kif);
    let resp = TellResponse {
        ok:       result.ok,
        errors:   result.errors.iter().map(|e| e.to_string()).collect(),
        warnings: result.warnings.iter().map(|w| w.to_string()).collect(),
    };
    ok_response(resp)
}

fn handle_ask(
    kb:           &mut KnowledgeBase,
    vampire_path: Option<&PathBuf>,
    params:       &Value,
) -> Response {
    let params: AskParams = match serde_json::from_value(params.clone()) {
        Ok(p)  => p,
        Err(e) => return err_response(INVALID_PARAMS, format!("invalid ask params: {}", e)),
    };

    let vampire_path = match vampire_path {
        Some(p) => p.clone(),
        None => return err_response(
            INTERNAL_ERROR,
            "vampire binary not available; set `--vampire <PATH>` at kernel startup".into(),
        ),
    };

    log::debug!(target: "sumo_native::serve",
        "ask: session={:?} query-bytes={} timeout={}s",
        params.session, params.query.len(), params.timeout_secs);

    // Drive AskOp.  MVP sticks to the subprocess backend + FOF.
    // `AskOp` handles backend selection, vampire-path threading,
    // and result assembly; the JSON-RPC handler just builds the
    // wire-shaped response.
    let result = match sigmakee_rs_sdk::AskOp::new(kb, &params.query)
        .session(&*params.session)
        .timeout_secs(params.timeout_secs)
        .vampire_path(vampire_path)
        .lang(sigmakee_rs_core::TptpLang::Fof)
        .run()
    {
        Ok(r) => r,
        Err(sigmakee_rs_sdk::SdkError::VampireNotFound(msg)) => {
            return err_response(INTERNAL_ERROR, format!("vampire not found: {}", msg));
        }
        Err(e) => {
            return err_response(INTERNAL_ERROR, format!("ask failed: {}", e));
        }
    };

    let status = match result.status {
        ProverStatus::Proved       => "Proved",
        ProverStatus::Disproved    => "Disproved",
        ProverStatus::Consistent   => "Consistent",
        ProverStatus::Inconsistent => "Inconsistent",
        ProverStatus::Timeout      => "Timeout",
        ProverStatus::Unknown      => "Unknown",
    };
    // `pretty_print()` injects ANSI colour escapes -- fine for a
    // terminal, hostile in a webview.  `Display` emits plain KIF.
    let proof_kif: Vec<String> = result.proof_kif.iter()
        .map(|step| step.formula.to_string())
        .collect();

    let resp = AskResponse {
        status:    status.to_string(),
        bindings:  result.bindings.iter().map(|b| b.to_string()).collect(),
        proof_kif,
        raw:       result.raw_output,
    };
    ok_response(resp)
}

fn handle_debug(
    kb:           &mut KnowledgeBase,
    vampire_path: Option<&PathBuf>,
    params:       &Value,
) -> Response {
    let params: DebugParams = match serde_json::from_value(params.clone()) {
        Ok(p)  => p,
        Err(e) => return err_response(INVALID_PARAMS, format!("invalid debug params: {}", e)),
    };

    if !(params.thoroughness > 0.0 && params.thoroughness <= 1.0) {
        return err_response(INVALID_PARAMS,
            format!("thoroughness must be in (0.0, 1.0]; got {}", params.thoroughness));
    }

    let vampire_path = match vampire_path {
        Some(p) => p.clone(),
        None => return err_response(INTERNAL_ERROR,
            "vampire binary not available; set `--vampire <PATH>` at kernel startup".into()),
    };

    // -- File-tag resolution: same three-tier strategy as `sumo debug`.
    // `resolve_file_tag` logs its own error context; we only need
    // to surface a concise JSON error when it fails.
    let file_path = PathBuf::from(&params.file);
    let tag_primary = file_path.display().to_string();
    let tag_canonical = file_path.canonicalize().ok().map(|p| p.display().to_string());
    let (tag, sids) = match resolve_file_tag(kb, &file_path, &tag_primary, tag_canonical.as_deref()) {
        Ok(v)   => v,
        Err(()) => return err_response(INVALID_PARAMS,
            format!("file '{}' is not loaded in the KB (check logs for candidates)", params.file)),
    };

    // -- Sampling (same as `run_debug`).
    let file_root_count = sids.len();
    let sample: Vec<SentenceId> = if params.thoroughness >= 1.0 {
        sids.clone()
    } else {
        let mut rng = rand::thread_rng();
        let mut shuffled = sids.clone();
        shuffled.shuffle(&mut rng);
        let take = ((file_root_count as f32) * params.thoroughness).ceil() as usize;
        shuffled.truncate(take.max(1));
        shuffled
    };
    let sample_count = sample.len();

    log::debug!(target: "sumo_native::serve",
        "debug: file={} sampled={}/{} thoroughness={}",
        tag, sample_count, file_root_count, params.thoroughness);

    // -- SInE expansion.
    let tolerance = params.scope.unwrap_or_else(|| SineParams::default().tolerance);
    let sine_params = SineParams::benevolent(tolerance);
    let query_kif: String = sample.iter()
        .map(|sid| kb.sentence_kif_str(*sid))
        .collect::<Vec<_>>()
        .join("\n");
    let selected = match kb.sine_select_for_query(&query_kif, sine_params) {
        Ok(s)  => s,
        Err(e) => return err_response(INTERNAL_ERROR,
            format!("SInE selection failed: {}", e)),
    };

    let mut check_set: HashSet<SentenceId> = sample.iter().copied().collect();
    check_set.extend(selected.iter().copied());
    let check_total = check_set.len();

    // -- Files-pulled inventory.
    let mut files_pulled: BTreeSet<String> = BTreeSet::new();
    for sid in selected.iter() {
        let Some(sent) = kb.sentence(*sid) else { continue };
        if sent.file == tag { continue; }
        if sent.file.starts_with("__") { continue; }
        files_pulled.insert(sent.file.clone());
    }

    // -- Consistency check.
    let runner = VampireRunner {
        vampire_path,
        timeout_secs: params.timeout_secs,
        tptp_dump_path: None,
    };
    let result = kb.check_consistency(&check_set, &runner, TptpLang::Fof);

    // -- Status mapping.
    let status = match result.status {
        ProverStatus::Consistent   => "Consistent",
        ProverStatus::Inconsistent => "Inconsistent",
        ProverStatus::Timeout      => "Timeout",
        ProverStatus::Unknown      => "Unknown",
        ProverStatus::Proved       => "Proved (unexpected)",
        ProverStatus::Disproved    => "Disproved (unexpected)",
    };

    // -- Contradiction + proof extraction (only when Inconsistent).
    let (contradictions, proof_kif) = if matches!(result.status, ProverStatus::Inconsistent)
        && !result.proof_kif.is_empty()
    {
        let src_idx = kb.build_axiom_source_index();

        // Summary: dedupe contributing axioms by sid, preserving proof order.
        let mut seen: HashSet<SentenceId> = HashSet::new();
        let mut contradictions: Vec<ContradictionEntry> = Vec::new();
        for step in &result.proof_kif {
            if step.rule != "axiom" { continue; }
            // Two-tier resolution mirror of `print_step_source`.
            let mut matched: Vec<&sigmakee_rs_core::AxiomSource> = Vec::new();
            if let Some(sid) = step.source_sid {
                if let Some(src) = src_idx.lookup_by_sid(sid) {
                    if !src.file.starts_with("__") { matched.push(src); }
                }
            }
            if matched.is_empty() {
                for src in src_idx.lookup(&step.formula) {
                    if !src.file.starts_with("__") { matched.push(src); }
                }
            }
            for src in matched {
                if !seen.insert(src.sid) { continue; }
                contradictions.push(ContradictionEntry {
                    sid:  src.sid,
                    file: src.file.clone(),
                    line: src.line,
                    kif:  kb.sentence_kif_str(src.sid),
                });
            }
        }

        // Full proof: each step gets axiom sources attached when
        // applicable.  Same resolution dispatch as the summary.
        let proof_steps: Vec<ProofStepEntry> = result.proof_kif.iter().map(|step| {
            let (src_sid, src_file, src_line) = if step.rule == "axiom" {
                resolve_proof_step_source(step, &src_idx)
            } else {
                (None, None, None)
            };
            ProofStepEntry {
                index:       step.index,
                rule:        step.rule.clone(),
                premises:    step.premises.clone(),
                formula:     step.formula.to_string(),
                source_sid:  src_sid,
                source_file: src_file,
                source_line: src_line,
            }
        }).collect();

        (contradictions, proof_steps)
    } else {
        (Vec::new(), Vec::new())
    };

    ok_response(DebugResponse {
        file:           tag,
        root_sentences: file_root_count,
        sampled:        sample_count,
        sine_expanded:  check_total.saturating_sub(sample_count),
        total_checked:  check_total,
        tolerance,
        files_pulled:   files_pulled.into_iter().collect(),
        status:         status.to_string(),
        contradictions,
        proof_kif,
        raw:            result.raw_output,
    })
}

/// Resolve one axiom-role proof step to its source `(sid, file, line)`
/// using the same two-tier strategy `print_step_source` uses in the
/// CLI path: sid-direct first, canonical-hash fallback, skip
/// ephemeral files.
fn resolve_proof_step_source(
    step:    &sigmakee_rs_core::KifProofStep,
    src_idx: &sigmakee_rs_core::AxiomSourceIndex,
) -> (Option<SentenceId>, Option<String>, Option<u32>) {
    if let Some(sid) = step.source_sid {
        if let Some(src) = src_idx.lookup_by_sid(sid) {
            if !src.file.starts_with("__") {
                return (Some(src.sid), Some(src.file.clone()), Some(src.line));
            }
        }
    }
    for src in src_idx.lookup(&step.formula) {
        if !src.file.starts_with("__") {
            return (Some(src.sid), Some(src.file.clone()), Some(src.line));
        }
    }
    (None, None, None)
}

fn handle_test(
    kb:           &mut KnowledgeBase,
    vampire_path: Option<&PathBuf>,
    params:       &Value,
) -> Response {
    let params: TestParams = match serde_json::from_value(params.clone()) {
        Ok(p)  => p,
        Err(e) => return err_response(INVALID_PARAMS, format!("invalid test params: {}", e)),
    };

    // Gather `.kif.tq` files, same directory-walk rules as `run_test`.
    let mut test_files: Vec<PathBuf> = Vec::new();
    for raw in &params.paths {
        let path = PathBuf::from(raw);
        if path.is_dir() {
            let entries = match std::fs::read_dir(&path) {
                Ok(e)  => e,
                Err(e) => return err_response(INTERNAL_ERROR,
                    format!("failed to read directory '{}': {}", raw, e)),
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() && p.to_string_lossy().ends_with(".kif.tq") {
                    test_files.push(p);
                }
            }
        } else if path.is_file() {
            test_files.push(path);
        } else {
            return err_response(INVALID_PARAMS,
                format!("path not found: '{}'", raw));
        }
    }
    test_files.sort();
    test_files.dedup();
    if test_files.is_empty() {
        return err_response(INVALID_PARAMS, "no .kif.tq files found".into());
    }

    // Resolve the Vampire binary only when the subprocess backend is
    // in play (the embedded backend has no external dependency).
    let resolved_vampire: Option<PathBuf> = if params.backend != "embedded" {
        match vampire_path {
            Some(p) => Some(p.clone()),
            None => return err_response(INTERNAL_ERROR,
                "vampire binary not available; set `--vampire <PATH>` at kernel startup".into()),
        }
    } else {
        None
    };

    let lang = parse_lang(&params.lang);
    let total = test_files.len();
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut results: Vec<TestCaseResult> = Vec::with_capacity(total);

    let prover_backend = if params.backend == "embedded" {
        #[cfg(feature = "integrated-prover")]
        { sigmakee_rs_sdk::ProverBackend::Embedded }
        #[cfg(not(feature = "integrated-prover"))]
        { sigmakee_rs_sdk::ProverBackend::Subprocess }
    } else {
        sigmakee_rs_sdk::ProverBackend::Subprocess
    };

    for test_file in &test_files {
        let file_display = test_file.display().to_string();
        let content = match std::fs::read_to_string(test_file) {
            Ok(c)  => c,
            Err(e) => {
                results.push(TestCaseResult {
                    file:             file_display.clone(),
                    note:             file_display.clone(),
                    outcome:          "Error".into(),
                    expected_proof:   true,
                    actual_proved:    false,
                    expected_answers: None,
                    found_answers:    Vec::new(),
                    missing_answers:  Vec::new(),
                    error:            Some(format!("read error: {}", e)),
                });
                failed += 1;
                continue;
            }
        };
        let mut test_case = match parse_test_content(&content, &file_display) {
            Ok(tc) => tc,
            Err(e) => {
                results.push(TestCaseResult {
                    file:             file_display.clone(),
                    note:             file_display.clone(),
                    outcome:          "Error".into(),
                    expected_proof:   true,
                    actual_proved:    false,
                    expected_answers: None,
                    found_answers:    Vec::new(),
                    missing_answers:  Vec::new(),
                    error:            Some(format!("parse error: {}", e)),
                });
                failed += 1;
                continue;
            }
        };
        if let Some(t) = params.timeout_secs {
            test_case.timeout = t;
        }
        let expected = test_case.expected_proof.unwrap_or(true);
        let expected_answers = test_case.expected_answer.clone();
        let note = test_case.note.clone();

        // Drive TestOp for this single case.  Per-case session
        // management, axiom load, validation, prover invocation, and
        // post-run flush all happen inside TestOp::run().  We
        // translate `TestOutcome` back into the wire-shaped
        // `TestCaseResult` the JSON-RPC client expects.
        let mut op = sigmakee_rs_sdk::TestOp::new(kb)
            .add_case(file_display.clone(), test_case)
            .backend(prover_backend)
            .lang(lang);
        if let Some(p) = resolved_vampire.clone() { op = op.vampire_path(p); }

        let suite = match op.run() {
            Ok(s) => s,
            Err(e) => {
                results.push(TestCaseResult {
                    file:             file_display.clone(),
                    note:             note.clone(),
                    outcome:          "Error".into(),
                    expected_proof:   expected,
                    actual_proved:    false,
                    expected_answers: expected_answers.clone(),
                    found_answers:    Vec::new(),
                    missing_answers:  Vec::new(),
                    error:            Some(format!("test op: {}", e)),
                });
                failed += 1;
                continue;
            }
        };
        let case = match suite.cases.into_iter().next() {
            Some(c) => c,
            None => {
                // TestOp returned an empty suite — internal error.
                results.push(TestCaseResult {
                    file:             file_display.clone(),
                    note:             note.clone(),
                    outcome:          "Error".into(),
                    expected_proof:   expected,
                    actual_proved:    false,
                    expected_answers: expected_answers.clone(),
                    found_answers:    Vec::new(),
                    missing_answers:  Vec::new(),
                    error:            Some("TestOp returned an empty case list".into()),
                });
                failed += 1;
                continue;
            }
        };

        match case.outcome {
            sigmakee_rs_sdk::TestOutcome::Passed => {
                results.push(TestCaseResult {
                    file:             file_display,
                    note,
                    outcome:          "Passed".into(),
                    expected_proof:   expected,
                    actual_proved:    expected,
                    expected_answers,
                    found_answers:    Vec::new(),
                    missing_answers:  Vec::new(),
                    error:            None,
                });
                passed += 1;
            }
            sigmakee_rs_sdk::TestOutcome::Failed { expected: e, got } => {
                results.push(TestCaseResult {
                    file:             file_display,
                    note,
                    outcome:          "Failed".into(),
                    expected_proof:   e,
                    actual_proved:    got,
                    expected_answers,
                    found_answers:    Vec::new(),
                    missing_answers:  Vec::new(),
                    error:            None,
                });
                failed += 1;
            }
            sigmakee_rs_sdk::TestOutcome::Incomplete { inferred, missing } => {
                results.push(TestCaseResult {
                    file:             file_display,
                    note,
                    outcome:          "Incomplete".into(),
                    expected_proof:   expected,
                    actual_proved:    expected,
                    expected_answers,
                    found_answers:    inferred,
                    missing_answers:  missing,
                    error:            None,
                });
                failed += 1;
            }
            sigmakee_rs_sdk::TestOutcome::ParseError(msg) => {
                results.push(TestCaseResult {
                    file:             file_display,
                    note,
                    outcome:          "Error".into(),
                    expected_proof:   expected,
                    actual_proved:    false,
                    expected_answers,
                    found_answers:    Vec::new(),
                    missing_answers:  Vec::new(),
                    error:            Some(format!("axiom parse error(s): {}", msg)),
                });
                failed += 1;
            }
            sigmakee_rs_sdk::TestOutcome::SemanticError(msg) => {
                results.push(TestCaseResult {
                    file:             file_display,
                    note,
                    outcome:          "Error".into(),
                    expected_proof:   expected,
                    actual_proved:    false,
                    expected_answers,
                    found_answers:    Vec::new(),
                    missing_answers:  Vec::new(),
                    error:            Some(format!("semantic error(s): {}", msg)),
                });
                failed += 1;
            }
            sigmakee_rs_sdk::TestOutcome::ProverError(msg) => {
                results.push(TestCaseResult {
                    file:             file_display,
                    note,
                    outcome:          "Error".into(),
                    expected_proof:   expected,
                    actual_proved:    false,
                    expected_answers,
                    found_answers:    Vec::new(),
                    missing_answers:  Vec::new(),
                    error:            Some(msg),
                });
                failed += 1;
            }
            sigmakee_rs_sdk::TestOutcome::NoQuery => {
                results.push(TestCaseResult {
                    file:             file_display,
                    note,
                    outcome:          "Error".into(),
                    expected_proof:   expected,
                    actual_proved:    false,
                    expected_answers,
                    found_answers:    Vec::new(),
                    missing_answers:  Vec::new(),
                    error:            Some("test file has no (query …) form".into()),
                });
                failed += 1;
            }
        }
    }

    ok_response(TestResponse { total, passed, failed, results })
}

fn handle_reconcile_file(
    kb:         &mut KnowledgeBase,
    persistent: bool,
    params:     &Value,
) -> Response {
    let params: ReconcileFileParams = match serde_json::from_value(params.clone()) {
        Ok(p)  => p,
        Err(e) => return err_response(INVALID_PARAMS,
            format!("invalid kb.reconcileFile params: {}", e)),
    };

    // Resolve the text: inline if the caller provided it,
    // otherwise read from disk.  The VSCode extension sends the
    // disk-only form (Option A per the plan) -- `text` is there
    // for future in-buffer syncs if we revisit that decision.
    let text = match &params.text {
        Some(t) => t.clone(),
        None => match fs::read_to_string(&params.path) {
            Ok(t)  => t,
            Err(e) => return err_response(INTERNAL_ERROR,
                format!("failed to read '{}': {}", params.path, e)),
        },
    };

    log::debug!(target: "sumo_native::serve",
        "kb.reconcileFile: path={} bytes={}", params.path, text.len());

    let report = kb.reconcile_file(&params.path, &text);
    let added   = report.added();
    let removed = report.removed();

    // Parse errors are fatal for this file -- the recovered
    // sentences are NOT committed (matches the LSP's "entire file
    // invariant" for parse failures).  The caller gets the error
    // list and can act on it (e.g. show Problems panel entries).
    if !report.parse_errors.is_empty() {
        let errors: Vec<String> = report.parse_errors.iter().map(|e| e.to_string()).collect();
        log::info!(target: "sumo_native::serve",
            "kb.reconcileFile: {} has {} parse error(s); delta not persisted",
            params.path, errors.len());
        return ok_response(ReconcileFileResponse {
            path:            params.path,
            added:           0,
            removed:         0,
            retained:        report.retained,
            revalidated:     report.revalidated,
            parse_errors:    errors,
            semantic_errors: report.semantic_errors.iter().map(|e| e.to_string()).collect(),
            persisted:       false,
        });
    }

    // Commit the delta to LMDB (if persistent).  Semantic errors
    // only populate under `-W` / `-Wall`; surface them to the
    // caller but don't refuse the commit -- editor workflows want
    // the DB to reflect whatever parsed cleanly.
    let mut persisted = false;
    if persistent && (!report.removed_sids.is_empty() || !report.added_sids.is_empty()) {
        if let Err(e) = kb.persist_reconcile_diff(&report.removed_sids, &report.added_sids) {
            return err_response(INTERNAL_ERROR,
                format!("persist_reconcile_diff failed for '{}': {}", params.path, e));
        }
        persisted = true;
    }

    ok_response(ReconcileFileResponse {
        path:            params.path,
        added,
        removed,
        retained:        report.retained,
        revalidated:     report.revalidated,
        parse_errors:    Vec::new(),
        semantic_errors: report.semantic_errors.iter().map(|e| e.to_string()).collect(),
        persisted,
    })
}

fn handle_remove_file(
    kb:         &mut KnowledgeBase,
    persistent: bool,
    params:     &Value,
) -> Response {
    let params: RemoveFileParams = match serde_json::from_value(params.clone()) {
        Ok(p)  => p,
        Err(e) => return err_response(INVALID_PARAMS,
            format!("invalid kb.removeFile params: {}", e)),
    };

    // Capture the roots *before* `remove_file` drops them -- we
    // need the ids for the persistent deletion pass.
    let sids: Vec<SentenceId> = kb.file_roots(&params.path).to_vec();
    let removed = sids.len();

    log::debug!(target: "sumo_native::serve",
        "kb.removeFile: path={} removing={}", params.path, removed);

    kb.remove_file(&params.path);

    let mut persisted = false;
    if persistent && !sids.is_empty() {
        if let Err(e) = kb.persist_reconcile_diff(&sids, &[]) {
            return err_response(INTERNAL_ERROR,
                format!("persist delete failed for '{}': {}", params.path, e));
        }
        persisted = true;
    }

    ok_response(RemoveFileResponse { removed, persisted })
}

fn handle_flush(
    kb:         &mut KnowledgeBase,
    persistent: bool,
    _db_path:   &PathBuf,
) -> Response {
    // Gather the full file set first; `remove_file` mutates the
    // iterator's source so we have to snapshot.
    let files: Vec<String> = kb.iter_files().map(|s| s.to_string()).collect();
    log::info!(target: "sumo_native::serve",
        "kb.flush: clearing {} file(s)", files.len());

    let mut sentences_removed = 0usize;
    let mut persisted         = false;
    for file in &files {
        let sids: Vec<SentenceId> = kb.file_roots(file).to_vec();
        sentences_removed += sids.len();
        kb.remove_file(file);
        if persistent && !sids.is_empty() {
            if let Err(e) = kb.persist_reconcile_diff(&sids, &[]) {
                return err_response(INTERNAL_ERROR,
                    format!("flush: persist delete failed for '{}': {}", file, e));
            }
            persisted = true;
        }
    }

    ok_response(FlushResponse {
        files_removed:    files.len(),
        sentences_removed,
        persisted,
    })
}

fn handle_generate_tptp(kb: &KnowledgeBase, params: &Value) -> Response {
    let params: GenerateTptpParams = match serde_json::from_value(params.clone()) {
        Ok(p)  => p,
        Err(e) => return err_response(INVALID_PARAMS,
            format!("invalid kb.generateTptp params: {}", e)),
    };

    // Resolve the dialect.  Unknown strings log and default to
    // fof rather than failing the request -- matches the tolerant
    // posture used by the CLI's `--lang` flag.
    let lang = match params.lang.to_ascii_lowercase().as_str() {
        "fof" => TptpLang::Fof,
        "tff" => TptpLang::Tff,
        other => {
            log::warn!(target: "sumo_native::serve",
                "kb.generateTptp: unknown lang '{}', defaulting to fof", other);
            TptpLang::Fof
        }
    };

    let opts = TptpOptions {
        lang,
        // KB export is *not* a conjecture, so leave `query = false`.
        // `show_kif_comment = true` adds `% <KIF>` comments above
        // each formula -- hugely useful when a user eyeballs the
        // emitted TPTP to understand what got through.
        show_kif_comment: true,
        ..TptpOptions::default()
    };

    log::debug!(target: "sumo_native::serve",
        "kb.generateTptp: lang={} session={:?}",
        lang.as_str(), params.session);

    let tptp = kb.to_tptp(&opts, params.session.as_deref());

    // Coarse formula count: match the `fof(`, `tff(`, and `cnf(`
    // prefixes at line starts.  Good enough for a status-bar
    // message; not a contract the caller should rely on for
    // anything precise.
    let formula_count = tptp.lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("fof(") || t.starts_with("tff(") || t.starts_with("cnf(")
        })
        .count();

    ok_response(GenerateTptpResponse {
        tptp,
        formula_count,
        lang: lang.as_str().to_string(),
    })
}

fn handle_list_files(kb: &KnowledgeBase) -> Response {
    // `iter_files()` borrows the store; collect into owned strings
    // so the response serialisation doesn't fight the borrow.
    let entries: Vec<FileEntry> = kb.iter_files()
        .map(|path| FileEntry {
            path:           path.to_string(),
            sentence_count: kb.file_roots(path).len(),
        })
        .collect();
    ok_response(ListFilesResponse { files: entries })
}

// -- Helpers ------------------------------------------------------------------

fn ok_response<T: Serialize>(body: T) -> Response {
    Response {
        id:     Value::Null,
        result: Some(serde_json::to_value(body).expect("serialisable")),
        error:  None,
    }
}

fn err_response(code: i32, message: String) -> Response {
    Response {
        id:     Value::Null,
        result: None,
        error:  Some(ResponseError { code, message }),
    }
}

/// Write a response envelope followed by a newline.  Best-effort:
/// if stdout has been closed (client hung up), we log and let the
/// loop terminate on the next read.
fn write_response<W: Write>(writer: &mut W, resp: &Response) {
    let serialised = serde_json::to_string(resp).expect("response serialisable");
    if let Err(e) = writeln!(writer, "{}", serialised) {
        log::warn!(target: "sumo_native::serve",
            "failed to write response: {}", e);
        return;
    }
    // Without the explicit flush each line can sit in the stdout
    // buffer until the process exits -- the client would time out
    // waiting for a response to a request it already sent.
    let _ = writer.flush();
}
