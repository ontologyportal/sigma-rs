/// sumo-parser — command-line interface.
///
/// Three subcommands:
///   validate  — check formula(s) for semantic correctness
///   ask       — run a conjecture through Vampire
///   translate — convert KIF to TPTP
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process;

use env_logger;
use inline_colorization::*;
use log;

use clap::{Parser, Subcommand};

use std::collections::HashSet;

use sumo_native::{ask as native_ask, load_cache, save_cache, AskOptions};
use sumo_parser_core::{
    kb_to_tptp, load_kif, sentence_to_tptp, KifStore, KnowledgeBase, ParseError, SemanticError,
    SentenceDisplay, Span, TptpLang, TptpOptions,
};

// Error reporting macros

macro_rules! parse_error {
    ($span:expr, $e:expr) => {
        log::error!(
            "{}{}{}, {}line {}{}\n{style_bold}{color_bright_red}{}{style_reset}",
            color_magenta,
            $span.file,
            color_reset,
            style_bold,
            $span.line,
            style_reset,
            $e
        );
    };

    ($span:expr, $e:expr, $txt:expr) => {
        let line_start = $txt[..$span.offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end = $txt[$span.offset..].find('\n').map(|i| i + $span.offset).unwrap_or($txt.len());
        let width: usize = $span.col as usize + 9;
        log::error!(
            "{}{}{}\n\n {:<6}| {}\n{color_bright_red}{style_bold}{:>width$} {}{color_reset}",
            color_magenta,
            $span.file,
            color_reset,
            $span.line,
            &$txt[line_start..line_end],
            "^",
            $e,
        );
    };
}

macro_rules! semantic_error {
    ($span:expr, $e:expr, $sid:expr, $kb:expr) => {
        let dis = SentenceDisplay::new($sid, &$kb.store);
        let width = ($span.col + 9) as usize;

        log::error!(
            "{}{}{}\n\n {:<6}| {}\n{color_bright_red}{style_bold}{:>width$} {}{color_reset}",
            color_magenta,
            $span.file,
            color_reset,
            $span.line,
            dis,
            "^",
            $e,
        );
    };
}


// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "sumo",
    about = "Parse, validate, translate, and query SUMO KIF knowledge bases",
    version
)]
struct Cli {
    /// Logging verbosity (-v = info, -vv = debug, -vvv = trace).
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Cmd,
}

/// Flags that appear on every subcommand.  Defined as a separate struct so
/// clap can flatten them into each variant without repetition.
#[derive(clap::Args, Clone, Debug)]
struct KbArgs {
    /// KIF file to load into the knowledge base (repeatable).
    #[arg(short = 'f', long = "file", value_name = "FILE")]
    files: Vec<PathBuf>,

    /// Directory whose *.kif files are loaded into the knowledge base (repeatable).
    #[arg(short = 'd', long = "dir", value_name = "DIR")]
    dirs: Vec<PathBuf>,

    /// Save the parsed knowledge base to a JSON cache file.
    #[arg(short = 'c', long = "cache", value_name = "FILE")]
    cache: Option<PathBuf>,

    /// Restore the knowledge base from a previously saved JSON cache
    /// (skips loading -f / -d files).
    #[arg(short = 'r', long = "restore", value_name = "FILE")]
    restore: Option<PathBuf>,

    /// Suppress a semantic error by code or name (repeatable).
    ///
    /// Accepts a short code (e.g. -W E005) or a full name
    /// (e.g. --warning=arity-mismatch).  Non-ignorable errors (E003, E013,
    /// E014) cannot be suppressed regardless of this flag.
    #[arg(short = 'W', long = "warning", value_name = "CODE_OR_NAME")]
    suppress: Vec<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Validate KIF formula(s) against the knowledge base.
    ///
    /// With FORMULA: validates that single formula in the KB context.
    /// Without FORMULA: validates every sentence loaded from -f / -d files.
    Validate {
        /// Formula to validate.  May also be supplied via stdin.
        /// Omit to validate all sentences loaded from files/directories.
        formula: Option<String>,

        #[command(flatten)]
        kb: KbArgs,
    },

