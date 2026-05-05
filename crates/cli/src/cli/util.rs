use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};

use sigmakee_rs_core::{KbError, KnowledgeBase, Span, TptpLang};
use sigmakee_rs_sdk::{IngestOp, LoadOp, SdkError};

use crate::cli::args::KbArgs;
// `expand_tilde` lives in `crate::config`; imported here so CLI
// argument paths (e.g. `--vampire "~/bin/vampire"`) pick up the same
// `~`-expansion as `--config` does.
use crate::config::expand_tilde;
use crate::parse_error;

// -- LMDB / KB helpers --------------------------------------------------------

/// Open an existing LMDB-backed `KnowledgeBase`.
/// Fails with a log error if the database directory does not exist.
pub fn open_existing_kb(args: &KbArgs) -> Result<KnowledgeBase, ()> {
    if !args.db.exists() {
        log::error!(
            "Database not found at '{}': run 'sumo load' first to initialise it",
            args.db.display()
        );
        return Err(());
    }
    KnowledgeBase::open(&args.db).map_err(|e| {
        log::error!("Failed to open database at '{}': {}", args.db.display(), e);
    })
}

/// Build a `KnowledgeBase` for read-only commands (`validate`, `ask`, `translate`, `test`).
///
/// - If `--db` exists, opens it.  Otherwise starts with an empty in-memory KB.
/// - If `-f`/`-d` files are given, bulk-loads them on top as in-memory axioms
///   (never commits to the database).
/// - If neither DB nor files are present, returns an empty KB with a warning.
pub fn open_or_build_kb(args: &KbArgs) -> Result<KnowledgeBase, ()> {
    open_or_build_kb_profiled(args, None)
}

/// Like [`open_or_build_kb`], but installs the given progress sink
/// onto the KB *before* the file-ingest and promote passes so those
/// phases are captured.  Pass `None` to get the un-profiled
/// behaviour (identical to [`open_or_build_kb`]).
///
/// File-loading orchestration is delegated to [`sigmakee_rs_sdk::IngestOp`].
/// The CLI keeps two concerns it owns:
///
/// 1. **Parallel pre-read** of disk files (via rayon) before handing
///    text-only sources to `IngestOp::add_sources`.  IngestOp's own
///    `add_file` path reads serially; pre-reading in parallel keeps
///    the bootstrap-load wall-clock tight on multi-file workspaces.
/// 2. **`parse_error!`-flavoured error rendering** — the SDK returns
///    a plain `SdkError::Kb(KbError::Parse(...))`, which we translate
///    back into the colourised macro using the source text we
///    already have in hand.
pub fn open_or_build_kb_profiled(
    args: &KbArgs,
    sink: Option<sigmakee_rs_core::DynSink>,
) -> Result<KnowledgeBase, ()> {
    let has_files = !args.files.is_empty() || !args.dirs.is_empty();

    let mut kb = if args.no_db {
        if !has_files {
            log::warn!("--no-db specified and no -f files given -- using empty KB");
        }
        KnowledgeBase::new()
    } else if args.db.exists() {
        KnowledgeBase::open(&args.db).map_err(|e| {
            log::error!("Failed to open database at '{}': {}", args.db.display(), e);
        })?
    } else {
        if !has_files {
            log::warn!(
                "No database found at '{}' and no -f files specified -- using empty KB",
                args.db.display()
            );
        }
        KnowledgeBase::new()
    };

    // Install the progress sink BEFORE any ingest so every phase
    // event flows through it from the very first instrumented call.
    if let Some(s) = sink {
        kb.set_progress_sink(s);
    }

    if has_files {
        let loaded = read_files_parallel(args)?;
        ingest_via_sdk(&mut kb, loaded)?;
    }

    Ok(kb)
}

/// Phase 1: parallel-read all `-f` / `-d` files (rayon when on,
/// serial otherwise).  Returns `(tag, text)` pairs in input order.
/// Read failures abort the whole load — matches the legacy
/// behaviour and gives the user a single "first bad file" message
/// rather than N races.
fn read_files_parallel(args: &KbArgs) -> Result<Vec<(String, String)>, ()> {
    let all_files = collect_kif_files(args)?;
    if all_files.is_empty() {
        return Ok(Vec::new());
    }

    #[cfg(feature = "parallel")]
    let raw: Vec<(PathBuf, Result<String, ()>)> = {
        use rayon::prelude::*;
        all_files.par_iter().map(|path| {
            (path.clone(), read_kif_file(path))
        }).collect()
    };
    #[cfg(not(feature = "parallel"))]
    let raw: Vec<(PathBuf, Result<String, ()>)> =
        all_files.iter().map(|path| {
            (path.clone(), read_kif_file(path))
        }).collect();

    let mut out: Vec<(String, String)> = Vec::with_capacity(raw.len());
    for (path, text_result) in raw {
        let text = text_result?;            // read_kif_file already logged
        out.push((path.display().to_string(), text));
    }
    Ok(out)
}

