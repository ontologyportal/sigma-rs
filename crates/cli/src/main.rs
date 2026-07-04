//! sumo-parser command-line interface.
use std::path::PathBuf;
use std::process;
use std::io::Write;
use sigmakee::style::*;

use sigmakee::cli::{Cli, Cmd};
use sigmakee::cli::{
    run_flush, run_load, run_load_warm, run_validate,
    run_translate, run_man, run_search, run_update, run_config
};
#[cfg(feature = "ask")]
use sigmakee::cli::{run_ask, run_test, run_audit};
#[cfg(feature = "server")]
use sigmakee::cli::run_serve;

use sigmakee_rs_sdk::{
    DynSink, KnowledgeBase, ProverLayer,
    ExternalProverLayer, ProvingLayer, TranslationLayer, TopLayer
};
use sigmakee_rs_sdk::prover::external::backends::{
    EproverRunner, 
    VampireRunner,
    IntegratedVampireRunner
};
use sigmakee_rs_sdk::{Prover, Session};
use sigmakee_rs_sdk::manager::{KBManager, ProverOptsFor};

fn main() {
    // Heavy ontologies blow past the 8 MB main-thread stack; run on a 64 MB worker.
    let handle = std::thread::Builder::new()
        .name("sumo-main".to_string())
        .stack_size(64 * 1024 * 1024)
        .spawn(main_worker)
        .expect("spawn sumo-main worker thread");
    match handle.join() {
        Ok(_)  => (),
        Err(_) => process::exit(101),
    }
}

