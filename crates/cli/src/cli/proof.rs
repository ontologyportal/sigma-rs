// crates/native/src/cli/proof.rs
//
// Shared `--proof <FORMAT>` rendering.  Used by `sumo ask` (where a
// proof is a refutation of the negated conjecture) and `sumo debug`
// (where a proof is a refutation of the contradictory axiom set).
// The wire format — `result.proof_tptp` + `result.proof_kif` — is the
// same in both cases, so the rendering is the same.
//
// Three branches:
//   - `tptp`                → dump `result.proof_tptp` verbatim
//   - `kif`                 → SUO-KIF pretty-print with per-axiom source
//   - any SUMO language id  → natural-language via format/termFormat
//
// Gated on `feature = "ask"` because both callers are ask-gated and
// `ProverResult` / `AxiomSourceIndex` live behind that feature.

#![cfg(feature = "ask")]

use inline_colorization::*;

use sigmakee_rs_core::{AxiomSourceIndex, KifProofStep, KnowledgeBase, ProverResult};

/// Dispatch the `--proof <FORMAT>` rendering.  Recognised values:
/// - `tptp`                     → dump `result.proof_tptp` verbatim
/// - `kif`                      → SUO-KIF pretty-print
/// - any SUMO language (e.g.
///   `EnglishLanguage`,
///   `ChineseLanguage`)         → natural-language via `format`/`termFormat`
///
/// Unknown values fall through to the language branch, which renders
/// via `termFormat`/`format` and reports any missing specifiers — so a
/// typo like `--proof Englsh` still produces legible output with a
/// clear warning about the unrecognised language.
pub fn print_proof(kb: &KnowledgeBase, result: &ProverResult, format: &str) {
    match format {
        "tptp" => {
            if result.proof_tptp.is_empty() {
                println!(
                    "\n{style_bold}Proof (TPTP):{style_reset} (none — Vampire did not emit a proof section)"
                );
                return;
            }
            println!("\n{style_bold}Proof (TPTP):{style_reset}");
            print!("{}", result.proof_tptp);
            if !result.proof_tptp.ends_with('\n') {
                println!();
            }
        }
        "kif" => {
            if result.proof_kif.is_empty() {
                println!(
                    "\n{style_bold}Proof (SUO-KIF):{style_reset} (none — parser extracted zero steps)"
                );
                return;
            }
            // Build the canonical-fingerprint index once so every
            // axiom-role step gets an O(1) source-file/line lookup.
            // Empty-hash lookups cost nothing when the role is not
            // `axiom`.
            let src_idx = kb.build_axiom_source_index();
            println!("\n{style_bold}Proof (SUO-KIF):{style_reset}");
            for step in &result.proof_kif {
                print_step_header(step);
                println!("        {}", step.formula.pretty_print(2).replace('\n', "\n        "));
                print_step_source(step, &src_idx);
            }
        }
        lang => {
            // Treat any other value as a SUMO language identifier and
            // render each step via format/termFormat.  When a step
            // references a symbol that lacks a spec in the chosen
            // language, fall back to the raw KIF for *that step only*
            // and emit a warning listing the missing specifiers.
            if result.proof_kif.is_empty() {
                println!(
                    "\n{style_bold}Proof ({}):{style_reset} (none — parser extracted zero steps)",
                    lang
                );
                return;
            }
            let src_idx = kb.build_axiom_source_index();
            println!("\n{style_bold}Proof ({}):{style_reset}", lang);
            let mut all_missing: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for step in &result.proof_kif {
                print_step_header(step);
                let report = kb.render_formula_colored(&step.formula, lang);
                if report.missing.is_empty() {
                    println!("        {}", report.rendered);
                } else {
                    // Missing specifiers — fall back to KIF for this
                    // step and annotate what was missing.  Per-step
                    // granularity keeps one bad symbol from nuking the
                    // whole proof's readability.
                    println!(
                        "        {color_bright_yellow}[kif — missing {} spec(s) in {}: {}]{color_reset}",
                        report.missing.len(),
                        lang,
                        report.missing.join(", "),
                    );
                    println!("        {}", step.formula.pretty_print(2).replace('\n', "\n        "));
                    all_missing.extend(report.missing);
                }
                print_step_source(step, &src_idx);
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

/// Print the `  N. [rule]<- [prems]` header line that introduces each
/// proof step.  Kept separate so the three `--proof` branches can all
/// agree on formatting without repeating the logic.
fn print_step_header(step: &KifProofStep) {
    let premises = if step.premises.is_empty() {
        String::new()
    } else {
        format!(
            " <- [{}]",
            step.premises.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
        )
    };
    println!("  {:>3}. [{}]{}", step.index + 1, step.rule, premises);
}

/// When `step` is an axiom-role step (imported verbatim from the KB,
/// rather than a CNF transformation or inference), print the source
/// file and line(s) the axiom came from.
///
/// Two lookup strategies, tried in order:
///
///   1. **Direct sid lookup** — Vampire's `--output_axiom_names on`
///      preserves our `kb_<sid>` names into the proof transcript's
///      source annotation, which the TPTP→KIF converter parses into
///      [`KifProofStep::source_sid`].  When present, [`lookup_by_sid`]
///      gives an O(1) exact hit, immune to CNF transforms and
///      quantifier normalisation.
///   2. **Canonical-hash fallback** — when the sid is absent (older
///      Vampire, embedded-prover backend, anonymous axiom), fall
///      back to [`AxiomSourceIndex::lookup`] via
///      `canonical_sentence_fingerprint`.  Alpha-equivalence-tolerant
///      but sensitive to the exact structural shape produced by the
///      translator.
///
/// Ephemeral internal files (`__query__`, `__sine_query__`, …) are
/// excluded — showing "axiom from the transient query buffer" is
/// just noise.  A single axiom-role step may list multiple sources
/// via the hash path if the same formula is declared in more than
/// one file (rare but SUMO ships some cross-file duplicates in its
/// upper ontology); the sid path always yields a single entry.
///
/// [`lookup_by_sid`]: sigmakee_rs_core::AxiomSourceIndex::lookup_by_sid
fn print_step_source(step: &KifProofStep, idx: &AxiomSourceIndex) {
    // Vampire emits the input axioms with the plain `axiom` role.
    // The negated conjecture (`negated_conjecture`) and every derived
    // step (`plain`, `cnf`, etc.) either have no source or point into
    // the ephemeral query buffer — skip them.
    if step.rule != "axiom" {
        return;
    }

    // Strategy 1 — direct sid lookup.  Fastest and most robust when
    // the transcript preserved the name.  Returns a single source
    // (sids are unique in the KB).
    if let Some(sid) = step.source_sid {
        if let Some(src) = idx.lookup_by_sid(sid) {
            if !src.file.starts_with("__") {
                println!(
                    "        {color_bright_black}↳ {}:{}{color_reset}",
                    src.file, src.line,
                );
                return;
            }
            // Ephemeral tag — deliberately suppress.  Don't fall
            // through to the hash path: the sid is authoritative,
            // so if the source is ephemeral the step genuinely came
            // from the query buffer and there's nothing to show.
            return;
        }
        // Sid didn't resolve (KB mutated between invocation and
        // display?) — fall through to the hash path as a best-effort.
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
    // Dim grey (`color_bright_black` from `inline_colorization`) —
    // prominent enough to scan for, quiet enough to not compete with
    // the formula text above.
    println!(
        "        {color_bright_black}↳ {}{color_reset}",
        joined.join(", ")
    );
}
