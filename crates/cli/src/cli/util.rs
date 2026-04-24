use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};

use sumo_kb::{KbError, KnowledgeBase, Span, TptpLang};

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

/// Like [`open_or_build_kb`], but installs the given profiler onto
/// the KB *before* the file-ingest and promote passes so those
/// phases are captured in the profile report.  Pass `None` to get
/// the un-profiled behaviour (identical to [`open_or_build_kb`]).
pub fn open_or_build_kb_profiled(
    args: &KbArgs,
    profiler: Option<std::sync::Arc<sumo_kb::Profiler>>,
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

    // Install the profiler BEFORE any ingest/promote so every phase
    // goes through instrumented code paths.
    if let Some(p) = profiler {
        kb.set_profiler(p);
    }

    if has_files {
        let all_files = collect_kif_files(args)?;
        const BASE: &str = sumo_kb::session_tags::SESSION_FILES;

        // Phase 1: read every file's contents.  I/O is independent
        // per file and embarrassingly parallel when the `parallel`
        // feature is on — hides disk latency across the batch.
        // Each element of `loaded` is `(path, text)` preserving the
        // input order from `all_files`.
        //
        // read_kif_file returns `Result<String, ()>`; we short-circuit
        // on the first failure after the parallel phase completes.
        //
        // Instrumented against the KB's profiler (if installed) under
        // `load.read_files` so the fine-grained `--profile` report
        // attributes the wall-clock to this phase.
        let _t_read = std::time::Instant::now();
        #[cfg(feature = "parallel")]
        let loaded: Vec<(PathBuf, Result<String, ()>)> = {
            use rayon::prelude::*;
            all_files.par_iter().map(|path| {
                (path.clone(), read_kif_file(path))
            }).collect()
        };
        #[cfg(not(feature = "parallel"))]
        let loaded: Vec<(PathBuf, Result<String, ()>)> =
            all_files.iter().map(|path| {
                (path.clone(), read_kif_file(path))
            }).collect();
        if let Some(p) = kb.profiler() {
            p.record("load.read_files", _t_read.elapsed());
        }

        // Phase 2: ingest each file serially.
        //
        // For every file we handle two cases:
        //   (a) The file tag is already present in `kb.file_roots()`
        //       because the DB was opened and it rehydrated sentences
        //       under that tag.  We diff the on-disk content against
        //       those sentences and apply the delta in-memory via
        //       `reconcile_file` — this is the "DB stale / disk
        //       fresh" case the user's reconcile design targets.
        //       The `ask` feature gate is required for reconcile
        //       (SInE maintenance lives behind it); builds without
        //       `ask` fall back to the classic load path and accept
        //       that stale DB axioms are visible.
        //   (b) The file tag is unknown to the KB — this is a fresh
        //       `-f` file.  Use the classic `load_kif` pipeline and
        //       let the trailing `make_session_axiomatic(BASE)`
        //       promote it to axiom status.
        //
        // Reconcile output is intentionally quiet here.  Only
        // `sumo load` promotes these deltas to the DB and surfaces
        // per-file add/remove/retain counts at info level.
        for (path, text_result) in loaded {
            let text = match text_result {
                Ok(t) => t,
                Err(()) => return Err(()),  // read_kif_file already logged
            };
            let tag = path.display().to_string();

            // DB-rehydrated files take the reconcile path; fresh
            // `-f` files (no existing entry in `file_roots`) fall
            // through to the classic `load_kif` loader below.
            // `reconcile_file` is now available in every feature
            // combo (previously `ask`-gated via SInE).
            if !kb.file_roots(&tag).is_empty() {
                let report = kb.reconcile_file(&tag, &text);
                if !report.parse_errors.is_empty() {
                    for e in &report.parse_errors {
                        log::error!("{}: {}", path.display(), e);
                    }
                    return Err(());
                }
                // Smart-revalidation findings surface at debug level
                // so `-v` (info) stays focused on higher-level
                // progress.  Each entry is a hard semantic error —
                // either naturally severe or promoted via
                // `-W` / `-Wall`.  Read-only commands (`ask`,
                // `validate`, `translate`, …) surface them but keep
                // going; only `sumo load` treats them as abort
                // triggers (see `cli::load`).
                for e in &report.semantic_errors {
                    log::debug!(target: "sumo_kb::reconcile",
                        "{}: {}", tag, e);
                }
                continue;
            }

            let result = kb.load_kif(&text, &tag, Some(BASE));
            if !result.ok {
                for e in &result.errors {
                    match e {
                        KbError::Parse(p) => parse_error!(p.get_span(), p, text),
                        _ => log::error!("{}: {}", path.display(), e)
                    }
                }
                return Err(());
            }
        }
        kb.make_session_axiomatic(BASE);
        log::info!("open_or_build_kb: loaded {} file(s) as in-memory axioms", all_files.len());
    }

    Ok(kb)
}

// -- KIF file loading ----------------------------------------------------------

/// Parse all KIF files referenced by `args` into an in-memory `KnowledgeBase`
/// (no LMDB).  Returns `Err(())` and logs errors on failure.
///
/// All loaded sentences are immediately promoted to axioms so that a
/// subsequent [`KnowledgeBase::ask`] call includes them in the TPTP problem.
pub fn build_kb_from_files(args: &KbArgs) -> Result<KnowledgeBase, ()> {
    let all_files = collect_kif_files(args)?;
    let mut kb = KnowledgeBase::new();
    const BASE: &str = sumo_kb::session_tags::SESSION_BASE;
    for path in &all_files {
        let text = read_kif_file(path)?;
        let tag = path.display().to_string();
        let result = kb.load_kif(&text, &tag, Some(BASE));
        if !result.ok {
            for e in &result.errors {
                match e {
                    KbError::Parse(p) => parse_error!(p.get_span(), p, text),
                    _ => log::error!("{}: {}", path.display(), e) 
                }
            }
            return Err(());
        }
    }
    kb.make_session_axiomatic(BASE);
    log::info!(
        "build_kb_from_files: loaded {} file(s) as axioms",
        all_files.len()
    );
    Ok(kb)
}

/// Parse KIF files -> open/create LMDB -> clausify -> commit to database.
///
/// Returns the `KnowledgeBase` (still open against the LMDB) so the caller
/// can run further operations (validation, translation) in the same session.
pub fn load_and_commit_files(args: &KbArgs) -> Result<KnowledgeBase, ()> {
    let all_files = collect_kif_files(args)?;

    let mut kb = KnowledgeBase::open(&args.db).map_err(|e| {
        log::error!("Failed to open database at '{}': {}", args.db.display(), e);
    })?;

    // kb.enable_cnf(ClausifyOptions { max_clauses_per_formula: args.max_clauses });

    const SESSION: &str = sumo_kb::session_tags::SESSION_LOAD;
    for path in &all_files {
        let text = read_kif_file(path)?;
        let tag = path.display().to_string();
        let result = kb.load_kif(&text, &tag, Some(SESSION));
        if !result.ok {
            for e in &result.errors {
                match e {
                    KbError::Parse(p) => parse_error!(p.get_span(), p, text),
                    _ => log::error!("{}: {}", path.display(), e) 
                }
            }
            return Err(());
        }
    }

    log::info!(
        "load_and_commit_files: promoting {} file(s) to LMDB at '{}'",
        all_files.len(),
        args.db.display()
    );
    kb.promote_assertions_unchecked(SESSION).map_err(|e| {
        log::error!("Failed to commit KB to database: {}", e);
    })?;

    Ok(kb)
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

