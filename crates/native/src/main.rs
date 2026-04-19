/// sumo-parser -- command-line interface.
use std::process;
use std::io::Write;
use log;
use clap::Parser;
use inline_colorization::*;

use sumo_native::cli::{Cli, Cmd, run_load, run_validate, run_translate, run_man};
#[cfg(feature = "ask")]
use sumo_native::cli::{run_ask, run_test};
use sumo_native::config::{resolve_config_path, parse_config_xml};

use sumo_kb::error::{promote_to_error, set_all_errors, suppress_warnings};

fn main() {
    log::trace!("main()");
    // Parse the CLI Options
    let mut cli = Cli::parse();

    let config_xml = if !cli.enable_config {
        None
    } else if let Some(config_path) = resolve_config_path(cli.config.as_deref()) {
        // println!("Found config file: {}", &config_path.to_str().unwrap());
        match parse_config_xml(&config_path) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                eprintln!("Could not parse config.xml at {}: {}", config_path.display(), e);
                None
            }
        }
    } else {
        None
    };

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

    if let Some(ref cfg) = config_xml {
        log::debug!("Found config_xml");
        let kb_name = cli.kb.as_deref().or_else(|| cfg.default_kb_name());
        
        let kb_args = match &mut cli.command {
            Cmd::Load { kb } => Some(kb),
            Cmd::Validate { kb, .. } => Some(kb),
            Cmd::Translate { kb, .. } => Some(kb),
            Cmd::Man { kb, .. } => Some(kb),
            #[cfg(feature = "ask")]
            Cmd::Ask { kb, .. } => Some(kb),
            #[cfg(feature = "ask")]
            Cmd::Test { kb, .. } => Some(kb),
        };

        if let Some(kb_args) = kb_args {
            if let Some(name) = kb_name {
                if let Some(files) = cfg.get_kb_files(name) {
                    // Prepend config files to manually specified files
                    let mut all_files = files;
                    all_files.extend(kb_args.files.clone());
                    kb_args.files = all_files;
                }
            }
            if kb_args.vampire.is_none() {
                kb_args.vampire = cfg.vampire_path();
            }
        }
    }

    for arg in &cli.suppress {
        if arg == "all" {
            set_all_errors(true);
        } else {
            promote_to_error(arg);
        }
    }

    let ok = match cli.command {
        Cmd::Load { kb } => run_load(kb),
        Cmd::Validate { formula, parse, no_kb_check, kb } => run_validate(formula, parse, no_kb_check, kb),
        Cmd::Translate {
            formula,
            lang,
            show_numbers,
            show_kif,
            session,
            kb,
        } => run_translate(
            formula,
            &lang,
            show_numbers,
            show_kif,
            session.as_deref(),
            kb,
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
        } => run_ask(formula, tell, timeout, session, backend, lang, kb, keep, proof, profile),
        #[cfg(feature = "ask")]
        Cmd::Test { paths, kb, keep, backend, lang, timeout, profile } => run_test(paths, kb, keep, backend, lang, timeout, profile),
        Cmd::Man { symbol, lang, kb } => run_man(symbol, lang, kb),
    };
    process::exit(if ok { 0 } else { 1 });
}
