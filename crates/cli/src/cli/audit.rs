//! `sumo audit [FILE]` -- sampled consistency audit: walk a seeded
//! pseudorandom sweep of the KB's axioms (or one file's) and check each
//! sentence's SInE neighbourhood for a contradiction, citing each one found.
//!
//! Pipeline:
//!   1. Use the loaded `Session` (the prover backend is already selected).
//!   2. A `.kif.tq` bundle -> its hypotheses, injected into a temp session and
//!      checked as one neighbourhood.
//!   3. Otherwise `KnowledgeBase::audit_sweep_order` orders the eligible
//!      sentences (whole KB, or FILE's) by `--seed`, and
//!      `KnowledgeBase::audit_sampled` checks `--count` of them from `--step`,
//!      `--batch` per subproblem, one subproblem per call so each prints a
//!      progress line.
//!   4. Render the contradictions, citing each axiom-role step back to its
//!      `file:line` via `build_axiom_source_index`, and print the position to
//!      resume from.
//!
//! Requires the `ask` feature.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use sigmakee_rs_sdk::manager::{KBManager, ProverOptsFor};
use sigmakee_rs_sdk::Session;
use sigmakee_rs_sdk::{
    parse_test_content, AstKif, AuditSample, CommonProverOpts, KifProofStep, KnowledgeBase,
    ProverStatus, ProvingLayer, SentenceId, TopLayer,
};

use crate::style::*;

/// Where a sampled audit's sweep starts and how much of it to check.
pub struct Sweep {
    /// Seed of the pseudorandom sweep order.
    pub seed: u32,
    /// Sweep position to start from.
    pub step: usize,
    /// Sentences to check; `None` = `--thoroughness` of what remains.
    pub count: Option<usize>,
    /// Sentences per subproblem.
    pub batch: usize,
    /// Print one JSON summary on stdout (progress goes to stderr).
    pub json: bool,
}

