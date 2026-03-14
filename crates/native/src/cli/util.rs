use std::collections::HashSet;
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};

use sumo_parser_core::{
    load_kif, KifStore, KnowledgeBase, ParseError, SemanticError, Span, TptpLang,
};
use sumo_store::{CommitOptions, LmdbEnv, StoreError, commit_kifstore, load_kifstore_from_db};

use crate::cli::args::KbArgs;
use crate::parse_error;

// ── Warning-set helpers ───────────────────────────────────────────────────────

/// Build a lookup set from a list of codes / names supplied via -W / --warning.
pub fn build_suppress_set(entries: &[String]) -> HashSet<String> {
    entries.iter().cloned().collect()
}

/// Returns true when `err` should be silently ignored.
pub fn is_suppressed(err: &SemanticError, suppress: &HashSet<String>) -> bool {
    suppress.contains(err.code()) || suppress.contains(err.name())
}

// ── LMDB helpers ──────────────────────────────────────────────────────────────

/// Open the LMDB environment at `args.db`.  The directory is created if it
/// does not exist.
pub fn open_db(args: &KbArgs) -> Result<LmdbEnv, StoreError> {
    log::info!("Opening database at {}", args.db.display());
    LmdbEnv::open(&args.db)
}

/// Open the LMDB environment only if it already exists on disk, returning
/// `StoreError::DatabaseNotFound` otherwise.
pub fn open_existing_db(args: &KbArgs) -> Result<LmdbEnv, StoreError> {
    if !args.db.exists() {
        return Err(StoreError::DatabaseNotFound { path: args.db.display().to_string() });
    }
    open_db(args)
}

// ── KIF file loading ──────────────────────────────────────────────────────────

/// Parse all KIF files referenced by `args` into an in-memory `KifStore`.
/// Returns `Err(())` and logs parse errors if any file fails.
pub fn build_store_from_files(args: &KbArgs) -> Result<KifStore, ()> {
    let mut all_files: Vec<PathBuf> = args.files.clone();
    for dir in &args.dirs {
        match kif_files_in_dir(dir) {
            Ok(f)          => all_files.extend(f),
            Err((span, e)) => { parse_error!(span, e); return Err(()); }
        }
    }
    log::debug!("build_store_from_files: found {} KIF file(s)", all_files.len());

    let mut store = KifStore::default();
    for path in &all_files {
        let text = std::fs::read_to_string(path).map_err(|e| {
            let fake_span = Span { file: format!("{}", path.display()), col: 0, line: 0, offset: 0 };
            let err = ParseError::Other { msg: format!("cannot read {}: {}", path.display(), e) };
            parse_error!(fake_span, err);
        })?;
        let tag    = path.display().to_string();
        let errors = load_kif(&mut store, &text, &tag);
        if !errors.is_empty() {
            for (span, e) in errors { parse_error!(span, e, text); }
            return Err(());
        }
    }
    log::info!(
        "build_store_from_files: loaded {} sentence(s) from {} file(s)",
        store.sentences.len(), all_files.len()
    );
    Ok(store)
}

/// Parse KIF files → validate → commit to LMDB.
///
/// Returns the opened `LmdbEnv` so the caller can run further operations
/// (e.g. additional validation or translation) within the same session.
pub fn load_and_commit_files(args: &KbArgs) -> Result<LmdbEnv, ()> {
    let store = build_store_from_files(args)?;
    let env   = open_db(args).map_err(|e| {
        log::error!("Database error: {}", e);
    })?;

    let opts = CommitOptions {
        max_clauses: args.max_clauses,
        session:     None,
    };

    commit_kifstore(&env, &store, &opts).map_err(|e| {
        log::error!("Failed to commit KB to database: {}", e);
    })?;

    log::info!(
        "load_and_commit_files: committed {} root sentence(s) to {}",
        store.roots.len(), args.db.display()
    );
    Ok(env)
}

/// Load a `KnowledgeBase` from the database for semantic validation.
pub fn load_kb_from_db(env: &LmdbEnv) -> Result<KnowledgeBase, ()> {
    let store = load_kifstore_from_db(env).map_err(|e| {
        log::error!("Failed to reconstruct KifStore from database: {}", e);
    })?;
    Ok(KnowledgeBase::new(store))
}

// ── Directory helpers ─────────────────────────────────────────────────────────

/// Collect all *.kif files in a directory, sorted for deterministic ordering.
pub fn kif_files_in_dir(dir: &Path) -> Result<Vec<PathBuf>, (Span, ParseError)> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        let span = Span { file: format!("{}", dir.display()), line: 0, col: 0, offset: 0 };
        (span, ParseError::Other { msg: format!("cannot read directory {}: {}", dir.display(), e) })
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

/// Read stdin if it is piped (not a TTY); return None if stdin is a terminal
/// or if the input is empty.
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