fn main_worker() {
    let (cli, arg_matches) = sigmakee::cli::args_project::parse();
    sigmakee::style::set_ugly(cli.ugly);

    let profile = cli.profile;
    let t_profile = std::time::Instant::now();
    sigmakee::progress::init(profile);

    let mut manager = build_manager(&cli);

    // Precedence: flag > env > config.xml > default.
    if let Err(e) = manager.apply_overrides(sigmakee::cli::args_project::overrides(&arg_matches)) {
        log::error!("config error: {e}");
        process::exit(2);
    }

    init_logging(&cli, &manager);

    if let Some(name) = cli.kb.as_deref() {
        manager.set_current_kb(name);
    }

    apply_global_overrides(&cli, &mut manager);

    if let Err(e) = manager.add_cli_sources(cli.files.clone(), cli.dirs.clone(), cli.git.clone()) {
        log::error!("error: {e}");
        process::exit(2);
    }

    // Handle `config` before `validate` so a misconfigured path still shows in
    // the dump; it needs no KB or session.
    if matches!(cli.command, Cmd::Config { .. }) {
        let cfg = sigmakee::config::resolve_config_path(cli.config.as_deref());
        let loaded = cli.enable_config && cfg.is_some();
        process::exit(if run_config(&manager, cfg, loaded) { 0 } else { 1 });
    }

    // CASC batch mode needs no `sumokbname` / base KB at all — every problem
    // builds its own fresh, self-contained `Session` internally (see
    // `run_casc`), so it routes before `validate()`'s "a default KB is
    // required" check, exactly like `Config` above.  Without this, `sumo
    // casc` would be unusable without `-c` even though it never touches the
    // configured ontology.
    #[cfg(feature = "ask")]
    if matches!(cli.command, Cmd::Casc { .. }) {
        let Cmd::Casc { path, timeout, jobs } = cli.command else { unreachable!() };
        let ok = sigmakee::cli::run_casc(&manager, path, timeout, jobs);
        process::exit(if ok { 0 } else { 1 });
    }

    if let Err(e) = manager.validate() {
        log::error!("config error: {e}");
        process::exit(2);
    }

    // Use the LMDB store at `<editDir>/<kb>.lmdb` when it exists and `--no-db`
    // wasn't passed; otherwise build fresh in memory.
    let sink: Option<DynSink> = sigmakee::progress::global_sink();
    let db = manager.db_path();
    let use_db = !cli.no_db && db.as_ref().is_some_and(|p| p.exists());
    // Flush before rebuild so the store can be recreated.
    if matches!(cli.command, Cmd::Load { flush } if flush == true && db.is_some()) {
        run_flush(&manager);
    }

    let session_name = cli.session;
    // Sweep drives the core crate directly; route it to a fresh native session
    // before the generic backend dispatch.
    #[cfg(feature = "sweep")]
    if matches!(cli.command, Cmd::Sweep { .. }) {
        let Cmd::Sweep { paths, configs, random, seed, budget, steps, timeout, jobs, out, lanes, portfolio_out, kb: _ } = cli.command else { unreachable!() };
        let session = Session::from_kb(KnowledgeBase::new_native(), session_name);
        let ok = sigmakee::cli::run_sweep(session, &manager, paths, configs, random, seed, budget, steps, timeout, jobs, out, lanes, portfolio_out);
        process::exit(if ok { 0 } else { 1 });
    }

    let ok = if matches!(cli.command, Cmd::Translate { .. } | Cmd::Man { .. }) {
        // Translation-only commands run on a `TranslationLayer`, independent of
        // `--backend`: the native `ProverLayer` lacks `HasTranslation`.
        let kb = open_or_new(
            use_db,
            || KnowledgeBase::<TranslationLayer>::open(db.as_deref().unwrap(), sink.clone()),
            KnowledgeBase::new,
        );
        if manager.real_numbers == Some(true) {
            kb.set_reals_only(true);
        }
        dispatch_translation(Session::from_kb(kb, session_name), manager, cli.command, sink, profile)
    } else {
        match manager.default_backend.as_str() {
            "native" => {
                let kb = open_or_new(
                    use_db,
                    || KnowledgeBase::<ProverLayer>::open(db.as_deref().unwrap(), sink.clone()),
                    KnowledgeBase::new_native,
                );
                dispatch(Session::from_kb(kb, session_name), manager, cli.command, &arg_matches, sink, profile)
            }
            // e / eprover / subprocess / embedded → external layer.
            _ => {
                // `--keep` (TPTP dump) is threaded from the prover subcommands
                // into the external runner.
                let keep = match &cli.command {
                    #[cfg(feature = "ask")]
                    Cmd::Ask { keep, .. } | Cmd::Test { keep, .. } | Cmd::Audit { keep, .. } =>
                        keep.clone(),
                    _ => None,
                };
                let runner = build_runner(&manager, keep);
                let kb = open_or_new(
                    use_db,
                    || KnowledgeBase::<ExternalProverLayer>::open(db.as_deref().unwrap(), sink.clone()),
                    || KnowledgeBase::new_external(runner.clone()),
                );
                let mut session = Session::from_kb(kb, session_name);
                // `open()` installs the default runner; override it with the
                // configured E/Vampire runner.
                session.set_runner(runner);
                // Reals-only TFF numerics: explicit `--real-numbers` wins;
                // unset defaults ON for the E backend under TFF (E 3.2.5
                // mistypes `$to_real` in equality position). Must be set before
                // dispatch so the lazy TFF caches fill in the chosen mode.
                let reals_only = manager.real_numbers.unwrap_or_else(|| {
                    matches!(manager.default_backend.as_str(), "e" | "eprover")
                        && manager.tptp_lang.eq_ignore_ascii_case("tff")
                });
                if reals_only {
                    session.kb().set_reals_only(true);
                }
                if matches!(cli.command, Cmd::Load { .. }) {
                    run_load_warm(session, manager)
                } else {
                    dispatch(session, manager, cli.command, &arg_matches, sink, profile)
                }
            }
        }
    };

    // Tear down the spinner, then emit the global `--profile` report.
    if let Some(sink) = sigmakee::progress::global() {
        sink.finish(); // clear the spinner so the report starts on a clean row
        if profile {
            match sink.report() {
                Some(rep) if sink.has_data() => {
                    eprintln!("\n\x1b[1mProfile (per phase, by total time):\x1b[0m");
                    eprint!("{}", rep);
                    eprintln!("  {:<48}  {:>10.3} ms", "total wall", t_profile.elapsed().as_secs_f64() * 1000.0);
                }
                _ => eprintln!("\n\x1b[1mProfile:\x1b[0m total wall {:.3} ms (no instrumented phases on this path)",
                        t_profile.elapsed().as_secs_f64() * 1000.0),
            }
        }
    }
    process::exit(if ok { 0 } else { 1 });
}

