// crates/core/src/prover/proof/emit.rs
//
// TPTP-formula → KIF/AST conversion and proof emission.
//
// `formula_to_ast`/`formula_to_kif` parse a bare Vampire/TPTP formula string
// into SUO-KIF (delegating to `Parser::Tptp` in formulas-only mode; the macro
// pipeline does top-level-forall stripping, quantifier collapse, literal
// decoding).  `proof_steps_to_kif` batches that over a proof transcript; and
// `proof_to_ast`/`emit_proof` lift a `KifProofStep` transcript to an annotated
// AST and frame it through an output dialect ([`crate::parse::dialect::Emitter`]).

use crate::parse::ast::{AstNode, OpKind};
use crate::parse::doc::DocItem;
use crate::parse::kif::dis::AstKif; // `.flat()` / `.pretty_print()` / `.format_plain()`
use crate::parse::{Parser, TptpParseOptions};

use super::model::{parse_kb_axiom_name, KifProofStep};

/// Convert a single Vampire/TPTP formula string to a SUO-KIF [`AstNode`].
///
/// Returns `None` when the formula cannot be parsed.
///
/// Top-level universal quantifiers are stripped by the macro pipeline
/// (SUO-KIF convention — free uppercase variables are implicitly
/// universally quantified).  All other post-processing (quantifier
/// collapsing, literal decoding, symbol remapping) is also handled
/// by the pipeline.
pub fn formula_to_ast(tptp: &str) -> Option<AstNode> {
    let parser = Parser::Tptp {
        options: Some(TptpParseOptions {
            formulas_only: true,
            ..Default::default()
        }),
    };
    let (nodes, _) = parser.parse(tptp.trim(), "");
    let DocItem::Stmt(node) = nodes.into_iter().next()? else { return None };
    Some(normalize_display_quantifiers(node))
}

/// SUO-KIF display normalization for proof formulas: collapse runs of LIKE
/// quantifiers into one binder list, then strip top-level universals (free
/// variables are implicitly universal in KIF).  Mixed chains (`∀…∃…`) and
/// nested universals under other connectives are preserved.
///
/// Display-ONLY, deliberately at the rendering seam: the parse/macro
/// pipeline used to do this for everything, and sharing it with prover
/// INPUT is what caused the conjecture-skolemization soundness bug — a
/// stripped top-level `forall` on a conjecture flips its skolemization.
fn normalize_display_quantifiers(node: AstNode) -> AstNode {
    strip_top_level_foralls(collapse_like_quantifiers(node))
}

/// The quantifier kind of a `(Q (vars…) body)` list, if it is one.
fn quant_kind(elements: &[AstNode]) -> Option<OpKind> {
    match elements {
        [AstNode::Operator { op, .. }, AstNode::List { .. }, _]
            if matches!(op, OpKind::ForAll | OpKind::Exists) => Some(op.clone()),
        _ => None,
    }
}

/// Bottom-up: `(Q (v1…) (Q (v2…) body))` → `(Q (v1… v2…) body)` for the
/// SAME quantifier `Q` only.
fn collapse_like_quantifiers(node: AstNode) -> AstNode {
    let AstNode::List { elements, span } = node else { return node };
    let mut elements: Vec<AstNode> =
        elements.into_iter().map(collapse_like_quantifiers).collect();
    if let Some(q) = quant_kind(&elements) {
        let body = elements.pop().expect("quantifier has a body");
        if let AstNode::List { elements: mut inner, span: inner_span } = body {
            if quant_kind(&inner) == Some(q) {
                let inner_body = inner.pop().expect("inner quantifier has a body");
                let inner_vars = inner.pop().expect("inner quantifier has vars");
                if let (
                    AstNode::List { elements: outer_vars, .. },
                    AstNode::List { elements: iv, .. },
                ) = (&mut elements[1], inner_vars)
                {
                    outer_vars.extend(iv);
                }
                elements.push(inner_body);
                return AstNode::List { elements, span };
            }
            elements.push(AstNode::List { elements: inner, span: inner_span });
            return AstNode::List { elements, span };
        }
        elements.push(body);
    }
    AstNode::List { elements, span }
}

