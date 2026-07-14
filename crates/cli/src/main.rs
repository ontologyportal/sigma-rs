//! sumo-parser command-line interface.
use std::path::PathBuf;
use std::process;
use std::io::{IsTerminal, Write};
use sigmakee::style::*;

use sigmakee::cli::{Cli, Cmd};
use sigmakee::cli::{
    run_flush, run_load, run_load_warm, run_validate,
    run_translate, run_man, run_search, run_update, run_config, run_config_write,
    run_config_tui, run_check, ConstituentEdit,
    maybe_notify_update,
};
use sigmakee::cli::args_project;
#[cfg(feature = "ask")]
use sigmakee::cli::{run_ask, run_ask_tui, run_test, run_audit};
// #[cfg(feature = "server")]
// use sigmakee::cli::run_serve;

use sigmakee_rs_sdk::{
    DynSink, KnowledgeBase, ProverLayer,
    ExternalProverLayer, ProvingLayer, TranslationLayer, TopLayer
};
use sigmakee_rs_sdk::prover::external::backends::{
    EproverRunner,
    VampireRunner,
};
#[cfg(feature = "integrated-prover")]
use sigmakee_rs_sdk::prover::external::backends::IntegratedVampireRunner;
use sigmakee_rs_sdk::{Prover, Session};
use sigmakee_rs_sdk::manager::{KBManager, ProverOptsFor};