    /// Run a KIF conjecture through Vampire against the knowledge base.
    ///
    /// The conjecture must be supplied as FORMULA or via stdin.
    Ask {
        /// KIF conjecture to prove.  May also be supplied via stdin.
        formula: Option<String>,

        /// Assert a KIF formula into the KB before asking (repeatable).
        #[arg(short = 't', long = "tell", value_name = "KIF")]
        tell: Vec<String>,

        /// Path to the Vampire executable (default: 'vampire' on PATH).
        #[arg(long, value_name = "PATH")]
        vampire: Option<PathBuf>,

        /// Vampire proof-search timeout in seconds.
        #[arg(long, value_name = "SECS", default_value_t = 30)]
        timeout: u32,

        /// Report semantic validation errors from --tell statements but
        /// proceed with the proof attempt instead of aborting.
        /// Tell statements that fail validation are skipped (not added to the KB).
        #[arg(short = 'i', long = "ignore-errors")]
        ignore_errors: bool,

        /// Session key for --tell assertions and TPTP hypothesis filtering.
        /// Assertions are stored under this key; only this session's assertions
        /// are passed to Vampire as hypotheses (default: "default").
        #[arg(long, value_name = "KEY", default_value = "default")]
        session: String,

        #[command(flatten)]
        kb: KbArgs,
    },

    /// Translate KIF formula(s) or a full KB to TPTP.
    ///
    /// With FORMULA: translates that formula using the KB for semantic context.
    /// Without FORMULA: outputs the entire KB as a self-contained TPTP file.
    Translate {
        /// Formula to translate.  May also be supplied via stdin.
        /// Omit to translate all sentences loaded from files/directories.
        formula: Option<String>,

        /// TPTP language variant to emit.
        #[arg(long, value_name = "LANG", default_value = "fof")]
        lang: String,

        /// Emit numeric literals as-is instead of encoding them as n__N tokens.
        #[arg(long)]
        show_numbers: bool,

        /// Run semantic validation, report any errors, but emit TPTP regardless.
        /// Without this flag, translate skips validation entirely.
        #[arg(short = 'i', long = "ignore-errors")]
        ignore_errors: bool,

        /// Session key controlling which assertions appear as TPTP hypotheses.
        /// Omit to include all sessions' assertions (default: all).
        #[arg(long, value_name = "KEY")]
        session: Option<String>,

        #[command(flatten)]
        kb: KbArgs,
    },
}

// Entry Point
fn main() {
    log::trace!("main()");
    // Parse the CLI Options
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => log::LevelFilter::Warn,
        1 => log::LevelFilter::Info,
        2 => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    };
    env_logger::Builder::new().filter_level(level).init();
    log::debug!("Debug logging enabled");

    let ok = match cli.command {
        Cmd::Validate { formula, kb } => run_validate(formula, kb),
        Cmd::Ask {
            formula,
            tell,
            vampire,
            timeout,
            ignore_errors,
            session,
            kb,
        } => run_ask(formula, tell, vampire, timeout, ignore_errors, session, kb),
        Cmd::Translate {
            formula,
            lang,
            show_numbers,
            ignore_errors,
            session,
            kb,
        } => run_translate(
            formula,
            &lang,
            show_numbers,
            ignore_errors,
            session.as_deref(),
            kb,
        ),
    };
    process::exit(if ok { 0 } else { 1 });
}

// ── Warning suppression ───────────────────────────────────────────────────────

/// Build a lookup set from a list of codes / names supplied via -W / --warning.
fn build_suppress_set(entries: &[String]) -> HashSet<String> {
    entries.iter().cloned().collect()
}

/// Returns true when `err` should be silently ignored.
///
/// An error is suppressed only when:
///  1. It is ignorable (not a hard type-system violation), AND
///  2. Its code or name appears in the suppress set.
fn is_suppressed(err: &SemanticError, suppress: &HashSet<String>) -> bool {
    if !err.is_ignorable() {
        return false;
    }
    suppress.contains(err.code()) || suppress.contains(err.name())
}