/// Peel every top-level universal (stacked ones included, collapsed or not).
fn strip_top_level_foralls(mut node: AstNode) -> AstNode {
    loop {
        match node {
            AstNode::List { mut elements, .. }
                if quant_kind(&elements) == Some(OpKind::ForAll) =>
            {
                node = elements.pop().expect("quantifier has a body");
            }
            other => return other,
        }
    }
}

/// Convert a single Vampire/TPTP formula string to a flat SUO-KIF string.
///
/// Returns a best-effort KIF string; unparseable input is returned
/// as a comment.  For indented output, convert with [`formula_to_ast`]
/// and call [`AstNode::pretty_print`].
#[allow(dead_code)]
pub fn formula_to_kif(tptp: &str) -> String {
    match formula_to_ast(tptp) {
        Some(node) => node.flat(),
        None => format!("; [unparseable] {}", tptp),
    }
}

/// Convert proof steps into a document of role/provenance-annotated statements,
/// ready for emission through a dialect ([`crate::parse::dialect::Emitter`]).
///
/// Mirrors the framing the bespoke TPTP proof emitter used: each step is named
/// `f{index+1}`; a premise-less input axiom cites `file(problem)`; the
/// premise-less negated conjecture cites a `negate_conjecture` inference;
/// derived steps cite `inference(rule, [parents])`.  Names are assigned before
/// parents are wired so `Source::Inference.parents` resolve.
pub fn proof_to_ast(steps: &[KifProofStep], problem: &str) -> Vec<AstNode> {
    use crate::parse::ast::{Role, Source};
    steps.iter().map(|s| {
        let role = match s.rule.as_str() {
            "axiom"              => Role::Axiom,
            "hypothesis"         => Role::Hypothesis,
            "negated_conjecture" => Role::NegatedConjecture,
            _                    => Role::Plain,
        };
        let source = if s.premises.is_empty() {
            if s.rule == "negated_conjecture" {
                Source::Inference { rule: "negate_conjecture".into(), parents: Vec::new() }
            } else {
                Source::Input(problem.to_string())
            }
        } else {
            Source::Inference {
                rule:    s.rule.clone(),
                parents: s.premises.iter().map(|p| format!("f{}", p + 1)).collect(),
            }
        };
        AstNode::Annotated {
            role,
            name:    Some(format!("f{}", s.index + 1)),
            source:  Some(source),
            formula: Box::new(s.formula.clone()),
            span:    crate::parse::Span::synthetic(),
        }
    }).collect()
}

/// Render a proof transcript in any output dialect — the unified proof-emission
/// seam behind `solve_tptp`'s `proof_tptp`, the KIF proof view, and CASC
/// `--proof <fof|tff|cnf>` (the generic `--proof tptp` mirrors the input dialect).
///
/// The proof is lifted to an annotated AST once ([`proof_to_ast`]) and framed by
/// the chosen [`Emitter`]: KIF needs no statement framing; `Tptp(Fof|Cnf|Auto)`
/// frames each step untyped; `Tptp(Tff)` adds typed binders and a monomorphic
/// `$i` type preamble.  The returned [`EmitResult`] reports any steps a dialect
/// could not represent (e.g. a non-clausal step under `Cnf`).
pub fn emit_proof(
    steps:   &[KifProofStep],
    problem: &str,
    dialect: crate::parse::dialect::Emitter,
) -> crate::parse::dialect::EmitResult {
    dialect.emit(&proof_to_ast(steps, problem))
}