/// `alloc-mi`: mimalloc as the process-global allocator.  Post-de-alloc
/// profiles still attribute 15-30% of equational-grind CPU to
/// malloc/free/bzero; this is the A/B knob for measuring whether a
/// faster allocator recovers it.  Declared in the BIN crate root so the
/// one-per-artifact `#[global_allocator]` covers the whole `sumo`
/// binary without touching library crates or their test harnesses.
#[cfg(feature = "alloc-mi")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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

    // Best-effort "a newer version exists" notice — cached, never blocks on
    // the network (see `maybe_notify_update`). Skipped under `-q` and for
    // `sumo update` itself, which already reports this explicitly.
    if !cli.quiet && !matches!(cli.command, Cmd::Update { .. }) {
        maybe_notify_update();
    }

    let profile = cli.profile;
    let t_profile = std::time::Instant::now();
    sigmakee::progress::init(profile);

    let is_load = matches!(cli.command, Cmd::Load { .. });
    let mut ingest_stats = IngestStats::default();

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

    // Without `-c`, config.xml's *preferences* still apply (sumokbname,
    // editDir, prover settings, …), but its `<kb>` constituent list doesn't
    // auto-load — only explicitly-supplied `-f`/`-d`/`--git` sources
    // (added next) get ingested.
    if !cli.load_kb {
        manager.clear_kb_constituents();
    }

    // `sumo config --kb NAME -f/-d ...` repurposes `-f`/`-d` to mean
    // "constituent to persist" (see the `Cmd::Config` dispatch below), not
    // "transient source to ingest this run" — skip the generic merge here so
    // an existing KB's `-f` doesn't get existence-checked/ingested twice
    // under a completely different (and stricter, non-`--declare`-aware)
    // code path before `Cmd::Config` ever runs.
    if !matches!(cli.command, Cmd::Config { .. }) {
        if let Err(e) = manager.add_cli_sources(cli.files.clone(), cli.dirs.clone(), cli.git.clone()) {
            log::error!("error: {e}");
            process::exit(2);
        }
    }

    // Handle `config` before `validate` so a misconfigured path still shows in
    // the dump; it needs no KB or session.
    //
    // Three modes: any `--<setting> value` flag supplied → patch just those
    // settings and persist (works anywhere, scripts included); no flags and
    // stdout is a real terminal → the interactive editor; no flags and not a
    // terminal (piped/redirected/CI) → today's read-only dump, unchanged, so
    // existing non-interactive uses of `sumo config` keep working.
    if matches!(cli.command, Cmd::Config { .. }) {
        let overrides = args_project::config_overrides(&arg_matches);
        // `--kb NAME` together with `-f`/`-d`/`--exclude` edits that one
        // KB's constituent list instead of (or alongside) the scalar
        // `--<setting>` overrides above.
        let constituent_edit = match &cli.kb {
            Some(name) if !cli.files.is_empty() || !cli.dirs.is_empty() || !cli.exclude.is_empty() =>
                Some(ConstituentEdit {
                    kb:        name.clone(),
                    add_files: cli.files.clone(),
                    add_dirs:  cli.dirs.clone(),
                    remove:    cli.exclude.clone(),
                    declare:   cli.declare,
                }),
            _ => None,
        };
        let ok = if !overrides.is_empty() || constituent_edit.is_some() {
            let target = sigmakee::config::resolve_config_path(cli.config.as_deref())
                .or_else(sigmakee::config::default_config_write_path)
                .unwrap_or_else(|| { log::error!("config: could not resolve $HOME to locate config.xml"); process::exit(2); });
            run_config_write(&target, overrides, constituent_edit)
        } else if std::io::stdout().is_terminal() && !sigmakee::style::is_ugly() {
            let target = sigmakee::config::resolve_config_path(cli.config.as_deref())
                .or_else(sigmakee::config::default_config_write_path)
                .unwrap_or_else(|| { log::error!("config: could not resolve $HOME to locate config.xml"); process::exit(2); });
            run_config_tui(&target)
        } else {
            let cfg = sigmakee::config::resolve_config_path(cli.config.as_deref());
            let loaded = !cli.no_config && cfg.is_some();
            // Reload leniently rather than reusing the outer `manager`: that one
            // came from `build_manager`'s *strict* `from_config_xml_path`, which
            // requires a valid `sumokbname` and falls back to
            // `KBManager::default()` on any validation failure (see
            // `build_manager`'s `Cmd::Config` special case just below it) --
            // exactly the state a config.xml being tuned via `sumo config
            // --<setting>` before a KB is configured is in. Reusing that
            // manager here would silently dump built-in defaults while still
            // reporting the config.xml path as loaded.
            let dump_manager = if loaded {
                match KBManager::from_config_xml_path_lenient(cfg.as_deref().unwrap()) {
                    Ok(m) => m,
                    Err(e) => { log::error!("config: cannot parse {}: {e}", cfg.as_deref().unwrap().display()); process::exit(2); }
                }
            } else {
                KBManager::default()
            };
            run_config(&dump_manager, cfg, loaded)
        };
        process::exit(if ok { 0 } else { 1 });
    }

    // CASC batch mode needs no `sumokbname` / base KB at all — every problem
    // builds its own fresh, self-contained `Session` internally (see
    // `run_casc`), so it routes before `validate()`'s "a default KB is
    // required" check, exactly like `Config` above.  Without this, `sumo
    // casc` would be unusable without a `sumokbname` configured, even though
    // it never touches the configured ontology.
    #[cfg(feature = "ask")]
    if matches!(cli.command, Cmd::Casc { .. }) {
        let Cmd::Casc { path, timeout, jobs } = cli.command else { unreachable!() };
        let ok = sigmakee::cli::run_casc(&manager, path, timeout, jobs);
        process::exit(if ok { 0 } else { 1 });
    }

    // `sumo test` over exclusively self-contained TPTP inputs (`.p` /
    // `.tptp` / `.ax`) likewise needs no configured base KB — each problem
    // runs on its own fresh Session (same machinery as `casc`).  Skipping
    // `validate()` here is what lets TPTP benchmarking run without a
    // configured `sumokbname`, i.e. without ingesting the whole configured
    // ontology underneath every problem.  A mixed invocation (any `.kif.tq`
    // / directory path) still requires the base KB and validates as before.
    let tptp_only_test = matches!(&cli.command, Cmd::Test { paths, .. }
        if !paths.is_empty() && paths.iter().all(|p| {
            // Extension check on the whole argument — works unchanged for a
            // git/http reference too (e.g. `repo.git#Axioms/T.ax` or
            // `https://…/PUZ001+1.p` both end with the right suffix).
            p.ends_with(".p") || p.ends_with(".tptp") || p.ends_with(".ax")
        }));
    if !tptp_only_test {
        if let Err(e) = manager.validate(cli.git.as_deref()) {
            log::error!("config error: {e}");
            process::exit(2);
        }
    }

    // Use the LMDB store at `<editDir>/<kb>.lmdb` when it exists and `--no-db`
    // wasn't passed; otherwise build fresh in memory. `load` is the exception:
    // its whole job is to create/refresh that store, so it always opens (and
    // thereby creates, via `LmdbEnv::open`'s create-if-missing) the path even
    // on a first run where nothing exists there yet. Without this, a
    // brand-new `load` would silently build its KB in memory, `persist()`
    // would no-op (no attached `db` env to snapshot into), and the reported
    // "load succeeded" would be a lie — no store ever hits disk.
    let sink: Option<DynSink> = sigmakee::progress::global_sink();
    let db = manager.db_path();
    let use_db = !cli.no_db && db.is_some() && (is_load || db.as_ref().is_some_and(|p| p.exists()));
    // Flush before rebuild so the store can be recreated.
    if matches!(cli.command, Cmd::Load { flush } if flush == true && db.is_some()) {
        run_flush(&manager);
    }

    // `sumo check` is read-only and layer-agnostic (it only needs
    // `KnowledgeBase::file_origin`, available on every `TopLayer`), so it
    // opens the persisted store directly instead of going through the
    // ingest/dispatch machinery below — no constituents get (re-)loaded.
    if matches!(cli.command, Cmd::Check { .. }) {
        let kb = if use_db {
            Some(KnowledgeBase::<TranslationLayer>::open(db.as_deref().unwrap(), sink.clone())
                .unwrap_or_else(|d| { log::error!("failed to open DB: {d}"); process::exit(1); }))
        } else {
            None
        };
        process::exit(if run_check(kb) { 0 } else { 1 });
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
        dispatch_translation(Session::from_kb(kb, session_name), manager, cli.command, sink, profile, cli.git.as_deref(), cli.branch.as_deref(), &mut ingest_stats)
    } else {
        match manager.default_backend.as_str() {
            "native" => {
                let kb = open_or_new(
                    use_db,
                    || KnowledgeBase::<ProverLayer>::open(db.as_deref().unwrap(), sink.clone()),
                    KnowledgeBase::new_native,
                );
                dispatch(Session::from_kb(kb, session_name), manager, cli.command, &arg_matches, sink, profile, cli.git.as_deref(), cli.branch.as_deref(), &mut ingest_stats)
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
                    ingest_constituents(&mut session, &manager, cli.git.as_deref(), cli.branch.as_deref(), &mut ingest_stats);
                    run_load_warm(session, manager)
                } else {
                    dispatch(session, manager, cli.command, &arg_matches, sink, profile, cli.git.as_deref(), cli.branch.as_deref(), &mut ingest_stats)
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

    if is_load {
        report_load(ok, db.as_deref(), &ingest_stats);
    }

    process::exit(if ok { 0 } else { 1 });
}

/// Running totals of diagnostics seen across every source ingested during
/// this invocation, kept so the end-of-run `load` report can summarize them
/// without re-walking `manager.current_sources_owned()`.
#[derive(Default)]
struct IngestStats {
    sources:  usize,
    errors:   usize,
    warnings: usize,
    infos:    usize,
    hints:    usize,
}

/// Print the final success/failure line for `sumo load`. Always names the DB
/// path and the ingest diagnostic counts so a failure (or a load that limped
/// through with warnings) is legible without re-running with more logging.
fn report_load(ok: bool, db: Option<&std::path::Path>, stats: &IngestStats) {
    let where_ = db.map(|p| p.display().to_string()).unwrap_or_else(|| "<in-memory, no --db>".to_string());
    let counts = format!(
        "{} source{} ingested, {} error{}, {} warning{}",
        stats.sources,  if stats.sources  == 1 { "" } else { "s" },
        stats.errors,   if stats.errors   == 1 { "" } else { "s" },
        stats.warnings, if stats.warnings == 1 { "" } else { "s" },
    );
    if ok {
        eprintln!(
            "{color_bright_green}{style_bold}load succeeded{style_reset}{color_reset} — DB: {where_} ({counts})"
        );
    } else {
        eprintln!(
            "{color_bright_red}{style_bold}load failed{style_reset}{color_reset} — DB: {where_} ({counts})"
        );
    }
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
    git: Option<&str>,
    branch: Option<&str>,
    stats: &mut IngestStats,
) -> bool
where
    L::Opts: ProverOptsFor,
{
    if let Some(s) = sink {
        session.set_progress_sink(s);
    }
    ingest_constituents(&mut session, &manager, git, branch, stats);
    // Skipped for `load`: everything was just re-recorded, so a
    // freshness check right after would trivially say "unchanged."
    if !matches!(cmd, Cmd::Load { .. }) {
        sigmakee::cli::maybe_notify_stale_local(&session);
        sigmakee::cli::maybe_notify_stale_git(&session);
    }

    match cmd {
        Cmd::Load { flush: _ } =>
            run_load(session, manager),

        Cmd::Validate { formula, parse } =>
            run_validate(session, manager, formula, parse),

        #[cfg(feature = "ask")]
        Cmd::Ask { formula, tell, interactive, kb: _, keep } => {
            if interactive {
                if std::io::stdout().is_terminal() && !sigmakee::style::is_ugly() {
                    // The interactive editor's result overlay always renders
                    // the proof steps when one is found — unlike one-shot
                    // `ask`, there's no `--proof`/`--want-proof` flag to opt
                    // in with, so force it on for the native backend (the
                    // only one that needs the flag; external backends always
                    // record a proof when they find one).
                    manager.native_prover.want_proof = true;
                    run_ask_tui(session, &manager)
                } else {
                    log::error!("ask -i requires an interactive terminal (not a TTY, or --no-color/dumb terminal)");
                    false
                }
            } else {
                run_ask(session, &manager, formula, tell, keep)
            }
        }

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
            run_test(session, manager, paths, keep, branch)
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

        // #[cfg(feature = "server")]
        // Cmd::Serve { kb: _ } => run_serve(session, &manager),

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
    git:         Option<&str>,
    branch:      Option<&str>,
    stats:       &mut IngestStats,
) -> bool {
    if let Some(s) = sink {
        session.set_progress_sink(s);
    }
    ingest_constituents(&mut session, &manager, git, branch, stats);
    sigmakee::cli::maybe_notify_stale_local(&session);
    sigmakee::cli::maybe_notify_stale_git(&session);

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
/// unchanged files), tallying diagnostics by severity into `stats` for the
/// end-of-run report. `git`/`branch` mirror `--git`/`--branch`: `git = Some`
/// re-roots the constituents onto that repo (see
/// `KBManager::resolve_sources`); `branch` only matters alongside it.
fn ingest_constituents<L: TopLayer>(
    session: &mut Session<L>, manager: &KBManager, git: Option<&str>, branch: Option<&str>, stats: &mut IngestStats,
) {
    for src in manager.resolve_sources(git, branch) {
        stats.sources += 1;
        for e in session.ingest(src, false) {
            match e.severity() {
                sigmakee_rs_sdk::Severity::Error   => { stats.errors   += 1; log::error!("ingest: {e}"); }
                sigmakee_rs_sdk::Severity::Warning => { stats.warnings += 1; log::warn!("ingest: {e}"); }
                sigmakee_rs_sdk::Severity::Info    => { stats.infos    += 1; log::info!("ingest: {e}"); }
                sigmakee_rs_sdk::Severity::Hint    => { stats.hints    += 1; log::debug!("ingest: {e}"); }
            }
        }
    }
}

// -- helpers -----------------------------------------------------------------

/// Build the KBManager from config.xml's preferences, or defaults.
///
/// Config.xml is read whenever `--no-config` is absent: an explicit
/// `--config` path must resolve (fatal otherwise), but the implicit default
/// location (`$SIGMA_HOME` / `~/.sigmakee/KBs/config.xml`) is optional — no
/// file there just means built-in defaults. This is independent of `-c`,
/// which only decides whether the active KB's *constituent files* get
/// ingested (see `ingest_constituents`'s callers).
fn build_manager(cli: &Cli) -> KBManager {
    if cli.no_config {
        return KBManager::default();
    }
    match sigmakee::config::resolve_config_path(cli.config.as_deref()) {
        Some(path) => match KBManager::from_config_xml_path(path) {
            Ok(m) => m,
            Err(e) => {
                // `sumo config` loads (leniently) and writes config.xml
                // itself — an explicit `--config` naming a file that doesn't
                // exist yet is the normal "create a new one" case there, not
                // a fatal error; the manager built here is discarded in
                // favor of that command's own load.
                if matches!(cli.command, Cmd::Config { .. }) {
                    return KBManager::default();
                }
                eprintln!("Error parsing config.xml: {e}");
                process::exit(2);
            }
        },
        None if cli.config.is_some() => {
            if matches!(cli.command, Cmd::Config { .. }) {
                return KBManager::default();
            }
            eprintln!("Could not locate config.xml from `--config {}`",
                cli.config.as_deref().map(|p| p.display().to_string()).unwrap_or_default());
            process::exit(2);
        }
        // No explicit `--config` and no config.xml at the default location:
        // fall back to built-in defaults rather than failing outright.
        None => KBManager::default(),
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
fn apply_global_overrides(cli: &Cli, manager: &mut KBManager) {
    use sigmakee_rs_sdk::{promote_to_error, set_all_errors};
    for arg in &cli.suppress {
        if arg == "all" { set_all_errors(true); } else { promote_to_error(arg); }
    }
    // `--profile` is one knob: it also turns on the native prover's
    // per-mechanism saturation-loop timers (formerly the separate
    // `--native-profile` flag), so the report it prints always has
    // something to show inside `ask.saturate` rather than needing a
    // second flag most callers didn't know to reach for.
    manager.native_prover.profile = cli.profile;
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
        #[cfg(feature = "integrated-prover")]
        "embedded" => Prover::VampireIntegrated(IntegratedVampireRunner),
        _ /* subprocess (or "embedded" without the integrated-prover feature) */ => {
            #[cfg(not(feature = "integrated-prover"))]
            if manager.default_backend == "embedded" {
                log::warn!("'embedded' backend requires the integrated-prover feature; falling back to 'subprocess'");
            }
            let path = manager.resolve_vampire().unwrap_or_else(|e| {
                log::warn!("{e}; falling back to 'vampire' on PATH");
                PathBuf::from("vampire")
            });
            Prover::VampireSubprocess(VampireRunner { vampire_path: path, tptp_dump_path: keep })
        }
    }
}


#[cfg(test)]
mod report_load_tests {
    use super::*;

    #[test]
    fn report_load_formats_without_panicking() {
        let stats = IngestStats { sources: 3, errors: 1, warnings: 2, infos: 0, hints: 0 };
        report_load(true, Some(std::path::Path::new("/tmp/x.lmdb")), &stats);
        report_load(false, None, &stats);
    }
}
