/// sumo-parser -- command-line interface.
use std::process;
use std::io::Write;
use clap::Parser;
use inline_colorization::*;

use sigmakee::cli::{Cli, Cmd, KbArgs, run_load, run_validate, run_translate, run_man, run_update};
#[cfg(feature = "ask")]
use sigmakee::cli::{run_ask, run_test, run_debug};
#[cfg(feature = "server")]
use sigmakee::cli::run_serve;
use sigmakee::config::{resolve_config_path, parse_config_xml};
use sigmakee::git::fetch_repo_sparse;

use sumo_kb::error::{promote_to_error, set_all_errors, suppress_warnings};

fn main() {
    log::trace!("main()");
    // Parse the CLI Options
    let cli = Cli::parse();

    // Distinguish "user explicitly asked for this config" from "auto-discover".
    // When the user passes `--config PATH`, a failure to load is a hard error:
    // silently falling back to an empty config surprised callers by producing
    // single-line TPTP dumps (empty KB → only the conjecture emitted) with no
    // obvious cause.  When the config is just auto-discovered, a missing or
    // unparseable file is still recoverable — continue with defaults.
    let user_specified_config = cli.config.is_some();
    let config_xml = if !cli.enable_config {
        None
    } else if let Some(config_path) = resolve_config_path(cli.config.as_deref()) {
        match parse_config_xml(&config_path) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                eprintln!("Could not parse config.xml at {}: {}", config_path.display(), e);
                if user_specified_config {
                    process::exit(2);
                }
                None
            }
        }
    } else if user_specified_config {
        eprintln!(
            "Could not locate config.xml from `--config {}` (after ~ expansion)",
            cli.config.as_deref().map(|p| p.display().to_string()).unwrap_or_default()
        );
        process::exit(2);
    } else {
        None
    };

    // Suppress the semantic warnings based on whether the quiet option
    // was passed
    suppress_warnings(cli.quiet);

    let level = if cli.verbose > 0 {
        match cli.verbose {
            1 => log::LevelFilter::Info,
            2 => log::LevelFilter::Debug,
            _ => log::LevelFilter::Trace,
        }
    } else if let Some(ref cfg) = config_xml {
        if let Some(lvl) = cfg.log_level() {
            match lvl.to_lowercase().as_str() {
                "info" => log::LevelFilter::Info,
                "debug" => log::LevelFilter::Debug,
                "trace" => log::LevelFilter::Trace,
                "error" => log::LevelFilter::Error,
                _ => log::LevelFilter::Warn,
            }
        } else {
            log::LevelFilter::Warn
        }
    } else {
        log::LevelFilter::Warn
    };

    env_logger::Builder::new()
        .filter_level(level)
        .format(|f, record| {
            let level_color = match record.level() {
                log::Level::Error => color_bright_red,
                log::Level::Warn  => color_bright_yellow,
                log::Level::Info  => color_cyan,
                log::Level::Debug => color_blue,
                log::Level::Trace => color_white,
            };
            if record.target() == "clean" {
                writeln!(f, "{}", record.args())
            } else {
                writeln!(f, "[{} {level_color}{}{color_reset}] {}", f.timestamp(), record.level(), record.args())
            }
        })
        .init();
    log::debug!("Debug logging enabled");

    // --git: sparse-checkout only the files the user asked for into a
    // temp dir.  The TempDir is kept alive in `_git_tempdir` for the
    // entire process so the checked-out files remain accessible.
    //
    // Sparse paths are collected NOW — before the clone — from all three
    // selection sources: -f, -d, and -c constituents.  We already have
    // the parsed config_xml at this point so -c paths are available.
    let _git_tempdir: Option<tempfile::TempDir>;
    let git_root: Option<std::path::PathBuf> = if let Some(ref url) = cli.git {
        if !matches!(cli.command, Cmd::Load { .. }) {
            eprintln!(
                "warning: --git without `load` downloads on the fly and is not \
                 cached to the database"
            );
        }

        // Collect every repo-relative path the user needs.
        let mut sparse_paths: Vec<String> = Vec::new();
        sparse_paths.extend(cli.files.iter().map(|p| p.display().to_string()));
        sparse_paths.extend(cli.dirs.iter().map(|p| p.display().to_string()));
        if let Some(ref cfg) = config_xml {
            let kb_name = cli.kb.as_deref().or_else(|| cfg.default_kb_name());
            if let Some(name) = kb_name {
                if let Some(paths) = cfg.get_kb_constituents(name) {
                    sparse_paths.extend(paths.into_iter().map(String::from));
                }
            }
        }

        match fetch_repo_sparse(url, &sparse_paths) {
            Ok((tmp, root)) => {
                _git_tempdir = Some(tmp);
                Some(root)
            }
            Err(()) => process::exit(1),
        }
    } else {
        _git_tempdir = None;
        None
    };

    // Build the universal source-selection struct from the
    // top-level globals.  Every `run_*` handler still takes a
    // `KbArgs`; for subcommands that flatten `KbArgs` (ask / test /
    // debug / serve), we merge the flattened `vampire` field on top
    // of this base.  For subcommands that don't flatten, the
    // synthesised base IS the argument they receive.
    //
    // When --git is active, rebase -f / -d paths against the repo root
    // so the user can supply paths relative to the repository.
    let (base_files, base_dirs) = match git_root.as_ref() {
        Some(root) => (
            cli.files.iter().map(|f| root.join(f)).collect(),
            cli.dirs.iter().map(|d| root.join(d)).collect(),
        ),
        None => (cli.files.clone(), cli.dirs.clone()),
    };

    let mut base_kb_args = KbArgs {
        files:   base_files,
        dirs:    base_dirs,
        db:      cli.db.clone(),
        no_db:   cli.no_db,
        vampire: None,
    };

    // Config.xml integration: prepend config-declared files to the
    // user's `-f` list and fall back to the config-declared Vampire
    // path when none was given on the command line.  When --git is
    // active, constituent paths are resolved relative to the repo root
    // instead of the config's kbDir.
    if let Some(ref cfg) = config_xml {
        log::debug!("Found config_xml");
        let kb_name = cli.kb.as_deref().or_else(|| cfg.default_kb_name());
        if let Some(name) = kb_name {
            let files = match git_root.as_ref() {
                Some(root) => cfg.get_kb_files_relative_to(name, root),
                None       => cfg.get_kb_files(name),
            };
            if let Some(files) = files {
                // Config files come first so the order matches the
                // canonical "Merge.kif before Mid-level-ontology.kif"
                // convention (`get_kb_files` already returns them in
                // that order); user-supplied `-f` files append after.
                let mut all_files = files;
                all_files.extend(base_kb_args.files);
                base_kb_args.files = all_files;
            }
        }
        if base_kb_args.vampire.is_none() {
            base_kb_args.vampire = cfg.vampire_path();
        }
    }

    for arg in &cli.suppress {
        if arg == "all" {
            set_all_errors(true);
        } else {
            promote_to_error(arg);
        }
    }

    // Helper for the four subcommands that do flatten `KbArgs` (to
    // expose `--vampire`): take their flattened value's `vampire`
    // field and graft it onto the base.  Every other field of the
    // flattened struct is a placeholder (`#[arg(skip)]` default) and
    // is discarded.
    let merge_vampire = |base: &KbArgs,
                         flattened: KbArgs|
                         -> KbArgs {
        KbArgs {
            vampire: flattened.vampire.or_else(|| base.vampire.clone()),
            files:   base.files.clone(),
            dirs:    base.dirs.clone(),
            db:      base.db.clone(),
            no_db:   base.no_db,
        }
    };

    let ok = match cli.command {
        Cmd::Load { flush } => run_load(base_kb_args, flush),
        Cmd::Validate { formula, parse, no_kb_check } =>
            run_validate(formula, parse, no_kb_check, base_kb_args),
        Cmd::Translate {
            formula,
            lang,
            show_numbers,
            show_kif,
            session,
        } => run_translate(
            formula,
            &lang,
            show_numbers,
            show_kif,
            session.as_deref(),
            base_kb_args,
        ),
        #[cfg(feature = "ask")]
        Cmd::Ask {
            formula,
            tell,
            timeout,
            session,
            backend,
            lang,
            kb,
            keep,
            proof,
            profile,
        } => run_ask(
            formula, tell, timeout, session, backend, lang,
            merge_vampire(&base_kb_args, kb),
            keep, proof, profile,
        ),
        #[cfg(feature = "ask")]
        Cmd::Test { paths, kb, keep, backend, lang, timeout, profile } =>
            run_test(paths, merge_vampire(&base_kb_args, kb), keep, backend, lang, timeout, profile),
        #[cfg(feature = "ask")]
        Cmd::Debug { file, thoroughness, scope, timeout, keep, proof, kb } =>
            run_debug(
                file, thoroughness, scope, timeout, keep, proof,
                merge_vampire(&base_kb_args, kb),
            ),
        Cmd::Man { symbol, lang, no_pager } =>
            run_man(symbol, lang, no_pager, base_kb_args),
        #[cfg(feature = "server")]
        Cmd::Serve { kb } => run_serve(merge_vampire(&base_kb_args, kb)),
        Cmd::Update { check } => run_update(check),
    };
    process::exit(if ok { 0 } else { 1 });
}