/// Convert a sequence of `(formula, rule, premise_indices, source_name)`
/// tuples to KIF steps.
///
/// The fourth element is the axiom's original TPTP name when Vampire
/// preserved it via `--output_axiom_names on` (e.g. `Some("kb_42")`);
/// `None` for derived steps or when the flag wasn't active.  When the
/// name matches our `kb_<sid>` convention, the numeric suffix is parsed
/// into [`KifProofStep::source_sid`] for direct source-axiom lookup.
pub fn proof_steps_to_kif(
    steps: &[(String, String, Vec<usize>, Option<String>)],
) -> Vec<KifProofStep> {
    steps
        .iter()
        .enumerate()
        .map(|(i, (formula, rule, premises, source_name))| KifProofStep {
            index: i,
            rule: rule.clone(),
            premises: premises.clone(),
            formula: formula_to_ast(formula)
                .unwrap_or_else(|| AstNode::Symbol {
                    name: format!("; [unparseable] {}", formula),
                    span: crate::parse::ast::Span::point(String::new(), 0, 0, 0),
                }),
            source_sid: source_name.as_deref().and_then(parse_kb_axiom_name),
        })
        .collect()
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn kif(tptp: &str) -> String {
        formula_to_kif(tptp)
    }

    #[test]
    fn simple_predicate() {
        assert_eq!(kif("s__likes(s__John, s__Mary)"), "(likes John Mary)");
    }

    #[test]
    fn negated_predicate() {
        assert_eq!(
            kif("~s__likes(s__John, s__Mary)"),
            "(not (likes John Mary))"
        );
    }

    #[test]
    fn variable() {
        assert_eq!(kif("s__likes(s__John, X0)"), "(likes John ?X0)");
    }

    #[test]
    fn forall_top_level_stripped() {
        assert_eq!(
            kif("! [X0] : s__likes(s__John, X0)"),
            "(likes John ?X0)"
        );
    }

    #[test]
    fn forall_nested_kept() {
        assert_eq!(
            kif("! [X0] : (s__foo(X0) => ! [X1] : s__bar(X1))"),
            "(=> (foo ?X0) (forall (?X1) (bar ?X1)))"
        );
    }

    #[test]
    fn exists() {
        assert_eq!(
            kif("? [X0,X1] : s__likes(X0, X1)"),
            "(exists (?X0 ?X1) (likes ?X0 ?X1))"
        );
    }

    #[test]
    fn or_clause() {
        assert_eq!(
            kif("~s__instance(X0, s__Carrying) | ~s__agent(X0, s__John)"),
            "(or (not (instance ?X0 Carrying)) (not (agent ?X0 John)))"
        );
    }

    #[test]
    fn implies() {
        assert_eq!(
            kif("s__instance(X0, s__Carrying) => s__instance(X0, s__Transfer)"),
            "(=> (instance ?X0 Carrying) (instance ?X0 Transfer))"
        );
    }

    #[test]
    fn equality() {
        assert_eq!(kif("s__Circle = X0"), "(equal Circle ?X0)");
    }

    #[test]
    fn disequality() {
        assert_eq!(kif("s__Circle != X0"), "(not (equal Circle ?X0))");
    }

    #[test]
    fn false_literal() {
        assert_eq!(kif("$false"), "False");
    }

    #[test]
    fn true_literal() {
        assert_eq!(kif("$true"), "True");
    }

    #[test]
    fn numeric_literal() {
        assert_eq!(kif("s__foo(n__42)"), "(foo 42)");
    }

    #[test]
    fn negative_numeric_literal() {
        assert_eq!(kif("s__foo(n__neg_3_14)"), "(foo -3.14)");
    }

    #[test]
    fn string_literal() {
        assert_eq!(kif("s__foo(str__hello)"), "(foo \"hello\")");
    }

    #[test]
    fn negated_conjecture() {
        let tptp = "~? [X0,X1] : (s__instance(X0, s__Carrying) & s__agent(X0, s__John) & s__instance(X1, s__Flower) & s__objectTransferred(X0, X1))";
        assert_eq!(
            kif(tptp),
            "(not (exists (?X0 ?X1) (and (instance ?X0 Carrying) (agent ?X0 John) (instance ?X1 Flower) (objectTransferred ?X0 ?X1))))"
        );
    }

    #[test]
    fn nested_exists_collapsed() {
        let tptp = "! [X0] : (s__instance(X0, s__Pair) => \
                    ? [X1] : ? [X2] : (s__member(X1, X0) \
                                        & s__member(X2, X0) \
                                        & X1 != X2))";
        assert_eq!(
            kif(tptp),
            "(=> (instance ?X0 Pair) \
              (exists (?X1 ?X2) \
                (and (member ?X1 ?X0) (member ?X2 ?X0) (not (equal ?X1 ?X2)))))"
        );
    }

    #[test]
    fn stacked_top_level_foralls_all_stripped() {
        let tptp = "! [X0] : ! [X1] : (s__sameRow(X0, X1) => s__sameRow(X1, X0))";
        assert_eq!(kif(tptp), "(=> (sameRow ?X0 ?X1) (sameRow ?X1 ?X0))");
    }

    #[test]
    fn nested_forall_inside_implies_collapsed() {
        let tptp = "! [X0] : (s__foo(X0) => ! [X1] : ! [X2] : s__bar(X1, X2))";
        assert_eq!(
            kif(tptp),
            "(=> (foo ?X0) (forall (?X1 ?X2) (bar ?X1 ?X2)))"
        );
    }

    #[test]
    fn mixed_quantifier_chain_not_collapsed() {
        let tptp = "! [X0] : (s__instance(X0, s__Top) => \
                    ! [X1] : ? [X2] : s__foo(X0, X1, X2))";
        assert_eq!(
            kif(tptp),
            "(=> (instance ?X0 Top) \
              (forall (?X1) (exists (?X2) (foo ?X0 ?X1 ?X2))))"
        );
    }

    #[test]
    fn nested_exists_under_not_not_collapsed_with_outer() {
        let tptp = "? [X1] : ? [X2] : (s__foo(X1, X2) & ~? [X3] : s__bar(X3))";
        assert_eq!(
            kif(tptp),
            "(exists (?X1 ?X2) (and (foo ?X1 ?X2) (not (exists (?X3) (bar ?X3)))))"
        );
    }

    #[test]
    fn unparseable_returns_comment() {
        let result = formula_to_kif("^^^");
        assert!(result.starts_with("; [unparseable]"));
    }

    // -- emit_proof dispatcher ------------------------------------------------

    fn step(index: usize, rule: &str, kif: &str, premises: Vec<usize>) -> KifProofStep {
        let doc = crate::parse::parse_document("t", kif, crate::Parser::Kif);
        KifProofStep {
            index, rule: rule.into(), premises,
            formula: doc.ast.into_iter().next().unwrap().as_stmt().cloned().unwrap(),
            source_sid: None,
        }
    }

    fn demo_proof() -> Vec<KifProofStep> {
        vec![
            step(0, "axiom", "(=> (human ?X) (mortal ?X))", vec![]),
            step(1, "hypothesis", "(human socrates)", vec![]),
            step(2, "resolve", "(mortal socrates)", vec![0, 1]),
        ]
    }

    #[test]
    fn emit_proof_dispatches_per_dialect() {
        use crate::parse::dialect::{Emitter, TptpLang};
        let p = demo_proof();

        // KIF: no statement framing — each step's bare formula.
        let kif = emit_proof(&p, "demo", Emitter::Kif);
        assert!(kif.is_complete());
        assert!(kif.text.contains("(=> (human ?X) (mortal ?X))"), "{}", kif.text);
        assert!(!kif.text.contains("fof("), "kif must not frame: {}", kif.text);

        // FOF: framed, untyped.
        let fof = emit_proof(&p, "demo", Emitter::Tptp(TptpLang::Fof));
        assert!(fof.text.contains("fof(f1, axiom, (human(X) => mortal(X)), file('demo'))."), "{}", fof.text);
        assert!(fof.text.contains("inference(resolve, [status(thm)], [f1,f2])"), "{}", fof.text);

        // TFF: typed preamble + binder-free body here (free var), framed as tff.
        let tff = emit_proof(&p, "demo", Emitter::Tptp(TptpLang::Tff));
        assert!(tff.text.contains("type, human: $i > $o)."), "{}", tff.text);
        assert!(tff.text.contains("tff(f1, axiom,"), "{}", tff.text);
    }
}
