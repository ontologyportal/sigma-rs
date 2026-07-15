//! Shared `--proof <FORMAT>` rendering, used by `sumo ask` and `sumo debug`.
//!
//! Five branches:
//!   - `tptp`                → dump `result.proof_tptp` verbatim
//!   - `casc`                → strict SZS-wrapped TPTP, matching Vampire's
//!                             own stdout (`% SZS status`/`% SZS output
//!                             start|end Proof`), no other output
//!   - `graphviz`            → the proof DAG as DOT syntax, no other output
//!   - `kif`                 → SUO-KIF pretty-print with per-axiom source
//!   - any SUMO language id  → natural-language via format/termFormat

#![cfg(feature = "ask")]

use crate::style::*;
use sigmakee_rs_sdk::AstKif;

use graphviz_rust::dot_structures::{
    Attribute, Edge, EdgeTy, Graph, GraphAttributes, Id, Node, NodeId, Stmt, Vertex,
};
use graphviz_rust::printer::{DotPrinter, PrinterContext};

use sigmakee_rs_sdk::{emit_proof, tptp_highlight, AxiomSourceIndex, Emitter, KifProofStep, KnowledgeBase, ProverResult, RenderReport, SzsStatus};
use sigmakee_rs_sdk::AstNode;
use sigmakee_rs_sdk::TopLayer;

/// `true` for the "machine-readable, nothing else on stdout" formats —
/// `casc` (strict SZS/TPTP) and `graphviz` (strict DOT) — both of which
/// `print_proof` fully owns the output for. Callers use this to suppress the
/// verdict banner, `Conjecture:` line, prose paraphrase, and other
/// interactive decoration that would otherwise interleave with it.
pub fn is_quiet_proof_format(format: &str) -> bool {
    matches!(format, "casc" | "graphviz")
}

/// Dispatch the `--proof <FORMAT>` rendering.  Recognised values:
/// - `tptp`                     → dump `result.proof_tptp` verbatim
/// - `casc`                     → strict SZS-wrapped TPTP (CASC submission
///                                 format): `% SZS status <status> for
///                                 <name>` then, if a proof exists, `% SZS
///                                 output start Proof for <name>` / the
///                                 proof / `% SZS output end Proof for
///                                 <name>` — nothing else
/// - `graphviz`                 → the proof DAG as DOT syntax on stdout
///                                 (one node per `proof_kif` step, one edge
///                                 per premise), nothing else — pipe straight
///                                 into `dot`/`neato`/etc.
/// - `kif`                      → SUO-KIF pretty-print
/// - any SUMO language (e.g.
///   `EnglishLanguage`,
///   `ChineseLanguage`)         → natural-language via `format`/`termFormat`
///
/// Unknown values fall through to the language branch, which renders via
/// `termFormat`/`format` and warns about any unrecognised language.
pub fn print_proof<L: TopLayer>(
    kb:     &KnowledgeBase<L>,
    result: &ProverResult,
    format: &str,
    name:   &str,
    status: SzsStatus,
) {
    print_proof_impl(
        &kb.build_axiom_source_index(),
        result,
        format,
        name,
        status,
        &|f, lang| kb.render_formula_colored(f, lang),
    )
}

/// Native-prover twin of [`print_proof`], taking a `KnowledgeBase<ProverLayer>`.
pub fn print_proof_native(
    kb:     &KnowledgeBase<sigmakee_rs_sdk::ProverLayer>,
    result: &ProverResult,
    format: &str,
    name:   &str,
    status: SzsStatus,
) {
    print_proof_impl(
        &kb.build_axiom_source_index(),
        result,
        format,
        name,
        status,
        &|f, lang| kb.render_formula_colored(f, lang),
    )
}

/// Resolve the proof text for the `tptp`/`casc` branches: the verbatim
/// subprocess transcript when one was captured, else a reconstruction from
/// `proof_kif` via the same dialect-emission seam `solve_tptp` uses.  Returns
/// `None` when there is no proof to show (no transcript, no KIF steps).
fn resolve_tptp_proof_text(result: &ProverResult) -> Option<String> {
    if !result.proof_tptp.is_empty() {
        return Some(result.proof_tptp.clone());
    }
    if result.proof_kif.is_empty() {
        return None;
    }
    let emitted = emit_proof(&result.proof_kif, "problem", Emitter::Tptp(result.proof_tptp_lang));
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
    Some(emitted.text)
}