/// Phase 2: drive `IngestOp` over the pre-read sources.  On a
/// parse-flavoured failure we re-render via `parse_error!` using
/// the file's text (which we still hold in `loaded`); other SDK
/// errors get the plain `log::error!` treatment.
fn ingest_via_sdk(
    kb:     &mut KnowledgeBase,
    loaded: Vec<(String, String)>,
) -> Result<(), ()> {
    let count = loaded.len();
    // Keep a copy of the text per tag so `parse_error!` can find
    // the offending source line when the SDK aborts.
    let by_tag: std::collections::HashMap<String, String> = loaded
        .iter()
        .map(|(t, s)| (t.clone(), s.clone()))
        .collect();

    let result = IngestOp::new(kb).add_sources(loaded).run();
    match result {
        Ok(_report) => {
            log::info!("loaded {} file(s) as in-memory axioms", count);
            Ok(())
        }
        Err(SdkError::Kb(KbError::Parse(p))) => {
            // The span carries the file tag; look up the source
            // text we read in phase 1 to render with context.
            let span = p.get_span();
            if let Some(text) = by_tag.get(&span.file) {
                parse_error!(span, p, text);
            } else {
                parse_error!(span, p);
            }
            Err(())
        }
        Err(SdkError::Kb(e)) => {
            log::error!("ingest failed: {}", e);
            Err(())
        }
        Err(e) => {
            log::error!("ingest failed: {}", e);
            Err(())
        }
    }
}

// -- KIF file loading ----------------------------------------------------------

/// Parse all KIF files referenced by `args` into an in-memory `KnowledgeBase`
/// (no LMDB).  Returns `Err(())` and logs errors on failure.
///
/// All loaded sentences are immediately promoted to axioms so that a
/// subsequent [`KnowledgeBase::ask`] call includes them in the TPTP problem.
///
/// Now a thin wrapper over [`sigmakee_rs_sdk::IngestOp`] — the previous
/// hand-rolled loop has been folded into the same SDK code path
/// `open_or_build_kb_profiled` uses.
pub fn build_kb_from_files(args: &KbArgs) -> Result<KnowledgeBase, ()> {
    let mut kb = KnowledgeBase::new();
    let loaded = read_files_parallel(args)?;
    ingest_via_sdk(&mut kb, loaded)?;
    Ok(kb)
}

/// Parse KIF files -> open/create LMDB -> clausify -> commit to database.
///
/// Returns the `KnowledgeBase` (still open against the LMDB) so the caller
/// can run further operations (validation, translation) in the same session.
///
/// Delegates to [`sigmakee_rs_sdk::LoadOp`] for the reconcile + persist
/// pipeline.  `parse_error!`-flavoured rendering is preserved by
/// pre-reading each file and threading the text into the error
/// translation if `LoadOp::run` aborts on a parse failure.
pub fn load_and_commit_files(args: &KbArgs) -> Result<KnowledgeBase, ()> {
    let mut kb = KnowledgeBase::open(&args.db).map_err(|e| {
        log::error!("Failed to open database at '{}': {}", args.db.display(), e);
    })?;

    let loaded = read_files_parallel(args)?;
    let count  = loaded.len();
    let by_tag: std::collections::HashMap<String, String> = loaded
        .iter()
        .map(|(t, s)| (t.clone(), s.clone()))
        .collect();

    let result = LoadOp::new(&mut kb).add_sources(loaded).run();
    match result {
        Ok(report) if report.committed => {
            log::info!(
                "load_and_commit_files: committed {} file(s) to LMDB at '{}': +{} -{}",
                count, args.db.display(), report.total_added, report.total_removed,
            );
            Ok(kb)
        }
        Ok(report) => {
            // Strict mode aborted the commit because of semantic errors.
            log::error!(
                "load_and_commit_files: {} semantic error(s) blocked commit",
                report.semantic_errors.len(),
            );
            for (_, e) in &report.semantic_errors { log::error!("{}", e); }
            Err(())
        }
        Err(SdkError::Kb(KbError::Parse(p))) => {
            let span = p.get_span();
            if let Some(text) = by_tag.get(&span.file) {
                parse_error!(span, p, text);
            } else {
                parse_error!(span, p);
            }
            Err(())
        }
        Err(e) => {
            log::error!("load_and_commit_files: {}", e);
            Err(())
        }
    }
}

