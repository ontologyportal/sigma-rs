//! `sumo sweep` — the strategy-tuning harness behind portfolio mode.
//!
//! Runs a grid of (strategy config × corpus problem) with the native
//! prover, reports which configs solve what, then greedily picks a
//! portfolio by marginal coverage: lane k+1 is chosen for the problems
//! lanes 1..k leave unsolved.
//!
//! Corpus: `.kif.tq` test files (run against the loaded SUMO KB) and
//! TPTP `.p` files (each run gets a fresh, self-contained KB). Configs:
//! `Strategy::base()` always; plus a JSON array of (partial) Strategy
//! specs via `--configs`; plus `--random N` seeded samples.

#![cfg(feature = "ask")]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use sigmakee_rs_core::{
    parse_test_content, KnowledgeBase, NativeOpts, ProverLayer, ProverStatus, Severity,
    SineParams, Strategy,
};
use sigmakee_rs_sdk::{Session, Source};
use sigmakee_rs_sdk::manager::KBManager;

enum Problem {
    /// A `.kif.tq` case: hypotheses already told into `session` on the
    /// shared KB; `expected` is the test's yes/no answer.
    Tq { name: String, query: String, session: String, expected: bool },
    /// A TPTP problem file; `Session::test` resolves any `include(...)`.
    Tptp { name: String, path: PathBuf, expected: Option<String> },
}

impl Problem {
    fn name(&self) -> &str {
        match self {
            Problem::Tq { name, .. } | Problem::Tptp { name, .. } => name,
        }
    }
}

struct Row {
    config:  usize,
    problem: usize,
    solved:  bool,
    status:  ProverStatus,
    steps:   Option<usize>,
    ms:      u128,
}