// Validation subcommand
fn run_validate(formula: Option<String>, kb_args: KbArgs) -> bool {
    log::trace!(
        "run_validate(formula={:?},\n\tkb_args={:#?})",
        formula,
        kb_args
    );
    log::debug!("Entering Validation command");
    let suppress = build_suppress_set(&kb_args.suppress);
    let store = match build_store(&kb_args) {
        Ok(s) => s,
        Err(..) => {
            return false;
        }
    };
    maybe_save_cache(&store, kb_args.cache.as_deref());

    let formula = formula.or_else(read_stdin);
    let mut kb = KnowledgeBase::new(store);

    match formula {
        Some(text) => validate_formula(&mut kb, &text, source_tag(), &suppress),
        None => validate_all_roots(&kb, &suppress),
    }
}

/// Validate a single formula string against the KB.
fn validate_formula(
    kb: &mut KnowledgeBase,
    text: &str,
    tag: &str,
    suppress: &HashSet<String>,
) -> bool {
    log::trace!(
        "validate_formula(kb={{KnowledgeBase}}), {:?}, {:?})",
        text,
        tag
    );
    log::debug!("Validating single formula: {}", text);
    // Warm semantic caches from existing KB sentences before checking new ones.
    kb.validate_kb_once();
    log::debug!("KB Validated");

    // Load the formula directly into the store (bypassing kb.load_kif which
    // would clear caches) so our chosen file tag appears in error spans.
    let parse_errors = load_kif(&mut kb.store, text, tag);
    let mut ok = true;
    for (span, e) in &parse_errors {
        parse_error!(span, e, text);
        ok = false;
    }
    if !ok {
        return false;
    }

    let sids: Vec<_> = kb.store.file_roots.get(tag).cloned().unwrap_or_default();

    if sids.is_empty() {
        log::error!("no sentences were parsed");
        return false;
    }

    for sid in sids {
        log::trace!("sid = {}", sid);
        let span = &kb.store.sentences[sid].span;
        if let Err(e) = kb.validate_sentence(sid) {
            if is_suppressed(&e, suppress) {
                continue;
            }
            semantic_error!(span, e, sid, kb);
            ok = false;
        }
    }
    ok
}

/// Validate every root sentence in the KB and report errors (files-only mode).
fn validate_all_roots(kb: &KnowledgeBase, suppress: &HashSet<String>) -> bool {
    log::trace!("validate_all_roots(kb={{KnowledgeBase}})");
    let failures: Vec<_> = kb
        .validate_all()
        .into_iter()
        .filter(|(_, e)| !is_suppressed(e, suppress))
        .collect();

    for (sid, e) in &failures {
        let sent = &kb.store.sentences[*sid];
        semantic_error!(sent.span, e, *sid, kb);
    }
    let total = kb.store.roots.len();
    let n_err = failures.len();
    if n_err == 0 {
        println!("{} formula(s) validated: all OK", total);
        true
    } else {
        log::warn!("{} formula(s) validated: {} error(s)", total, n_err);
        false
    }
}

// ── ask ───────────────────────────────────────────────────────────────────────

fn run_ask(
    formula: Option<String>,
    tell: Vec<String>,
    vampire: Option<PathBuf>,
    timeout: u32,
    ignore_errors: bool,
    session: String,
    kb_args: KbArgs,
) -> bool {
    log::trace!(
        "run_ask(\n\t
        formula={:?},\n\t
        tell={:?},\n\t
        vampire={:?},\n\t
        timeout={:?},\n\t
        ignore_errors={:?},\n\t
        kb_ags={:#?}\n\t
    )",
        formula,
        tell,
        vampire,
        timeout,
        ignore_errors,
        kb_args
    );
    let conjecture = match formula.or_else(read_stdin) {
        Some(f) => f,
        None => {
            log::error!(
                "error: ask requires a conjecture formula \
                       (supply as argument or via stdin)"
            );
            return false;
        }
    };

    let store = match build_store(&kb_args) {
        Ok(s) => s,
        Err(..) => {
            return false;
        }
    };
    log::info!(
        "Completed parsing knowledge base ({} axioms)",
        store.roots.len()
    );
    maybe_save_cache(&store, kb_args.cache.as_deref());

    let mut kb = KnowledgeBase::new(store);

    // Apply tell statements into the KB under the specified session.
    for kif in &tell {
        log::debug!("Telling KB (session={:?}): {}", session, kif);
        let r = kb.tell(&session, kif);
        if !r.ok {
            for e in &r.errors {
                log::error!("tell error: {}", e);
            }
            if !ignore_errors {
                return false;
            }
            // else: skip this tell statement but continue
        }
    }
    log::debug!("Completed telling axioms to the KB");

    let result = native_ask(
        &mut kb,
        &conjecture,
        AskOptions {
            vampire_path: vampire,
            timeout_secs: Some(timeout),
            keep_tmp_file: false,
            session: Some(session),
            ..AskOptions::default()
        },
    );

    if !result.errors.is_empty() {
        for e in &result.errors {
            log::error!("error: {}", e);
        }
        return false;
    }

    print!(
        "{style_bold}Theorem prover completed successfully: {style_reset}{}",
        result.output
    );
    result.proved
}