// -- Internal helpers ----------------------------------------------------------

pub(crate) fn collect_kif_files(args: &KbArgs) -> Result<Vec<PathBuf>, ()> {
    let mut all_files: Vec<PathBuf> = args.files.clone();
    for dir in &args.dirs {
        match kif_files_in_dir(dir) {
            Ok(f) => all_files.extend(f),
            Err((span, e)) => {
                parse_error!(span, e);
                return Err(());
            }
        }
    }
    log::debug!("collect_kif_files: {} file(s)", all_files.len());
    Ok(all_files)
}

pub(crate) fn read_kif_file(path: &Path) -> Result<String, ()> {
    std::fs::read_to_string(path).map_err(|e| {
        log::error!("cannot read '{}': {}", path.display(), e);
    })
}

// -- Directory helpers ---------------------------------------------------------

/// Collect all `*.kif` files in a directory, sorted for deterministic ordering.
pub fn kif_files_in_dir(dir: &Path) -> Result<Vec<PathBuf>, (Span, KbError)> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        let span = Span::point(format!("{}", dir.display()), 0, 0, 0);
        (span, KbError::Other(format!("cannot read directory '{}': {}", dir.display(), e)))
    })?;
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("kif"))
        .collect();
    files.sort();
    Ok(files)
}

// -- stdin / source tag --------------------------------------------------------

/// Read stdin if it is piped (not a TTY); return `None` if empty or a TTY.
pub fn read_stdin() -> Option<String> {
    if io::stdin().is_terminal() {
        return None;
    }
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf).ok();
    if buf.trim().is_empty() { None } else { Some(buf) }
}

/// File tag used for formulas supplied inline or via stdin.
pub fn source_tag() -> &'static str {
    if io::stdin().is_terminal() { "<inline>" } else { "<stdin>" }
}

pub fn parse_lang(s: &str) -> TptpLang {
    match s { "tff" => TptpLang::Tff, _ => TptpLang::Fof }
}

// -- Vampire binary discovery --------------------------------------------------

/// Resolve the caller-supplied Vampire path to an existing executable, or
/// error out with a clear message pointing to the root cause.
///
/// When the user selects the `subprocess` prover backend without
/// `--integrated-prover`, spawning an unresolved binary would silently
/// return `ProverStatus::Unknown` with the spawn error buried in
/// `raw_output` (only visible with `-v`).  Running this up-front turns
/// that into a visible `log::error!` and a non-zero exit code before we
/// waste time clausifying the KB for a prover that can't run.
///
/// Resolution rules:
/// * If `candidate` contains a separator (absolute or relative like
///   `./vampire`, `~/bin/vampire`), it is checked directly.
/// * Otherwise it is treated as a PATH name and each `$PATH` entry is
///   probed for an existing regular file with that name.
/// * `~` is expanded via `$HOME` on Unix as a courtesy — `Command::spawn`
///   does not expand it and the XML config commonly stores
///   `~/path/to/vampire`.
pub fn resolve_vampire_path(candidate: &Path) -> Result<PathBuf, ()> {
    let expanded = expand_tilde(candidate);

    let has_separator = expanded.components().count() > 1
        || expanded.is_absolute()
        || candidate.to_string_lossy().contains('/');

    if has_separator {
        if expanded.is_file() {
            return Ok(expanded);
        }
        log::error!(
            "vampire binary not found at '{}': supply a valid --vampire path, \
             set <preference name=\"inferenceEngine\"> in config.xml, or install \
             the integrated prover with `--features integrated-prover`",
            expanded.display()
        );
        return Err(());
    }

    // Bare name → walk $PATH.
    let name = expanded.as_os_str();
    if let Some(path_env) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_env) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    log::error!(
        "vampire binary '{}' not found on PATH: supply an explicit --vampire \
         path, set <preference name=\"inferenceEngine\"> in config.xml, or \
         install the integrated prover with `--features integrated-prover`",
        candidate.display()
    );
    Err(())
}

