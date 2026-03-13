/// sumo-parser — command-line interface.
use std::process;
use std::io::Write;
use log;
use clap::Parser;
use inline_colorization::*;

use sumo_native::cli::{Cli, Cmd, run_validate, run_ask, run_translate, run_test};
use sumo_native::config::{resolve_config_path, parse_config_xml};

use sumo_parser_core::error::{promote_to_error, set_all_errors, supress_warnings};

fn main() {
    log::trace!("main()");
    // Parse the CLI Options
    let mut cli = Cli::parse();

    let config_xml = if let Some(config_path) = resolve_config_path(cli.config.as_deref()) {
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

    supress_warnings(cli.quiet);

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
            Cmd::Validate { kb, .. } => Some(kb),
            Cmd::Ask { kb, .. } => Some(kb),
            Cmd::Translate { kb, .. } => Some(kb),
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
        Cmd::Validate { formula, kb } => run_validate(formula, kb),
        Cmd::Ask {
            formula,
            tell,
            timeout,
            session,
            kb,
            keep
        } => run_ask(formula, tell, timeout, session, kb, keep),
        Cmd::Translate {
            formula,
            lang,
            show_numbers,
            session,
            kb,
        } => run_translate(
            formula,
            &lang,
            show_numbers,
            session.as_deref(),
            kb,
        ),
        Cmd::Test { path, kb, keep } => run_test(path, kb, keep),
    };
    process::exit(if ok { 0 } else { 1 });
}