/// Run the strategy sweep over the given corpus and configs, printing the
/// per-config results and greedy portfolio.
///
/// Discovers `.kif.tq` and TPTP `.p` files under `paths`, runs every
/// strategy against every problem, optionally writes the result matrix to
/// `out` (CSV) and the chosen portfolio to `portfolio_out` (JSON). Returns
/// `false` on any fatal error (unreadable path, empty corpus, write failure).
#[allow(clippy::too_many_arguments)]
pub fn run_sweep(
    mut session:   Session<ProverLayer>,
    _manager:      &KBManager,
    paths:         Vec<PathBuf>,
    configs:       Option<PathBuf>,
    random:        usize,
    seed:          u64,
    budget:        usize,
    max_steps:     usize,
    timeout:       u32,
    jobs:          Option<usize>,
    out:           Option<PathBuf>,
    lanes:         usize,
    portfolio_out: Option<PathBuf>,
) -> bool {
    // -- corpus discovery -------------------------------------------------
    let mut tq_files: Vec<PathBuf> = Vec::new();
    let mut tptp_files: Vec<PathBuf> = Vec::new();
    for path in &paths {
        if path.is_dir() {
            let Ok(entries) = std::fs::read_dir(path) else {
                log::error!("sweep: cannot read directory {}", path.display());
                return false;
            };
            for entry in entries.flatten() {
                classify_file(entry.path(), &mut tq_files, &mut tptp_files);
            }
        } else if path.is_file() {
            classify_file(path.clone(), &mut tq_files, &mut tptp_files);
        } else {
            log::error!("sweep: path not found: {}", path.display());
            return false;
        }
    }
    tq_files.sort();
    tptp_files.sort();
    if tq_files.is_empty() && tptp_files.is_empty() {
        log::error!("sweep: no .kif.tq or .p files found under the given paths");
        return false;
    }

    // -- strategy lanes ----------------------------------------------------
    let mut strategies: Vec<Strategy> = vec![Strategy::base()];
    if let Some(cfg_path) = &configs {
        let text = match std::fs::read_to_string(cfg_path) {
            Ok(t) => t,
            Err(e) => {
                log::error!("sweep: cannot read {}: {e}", cfg_path.display());
                return false;
            }
        };
        match serde_json::from_str::<Vec<Strategy>>(&text) {
            Ok(mut v) => strategies.append(&mut v),
            Err(e) => {
                log::error!(
                    "sweep: {} is not a JSON array of strategy specs: {e}",
                    cfg_path.display());
                return false;
            }
        }
    }
    for i in 0..random {
        strategies.push(Strategy::sample(seed.wrapping_add(i as u64)));
    }
    // Names must be unique: they are the join key in every report.
    {
        let mut seen: HashSet<String> = HashSet::new();
        for (i, s) in strategies.iter_mut().enumerate() {
            if !seen.insert(s.name.clone()) {
                s.name = format!("{}#{i}", s.name);
                seen.insert(s.name.clone());
            }
        }
    }

    // -- shared KB + TQ session setup (sequential; tells mutate) -----------
    let mut problems: Vec<Problem> = Vec::new();
    let has_tq = !tq_files.is_empty();
    for (idx, file) in tq_files.iter().enumerate() {
        let Ok(content) = std::fs::read_to_string(file) else {
            log::warn!("sweep: skipping unreadable {}", file.display());
            continue;
        };
        let tc = match parse_test_content(&content, &file.to_string_lossy()) {
            Ok(tc) => tc,
            Err(e) => {
                log::warn!("sweep: skipping {}: {}", file.display(), e.message);
                continue;
            }
        };
        let Some(query) = tc.query_kif() else {
            log::warn!("sweep: skipping {} (no query)", file.display());
            continue;
        };
        let session_name = format!("sweep-{idx}");
        let axiom_text = tc.axiom_kif();
        if !axiom_text.is_empty() {
            let r = session.kb_mut().tell(&axiom_text, &session_name);
            if !r.ok {
                log::warn!("sweep: skipping {} (axiom parse error)", file.display());
                session.kb_mut().flush_session(&session_name);
                continue;
            }
            let errors: Vec<_> = session.kb().validate_session(&session_name).into_iter()
                .filter(|d| matches!(d.severity, Severity::Error))
                .collect();
            if !errors.is_empty() {
                log::warn!("sweep: skipping {} (semantic errors)", file.display());
                session.kb_mut().flush_session(&session_name);
                continue;
            }
        }
        problems.push(Problem::Tq {
            name: tc.note.clone(),
            query,
            session: session_name,
            expected: tc.expected_proof.unwrap_or(true),
        });
    }
    for file in &tptp_files {
        let name = file.file_name().map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| file.display().to_string());
        let Ok(raw) = std::fs::read_to_string(file) else {
            log::warn!("sweep: skipping unreadable {}", file.display());
            continue;
        };
        problems.push(Problem::Tptp {
            name,
            path: file.clone(),
            expected: expected_tptp_status(&raw),
        });
    }

    let shared_kb: Option<&KnowledgeBase<ProverLayer>> =
        if has_tq { Some(session.kb()) } else { None };
    if problems.is_empty() {
        log::error!("sweep: every corpus file was skipped");
        return false;
    }

    // -- the grid -----------------------------------------------------------
    let n_tasks = strategies.len() * problems.len();
    let workers = jobs.unwrap_or_else(|| {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
    }).clamp(1, n_tasks);
    println!(
        "Sweep: {} configs x {} problems = {} runs  \
         ({} threads, budget {}, {} steps, {}s timeout)",
        strategies.len(), problems.len(), n_tasks,
        workers, budget, max_steps, timeout);

    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let rows: Mutex<Vec<Row>> = Mutex::new(Vec::with_capacity(n_tasks));
    let t_grid = Instant::now();

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let task = next.fetch_add(1, Ordering::Relaxed);
                if task >= n_tasks {
                    break;
                }
                let (ci, pi) = (task / problems.len(), task % problems.len());
                let strat = &strategies[ci];
                let t0 = Instant::now();
                let (result, solved) = run_one(
                    shared_kb, &problems[pi], strat, budget, max_steps, timeout);
                rows.lock().unwrap().push(Row {
                    config: ci,
                    problem: pi,
                    solved,
                    status: result.status,
                    steps: result.given_steps,
                    ms: t0.elapsed().as_millis(),
                });
                let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                if d % (n_tasks / 20).max(1) == 0 || d == n_tasks {
                    eprint!("\rsweep: {d}/{n_tasks}");
                }
            });
        }
    });
    eprintln!();
    let grid_elapsed = t_grid.elapsed();

    let mut rows = rows.into_inner().unwrap();
    rows.sort_by_key(|r| (r.config, r.problem));

    // -- CSV ---------------------------------------------------------------
    if let Some(out_path) = &out {
        let mut csv = String::from("config,problem,solved,status,steps,ms\n");
        for r in &rows {
            csv.push_str(&format!(
                "{},{},{},{:?},{},{}\n",
                csv_field(&strategies[r.config].name),
                csv_field(problems[r.problem].name()),
                r.solved,
                r.status,
                r.steps.map(|s| s.to_string()).unwrap_or_default(),
                r.ms));
        }
        if let Err(e) = std::fs::write(out_path, csv) {
            log::error!("sweep: cannot write {}: {e}", out_path.display());
            return false;
        }
        println!("Matrix written to {}", out_path.display());
    }

    // -- per-config summary --------------------------------------------------
    let solved_sets: Vec<HashSet<usize>> = (0..strategies.len())
        .map(|ci| rows.iter()
            .filter(|r| r.config == ci && r.solved)
            .map(|r| r.problem)
            .collect())
        .collect();
    let config_steps: Vec<u64> = (0..strategies.len())
        .map(|ci| rows.iter()
            .filter(|r| r.config == ci && r.solved)
            .map(|r| r.steps.unwrap_or(0) as u64)
            .sum())
        .collect();

    let mut order: Vec<usize> = (0..strategies.len()).collect();
    order.sort_by_key(|&ci| (std::cmp::Reverse(solved_sets[ci].len()), config_steps[ci]));
    println!("\nPer-config results (solved / {}; total steps on solved):",
        problems.len());
    for &ci in order.iter().take(20) {
        println!("  {:<28} {:>3}  ({} steps)",
            strategies[ci].name, solved_sets[ci].len(), config_steps[ci]);
    }
    if order.len() > 20 {
        println!("  ... {} more configs (see CSV)", order.len() - 20);
    }

    // -- greedy set cover ------------------------------------------------------
    let mut covered: HashSet<usize> = HashSet::new();
    let mut chosen: Vec<usize> = Vec::new();
    println!("\nGreedy portfolio (marginal coverage):");
    for slot in 0..lanes.min(strategies.len()) {
        let best = (0..strategies.len())
            .filter(|ci| !chosen.contains(ci))
            .map(|ci| {
                let marginal = solved_sets[ci].difference(&covered).count();
                (marginal, std::cmp::Reverse(config_steps[ci]), ci)
            })
            .max();
        let Some((marginal, _, ci)) = best else { break };
        if marginal == 0 {
            println!("  (stopping at {} lanes: no config adds coverage)", slot);
            break;
        }
        covered.extend(solved_sets[ci].iter().copied());
        chosen.push(ci);
        println!("  {}. {:<26} +{:<3} -> {}/{}",
            slot + 1, strategies[ci].name, marginal, covered.len(), problems.len());
    }
    let best_single = order.first().map(|&ci| solved_sets[ci].len()).unwrap_or(0);
    println!(
        "\nPortfolio coverage: {}/{}  (best single config: {}/{})",
        covered.len(), problems.len(), best_single, problems.len());
    let unsolved: Vec<&str> = (0..problems.len())
        .filter(|pi| !covered.contains(pi))
        .map(|pi| problems[pi].name())
        .collect();
    if !unsolved.is_empty() {
        println!("Unsolved by every config (not a tuning problem): {}",
            unsolved.join(", "));
    }
    println!("Grid wall time: {:.1}s", grid_elapsed.as_secs_f64());

    // -- emit the chosen lanes ----------------------------------------------
    if let Some(pf_path) = &portfolio_out {
        let lanes_json: Vec<&Strategy> = chosen.iter().map(|&ci| &strategies[ci]).collect();
        match serde_json::to_string_pretty(&lanes_json) {
            Ok(json) => {
                if let Err(e) = std::fs::write(pf_path, json) {
                    log::error!("sweep: cannot write {}: {e}", pf_path.display());
                    return false;
                }
                println!("Portfolio written to {}", pf_path.display());
            }
            Err(e) => {
                log::error!("sweep: portfolio serialization failed: {e}");
                return false;
            }
        }
    }
    true
}

