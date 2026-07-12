//! Plain-text (no ANSI) renderers turning core/SDK result types into the
//! semi-human-readable strings returned as MCP tool output. An LLM client
//! reads these directly, so every renderer favors explicit labels and
//! `file:line`-style traceability over compact machine encodings.

use std::collections::HashSet;
use std::fmt::Write as _;

use sigmakee_rs_core::RenderReport;
use sigmakee_rs_sdk::{
    AstKif, AstNode, Diagnostic, DocSpan, KifProofStep, KnowledgeBase, ManPageView, ProverLayer,
    ProverResult, ProverStatus, SdkError, SearchHit, Severity, parse_doc_spans,
};

/// `KnowledgeBase::render_diagnostic` is written for a terminal (it embeds
/// `inline_colorization` ANSI escapes unconditionally); strip them here so
/// tool output is plain text an LLM client reads cleanly.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // ESC '[' ... final-byte (0x40..=0x7E) — a CSI sequence.
            let mut lookahead = chars.clone();
            if lookahead.next() == Some('[') {
                for c2 in lookahead.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c2) {
                        break;
                    }
                }
                chars = lookahead;
                continue;
            }
        }
        out.push(c);
    }
    out
}

fn render_one_diagnostic(kb: &KnowledgeBase<ProverLayer>, d: &Diagnostic) -> String {
    strip_ansi(&kb.render_diagnostic(d))
}

/// Render a diagnostics batch: a one-line summary followed by every
/// distinct finding (errors first, then warnings), deduplicated by
/// rendered text the way `sumo validate` does.
pub fn render_diagnostics(kb: &KnowledgeBase<ProverLayer>, diags: &[Diagnostic]) -> String {
    if diags.is_empty() {
        return "clean: no errors or warnings".to_string();
    }
    let (errors, warnings): (Vec<_>, Vec<_>) =
        diags.iter().partition(|d| matches!(d.severity, Severity::Error));

    let mut seen: HashSet<String> = HashSet::new();
    let mut out = String::new();
    let mut n_err = 0usize;
    let mut n_warn = 0usize;

    for d in &errors {
        let rendered = render_one_diagnostic(kb, d);
        if seen.insert(rendered.clone()) {
            n_err += 1;
            let _ = writeln!(out, "error[{}]: {}", d.code, rendered);
        }
    }
    for d in &warnings {
        let rendered = render_one_diagnostic(kb, d);
        if seen.insert(rendered.clone()) {
            n_warn += 1;
            let _ = writeln!(out, "warning[{}]: {}", d.code, rendered);
        }
    }

    format!(
        "{}\n{}",
        count_phrase(n_err, "error") + ", " + &count_phrase(n_warn, "warning"),
        out.trim_end()
    )
}

/// Render the result of `Session::ingest` — a flat `Vec<SdkError>` mixing
/// KB diagnostics (`SdkError::Kb`) with infrastructural failures (bad path,
/// I/O). Same shape as [`render_diagnostics`] but tolerant of the
/// non-`Diagnostic` variants.
pub fn render_sdk_errors(kb: &KnowledgeBase<ProverLayer>, errs: &[SdkError]) -> String {
    if errs.is_empty() {
        return "clean: ingested with no errors or warnings".to_string();
    }
    let mut out = String::new();
    let mut n_err = 0usize;
    let mut n_warn = 0usize;
    for e in errs {
        match e {
            SdkError::Kb(d) => {
                if d.is_err() {
                    n_err += 1;
                    let _ = writeln!(out, "error[{}]: {}", d.code, render_one_diagnostic(kb, d));
                } else {
                    n_warn += 1;
                    let _ = writeln!(out, "warning[{}]: {}", d.code, render_one_diagnostic(kb, d));
                }
            }
            other => {
                n_err += 1;
                let _ = writeln!(out, "error: {other}");
            }
        }
    }
    format!(
        "{}\n{}",
        count_phrase(n_err, "error") + ", " + &count_phrase(n_warn, "warning"),
        out.trim_end()
    )
}