/// Open the persisted KB (exiting with a logged error on failure) or build a
/// fresh in-memory one when `use_db` is false.
fn open_or_new<L, E: std::fmt::Display>(
    use_db: bool,
    open:   impl FnOnce() -> Result<KnowledgeBase<L>, E>,
    fresh:  impl FnOnce() -> KnowledgeBase<L>,
) -> KnowledgeBase<L> {
    if !use_db {
        return fresh();
    }
    match open() {
        Ok(kb) => kb,
        Err(d) => { log::error!("failed to open DB: {d}"); process::exit(1); }
    }
}

/// Route a parsed command against a ready proving `Session`. Ingests the
/// selected KB's constituents, then dispatches `cmd` to its handler. Returns
/// whether the command succeeded.
fn dispatch<L: ProvingLayer>(
    mut session: Session<L>,
    mut manager: KBManager,
    cmd: Cmd,
    arg_matches: &clap::ArgMatches,
    sink: Option<DynSink>,
    _profile: bool,
) -> bool
where
    L::Opts: ProverOptsFor,
{
    if let Some(s) = sink {
        session.set_progress_sink(s);
    }
    ingest_constituents(&mut session, &manager);

    match cmd {
        Cmd::Load { flush: _ } =>
            run_load(session, manager),

        Cmd::Validate { formula, parse } =>
            run_validate(session, manager, formula, parse),

        #[cfg(feature = "ask")]
        Cmd::Ask { formula, tell, kb: _, keep } =>
            run_ask(session, &manager, formula, tell, keep),

        #[cfg(feature = "ask")]
        Cmd::Test { paths, kb: _, keep, step, full_kb } => {
            if full_kb { manager.disable_selection = true; }
            manager.native_prover.step = step;
            // An explicit `--timeout` overrides every case; its absence means
            // "use each case's own `(time N)`". Clear the global budget so
            // `Session::test` stamps `tc.timeout` per case.
            if !supplied(arg_matches, "timeout") {
                manager.native_prover.time_limit_secs = 0;
                manager.external_prover.timeout_secs  = 0;
            }
            run_test(session, manager, paths, keep)
        }

        #[cfg(feature = "ask")]
        Cmd::Audit { file, thoroughness, limit, keep, kb: _ } => {
            manager.thoroughness = thoroughness;
            manager.limit = limit;
            // `--scope` pins a fixed tolerance (no auto-budget) for the audit.
            if supplied(arg_matches, "scope") {
                manager.native_prover.selection.auto_budget = None;
            }
            if !supplied(arg_matches, "timeout") {
                manager.native_prover.time_limit_secs = 60;
            }
            manager.native_prover.want_proof = true;
            manager.native_prover.max_steps  = 500_000;
            manager.native_prover.max_lits   = 12;
            manager.native_prover.forward_close = true;
            run_audit(session, &manager, file, keep)
        }

        Cmd::Search { query, kind, lang, limit } =>
            run_search(session, manager, query, kind, lang, limit),

        #[cfg(feature = "server")]
        Cmd::Serve { kb: _ } => run_serve(session, &manager),

        Cmd::Update { check } => run_update(check),

        _ => false
    }
}

/// Dispatch the translation-only commands (`translate`, `man`) on a
/// `TranslationLayer` session. These don't use a prover, so they run
/// independent of `--backend`.
fn dispatch_translation(
    mut session: Session<TranslationLayer>,
    mut manager: KBManager,
    cmd:         Cmd,
    sink:        Option<DynSink>,
    _profile:    bool,
) -> bool {
    if let Some(s) = sink {
        session.set_progress_sink(s);
    }
    ingest_constituents(&mut session, &manager);

    match cmd {
        Cmd::Translate { formula, show_numbers, show_kif, test, full_kb, keep: _ } => {
            // Translation never runs the prover-feedback autoscaling ladder.
            manager.show_kif = show_kif;
            if full_kb { manager.disable_selection = true; }
            manager.native_prover.selection.autoscale = false;
            run_translate(session, manager, formula, show_numbers, test)
        }
        Cmd::Man { symbol, lang, no_pager } =>
            run_man(session, manager, symbol, lang, no_pager),
        _ => unreachable!("dispatch_translation only handles Translate/Man"),
    }
}

