// crates/core/src/parse/szs/parser.rs
//
// SZS (prover-output transcript) parser. A captured TSTP-compliant prover
// run — status line, an optional `SZS output start/end` proof block, and
// (ignored) banner noise outside it — parsed like the `tq` submodule parses
// `.kif.tq`: the surrounding scaffolding rides as `DocItem::Meta` (mirroring
// how `tq` handles `(time …)`/`(note …)` directives), and the proof-step
// lines **reuse the TPTP grammar** (`crate::parse::tptp`) for their
// structure — each step is an ordinary annotated formula, so its `role` and
// `source` (parent steps / originating axiom) come for free from the TPTP
// parser's own `Role`/`Source` support, no bespoke text scraping needed.
//
// What this module does NOT do: recognize Vampire's *default* human-readable
// proof format (`5413. FORMULA [annotation]`, emitted when `-p tptp` is not
// passed to Vampire) — that is not TPTP syntax at all (no `fof(...)` framing,
// no statement-terminating `.`), so it cannot be parsed by reusing the TPTP
// grammar the way this module is designed to. A transcript in that format
// will tokenize its `% SZS …` lines fine but yield parse errors for every
// proof-step line — see this module's tests for exactly what that looks like.

use crate::parse::ast::AstNode;
use crate::parse::doc::{DocItem, MetaNode};
use crate::parse::error::ParseError;
use crate::parse::span::Span;
use crate::parse::TptpParseOptions;
use crate::parse::{tptp, ParseResult};

/// Parse a captured prover transcript (`raw`, from a subprocess or a WASM
/// build's captured stdout+stderr) into a document: `% SZS …` scaffolding as
/// `DocItem::Meta`, each proof step (inside `SZS output start`/`end`, if
/// present) as `DocItem::Stmt(AstNode::Annotated { role, source, .. })`.
///
/// `file` is the virtual filename attached to spans/errors (there is no real
/// file — this is prover stdout — so callers typically pass something like
/// `"vampire"`).
///
/// Recognized `Meta` keys (one `MetaNode` per line found, in document order):
///   * `szs_status`       — args: `[Symbol(<word>)]`, e.g. `Theorem`,
///     `ContradictoryAxioms`, `CounterSatisfiable`. From `% SZS status …`.
///   * `szs_output_start` / `szs_output_end` — args: `[Symbol(<label>)]`,
///     e.g. `Proof`, `Saturation`, `Refutation`. From `% SZS output
///     start/end …`.
///
/// A transcript with no `SZS output start`/`end` block (e.g. a bare
/// `Timeout`/`GaveUp` status with no proof) yields only `Meta` items and no
/// parse errors — there is nothing to hand to the formula parser.
pub fn parse_szs(raw: &str, file: &str) -> ParseResult<DocItem> {
    let mut doc: Vec<DocItem> = Vec::new();
    for (text, span) in scan_lines(raw, file) {
        if let Some(meta) = recognize_szs_pragma(text, span) {
            doc.push(DocItem::Meta(meta));
        }
    }

    let body = match proof_block(raw) {
        Some(b) => b,
        None => return (doc, Vec::new()),
    };

    // `keep_conjectures`: a proof-step transcript's roles are a closed,
    // known set (axiom/plain/negated_conjecture/conjecture — verified
    // against real Vampire/E fixtures elsewhere in this crate), so unlike
    // ordinary KB-file loading there is no reason to drop a bare
    // `conjecture`-role step.
    let opts = TptpParseOptions {
        keep_conjectures: true,
        ..TptpParseOptions::default()
    };
    let (tokens, tok_errors, extra_metas) = tptp::tokenize_with_meta(body, file);
    let (mut stmts, parse_errors) = tptp::parse(tokens, file, Some(opts.clone()));

    let literal_decode_parser = crate::parse::Parser::Tptp {
        options: Some(opts),
    };
    for stmt in &mut stmts {
        crate::parse::macros::decode_tptp_literals(stmt, &literal_decode_parser);
    }

    doc.extend(extra_metas.into_iter().map(DocItem::Meta));
    doc.extend(stmts.into_iter().map(DocItem::Stmt));

    let mut errors: Vec<(Span, Box<dyn ParseError>)> = tok_errors
        .into_iter()
        .map(|e| (e.get_span(), Box::new(e) as Box<dyn ParseError>))
        .collect();
    errors.extend(
        parse_errors
            .into_iter()
            .map(|e| (e.get_span(), Box::new(e) as Box<dyn ParseError>)),
    );

    (doc, errors)
}