fn count_phrase(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

fn status_word(status: &ProverStatus) -> &'static str {
    match status {
        ProverStatus::Proved => "Proved",
        ProverStatus::Disproved => "Disproved",
        ProverStatus::Consistent => "Consistent",
        ProverStatus::Inconsistent => "Inconsistent",
        ProverStatus::Timeout => "Timeout",
        ProverStatus::InputError => "InputError",
        ProverStatus::Unknown => "Unknown",
    }
}

/// Why there's no proof transcript to show, given the verdict — mirrors the
/// CLI's `sumo ask` explanatory note so the LLM doesn't misread an empty
/// `proof_kif` as "the prover found nothing to say".
fn no_proof_note(status: &ProverStatus, proof_recorded: bool) -> &'static str {
    match status {
        ProverStatus::Proved | ProverStatus::Inconsistent if !proof_recorded =>
            "(proof not recorded — this call did not set want_proof)",
        ProverStatus::Proved | ProverStatus::Inconsistent =>
            "(proof found, but the prover returned no renderable transcript)",
        ProverStatus::Disproved | ProverStatus::Consistent =>
            "(no proof exists: the prover saturated without finding a refutation — a completeness certificate)",
        ProverStatus::Timeout => "(no proof: the prover timed out before finding a refutation)",
        ProverStatus::InputError => "(no proof: the prover rejected the input before running)",
        ProverStatus::Unknown => "(no proof: the prover found no refutation within its step budget)",
    }
}

fn render_step_list(steps: &[KifProofStep]) -> String {
    let mut out = String::new();
    for s in steps {
        let premises = if s.premises.is_empty() {
            String::new()
        } else {
            format!(
                " <- [{}]",
                s.premises.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
            )
        };
        let _ = writeln!(out, "  {:>3}. [{:<20}] {}{}", s.index, s.rule, s.formula.flat(), premises);
    }
    out
}

/// Render one `ask` / `check_consistency` verdict: status, bindings, a
/// step-wise KIF proof transcript (when present), and — when `conjecture`
/// and `want_prose` are given — an additional English paragraph via the
/// KB's discourse-level prose renderer.
pub fn render_prover_result(
    kb: &KnowledgeBase<ProverLayer>,
    conjecture: Option<&AstNode>,
    result: &ProverResult,
    proof_recorded: bool,
    want_prose: bool,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Result: {}", status_word(&result.status));

    if !result.bindings.is_empty() {
        let _ = writeln!(out, "Bindings:");
        for b in &result.bindings {
            let _ = writeln!(out, "  {b}");
        }
    }

    if result.proof_kif.is_empty() {
        let _ = writeln!(out, "{}", no_proof_note(&result.status, proof_recorded));
    } else {
        let _ = writeln!(out, "\nProof steps ({}):", result.proof_kif.len());
        out.push_str(&render_step_list(&result.proof_kif));

        if want_prose {
            let report: RenderReport =
                kb.render_proof_prose(conjecture, &result.proof_kif, "EnglishLanguage");
            let _ = writeln!(out, "\nProof (prose):\n{}", report.rendered.trim_end());
            if !report.missing.is_empty() {
                let _ = writeln!(
                    out,
                    "\n(no format/termFormat in EnglishLanguage for: {})",
                    report.missing.join(", ")
                );
            }
        }
    }

    if !result.contradiction_proofs.is_empty() {
        let _ = writeln!(
            out,
            "\n{} input contradiction(s) — the axioms/hypotheses are mutually inconsistent:",
            result.contradiction_proofs.len()
        );
        for (n, steps) in result.contradiction_proofs.iter().enumerate() {
            let _ = writeln!(out, "\nContradiction #{} ({} steps):", n + 1, steps.len());
            out.push_str(&render_step_list(steps));
        }
    }

    // A tail of the raw transcript is useful for the inconclusive verdicts,
    // where there's no structured proof to point at.
    if matches!(
        result.status,
        ProverStatus::Timeout | ProverStatus::Unknown | ProverStatus::InputError
    ) && !result.raw_output.is_empty()
    {
        let raw = &result.raw_output;
        let tail_start = raw.len().saturating_sub(2000);
        // Don't split a UTF-8 char boundary.
        let tail_start = (tail_start..raw.len()).find(|&i| raw.is_char_boundary(i)).unwrap_or(0);
        let _ = writeln!(out, "\nRaw prover output (tail):\n{}", &raw[tail_start..]);
    }

    out
}

