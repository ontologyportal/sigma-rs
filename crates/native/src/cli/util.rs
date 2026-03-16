use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};

use sumo_kb::{KnowledgeBase, ParseError, Span, TptpLang};

use crate::cli::args::KbArgs;
use crate::parse_error;

// ── LMDB / KB helpers ────────────────────────────────────────────────────────

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
    let has_files = !args.files.is_empty() || !args.dirs.is_empty();

    let mut kb = if args.db.exists() {
        KnowledgeBase::open(&args.db).map_err(|e| {
            log::error!("Failed to open database at '{}': {}", args.db.display(), e);
        })?
    } else {
        if !has_files {
            log::warn!(
                "No database found at '{}' and no -f files specified — using empty KB",
                args.db.display()
            );
        }
        KnowledgeBase::new()
    };

    if has_files {
        let all_files = collect_kif_files(args)?;
        const BASE: &str = "__files__";
        for path in &all_files {
            let text = read_kif_file(path)?;
            let tag = path.display().to_string();
            let result = kb.load_kif(&text, &tag, Some(BASE));
            if !result.ok {
                for e in &result.errors {
                    log::error!("{}: {}", path.display(), e);
                }
                return Err(());
            }
        }
        kb.make_session_axiomatic(BASE);
        log::info!("open_or_build_kb: loaded {} file(s) as in-memory axioms", all_files.len());
    }

    Ok(kb)
}

// ── KIF file loading ──────────────────────────────────────────────────────────

/// Parse all KIF files referenced by `args` into an in-memory `KnowledgeBase`
/// (no LMDB).  Returns `Err(())` and logs errors on failure.
///
/// All loaded sentences are immediately promoted to axioms so that a
/// subsequent [`KnowledgeBase::ask`] call includes them in the TPTP problem.
pub fn build_kb_from_files(args: &KbArgs) -> Result<KnowledgeBase, ()> {
    let all_files = collect_kif_files(args)?;
    let mut kb = KnowledgeBase::new();
    const BASE: &str = "__base__";
    for path in &all_files {
        let text = read_kif_file(path)?;
        let tag = path.display().to_string();
        let result = kb.load_kif(&text, &tag, Some(BASE));
        if !result.ok {
            for e in &result.errors {
                log::error!("{}: {}", path.display(), e);
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

/// Parse KIF files → open/create LMDB → clausify → commit to database.
///
/// Returns the `KnowledgeBase` (still open against the LMDB) so the caller
/// can run further operations (validation, translation) in the same session.
pub fn load_and_commit_files(args: &KbArgs) -> Result<KnowledgeBase, ()> {
    let all_files = collect_kif_files(args)?;

    let mut kb = KnowledgeBase::open(&args.db).map_err(|e| {
        log::error!("Failed to open database at '{}': {}", args.db.display(), e);
    })?;

    // kb.enable_cnf(ClausifyOptions { max_clauses_per_formula: args.max_clauses });

    const SESSION: &str = "__load__";
    for path in &all_files {
        let text = read_kif_file(path)?;
        let tag = path.display().to_string();
        let result = kb.load_kif(&text, &tag, Some(SESSION));
        if !result.ok {
            for e in &result.errors {
                log::error!("{}: {}", path.display(), e);
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

// ── Internal helpers ──────────────────────────────────────────────────────────

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

// ── Directory helpers ─────────────────────────────────────────────────────────

/// Collect all `*.kif` files in a directory, sorted for deterministic ordering.
pub fn kif_files_in_dir(dir: &Path) -> Result<Vec<PathBuf>, (Span, ParseError)> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        let span = Span { file: format!("{}", dir.display()), line: 0, col: 0, offset: 0 };
        (span, ParseError::Other { msg: format!("cannot read directory '{}': {}", dir.display(), e) })
    })?;
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("kif"))
        .collect();
    files.sort();
    Ok(files)
}

// ── stdin / source tag ────────────────────────────────────────────────────────

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
