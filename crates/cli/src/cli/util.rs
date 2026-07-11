use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};

use sigmakee_rs_sdk::{
    Diagnostic, Span, TptpLang,
};

#[cfg(feature = "server")]
use crate::cli::args::KbArgs;
#[cfg(feature = "server")]
use crate::parse_error;

// -- Internal helpers ----------------------------------------------------------

// Only the JSON-RPC server (`serve`) ingests loose kif files this way.
#[cfg(feature = "server")]
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

#[cfg(feature = "server")]
pub(crate) fn read_kif_file(path: &Path) -> Result<String, ()> {
    std::fs::read_to_string(path).map_err(|e| {
        log::error!("cannot read '{}': {}", path.display(), e);
    })
}

// -- Directory helpers ---------------------------------------------------------

/// Collect all `*.kif` files in a directory, sorted for deterministic ordering.
pub fn kif_files_in_dir(dir: &Path) -> Result<Vec<PathBuf>, (Span, Diagnostic)> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        let span = Span::point(format!("{}", dir.display()), 0, 0, 0);
        (span.clone(), Diagnostic::new_error("kb", "io-error", format!("cannot read directory '{}': {}", dir.display(), e)))
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
