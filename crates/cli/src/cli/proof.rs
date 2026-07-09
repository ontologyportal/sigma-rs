//! Shared `--proof <FORMAT>` rendering, used by `sumo ask` and `sumo debug`.
//!
//! Three branches:
//!   - `tptp`                → dump `result.proof_tptp` verbatim
//!   - `kif`                 → SUO-KIF pretty-print with per-axiom source
//!   - any SUMO language id  → natural-language via format/termFormat

#![cfg(feature = "ask")]

use crate::style::*;
use sigmakee_rs_sdk::AstKif;

use sigmakee_rs_sdk::{emit_proof, AxiomSourceIndex, Emitter, KifProofStep, KnowledgeBase, ProverResult, RenderReport};
use sigmakee_rs_sdk::AstNode;
use sigmakee_rs_sdk::TopLayer;

/// Dispatch the `--proof <FORMAT>` rendering.  Recognised values:
/// - `tptp`                     → dump `result.proof_tptp` verbatim
/// - `kif`                      → SUO-KIF pretty-print
/// - any SUMO language (e.g.
///   `EnglishLanguage`,
///   `ChineseLanguage`)         → natural-language via `format`/`termFormat`
///
/// Unknown values fall through to the language branch, which renders via
/// `termFormat`/`format` and warns about any unrecognised language.
pub fn print_proof<L: TopLayer>(kb: &KnowledgeBase<L>, result: &ProverResult, format: &str) {
    print_proof_impl(
        &kb.build_axiom_source_index(),
        result,
        format,
        &|f, lang| kb.render_formula_colored(f, lang),
    )
}

/// Native-prover twin of [`print_proof`], taking a `KnowledgeBase<ProverLayer>`.
pub fn print_proof_native(
    kb:     &KnowledgeBase<sigmakee_rs_sdk::ProverLayer>,
    result: &ProverResult,
    format: &str,
) {
    print_proof_impl(
        &kb.build_axiom_source_index(),
        result,
        format,
        &|f, lang| kb.render_formula_colored(f, lang),
    )
}

fn print_proof_impl(
    src_idx: &AxiomSourceIndex,
    result:  &ProverResult,
    format:  &str,
    render:  &dyn Fn(&AstNode, &str) -> RenderReport,
) {
    match format {
        "tptp" => {
            if !result.proof_tptp.is_empty() {
                println!("\n{style_bold}Proof (TPTP):{style_reset}");
                print!("{}", result.proof_tptp);
                if !result.proof_tptp.ends_with('\n') {
                    println!();
                }
                return;
            }
            // Subprocess backends stash Vampire/E's verbatim transcript in
            // `proof_tptp`; the native `ProverLayer` and embedded FFI Vampire
            // backend have no such transcript but still carry a parsed
            // `proof_kif` — reconstruct TPTP text from it via the same
            // dialect-emission seam `solve_tptp` uses.
            if result.proof_kif.is_empty() {
                println!(
                    "\n{style_bold}Proof (TPTP):{style_reset} (none — Vampire did not emit a proof section)"
                );
                return;
            }
            let emitted = emit_proof(&result.proof_kif, "problem", Emitter::Tptp(result.proof_tptp_lang));
            println!("\n{style_bold}Proof (TPTP):{style_reset}");
            print!("{}", emitted.text);
            if !emitted.text.ends_with('\n') {
                println!();
            }
            if !emitted.is_complete() {
                eprintln!(
                    "{color_bright_yellow}warning:{color_reset} {} proof step(s) could not be represented in TPTP:",
                    emitted.dropped.len(),
                );
                for d in &emitted.dropped {
                    eprintln!(
                        "  - {}: {}",
                        d.name.as_deref().unwrap_or("<unnamed>"),
                        d.reason,
                    );
                }
            }
        }
        "kif" => {
            if result.proof_kif.is_empty() {
                println!(
                    "\n{style_bold}Proof (SUO-KIF):{style_reset} (none — parser extracted zero steps)"
                );
                return;
            }
            println!("\n{style_bold}Proof (SUO-KIF):{style_reset}");
            for step in &result.proof_kif {
                print_step_header(step);
                println!("        {}", step.formula.pretty_print(2).replace('\n', "\n        "));
                print_step_source(step, src_idx);
            }
        }
        lang => {
            if result.proof_kif.is_empty() {
                println!(
                    "\n{style_bold}Proof ({}):{style_reset} (none — parser extracted zero steps)",
                    lang
                );
                return;
            }
            println!("\n{style_bold}Proof ({}):{style_reset}", lang);
            let mut all_missing: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for step in &result.proof_kif {
                print_step_header(step);
                let report = render(&step.formula, lang);
                println!("        {}", report.rendered);
                if !report.missing.is_empty() {
                    println!(
                        "        {color_bright_black}({} term(s) shown by name — no {} entry: {}){color_reset}",
                        report.missing.len(),
                        lang,
                        report.missing.join(", "),
                    );
                    all_missing.extend(report.missing);
                }
                print_step_source(step, src_idx);
            }
            if !all_missing.is_empty() {
                eprintln!(
                    "{color_bright_yellow}warning:{color_reset} {} symbol(s) had no `format`/`termFormat` entry in `{}`:",
                    all_missing.len(), lang,
                );
                for m in &all_missing {
                    eprintln!("  - {}", m);
                }
            }
        }
    }
}

