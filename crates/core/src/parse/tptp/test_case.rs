// crates/core/src/parse/tptp/test_case.rs
//
// TPTP problem (`.p` / `.tptp`) entry points onto the shared `TestCase`
// model (`crate::parse::tq::TestCase`) -- the TPTP-dialect counterpart to
// `.kif.tq`'s `parse_test_content` / `TestCase::from_doc_items`.

use crate::parse::ast::AstNode;
use crate::parse::doc::DocItem;
use crate::parse::tq::TestCase;
use crate::parse::{ParseError, Parser, Span, TptpParseOptions};
use crate::{DiagResult, ToDiagnostic};

type ParsedTestCase = (TestCase, Vec<AstNode>, Vec<(Span, Box<dyn ParseError>)>);

impl TestCase {
    /// Build a test case from a TPTP problem `text` (FOF / CNF / TFF).  Parses
    /// with conjectures kept, then partitions by role exactly like
    /// [`from_doc_items`](Self::from_doc_items): `conjecture` /
    /// `negated_conjecture` → query, `hypothesis` → support, and the background
    /// theory (`axiom` / `plain` / …) is returned as the background-axiom
    /// `Vec<AstNode>` for the caller to ingest as ordinary, SInE-selectable KB
    /// axioms.  (TPTP carries no harness directives, so the leftover is all
    /// statements — flattened to bare `AstNode`s here for the caller's
    /// convenience.)  `include(...)` directives must already be spliced by the
    /// caller (filesystem work the core deliberately leaves to the SDK).
    pub fn from_tptp(text: &str, name: &str) -> ParsedTestCase {
        let probe = Parser::Tptp {
            options: Some(TptpParseOptions {
                keep_conjectures: true,
                ..TptpParseOptions::none()
            }),
        };
        let (items, errors) = probe.parse(text, name);
        let (tc, leftover) = TestCase::from_doc_items(&items, name);
        let background = leftover
            .into_iter()
            .filter_map(|d| match d {
                DocItem::Stmt(n) => Some(n),
                DocItem::Meta(_) => None,
            })
            .collect();
        (tc, background, errors)
    }
}