/// Ingest the manager's selected constituents into `session` (core dedups
/// unchanged files).
fn ingest_constituents<L: TopLayer>(session: &mut Session<L>, manager: &KBManager) {
    for src in manager.current_sources_owned() {
        for e in session.ingest(src, false) {
            log::error!("ingest: {e}");
        }
    }
}

// -- helpers -----------------------------------------------------------------

/// Build the KBManager from `-c`/`--config`, or defaults.
fn build_manager(cli: &Cli) -> KBManager {
    if !cli.enable_config {
        return KBManager::default();
    }
    match sigmakee::config::resolve_config_path(cli.config.as_deref()) {
        Some(path) => match KBManager::from_config_xml_path(path) {
            Ok(m) => m,
            Err(e) => { eprintln!("Error parsing config.xml: {e}"); process::exit(2); }
        },
        None => {
            eprintln!("Could not locate config.xml from `--config {}`",
                cli.config.as_deref().map(|p| p.display().to_string()).unwrap_or_default());
            process::exit(2);
        }
    }
}

/// `-v`/`-q` override the configured level; install the env_logger format.
fn init_logging(cli: &Cli, manager: &KBManager) {
    let level = if cli.verbose > 0 {
        match cli.verbose { 1 => log::LevelFilter::Info, 2 => log::LevelFilter::Debug, _ => log::LevelFilter::Trace }
    } else if cli.quiet {
        log::LevelFilter::Error
    } else {
        manager.log_level
    };
    env_logger::Builder::new()
        .filter_level(level)
        .format(|f, record| {
            let lc = match record.level() {
                log::Level::Error => color_bright_red,  log::Level::Warn => color_bright_yellow,
                log::Level::Info  => color_cyan,        log::Level::Debug => color_blue,
                log::Level::Trace => color_white,
            };
            if record.target() == "clean" {
                writeln!(f, "{}", record.args())
            } else {
                writeln!(f, "[{} {lc}{}{color_reset}] {}", f.timestamp(), record.level(), record.args())
            }
        })
        .init();
}

/// `-W` warning-elevation policy → core promotion flags.
fn apply_global_overrides(cli: &Cli, _manager: &mut KBManager) {
    use sigmakee_rs_sdk::{promote_to_error, set_all_errors};
    for arg in &cli.suppress {
        if arg == "all" { set_all_errors(true); } else { promote_to_error(arg); }
    }
}

/// Whether the user (or its env var) supplied the projected option `field` on
/// the active subcommand — for arms that special-case a flag's *presence* (e.g.
/// `test`/`audit` reading `--timeout` / `--scope`).
#[cfg(feature = "ask")]
fn supplied(matches: &clap::ArgMatches, field: &str) -> bool {
    use clap::parser::ValueSource;
    matches.subcommand().is_some_and(|(_, sm)| {
        matches!(sm.value_source(field), Some(ValueSource::CommandLine) | Some(ValueSource::EnvVariable))
    })
}

/// Construct the external runner the resolved backend names.
fn build_runner(manager: &KBManager, keep: Option<PathBuf>) -> Prover {
    match manager.default_backend.as_str() {
        "e" | "eprover" => {
            // Resolve against systemsDir / $PATH; a bogus configured path falls
            // back to the bare name so a PATH-installed binary still works.
            let path = manager.resolve_eprover().unwrap_or_else(|e| {
                log::warn!("{e}; falling back to 'eprover' on PATH");
                PathBuf::from("eprover")
            });
            Prover::Eprover(EproverRunner { eprover_path: path, tptp_dump_path: keep })
        }
        "embedded"      => Prover::VampireIntegrated(IntegratedVampireRunner),
        _ /* subprocess */ => {
            let path = manager.resolve_vampire().unwrap_or_else(|e| {
                log::warn!("{e}; falling back to 'vampire' on PATH");
                PathBuf::from("vampire")
            });
            Prover::VampireSubprocess(VampireRunner { vampire_path: path, tptp_dump_path: keep })
        }
    }
}