fn render_doc_spans(spans: &[DocSpan]) -> String {
    let mut out = String::new();
    for span in spans {
        match span {
            DocSpan::Text(t) => out.push_str(t),
            DocSpan::Link { text, .. } => out.push_str(text),
        }
    }
    out
}

/// Render a structured man page: kinds, taxonomic parents, signature,
/// documentation/termFormat/format, and where the symbol is used.
pub fn render_manpage(view: &ManPageView) -> String {
    let mut out = String::new();
    let kinds = view.kinds.iter().map(|k| format!("{k:?}")).collect::<Vec<_>>().join(", ");
    let _ = writeln!(out, "{} [{}]", view.name, kinds);

    if !view.parents.is_empty() {
        let _ = writeln!(out, "\nParents:");
        for p in &view.parents {
            let _ = writeln!(out, "  ({} {} {})", p.relation, view.name, p.parent);
        }
    }

    let sig = &view.signature;
    if sig.arity.is_some() || !sig.domains.is_empty() || sig.range.is_some() {
        let _ = writeln!(out, "\nSignature:");
        if let Some(a) = sig.arity {
            let _ = writeln!(out, "  arity: {a}");
        }
        for (pos, dom) in &sig.domains {
            let marker = if dom.subclass { " (subclass)" } else { "" };
            let _ = writeln!(out, "  arg {pos}: {}{}", dom.class, marker);
        }
        if let Some(r) = &sig.range {
            let marker = if r.subclass { " (subclass)" } else { "" };
            let _ = writeln!(out, "  range: {}{}", r.class, marker);
        }
    }

    for (label, blocks) in [
        ("Documentation", &view.documentation),
        ("Term format", &view.term_format),
        ("Format", &view.format),
    ] {
        if blocks.is_empty() {
            continue;
        }
        let _ = writeln!(out, "\n{label}:");
        for b in blocks {
            let _ = writeln!(out, "  [{}] {}", b.language, render_doc_spans(&b.spans));
        }
    }

    let head_refs = view.references.by_position.first().map(|v| v.len()).unwrap_or(0);
    let arg_refs: usize = view.references.by_position.iter().skip(1).map(|v| v.len()).sum();
    let nested_refs = view.references.nested.len();
    let _ = writeln!(
        out,
        "\nReferences: {head_refs} as head, {arg_refs} as argument, {nested_refs} nested"
    );

    out
}

/// Render a batch of substring-search hits.
pub fn render_search_hits(hits: &[SearchHit]) -> String {
    if hits.is_empty() {
        return "no matches".to_string();
    }
    let mut out = String::new();
    let _ = writeln!(out, "{} match(es):", hits.len());
    for h in hits {
        let kinds = h.kinds.iter().map(|k| format!("{k:?}")).collect::<Vec<_>>().join(",");
        // Strip `&%Symbol` cross-reference markers the same way `man` does,
        // so a search hit reads as plain prose.
        let text = render_doc_spans(&parse_doc_spans(&h.text));
        let snippet: String = text.chars().take(200).collect();
        let ellipsis = if text.chars().count() > 200 { "…" } else { "" };
        let _ = writeln!(
            out,
            "  {} [{}] ({}, {}): {}{}",
            h.symbol,
            kinds,
            h.source.as_str(),
            h.language,
            snippet,
            ellipsis
        );
    }
    out
}