/// A human-readable line: stdout normally, stderr under `--json` so stdout
/// carries only the JSON summary.
fn say(json: bool, line: String) {
    if json {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

/// Run `sumo audit`: sample-check the KB, the given file, or a test bundle,
/// print any cited contradiction derivations and where to resume. Returns
/// `true` when no contradiction is found.
pub fn run_audit<L>(
    mut session: Session<L>,
    manager: &KBManager,
    file: Option<PathBuf>,
    sweep: Sweep,
    keep: Option<PathBuf>,
) -> bool
where
    L: ProvingLayer,
    L::Opts: ProverOptsFor,
{
    // `--keep` (TPTP dump) does not apply to the in-CLI audit transcript.
    let _ = keep;

    let thoroughness = manager.thoroughness;
    if !(thoroughness > 0.0 && thoroughness <= 1.0) {
        log::error!("--thoroughness must be in (0.0, 1.0]; got {}", thoroughness);
        return false;
    }

    let is_test_file = file
        .as_ref()
        .and_then(|f| f.file_name())
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".kif.tq"));
    let (proofs, summary) = match file {
        Some(file) if is_test_file => {
            let Ok((tag, sids, sess)) = inject_test_case(session.kb_mut(), &file) else {
                return false;
            };
            say(
                sweep.json,
                format!(
                    "{style_bold}Audit:{style_reset} {} -- test bundle, {} sentence(s)",
                    tag,
                    sids.len()
                ),
            );
            let mut opts = <L::Opts as ProverOptsFor>::from_manager(manager);
            opts.set_session(Some(sess.clone()));
            let mut result = session.kb().audit_consistency(&sids, opts, manager.limit);
            session.kb_mut().flush_session(&sess);
            log::info!("{}", result.raw_output);
            if result.contradiction_proofs.is_empty() && !result.proof_kif.is_empty() {
                result
                    .contradiction_proofs
                    .push(std::mem::take(&mut result.proof_kif));
            }
            let summary = serde_json::json!({ "test_bundle": tag, "sentences": sids.len() });
            (result.contradiction_proofs, summary)
        }
        file => {
            let scope = match &file {
                Some(file) => {
                    let tag_primary = file.display().to_string();
                    let tag_canonical = file.canonicalize().ok().map(|p| p.display().to_string());
                    match resolve_file_tag(
                        session.kb(),
                        file,
                        &tag_primary,
                        tag_canonical.as_deref(),
                    ) {
                        Ok((tag, _)) => Some(tag),
                        Err(()) => return false,
                    }
                }
                None => None,
            };
            sampled_sweep(session.kb(), manager, scope.as_deref(), &sweep)
        }
    };
    let n = proofs.len();
    if sweep.json {
        let src_idx = session.kb().build_axiom_source_index();
        let contradictions: Vec<serde_json::Value> = proofs
            .iter()
            .map(|steps| {
                let mut seen = BTreeSet::new();
                let axioms: Vec<serde_json::Value> = steps
                    .iter()
                    .filter_map(|st| {
                        let sid = st.source_sid.filter(|sid| seen.insert(*sid))?;
                        let a = src_idx.lookup_by_sid(sid);
                        Some(serde_json::json!({
                            "file": a.map(|a| a.file.clone()),
                            "line": a.map(|a| a.line),
                            "kif": st.formula.flat(),
                        }))
                    })
                    .collect();
                serde_json::json!({ "axioms": axioms, "steps": steps.len() })
            })
            .collect();
        let mut out = summary;
        out["inconsistent"] = serde_json::json!(n > 0);
        out["contradictions"] = serde_json::json!(contradictions);
        println!("{out}");
        return n == 0;
    }
    if n > 0 {
        println!(
            "{style_bold}Result:{style_reset} {color_bright_red}Inconsistent{color_reset} -- {} distinct contradiction(s)",
            n
        );
    }

    if n > 0 {
        let src_idx = session.kb().build_axiom_source_index();
        let plain =
            crate::style::is_ugly() || !std::io::IsTerminal::is_terminal(&std::io::stdout());

        for (i, steps) in proofs.iter().enumerate() {
            let mut seen = BTreeSet::new();
            let mut axioms: Vec<(String, String)> = Vec::new();
            for st in steps {
                if let Some(sid) = st.source_sid {
                    if !seen.insert(sid) {
                        continue;
                    }
                    if let Some(a) = src_idx.lookup_by_sid(sid) {
                        let f = fmt_formula(&st.formula, 6, plain);
                        axioms.push((f, format!("{}:{}", a.file, a.line)));
                    }
                }
            }
            println!(
                "\n{style_bold}#{} — {} axiom(s):{style_reset}",
                i + 1,
                axioms.len()
            );
            for (formula, loc) in &axioms {
                println!("    {color_bright_black}[{}]{color_reset}", loc);
                println!("      {}", formula);
            }
        }

        if manager.proof != "none" {
            let pages: Vec<String> = proofs
                .iter()
                .enumerate()
                .map(|(i, steps)| render_derivation(i + 1, steps, &src_idx, plain))
                .collect();
            let paged = !plain && page_derivations(&pages).is_ok();
            if !paged {
                for p in &pages {
                    println!("\n{p}");
                }
            }
        }
    }

    n == 0
}

