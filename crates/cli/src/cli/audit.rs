//! `sumo audit [FILE]` — consistency-check the knowledge base (or a single
//! file / `.kif.tq` test bundle) by enumerating distinct contradictions with
//! cited derivations, via the session's prover.
//!
//! Pipeline:
//!   1. Use the loaded `Session` (the prover backend is already selected).
//!   2. No FILE ⇒ audit the entire promoted base (empty focus). A `.kif` file
//!      ⇒ its roots, optionally subsampled by `--thoroughness`. A `.kif.tq`
//!      bundle ⇒ its hypotheses, injected into a temp session.
//!   3. `KnowledgeBase::audit_consistency` enumerates up to `--limit`
//!      contradictions over the focus's neighbourhood.
//!   4. Render the verdict; on `Inconsistent`, cite each axiom-role step back
//!      to its `file:line` via `build_axiom_source_index`.
//!
//! Requires the `ask` feature.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rand::seq::SliceRandom;

use sigmakee_rs_sdk::{
    parse_test_content, AstKif, CommonProverOpts, KnowledgeBase, ProverStatus, ProvingLayer,
    SentenceId, TopLayer,
};
use sigmakee_rs_sdk::Session;
use sigmakee_rs_sdk::manager::{KBManager, ProverOptsFor};

use crate::style::*;

/// Run `sumo audit`: consistency-check the KB or the given file/test bundle,
/// print the verdict and any cited contradiction derivations. Returns `true`
/// when no contradiction is found.
pub fn run_audit<L>(
    mut session: Session<L>,
    manager:     &KBManager,
    file:        Option<PathBuf>,
    keep:        Option<PathBuf>,
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

    // No file ⇒ the entire KB (empty focus). Otherwise a `.kif` file (its
    // roots, optionally subsampled) or a `.kif.tq` test bundle.
    let (tag, sample, header_count, debug_session):
        (String, Vec<SentenceId>, Option<usize>, Option<String>) =
        match &file {
            None => ("the entire KB".to_string(), Vec::new(), None, None),
            Some(file) => {
                let is_test_file = file.file_name().and_then(|n| n.to_str())
                    .map(|n| n.ends_with(".kif.tq")).unwrap_or(false);
                let (tag, sids, sess) = if is_test_file {
                    match inject_test_case(session.kb_mut(), file) {
                        Ok((tag, sids, sess)) => (tag, sids, Some(sess)),
                        Err(()) => return false,
                    }
                } else {
                    let tag_primary = file.display().to_string();
                    let tag_canonical = file.canonicalize().ok().map(|p| p.display().to_string());
                    match resolve_file_tag(session.kb(), file, &tag_primary, tag_canonical.as_deref()) {
                        Ok((tag, sids)) => (tag, sids, None),
                        Err(()) => return false,
                    }
                };
                let total = sids.len();
                let sample = if thoroughness >= 1.0 || is_test_file {
                    sids
                } else {
                    let mut rng = rand::rng();
                    let mut shuffled = sids;
                    shuffled.shuffle(&mut rng);
                    let keep_n = ((shuffled.len() as f32) * thoroughness).ceil().max(1.0) as usize;
                    shuffled.truncate(keep_n);
                    shuffled
                };
                (tag, sample, Some(total), sess)
            }
        };

    match header_count {
        Some(total) => println!(
            "{style_bold}Audit:{style_reset} {} — {} of {} sentence(s)",
            tag, sample.len(), total),
        None => println!("{style_bold}Audit:{style_reset} {}", tag),
    }

    let mut opts = <L::Opts as ProverOptsFor>::from_manager(manager);
    opts.set_session(debug_session.clone());

    let result = session.kb().audit_consistency(&sample, opts, manager.limit);
    if let Some(s) = debug_session.as_ref() { session.kb_mut().flush_session(s); }

    let n = result.contradiction_proofs.len();
    match result.status {
        ProverStatus::Consistent => {
            println!("{style_bold}Result:{style_reset} {color_bright_green}Consistent{color_reset} (saturated — no contradiction reachable from this sample)");
        }
        ProverStatus::Inconsistent => {
            println!("{style_bold}Result:{style_reset} {color_bright_red}Inconsistent{color_reset} — {} distinct contradiction(s)", n);
        }
        other => {
            println!("{style_bold}Result:{style_reset} {color_bright_yellow}{:?}{color_reset} (budget exhausted; no contradiction found — weaker than Consistent)", other);
        }
    }
    log::info!("{}", result.raw_output);

    if n > 0 {
        let src_idx = session.kb().build_axiom_source_index();
        let plain = crate::style::is_ugly()
            || !std::io::IsTerminal::is_terminal(&std::io::stdout());

        for (i, steps) in result.contradiction_proofs.iter().enumerate() {
            let mut seen = BTreeSet::new();
            let mut axioms: Vec<(String, String)> = Vec::new();
            for st in steps {
                if let Some(sid) = st.source_sid {
                    if !seen.insert(sid) { continue; }
                    if let Some(a) = src_idx.lookup_by_sid(sid) {
                        let f = fmt_formula(&st.formula, 6, plain);
                        axioms.push((f, format!("{}:{}", a.file, a.line)));
                    }
                }
            }
            println!("\n{style_bold}#{} — {} axiom(s):{style_reset}", i + 1, axioms.len());
            for (formula, loc) in &axioms {
                println!("    {color_bright_black}[{}]{color_reset}", loc);
                println!("      {}", formula);
            }
        }

        if manager.proof != "none" {
            let pages: Vec<String> = result.contradiction_proofs.iter().enumerate()
                .map(|(i, steps)| render_derivation(i + 1, steps, &src_idx, plain))
                .collect();
            let paged = !plain && page_derivations(&pages).is_ok();
            if !paged {
                for p in &pages { println!("\n{p}"); }
            }
        }
    }

    n == 0 && matches!(result.status, ProverStatus::Consistent)
}