/// Run one (config, problem) cell: a single-shot solve with a fixed SInE
/// budget. Returns the prover result and whether it matched the expected
/// verdict.
fn run_one(
    kb:        Option<&KnowledgeBase<ProverLayer>>,
    problem:   &Problem,
    strat:     &Strategy,
    budget:    usize,
    max_steps: usize,
    timeout:   u32,
) -> (sigmakee_rs_core::prover::ProverResult, bool) {
    let sine = SineParams {
        autoscale: false,
        auto_budget: Some(budget),
        select_all: false,
        ..Default::default()
    };
    match problem {
        Problem::Tq { query, session, expected, .. } => {
            let opts = NativeOpts {
                selection: sine,
                session: None,
                max_steps,
                max_lits: 12,
                time_limit_secs: u64::from(timeout),
                forward_close: true,
                profile: false,
                want_proof: false,
                strategy: strat.clone(),
                cancel: None,
                step: false,
                ..Default::default()
            };
            let kb = kb.expect("TQ problems imply a shared KB");
            let r = kb.ask_query(query, Some(session), sine, opts);
            let got = r.status == ProverStatus::Proved;
            let ok = got == *expected;
            (r, ok)
        }
        Problem::Tptp { name, path, expected } => {
            let opts = NativeOpts {
                selection: sine,
                session: None,
                max_steps,
                // Long FOF input clauses must not be dropped.
                max_lits: 64,
                time_limit_secs: u64::from(timeout),
                forward_close: true,
                profile: false,
                want_proof: false,
                strategy: strat.clone(),
                cancel: None,
                step: false,
                ..Default::default()
            };
            let mut sess = Session::<ProverLayer>::new(format!("sweep-{name}"));
            let r = match sess.test(Source::Local(vec![path.clone()]), Some(opts)) {
                Ok(oc) => oc.result,
                Err(errs) => {
                    for e in errs { log::warn!("sweep: {name}: {e}"); }
                    sigmakee_rs_core::prover::ProverResult {
                        status: ProverStatus::InputError,
                        ..Default::default()
                    }
                }
            };
            let ok = match expected.as_deref() {
                Some("Theorem") => r.status == ProverStatus::Proved,
                Some("CounterSatisfiable") | Some("Satisfiable") =>
                    r.status != ProverStatus::Proved
                        && r.status != ProverStatus::InputError,
                Some("Unsatisfiable") | Some("ContradictoryAxioms") =>
                    matches!(r.status,
                        ProverStatus::Proved | ProverStatus::Inconsistent),
                // No (or unknown) header: count a proof as solving it.
                _ => r.status == ProverStatus::Proved,
            };
            (r, ok)
        }
    }
}

fn classify_file(p: PathBuf, tq: &mut Vec<PathBuf>, tptp: &mut Vec<PathBuf>) {
    let s = p.to_string_lossy().to_string();
    if !p.is_file() {
        return;
    }
    if s.ends_with(".kif.tq") {
        tq.push(p);
    } else if s.ends_with(".p") || s.ends_with(".tptp") {
        tptp.push(p);
    }
}

/// The `% Status : <SZS>` header line, if present (TPTP convention).
fn expected_tptp_status(text: &str) -> Option<String> {
    for line in text.lines().take(60) {
        let l = line.trim_start_matches('%').trim();
        if let Some(rest) = l.strip_prefix("Status") {
            let v = rest.trim_start_matches([' ', ':']).split_whitespace().next()?;
            return Some(v.to_string());
        }
    }
    None
}

fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