/// Walk `sweep`'s slice of the sweep one subproblem at a time, printing a
/// progress line per subproblem and the position to resume from; returns the
/// distinct contradictions found and the run's JSON summary.
fn sampled_sweep<L>(
    kb: &KnowledgeBase<L>,
    manager: &KBManager,
    scope: Option<&str>,
    sweep: &Sweep,
) -> (Vec<Vec<KifProofStep>>, serde_json::Value)
where
    L: ProvingLayer,
    L::Opts: ProverOptsFor,
{
    let order = kb.audit_sweep_order(scope, sweep.seed);
    let start = sweep.step.min(order.len());
    let remaining = order.len() - start;
    let count = sweep
        .count
        .unwrap_or_else(|| ((remaining as f32) * manager.thoroughness).ceil() as usize)
        .min(remaining);
    let batch = sweep.batch.max(1);
    say(
        sweep.json,
        format!(
            "{style_bold}Audit:{style_reset} {}: checking {} of {} sentence(s) from step {} (seed {}, {} per subproblem)",
            scope.unwrap_or("the entire KB"),
            count,
            order.len(),
            start,
            sweep.seed,
            batch
        ),
    );

    let opts = <L::Opts as ProverOptsFor>::from_manager(manager);
    let src_idx = kb.build_axiom_source_index();
    let mut seen: HashSet<Vec<SentenceId>> = HashSet::new();
    let mut proofs: Vec<Vec<KifProofStep>> = Vec::new();
    let (mut problems, mut clean, mut contradictory) = (0usize, 0usize, 0usize);
    let end = start + count;
    let mut pos = start;
    while pos < end && proofs.len() < manager.limit {
        let take = batch.min(end - pos);
        let sample = AuditSample {
            seed: sweep.seed,
            step: pos,
            count: take,
            batch: take,
            limit: manager.limit - proofs.len(),
        };
        let out = kb.audit_sampled(&order, sample, &opts);
        for b in &out.batches {
            problems += 1;
            clean += usize::from(b.outcome.status == ProverStatus::Consistent);
            contradictory += usize::from(b.outcome.status == ProverStatus::Inconsistent);
            let locs: Vec<String> = b
                .focus
                .iter()
                .map(|sid| {
                    src_idx
                        .lookup_by_sid(*sid)
                        .map_or_else(|| "?".to_string(), |a| format!("{}:{}", a.file, a.line))
                })
                .collect();
            say(
                sweep.json,
                format!(
                    "  [{}/{}] {:?} in {} ms : {}",
                    out.next_step - start,
                    count,
                    b.outcome.status,
                    b.elapsed.as_millis(),
                    locs.join(", ")
                ),
            );
        }
        for steps in out.result.contradiction_proofs {
            let mut culprits: Vec<SentenceId> = steps.iter().filter_map(|s| s.source_sid).collect();
            culprits.sort_unstable();
            culprits.dedup();
            if seen.insert(culprits) {
                proofs.push(steps);
            }
        }
        pos = out.next_step.max(pos + take);
    }

    if proofs.is_empty() {
        say(
            sweep.json,
            format!(
                "{style_bold}Result:{style_reset} {color_bright_yellow}No contradiction found{color_reset} in {} neighbourhood(s) ({} saturated clean, {} hit a limit) -- this does not certify the KB consistent",
                problems,
                clean,
                problems - clean - contradictory
            ),
        );
    }
    say(
        sweep.json,
        format!(
            "Checked steps {}..{} of {}. Resume with: --seed {} --step {}",
            start,
            pos,
            order.len(),
            sweep.seed,
            pos
        ),
    );
    let summary = serde_json::json!({
        "scope": scope,
        "seed": sweep.seed,
        "step": start,
        "next_step": pos,
        "total": order.len(),
        "subproblems": {
            "total": problems,
            "clean": clean,
            "contradictory": contradictory,
            "hit_limit": problems - clean - contradictory,
        },
    });
    (proofs, summary)
}

/// Inject a `.kif.tq` test bundle's hypotheses into a temp session and return
/// its tag, sids (the audit focus), and session name.
fn inject_test_case<L: TopLayer>(
    kb: &mut KnowledgeBase<L>,
    file: &Path,
) -> Result<(String, Vec<SentenceId>, String), ()> {
    let tag = file.display().to_string();
    let content = std::fs::read_to_string(file).map_err(|e| {
        log::error!("failed to read test file '{}': {}", tag, e);
    })?;
    let tc = parse_test_content(&content, &tag).map_err(|e| {
        log::error!("failed to parse test file '{}': {}", tag, e);
    })?;
    let session = format!("debug-{}", std::process::id());
    let bundle = tc.axiom_kif();
    let result = kb.tell(&bundle, &session);
    if !result.ok {
        for e in &result.diagnostics {
            log::error!("{}: {}", tag, e.message);
        }
        kb.flush_session(&session);
        return Err(());
    }
    let sids = kb.session_sids(&session);
    Ok((tag, sids, session))
}

