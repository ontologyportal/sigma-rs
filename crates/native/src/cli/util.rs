use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::collections::HashSet;
use sumo_parser_core::{
    load_kif, KifStore, ParseError, SemanticError, Span, TptpLang,
};
use crate::cache::{load_cache, save_cache};
use crate::cli::args::KbArgs;
use crate::parse_error;

/// Build a lookup set from a list of codes / names supplied via -W / --warning.
pub fn build_suppress_set(entries: &[String]) -> HashSet<String> {
    entries.iter().cloned().collect()
}

/// Returns true when `err` should be silently ignored.
pub fn is_suppressed(err: &SemanticError, suppress: &HashSet<String>) -> bool {
    suppress.contains(err.code()) || suppress.contains(err.name())
}

/// KB construction helper
pub fn build_store(args: &KbArgs) -> Result<KifStore, ()> {
    log::trace!("build_store({:?})", args);
    // --restore takes precedence over --file / --dir.
    if let Some(ref cache_path) = args.restore {
        log::debug!("Restoring state from cache");
        match load_cache(cache_path) {
            Ok(c) => {
                log::debug!("Successfully restored from cache");
                return Ok(c)
            },
            Err((span, e)) => {
                parse_error!(span, e);
                return Err(())
            }
        }
    }

    // Collect the files
    let mut all_files: Vec<PathBuf> = args.files.clone();
    for dir in &args.dirs {
        let extra_files = kif_files_in_dir(dir);
        match extra_files {
            Ok(f) => all_files.extend(f),
            Err((span, e)) => {
                parse_error!(span, e);
                return Err(())
            }
        }
    }
    log::debug!("Found {} constituents", all_files.len());

    let mut store = KifStore::default();
    for path in &all_files {
        // Open the file
        let text = std::fs::read_to_string(path)
            .map_err(|e| {
                let fake_span = Span { file: format!("{}", path.display()), col: 0, line: 0, offset: 0 };
                let e = ParseError::Other { msg: format!("cannot read {}: {}", path.display(), e) };
                parse_error!(fake_span, e);
                ()    
        })?;
        let tag = path.display().to_string();
        let errors = load_kif(&mut store, &text, &tag);
        if !errors.is_empty() {
            for (span, e) in errors {
                parse_error!(span, e, text);
            }
            return Err(());
        }
    }
    Ok(store)
}

/// Get all KIF files in a directory
pub fn kif_files_in_dir(dir: &Path) -> Result<Vec<PathBuf>, (Span, ParseError)> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        let fake_span = Span {
            file: format!("{}", dir.display()),
            line: 0,
            col: 0,
            offset: 0
        };
        (
            fake_span.clone(),
            ParseError::Other {
                msg: format!("cannot read directory {}: {}", dir.display(), e),
            },
        )
    })?;
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("kif"))
        .collect();
    files.sort(); // deterministic ordering
    Ok(files)
}

/// Read stdin if it is piped (not a TTY); return None if stdin is a terminal
/// or if the input is empty.
pub fn read_stdin() -> Option<String> {
    if io::stdin().is_terminal() {
        return None;
    }
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf).ok();
    if buf.trim().is_empty() {
        None
    } else {
        Some(buf)
    }
}

/// File tag used for formulas supplied inline or via stdin.
pub fn source_tag() -> &'static str {
    if io::stdin().is_terminal() {
        "<inline>"
    } else {
        "<stdin>"
    }
}

pub fn parse_lang(s: &str) -> TptpLang {
    match s {
        "tff" => TptpLang::Tff,
        _ => TptpLang::Fof,
    }
}

pub fn maybe_save_cache(store: &KifStore, path: Option<&Path>) {
    if let Some(p) = path {
        log::debug!("Attempting to save cache");
        if let Err(e) = save_cache(store, p) {
            log::warn!("warning: could not save cache: {}", e);
        }
    }
}