/// The text strictly between the `SZS output start …` line and the `SZS
/// output end …` line, or `None` if either marker is absent (or out of
/// order) — e.g. a `Timeout`/`GaveUp` transcript has a status but no proof.
/// Banter outside this span (Vampire's version banner, termination-reason
/// tail, …) is not valid TPTP and is never handed to the formula parser.
fn proof_block(raw: &str) -> Option<&str> {
    let start = raw.find("SZS output start")?;
    let end = raw.find("SZS output end")?;
    if end <= start {
        return None;
    }
    let body_start = raw[start..]
        .find('\n')
        .map(|nl| start + nl + 1)
        .unwrap_or(start);
    Some(&raw[body_start..end])
}

/// Split `raw` into `(line text, span at the line's start)` pairs. Line
/// endings are stripped from the text; the span is a zero-width point at
/// column 1 (matching the existing TPTP tokenizer's own header-pragma spans
/// — see `record_status_pragma` — which are informational, not used for
/// byte-precise highlighting).
fn scan_lines<'a>(raw: &'a str, file: &str) -> Vec<(&'a str, Span)> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    let mut line_no = 1u32;
    for line in raw.split_inclusive('\n') {
        let text = line.trim_end_matches(['\n', '\r']);
        out.push((text, Span::point(file.to_string(), line_no, 1, offset)));
        offset += line.len();
        line_no += 1;
    }
    out
}

/// Recognize one `% SZS …` line, if `text` is one — `None` for an ordinary
/// (non-SZS) comment or code line.
fn recognize_szs_pragma(text: &str, span: Span) -> Option<MetaNode> {
    let rest = text.trim_start().strip_prefix('%')?.trim_start();
    let rest = rest.strip_prefix("SZS")?.trim_start();

    if let Some(rest) = rest.strip_prefix("status") {
        let word = rest.split_whitespace().next()?;
        return Some(meta("szs_status", word, span));
    }
    if let Some(rest) = rest.strip_prefix("output start") {
        let label = rest.split_whitespace().next().unwrap_or("");
        return Some(meta("szs_output_start", label, span));
    }
    if let Some(rest) = rest.strip_prefix("output end") {
        let label = rest.split_whitespace().next().unwrap_or("");
        return Some(meta("szs_output_end", label, span));
    }
    None
}