// ── translate ─────────────────────────────────────────────────────────────────

fn run_translate(
    formula: Option<String>,
    lang: &str,
    show_numbers: bool,
    ignore_errors: bool,
    session: Option<&str>,
    kb_args: KbArgs,
) -> bool {
    let suppress = build_suppress_set(&kb_args.suppress);
    let store = match build_store(&kb_args) {
        Ok(s) => s,
        Err(..) => {
            return false;
        }
    };
    maybe_save_cache(&store, kb_args.cache.as_deref());

    let tptp_lang = parse_lang(lang);
    let opts = TptpOptions {
        lang: tptp_lang,
        hide_numbers: !show_numbers,
        ..TptpOptions::default()
    };

    let formula = formula.or_else(read_stdin);
    let mut kb = KnowledgeBase::new(store);

    match formula {
        Some(text) => {
            // Load into the store (not through kb.load_kif which clears caches)
            // so the KB semantic context is available for mention-suffix detection.
            let tag = source_tag();
            let errors = load_kif(&mut kb.store, &text, tag);
            let mut ok = true;
            for (span, e) in &errors {
                parse_error!(span, e);
                ok = false;
            }
            if !ok {
                return false;
            }

            let sids: Vec<_> = kb.store.file_roots.get(tag).cloned().unwrap_or_default();

            if sids.is_empty() {
                log::error!("error: no sentences parsed from input");
                return false;
            }

            if ignore_errors {
                // Run validation and report errors, but proceed with translation.
                kb.validate_kb_once();
                for &sid in &sids {
                    let span = &kb.store.sentences[sid].span;
                    if let Err(e) = kb.validate_sentence(sid) {
                        if !is_suppressed(&e, &suppress) {
                            semantic_error!(span, e, sid, kb);
                        }
                    }
                }
            }

            for sid in sids {
                println!("{}", sentence_to_tptp(sid, &kb, &opts));
            }
            true
        }
        None => {
            // Files-only mode: emit the full KB as TPTP.
            if ignore_errors {
                for (sid, err) in &kb.validate_all() {
                    if is_suppressed(err, &suppress) {
                        continue;
                    }
                    let span = &kb.store.sentences[*sid].span;
                    semantic_error!(span, err, *sid, kb);
                }
            }
            print!("{}", kb_to_tptp(&kb, "kb", &opts, session));
            true
        }
    }
}

/// KB construction helper
fn build_store(args: &KbArgs) -> Result<KifStore, ()> {
    // --restore takes precedence over --file / --dir.
    if let Some(ref cache_path) = args.restore {
        match load_cache(cache_path) {
            Ok(c) => return Ok(c),
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
fn kif_files_in_dir(dir: &Path) -> Result<Vec<PathBuf>, (Span, ParseError)> {
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

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Read stdin if it is piped (not a TTY); return None if stdin is a terminal
/// or if the input is empty.
fn read_stdin() -> Option<String> {
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
fn source_tag() -> &'static str {
    if io::stdin().is_terminal() {
        "<inline>"
    } else {
        "<stdin>"
    }
}

fn parse_lang(s: &str) -> TptpLang {
    match s {
        "tff" => TptpLang::Tff,
        _ => TptpLang::Fof,
    }
}

fn maybe_save_cache(store: &KifStore, path: Option<&Path>) {
    if let Some(p) = path {
        if let Err(e) = save_cache(store, p) {
            log::warn!("warning: could not save cache: {}", e);
        }
    }
}