/// Render `result.proof_kif` as a DOT digraph: one node per proof step
/// (labelled `N. [rule]` plus the flattened formula), one edge per premise
/// pointing from the premise step into the step it derives. Always produces
/// a syntactically valid graph — including when there is no proof — so the
/// output is safe to pipe straight into `dot`/`neato`/etc.
fn render_graphviz(result: &ProverResult, name: &str, status: SzsStatus) -> String {
    let mut stmts = vec![
        Stmt::GAttribute(GraphAttributes::Graph(vec![Attribute(
            Id::Plain("label".to_string()),
            dot_escaped(&format!("SZS status {status} for {name}")),
        )])),
        Stmt::GAttribute(GraphAttributes::Node(vec![Attribute(
            Id::Plain("shape".to_string()),
            Id::Plain("box".to_string()),
        )])),
    ];

    for step in &result.proof_kif {
        let node = node_id(step.index);
        let label = format!("{}. [{}]\n{}", step.index + 1, step.rule, step.formula.flat());
        stmts.push(Stmt::Node(Node::new(
            NodeId(Id::Plain(node.clone()), None),
            vec![Attribute(Id::Plain("label".to_string()), dot_escaped(&label))],
        )));
        for &premise in &step.premises {
            stmts.push(Stmt::Edge(Edge {
                ty: EdgeTy::Pair(
                    Vertex::N(NodeId(Id::Plain(node_id(premise)), None)),
                    Vertex::N(NodeId(Id::Plain(node.clone()), None)),
                ),
                attributes: vec![],
            }));
        }
    }

    let graph = Graph::DiGraph { id: Id::Plain("proof".to_string()), strict: false, stmts };
    graph.print(&mut PrinterContext::default())
}

fn node_id(step_index: usize) -> String {
    format!("n{step_index}")
}

/// Quote and escape a string for use as a DOT `Id::Escaped` — `dot_structures`
/// prints an `Escaped` id verbatim, so the surrounding quotes and internal
/// escaping are the caller's responsibility (see `dot_generator`'s `esc`
/// macro, which this mirrors).
fn dot_escaped(s: &str) -> Id {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    Id::Escaped(format!("\"{escaped}\""))
}

fn print_step(text: &str) {
    print!("{}", text);
    if !text.ends_with('\n') {
        println!();
    }
}

fn print_proof_impl(
    src_idx: &AxiomSourceIndex,
    result:  &ProverResult,
    format:  &str,
    name:    &str,
    status:  SzsStatus,
    render:  &dyn Fn(&AstNode, &str) -> RenderReport,
) {
    match format {
        "casc" => {
            println!("% SZS status {} for {}", status, name);
            if let Some(text) = resolve_tptp_proof_text(result) {
                println!("% SZS output start Proof for {}", name);
                print_step(&text);
                println!("% SZS output end Proof for {}", name);
            }
        }
        "graphviz" => {
            print_step(&render_graphviz(result, name, status));
        }
        "tptp" => {
            match resolve_tptp_proof_text(result) {
                Some(text) => {
                    println!("\n{style_bold}Proof (TPTP):{style_reset}");
                    // Syntax-highlight for terminal readability; `casc` stays
                    // plain (it's meant to be pasted verbatim into a CASC
                    // submission / SZS transcript, not viewed in a terminal).
                    let shown = if crate::style::is_ugly() { text } else { tptp_highlight(&text) };
                    print_step(&shown);
                }
                None => println!(
                    "\n{style_bold}Proof (TPTP):{style_reset} (none — Vampire did not emit a proof section)"
                ),
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
                // Same gate as the TPTP arm above: `--ugly` output must be
                // plain re-parseable text, not terminal syntax highlighting.
                let text = if crate::style::is_ugly() {
                    step.formula.format_plain(2)
                } else {
                    step.formula.pretty_print(2)
                };
                println!("        {}", text.replace('\n', "\n        "));
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