/// Inject a `.kif.tq` test bundle's hypotheses into a temp session and return
/// its tag, sids (the audit focus), and session name.
fn inject_test_case<L: TopLayer>(
    kb:   &mut KnowledgeBase<L>,
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
        for e in &result.diagnostics { log::error!("{}: {}", tag, e.message); }
        kb.flush_session(&session);
        return Err(());
    }
    let sids = kb.session_sids(&session);
    Ok((tag, sids, session))
}

/// Resolve a `.kif` FILE argument to a loaded tag + its root sids.
fn resolve_file_tag<L: TopLayer>(
    kb:            &KnowledgeBase<L>,
    file:          &Path,
    tag_primary:   &str,
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
    let needle = file.file_name().and_then(|n| n.to_str()).unwrap_or(tag_primary);
    let mut hits: Vec<String> = kb.iter_files()
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
                kb.iter_files().join(", "));
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
    if plain { f.format_plain(indent) } else { f.pretty_print(indent) }
}

/// Render one contradiction's full derivation as a string.
fn render_derivation(
    num:     usize,
    steps:   &[sigmakee_rs_sdk::KifProofStep],
    src_idx: &sigmakee_rs_sdk::AxiomSourceIndex,
    plain:   bool,
) -> String {
    let mut s = format!("Contradiction #{num} ({} steps):\n", steps.len());
    for st in steps {
        let trace = st.source_sid
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
        terminal::{disable_raw_mode, enable_raw_mode, size, Clear, ClearType,
            EnterAlternateScreen, LeaveAlternateScreen},
    };
    use std::io::Write;
    if pages.is_empty() { return Ok(()); }

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
            write!(out, "\x1b[7m #{}/{}  space/n next · p prev · j/k scroll · q quit \x1b[0m",
                idx + 1, pages.len())?;
            out.flush()?;
            match read()? {
                Event::Key(k) => match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('n') | KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Right => {
                        if idx + 1 < pages.len() { idx += 1; top = 0; }
                    }
                    KeyCode::Char('p') | KeyCode::Left => {
                        if idx > 0 { idx -= 1; top = 0; }
                    }
                    KeyCode::Char('j') | KeyCode::Down => { if top < max_top { top += 1; } }
                    KeyCode::Char('k') | KeyCode::Up   => { top = top.saturating_sub(1); }
                    _ => {}
                },
                _ => {}
            }
        }
        Ok(())
    })();
    execute!(out, cursor::Show, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    result
}