fn meta(key: &str, word: &str, span: Span) -> MetaNode {
    MetaNode {
        key: key.to_string(),
        args: vec![AstNode::Symbol {
            name: word.to_string(),
            span: span.clone(),
        }],
        span,
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::ast::{Role, Source};

    fn metas<'a>(doc: &'a [DocItem], key: &str) -> Vec<&'a MetaNode> {
        doc.iter()
            .filter_map(DocItem::as_meta)
            .filter(|m| m.key == key)
            .collect()
    }

    fn stmts(doc: &[DocItem]) -> Vec<&AstNode> {
        doc.iter().filter_map(DocItem::as_stmt).collect()
    }

    fn symbol_arg(m: &MetaNode) -> &str {
        match &m.args[0] {
            AstNode::Symbol { name, .. } => name,
            other => panic!("expected a Symbol arg, got {:?}", other),
        }
    }

    #[test]
    fn status_only_transcript_yields_no_stmts_and_no_errors() {
        // Timeout/GaveUp: a status line, no proof block.
        let raw = "% SZS status Timeout for input\n";
        let (doc, errors) = parse_szs(raw, "vampire");
        assert!(errors.is_empty());
        assert!(stmts(&doc).is_empty());
        let status = metas(&doc, "szs_status");
        assert_eq!(status.len(), 1);
        assert_eq!(symbol_arg(status[0]), "Timeout");
    }

    #[test]
    fn tptp_shaped_proof_block_parses_into_annotated_stmts() {
        let raw = "\
% SZS status ContradictoryAxioms for input
% SZS output start Proof for input
fof(f1,axiom,(p(a)),file('/dev/stdin',kb_42)).
fof(f2,axiom,(~p(a))).
fof(f3,plain,($false),inference(resolution,[status(thm)],[f1,f2])).
% SZS output end Proof for input
% ------------------------------
% Termination reason: Refutation
";
        let (doc, errors) = parse_szs(raw, "vampire");
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);

        let status = metas(&doc, "szs_status");
        assert_eq!(symbol_arg(status[0]), "ContradictoryAxioms");
        let start = metas(&doc, "szs_output_start");
        assert_eq!(symbol_arg(start[0]), "Proof");
        let end = metas(&doc, "szs_output_end");
        assert_eq!(symbol_arg(end[0]), "Proof");

        let steps = stmts(&doc);
        assert_eq!(steps.len(), 3);
        match steps[0] {
            AstNode::Annotated {
                role: Role::Axiom,
                name: Some(n),
                source: Some(Source::Input { file, name }),
                ..
            } => {
                assert_eq!(n, "f1");
                assert_eq!(file, "/dev/stdin");
                assert_eq!(name.as_deref(), Some("kb_42"));
            }
            other => panic!("unexpected step 0: {:?}", other),
        }
        match steps[2] {
            AstNode::Annotated {
                role: Role::Plain,
                source: Some(Source::Inference { rule, parents }),
                ..
            } => {
                assert_eq!(rule, "resolution");
                assert_eq!(parents, &vec!["f1".to_string(), "f2".to_string()]);
            }
            other => panic!("unexpected step 2: {:?}", other),
        }
    }

    #[test]
    fn no_output_block_at_all_is_status_only() {
        let raw = "% SZS status GaveUp for input\nsome banner noise, not TPTP\n";
        let (doc, errors) = parse_szs(raw, "vampire");
        assert!(errors.is_empty());
        assert!(stmts(&doc).is_empty());
    }

    #[test]
    fn vampire_default_human_readable_format_is_not_understood() {
        // Vampire's DEFAULT proof format (emitted when `-p tptp` is not
        // passed) is `<id>. <bare formula> [<annotation>]` — not TPTP syntax
        // at all (no `fof(...)` framing, no statement-terminating `.` after
        // the annotation). This module only understands the TPTP-shaped
        // format (see the module doc comment) — this test locks in exactly
        // how that shows up: the `% SZS …` scaffolding still parses fine
        // (it's an ordinary comment line either way), but every proof step
        // is lost to a single parse error at the very first token, because
        // there is no synchronization point to recover to for a wholly
        // different statement grammar.
        let raw = "\
% Refutation found. Thanks to Tanya!
% SZS status ContradictoryAxioms for input
% SZS output start Proof for input
5413. ! [X3] : ! [X0] : s__instance(s__ViralPartFn(X0,X3),X3) [input(axiom)]
43843. $false [resolution 33238,43416]
% SZS output end Proof for input
% ------------------------------
% Termination reason: Refutation
";
        let (doc, errors) = parse_szs(raw, "vampire");

        // The scaffolding is unaffected by the proof-step format.
        assert_eq!(
            symbol_arg(metas(&doc, "szs_status")[0]),
            "ContradictoryAxioms"
        );
        assert_eq!(symbol_arg(metas(&doc, "szs_output_start")[0]), "Proof");
        assert_eq!(symbol_arg(metas(&doc, "szs_output_end")[0]), "Proof");

        // But no proof steps survive, and the whole block is one error.
        assert!(stmts(&doc).is_empty());
        assert_eq!(errors.len(), 1);
        assert!(format!("{}", errors[0].1).contains("5413"));
    }

    #[test]
    fn real_vampire_tptp_shaped_proof_parses_cleanly() {
        // A real captured `-p tptp` Vampire refutation (ContradictoryAxioms,
        // 14 proof steps) — exercises axiom citations, chained inference
        // steps, a step with no parents (`introduced`), and one step whose
        // annotation's middle argument is itself a nested list of function
        // calls (`skolemisation`'s `[status(esa), new_symbols(skolem, [...])]`)
        // that must not be mistaken for the parent list.
        let raw = "\
% Refutation found. Thanks to Tanya!
% SZS status ContradictoryAxioms for input
% SZS output start Proof for input
fof(f4735,axiom,(
  ! [X3] : ! [X0] : s__instance(s__ViralPartFn(X0,X3),X3)),
  file('/work/input.p',kb_7369543776367022530)).
fof(f6897,axiom,(
  ! [X0] : (s__instance(X0,s__Pair) => ? [X3] : ? [X2] : (s__member(X3,X0) & s__member(X2,X0) & X2 != X3 & ~? [X1] : s__member(X1,X0)))),
  file('/work/input.p',kb_10720058774026927641)).
fof(f13862,plain,(
  ! [X0] : ! [X1] : s__instance(s__ViralPartFn(X1,X0),X0)),
  inference(rectify,[],[f4735])).
fof(f23100,plain,(
  ! [X0] : (? [X1,X2] : (s__member(X1,X0) & s__member(X2,X0) & X1 != X2 & ! [X3] : ~s__member(X3,X0)) => (s__member(sK1035(X0),X0) & s__member(sK1036(X0),X0) & sK1035(X0) != sK1036(X0) & ! [X3] : ~s__member(X3,X0)))),
  introduced(definition,[],[choice_axiom])).
fof(f23101,plain,(
  ! [X0] : ((s__member(sK1035(X0),X0) & s__member(sK1036(X0),X0) & sK1035(X0) != sK1036(X0) & ! [X3] : ~s__member(X3,X0)) | ~s__instance(X0,s__Pair))),
  inference(skolemisation,[status(esa),new_symbols(skolem,[sK1035,sK1036])],[f20138,f23100])).
fof(f38741,plain,(
  $false),
  inference(resolution,[],[f29408,f38345])).
% SZS output end Proof for input
% ------------------------------
% Version: Vampire 5.0.0 (MinSizeRel build, commit 5a7e9a5e8 on 2025-10-11 23:26:53 +0100)
% Termination reason: Refutation
";
        let (doc, errors) = parse_szs(raw, "vampire");
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);

        assert_eq!(
            symbol_arg(metas(&doc, "szs_status")[0]),
            "ContradictoryAxioms"
        );

        let steps = stmts(&doc);
        assert_eq!(steps.len(), 6);

        match steps[0] {
            AstNode::Annotated {
                role: Role::Axiom,
                source: Some(Source::Input { name, .. }),
                ..
            } => assert_eq!(name.as_deref(), Some("kb_7369543776367022530")),
            other => panic!("unexpected step 0: {:?}", other),
        }
        match steps[3] {
            // The `choice_axiom` step has no parents — `introduced`, not `inference`.
            AstNode::Annotated {
                source: Some(Source::Introduced(mechanism)),
                ..
            } => assert_eq!(mechanism, "definition"),
            other => panic!("unexpected step 3 (introduced): {:?}", other),
        }
        match steps[4] {
            // The tricky one: middle arg is a nested list of function calls,
            // must not leak into `parents`.
            AstNode::Annotated {
                source: Some(Source::Inference { rule, parents }),
                ..
            } => {
                assert_eq!(rule, "skolemisation");
                assert_eq!(parents, &vec!["f20138".to_string(), "f23100".to_string()]);
            }
            other => panic!("unexpected step 4 (skolemisation): {:?}", other),
        }
        match steps[5] {
            AstNode::Annotated {
                role: Role::Plain,
                source: Some(Source::Inference { rule, parents }),
                ..
            } => {
                assert_eq!(rule, "resolution");
                assert_eq!(parents, &vec!["f29408".to_string(), "f38345".to_string()]);
            }
            other => panic!("unexpected final step: {:?}", other),
        }
    }
}