/// Resolve a `.kif` FILE argument to a loaded tag + its root sids.
fn resolve_file_tag<L: TopLayer>(
    kb: &KnowledgeBase<L>,
    file: &Path,
    tag_primary: &str,
    tag_canonical: Option<&str>,
) -> Result<(String, Vec<SentenceId>), ()> {
    let roots = kb.file_roots(tag_primary);
    if !roots.is_empty() {
        return Ok((tag_primary.to_string(), roots));
    }
    if let Some(canon) = tag_canonical {
        let roots = kb.file_roots(canon);
        if !roots.is_empty() {
            return Ok((canon.to_string(), roots));
        }
    }
    // Basename suffix match against loaded tags.
    let needle = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(tag_primary);
    let mut hits: Vec<String> = kb
        .iter_files()
        .into_iter()
        .filter(|t| t.ends_with(needle))
        .collect();
    hits.sort();
    hits.dedup();
    match hits.len() {
        1 => {
            let hit = hits.pop().unwrap();
            let sids = kb.file_roots(&hit);
            Ok((hit, sids))
        }
        0 => {
            log::error!(
                "'{}' is not a loaded file (load it with -f/-d/-c); loaded: {}",
                tag_primary,
                kb.iter_files().join(", ")
            );
            Err(())
        }
        _ => {
            log::error!("'{}' is ambiguous; candidates: {}", needle, hits.join(", "));
            Err(())
        }
    }
}

/// Render a proof step's formula as KIF via its AST: `pretty_print` (colored,
/// multi-line) when `plain` is false, else `format_plain`.
fn fmt_formula(f: &sigmakee_rs_sdk::AstNode, indent: usize, plain: bool) -> String {
    if plain {
        f.format_plain(indent)
    } else {
        f.pretty_print(indent)
    }
}

/// Render one contradiction's full derivation as a string.
fn render_derivation(
    num: usize,
    steps: &[sigmakee_rs_sdk::KifProofStep],
    src_idx: &sigmakee_rs_sdk::AxiomSourceIndex,
    plain: bool,
) -> String {
    let mut s = format!("Contradiction #{num} ({} steps):\n", steps.len());
    for st in steps {
        let trace = st
            .source_sid
            .and_then(|sid| src_idx.lookup_by_sid(sid))
            .map(|a| format!("   [{}:{}]", a.file, a.line))
            .unwrap_or_default();
        s.push_str(&format!("  {:>3}. [{:<18}]{}\n", st.index, st.rule, trace));
        let body = fmt_formula(&st.formula, 8, plain);
        s.push_str(&format!("        {body}\n"));
    }
    s
}

/// Minimal one-section-per-page pager (crossterm): each contradiction's
/// derivation is its own page.  `n`/space/→ next, `p`/← prev, `j`/`k` or ↑/↓
/// scroll a long derivation, `q` quit.  Returns `Err` if the terminal can't be
/// driven (caller falls back to inline printing).
fn page_derivations(pages: &[String]) -> std::io::Result<()> {
    use crossterm::{
        cursor,
        event::{read, Event, KeyCode},
        execute,
        terminal::{
            disable_raw_mode, enable_raw_mode, size, Clear, ClearType, EnterAlternateScreen,
            LeaveAlternateScreen,
        },
    };
    use std::io::Write;
    if pages.is_empty() {
        return Ok(());
    }

    let mut out = std::io::stdout();
    enable_raw_mode()?;
    execute!(out, EnterAlternateScreen, cursor::Hide)?;
    let result = (|| -> std::io::Result<()> {
        let (mut idx, mut top) = (0usize, 0usize);
        loop {
            let (_cols, rows) = size().unwrap_or((80, 24));
            let body_rows = rows.saturating_sub(1) as usize; // one row for the status bar
            let lines: Vec<&str> = pages[idx].lines().collect();
            let max_top = lines.len().saturating_sub(body_rows);
            top = top.min(max_top);
            execute!(out, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
            for line in lines.iter().skip(top).take(body_rows) {
                write!(out, "{line}\r\n")?;
            }
            execute!(out, cursor::MoveTo(0, rows.saturating_sub(1)))?;
            write!(
                out,
                "\x1b[7m #{}/{}  space/n next · p prev · j/k scroll · q quit \x1b[0m",
                idx + 1,
                pages.len()
            )?;
            out.flush()?;
            if let Event::Key(k) = read()? {
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('n') | KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Right => {
                        if idx + 1 < pages.len() {
                            idx += 1;
                            top = 0;
                        }
                    }
                    KeyCode::Char('p') | KeyCode::Left => {
                        if idx > 0 {
                            idx -= 1;
                            top = 0;
                        }
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        if top < max_top {
                            top += 1;
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        top = top.saturating_sub(1);
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    execute!(out, cursor::Show, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    result
}