/// Print the `  N. [rule] <- [prems]` header line that introduces each
/// proof step.
fn print_step_header(step: &KifProofStep) {
    let premises = if step.premises.is_empty() {
        String::new()
    } else {
        format!(
            " <- [{}]",
            step.premises.iter().map(|p| (p + 1).to_string()).collect::<Vec<_>>().join(", ")
        )
    };
    println!("  {:>3}. [{}]{}", step.index + 1, step.rule, premises);
}

/// When `step` is an axiom-role step, print the source file and line(s) the
/// axiom came from.
///
/// Two lookup strategies, tried in order:
///
///   1. **Direct sid lookup** — when [`KifProofStep::source_sid`] is present,
///      [`lookup_by_sid`] gives an exact hit.
///   2. **Canonical-hash fallback** — when the sid is absent, fall back to
///      [`AxiomSourceIndex::lookup`] via `canonical_sentence_fingerprint`.
///
/// Ephemeral internal files (`__query__`, `__sine_query__`, …) are excluded.
/// The hash path may list multiple sources when the same formula is declared
/// in more than one file; the sid path always yields a single entry.
///
/// [`lookup_by_sid`]: sigmakee_rs_sdk::AxiomSourceIndex::lookup_by_sid
fn print_step_source(step: &KifProofStep, idx: &AxiomSourceIndex) {
    if step.rule != "axiom" {
        return;
    }

    // Strategy 1 — direct sid lookup.
    if let Some(sid) = step.source_sid {
        if let Some(src) = idx.lookup_by_sid(sid) {
            if !src.file.starts_with("__") {
                println!(
                    "        {color_bright_black}↳ {}:{}{color_reset}",
                    src.file, src.line,
                );
                return;
            }
            // Ephemeral source: the sid is authoritative, so suppress
            // rather than falling through to the hash path.
            return;
        }
    }

    // Strategy 2 — canonical-hash fallback.
    let sources = idx.lookup(&step.formula);
    let visible: Vec<_> = sources
        .iter()
        .filter(|s| !s.file.starts_with("__"))
        .collect();
    if visible.is_empty() {
        return;
    }
    let joined: Vec<String> = visible
        .iter()
        .map(|s| format!("{}:{}", s.file, s.line))
        .collect();
    println!(
        "        {color_bright_black}↳ {}{color_reset}",
        joined.join(", ")
    );
}