/// Parse a standalone TPTP problem (`.p` / `.tptp`) straight into a
/// [`TestCase`] — the TPTP-dialect counterpart to
/// [`parse_test_content`](crate::parse::tq::parse_test_content).
/// TPTP carries no harness directives, so `note`/`timeout`/`expected_answer`
/// stay at their defaults and `expected_proof` comes only from a `% Status`
/// header pragma, when present. Background theory (`axiom`/`plain`/… role
/// statements outside the `hypothesis`/`conjecture` roles) is folded into
/// `axioms` alongside the hypotheses — this function has no KB to promote it
/// into separately, so the whole non-conjecture theory becomes force-included
/// support, same as `.tq`'s hypotheses. Aborts on the first hard parse error.
///
/// `remap` decodes SUMO-mangled symbol names (`s__subclassOf` →
/// `subclassOf`, …) back to their real SUMO names — see
/// [`TptpParseOptions`]'s `remap_*` fields. Turn it on when `content` is
/// meant to be proved against the real SUMO KB (the live KB's symbols are
/// unmangled, so a SUMO-generated TPTP problem's names must be decoded to
/// unify with them); leave it off for a self-contained problem with its own,
/// already-plain vocabulary.
pub fn parse_tptp_test_content(
    content: &str,
    file_name: &str,
    remap: bool,
) -> DiagResult<TestCase> {
    let base = if remap {
        TptpParseOptions::default()
    } else {
        TptpParseOptions::none()
    };
    let probe = Parser::Tptp {
        options: Some(TptpParseOptions {
            keep_conjectures: true,
            ..base
        }),
    };
    let (items, mut errors) = probe.parse(content, file_name);
    if !errors.is_empty() {
        return Err(errors.remove(0).1.to_diagnostic().into());
    }
    let (mut tc, leftover) = TestCase::from_doc_items(&items, file_name);
    let background = leftover.into_iter().filter_map(|d| match d {
        DocItem::Stmt(n) => Some(n),
        DocItem::Meta(_) => None,
    });
    tc.axioms.extend(background);
    Ok(tc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::ast::Role;

    #[test]
    fn parse_tptp_test_content_folds_background_into_axioms() {
        let src = "\
            fof(a1, axiom, subclass(dog, mammal)).\n\
            fof(a2, hypothesis, instance(rex, dog)).\n\
            fof(g, conjecture, instance(rex, mammal)).\n";
        let tc = parse_tptp_test_content(src, "t.p", false).unwrap();
        // Both the axiom-role background and the hypothesis land in axioms.
        assert_eq!(tc.axioms.len(), 2, "axioms: {:?}", tc.axioms);
        let q = tc.query.expect("query");
        assert!(matches!(q.role(), Some(Role::Conjecture)));
        assert_eq!(q.formula().to_string(), "(instance rex mammal)");
        // TPTP carries no harness directives; `note` defaults to the file name.
        assert_eq!(tc.note, "t.p");
        assert_eq!(tc.expected_answer, None);
    }

    #[test]
    fn parse_tptp_test_content_reports_the_first_parse_error() {
        let src = "fof(a1, axiom, subclass(dog, mammal)"; // missing `)).`
        let err = parse_tptp_test_content(src, "t.p", false);
        assert!(err.is_err(), "expected a parse error, got: {err:?}");
    }

    #[test]
    fn parse_tptp_test_content_remap_decodes_mangled_symbols() {
        let src = "\
            fof(a1, axiom, s__subclassOf(s__Dog, s__Mammal)).\n\
            fof(g, conjecture, s__subclassOf(s__Dog, s__Mammal)).\n";
        let unmapped = parse_tptp_test_content(src, "t.p", false).unwrap();
        assert_eq!(
            unmapped.axioms[0].formula().to_string(),
            "(s__subclassOf s__Dog s__Mammal)"
        );
        let remapped = parse_tptp_test_content(src, "t.p", true).unwrap();
        assert_eq!(
            remapped.axioms[0].formula().to_string(),
            "(subclassOf Dog Mammal)"
        );
    }

    // ALL `negated_conjecture` statements are kept (GRA001-1's shape: a CNF
    // problem whose clauses are all goal-role).  The query is
    // `(not (and NC₁ … NCₖ))`, so the prover's refutation negation restores
    // every NC clause; keeping only the last one silently dropped the rest
    // and produced false Satisfiable verdicts.
    #[test]
    fn multiple_negated_conjectures_all_kept() {
        let problem = "\
            cnf(c1, negated_conjecture, a | b).\n\
            cnf(c2, negated_conjecture, ~a).\n\
            cnf(c3, negated_conjecture, ~b).\n";
        let (tc, background, errors) = TestCase::from_tptp(problem, "multi");
        assert!(errors.is_empty(), "{errors:?}");
        assert!(background.is_empty());
        assert!(
            !tc.has_fof_conjecture,
            "pure-NC problems report Unsat/Sat SZS"
        );
        assert_eq!(tc.input_formulas, 3);
        assert_eq!(tc.unaccounted_inputs, 0, "every parsed formula accounted");
        let q = tc.query.expect("query").formula().to_string();
        assert_eq!(q, "(not (and (or a b) (not a) (not b)))", "query: {q}");
    }

    // Multiple positive conjectures conjoin (TPTP: prove them together).
    #[test]
    fn multiple_conjectures_conjoin() {
        let problem = "\
            fof(g1, conjecture, p(a)).\n\
            fof(g2, conjecture, q(a)).\n";
        let (tc, _, errors) = TestCase::from_tptp(problem, "multi2");
        assert!(errors.is_empty(), "{errors:?}");
        assert!(tc.has_fof_conjecture);
        assert_eq!(tc.unaccounted_inputs, 0);
        let q = tc.query.expect("query").formula().to_string();
        assert_eq!(q, "(and (p a) (q a))", "query: {q}");
    }

    // End-to-end through the real TPTP parser: `axiom` → background leftover,
    // `hypothesis` → support, `conjecture` → query.
    #[test]
    fn from_tptp_partitions_by_role() {
        let problem = "\
            fof(a1, axiom, ![X] : (dog(X) => mammal(X))).\n\
            fof(h1, hypothesis, dog(rex)).\n\
            fof(g, conjecture, mammal(rex)).\n";
        let (tc, background, errors) = TestCase::from_tptp(problem, "mini");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(background.len(), 1, "the `axiom` is background theory");
        assert!(matches!(background[0].role(), Some(Role::Axiom)));
        assert_eq!(tc.axioms.len(), 1, "the `hypothesis` is support");
        assert!(matches!(tc.axioms[0].role(), Some(Role::Hypothesis)));
        assert!(matches!(
            tc.query.and_then(|q| q.role().cloned()),
            Some(Role::Conjecture)
        ));
    }
}
