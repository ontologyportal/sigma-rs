//! `sumo casc` — CASC-style batch runs over a directory (or list file) of
//! standalone TPTP problems.
//!
//! Reuses the exact same machinery `sumo test` already uses for a `.p` /
//! `.tptp` file: `Session::<ProverLayer>::test` gives each problem its own
//! fresh, self-contained KB (native backend, TPTP regime auto-detected from
//! the file extension — full saturation + the 5-lane strategy portfolio),
//! and reports the SZS status the same way. This module only adds the batch
//! plumbing: corpus discovery, `--jobs` parallelism (one fresh `Session` per
//! problem per worker thread — no shared, mutated KB, so this is safe), and
//! the summary block.
//!
//! Output is deliberately plain (as if `--ugly` were passed): every line is
//! meant to be greppable / diffable against a reference CASC run, not
//! decorated for a human reading a terminal.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use sigmakee_rs_sdk::{NativeOpts, ProverLayer, Session, Source, SzsStatus, TestOutcome};
use sigmakee_rs_sdk::manager::KBManager;

/// Entry point for `sumo casc <path> [--timeout N] [--jobs K]`.
///
/// `path` is either a directory (every `.p`/`.tptp` file directly inside it)
/// or a plain-text list file (one problem path per line; blank lines and
/// `#`-prefixed comments skipped). Returns `false` on any fatal setup error
/// (bad path, empty corpus) — individual problem failures are reported per
/// line and folded into the summary, not treated as a harness failure.
pub fn run_casc(manager: &KBManager, path: PathBuf, timeout: u32, jobs: usize) -> bool {
    // CASC mode is always plain output, regardless of the global `--ugly`
    // flag — the per-problem SZS lines and summary are meant to be
    // greppable/diffable, not styled for a terminal.
    crate::style::set_ugly(true);

    let problems = match discover_problems(&path) {
        Ok(p) => p,
        Err(e) => {
            log::error!("casc: {e}");
            return false;
        }
    };
    if problems.is_empty() {
        log::error!("casc: no TPTP problems found under {}", path.display());
        return false;
    }

    let base_opts = manager.native_opts();
    let workers = jobs.max(1).min(problems.len());
    let next = AtomicUsize::new(0);
    let rows: Mutex<Vec<Row>> = Mutex::new(Vec::with_capacity(problems.len()));
    let t_all = Instant::now();

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let idx = next.fetch_add(1, Ordering::Relaxed);
                if idx >= problems.len() {
                    break;
                }
                let row = run_one(&problems[idx], &base_opts, timeout);
                // Print as each problem finishes (not sorted/batched at the
                // end) so a long batch shows live progress; stdout lines
                // from concurrent threads may interleave across problems
                // but never WITHIN a `println!` call, so each printed line
                // stays intact.
                println!("% SZS status {} for {}", row.szs, row.name);
                rows.lock().unwrap().push(row);
            });
        }
    });

    let wall = t_all.elapsed().as_secs_f64();
    let mut rows = rows.into_inner().unwrap();
    rows.sort_by(|a, b| a.name.cmp(&b.name));

    print_summary(&rows, wall);
    true
}

struct Row {
    name:   String,
    szs:    SzsStatus,
    solved: bool,
}

/// Run one TPTP problem to completion on a fresh, isolated `Session` —
/// mirrors `sweep.rs`'s `Problem::Tptp` arm, the existing template for
/// per-problem KB isolation under `--jobs` parallelism.
fn run_one(path: &Path, base_opts: &NativeOpts, timeout: u32) -> Row {
    let name = basename(path);
    let mut opts = base_opts.clone();
    // An explicit `--timeout` always pins the budget (mirrors `sumo test`'s
    // `Cmd::Test` dispatch): CASC problems don't rely on a per-file `(time
    // N)` directive the way `.kif.tq` cases can.
    opts.time_limit_secs = u64::from(timeout);

    let mut session = Session::<ProverLayer>::new(format!("casc-{name}"));
    match session.test(Source::Local(vec![path.to_path_buf()]), Some(opts)) {
        Ok(oc) => Row {
            name,
            szs: oc.szs,
            solved: matches!(oc.outcome, TestOutcome::Passed | TestOutcome::Incomplete { .. }),
        },
        Err(errs) => {
            for e in &errs {
                log::error!("casc: {name}: {e}");
            }
            Row { name, szs: SzsStatus::GaveUp, solved: false }
        }
    }
}

/// Discover the TPTP problem set: `path` is either a directory (every
/// `.p`/`.tptp` file directly inside it, sorted) or a plain-text list file
/// (one path per line; blank lines and `#` comments skipped; relative paths
/// resolve against the list file's own directory, matching shell-script
/// convention for corpus manifests).
fn discover_problems(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_dir() {
        let entries = std::fs::read_dir(path)
            .map_err(|e| format!("cannot read directory {}: {e}", path.display()))?;
        let mut out: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && is_tptp_file(p))
            .collect();
        out.sort();
        Ok(out)
    } else if path.is_file() {
        // A `.p`/`.tptp` file directly named on the command line is a
        // (degenerate, one-problem) corpus; anything else is a list file.
        if is_tptp_file(path) {
            return Ok(vec![path.to_path_buf()]);
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read list file {}: {e}", path.display()))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let p = PathBuf::from(line);
            // Resolution order for relative entries: the list file's own
            // directory (shell-manifest convention), then `$TPTP/Problems/`
            // and `$TPTP/` (TPTP-corpus convention — index files list
            // problems as `DOM/DOM123+1.p`).  First existing wins; a path
            // that resolves nowhere is kept as the list-dir join so the
            // per-problem error names something sensible.
            let resolved = if p.is_absolute() {
                p
            } else {
                let local = base.join(&p);
                if local.is_file() {
                    local
                } else if let Some(tptp) = std::env::var_os("TPTP") {
                    let root = PathBuf::from(tptp);
                    let under_problems = root.join("Problems").join(&p);
                    let under_root = root.join(&p);
                    if under_problems.is_file() {
                        under_problems
                    } else if under_root.is_file() {
                        under_root
                    } else {
                        local
                    }
                } else {
                    local
                }
            };
            out.push(resolved);
        }
        Ok(out)
    } else {
        Err(format!("path not found: {}", path.display()))
    }
}

fn is_tptp_file(p: &Path) -> bool {
    let s = p.to_string_lossy();
    s.ends_with(".p") || s.ends_with(".tptp")
}

/// The bare file-stem SZS convention prints (`PUZ001+1`, not the full path
/// or extension) — matches `sumo test`'s own `basename` exactly.
fn basename(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Final summary block: solved/total broken down by SZS class, plus total
/// wall time for the whole batch.
fn print_summary(rows: &[Row], wall_secs: f64) {
    let total = rows.len();
    let solved = rows.iter().filter(|r| r.solved).count();

    println!();
    println!("CASC summary: {solved} / {total} solved");

    let classes = [
        SzsStatus::Theorem,
        SzsStatus::Unsatisfiable,
        SzsStatus::CounterSatisfiable,
        SzsStatus::Satisfiable,
        SzsStatus::GaveUp,
        SzsStatus::Timeout,
    ];
    for class in classes {
        let in_class: Vec<&Row> = rows.iter().filter(|r| r.szs == class).collect();
        if in_class.is_empty() {
            continue;
        }
        let class_solved = in_class.iter().filter(|r| r.solved).count();
        println!("  {:<18} {:>4} / {}", class.to_string(), class_solved, in_class.len());
    }
    println!("Total wall time: {wall_secs:.2}s");
}
