use std::path::PathBuf;

use sigmakee_rs_sdk::{szs_status, AstKif, ProverStatus, ProvingLayer};
use sigmakee_rs_sdk::Session;
use sigmakee_rs_sdk::manager::{KBManager, ProverOptsFor};
use crate::style::*;
use crate::cli::proof::{is_quiet_proof_format, print_proof};
use crate::cli::util::read_stdin;

pub fn run_ask<L>(
    mut session: Session<L>,
    manager:     &KBManager,
    formula:     Option<String>,
    tell:        Vec<String>,
    _keep:       Option<PathBuf>,
) -> bool
where
    // `ProvingLayer: TopLayer: Layer`, so the KB renderers (which want
    // `TopLayer + Layer`) are all reachable.  `L::Opts: ProverOptsFor` lets us
    // derive the prover opts from the configured manager.
    L: ProvingLayer,
    L::Opts: ProverOptsFor,
{
    log::debug!("run_ask: formula={:?}, tell={}", formula.is_some(), tell.len());

    // Fail fast on a missing conjecture.  `read_stdin` checks `is_terminal()`
    // and returns None on a TTY, so this is safe when run interactively.
    let conjecture = match formula.or_else(read_stdin) {
        Some(f) => f,
        None => {
            log::error!("ask requires a conjecture formula (supply as argument or via stdin)");
            return false;
        }
    };

    // The progress sink was already installed by `dispatch`.  Derive the prover
    // opts (selection / timeout / proof dialect) from the configured manager.
    let opts = <L::Opts as ProverOptsFor>::from_manager(manager);
    // Consulted below when `proof_kif` comes back empty: did we even ask the
    // prover to record a proof?  (Native: only with `--want-proof true`.)
    let proof_recorded = opts.records_proof();
    let open = tell.iter().try_fold(session.open_session(), |s, t| s.tell(t));
    let open = match open {
        Ok(o)  => o,
        Err(errs) => { 
            log::error!("tell errors:"); 
            for e in errs {
                log::error!("{}", e);
            }
            return false;
        }
    };

    let result = match open.ask(&conjecture, Some(opts)) {
        Ok(r) => r,
        Err(errs) => {
            log::error!("Error asking KB conjecture:");
            for e in errs {
                log::error!("{}", e);
            }
            return false;
        }
    };
    // Fold the native prover's per-mechanism saturation timers (only
    // populated under `--profile`) into the same aggregator the coarser
    // `ask.*` phases report through, so `--profile`'s output covers what's
    // inside `ask.saturate`, not just its total.
    if let Some(sink) = crate::progress::global() {
        crate::progress::record_mechanism_profile(&sink, &result.phase_profile);
    }

    // --proof: shared three-way rendering (`kif` pretty-print / `tptp` dump /
    // any SUMO language via format+termFormat).  For the native backend `tptp`
    // is auto-stubbed: `print_proof` prints "(none)" when `proof_tptp` is empty.
    let format = manager.proof.as_str();

    // Always surface the verdict, regardless of `-v` — friendlier for scripting
    // against stdout and for interactive users.  Suppressed under `casc`/
    // `graphviz`: that output must be pure SZS/TPTP or DOT text, and
    // `print_proof`'s own branch for each already reports the SZS status.
    if !is_quiet_proof_format(format) {
        let (verdict, colour) = match result.status {
            ProverStatus::Proved       => ("Proved",       color_bright_green),
            ProverStatus::Disproved    => ("Disproved",    color_bright_yellow),
            ProverStatus::Consistent   => ("Consistent",   color_bright_green),
            ProverStatus::Inconsistent => ("Inconsistent", color_bright_red),
            ProverStatus::Timeout      => ("Timeout",      color_bright_yellow),
            ProverStatus::InputError   => ("Input Error",  color_bright_red),
            ProverStatus::Unknown      => ("Unknown",      color_bright_red),
        };
        println!("{style_bold}Result:{style_reset} {colour}{}{color_reset}", verdict);

        if !result.bindings.is_empty() {
            for b in &result.bindings {
                println!("  {style_bold}{}{style_reset}", b);
            }
        }
    }

    if format != "none" {
        if format != "tptp" && format != "casc" && format != "graphviz" && result.proof_kif.is_empty() {
            // Say WHY there are no steps to render, per verdict.  Proved and
            // Inconsistent mean a refutation WAS found — a proof exists, we
            // just don't have a transcript.  Disproved and Consistent are
            // saturation certificates — no refutation exists, so there is
            // genuinely nothing to render.
            let note = match result.status {
                ProverStatus::Proved | ProverStatus::Inconsistent if !proof_recorded =>
                    "(proof not recorded — rerun with --want-proof true to render it)",
                ProverStatus::Proved | ProverStatus::Inconsistent =>
                    "(proof found, but the prover returned no renderable transcript)",
                ProverStatus::Disproved | ProverStatus::Consistent =>
                    "(no proof exists: the prover saturated without finding a refutation)",
                ProverStatus::Timeout =>
                    "(no proof: the prover timed out before finding a refutation)",
                ProverStatus::InputError =>
                    "(no proof: the prover rejected the input before running)",
                ProverStatus::Unknown =>
                    "(no proof: the prover found no refutation)",
            };
            println!("{}", note);
        } else {
            if !is_quiet_proof_format(format) {
                println!("\n{style_bold}Conjecture:{style_reset} {}", conjecture.trim());
            }
            let status = szs_status(&result, true);
            print_proof(session.kb(), &result, format, "problem", status);
        }
    }

    // --prose: ADDITIVE paragraph rendering (the step view above is the
    // transformation source, not replaced).  Language follows --proof when it
    // names a SUMO language; EnglishLanguage otherwise.  Suppressed under
    // `casc`/`graphviz` along with everything else non-machine-readable.
    if manager.prose && !is_quiet_proof_format(format) && !result.proof_kif.is_empty() {
        let lang = match format {
            "kif" | "tptp" | "none" => "EnglishLanguage",
            other                   => other,
        };
        let goal_doc = sigmakee_rs_sdk::parse_document(
            "__prose_goal__", conjecture.to_string(), sigmakee_rs_sdk::Parser::Kif);
        let goal_ast = goal_doc.ast.iter().find_map(|d| d.as_stmt());
        let report = session.kb().render_proof_prose(goal_ast, &result.proof_kif, lang);
        println!("\n{style_bold}Proof (prose, {}):{style_reset}\n", lang);
        println!("{}", report.rendered);
        if !report.missing.is_empty() {
            eprintln!(
                "{color_bright_yellow}warning:{color_reset} {} symbol(s) shown by bare name (no format/termFormat in {}): {}",
                report.missing.len(), lang, report.missing.join(", "));
        }
    }

    if !result.contradiction_proofs.is_empty() {
        log::warn!(
            "{} input contradiction(s) detected — the axioms/hypotheses are \
             mutually inconsistent (rerun with --proof kif to see the derivations)",
            result.contradiction_proofs.len());
        if format != "none" && !is_quiet_proof_format(format) {
            let src_idx = session.kb().build_axiom_source_index();
            for (n, steps) in result.contradiction_proofs.iter().enumerate() {
                println!("\n{style_bold}Input contradiction #{} ({} steps):{style_reset}",
                    n + 1, steps.len());
                for s in steps {
                    let trace = s.source_sid
                        .and_then(|sid| src_idx.lookup_by_sid(sid))
                        .map(|a| format!("   {color_bright_black}[{}:{}]{color_reset}", a.file, a.line))
                        .unwrap_or_default();
                    println!("  {:>3}. [{:<18}] {}{}", s.index, s.rule, s.formula.flat(), trace);
                }
            }
        }
    }

    // Promote the raw prover transcript to `debug`.
    log::debug!(
        "{style_bold}Theorem prover output:{style_reset}\n{}",
        result.raw_output
    );
    // (Per-phase profiling now lives in the consolidated global sink, reported
    // by `main` — no per-command profile block here.)

    matches!(result.status, ProverStatus::Proved)
}
