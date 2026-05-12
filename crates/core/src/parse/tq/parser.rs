// crates/core/src/parse/tq/parser.rs
//
// TQ (test-query) parser.  A `.kif.tq` file is KIF plus a few harness
// directives.  The parser **reuses the KIF grammar** (`Parser::Kif`) for the
// raw structure — the KIF parser itself stays directive-blind, emitting
// `(query …)`/`(time …)` as ordinary lists — and then classifies each top-level
// form into a [`DocItem`]:
//
//   * `(query F)`         → `DocItem::Stmt(Annotated { Conjecture })`.
//   * `(note …)`, `(time N)`, `(answer …)`, `(file …)` → `DocItem::Meta`.
//   * everything else     → `DocItem::Stmt(Annotated { Hypothesis })`.
//
// [`parse_tq`] returns that `Vec<DocItem>`; [`TestCase::from_doc_items`]
// interprets the directives into harness fields.  Keeping directives as
// `MetaNode`s (not `AstNode` variants) means the logical content never carries
// non-formula nodes into the store.

use crate::parse::ast::{AstNode, OpKind, Role};
use crate::parse::doc::{DocItem, MetaNode};
use crate::parse::kif;
use crate::parse::kif::error::KifParseError;
use crate::parse::{ParseError, Parser, Span, TptpParseOptions};
use crate::{Diagnostic, ToDiagnostic};

/// Directive head keywords that classify a top-level `(kw …)` list as a
/// [`MetaNode`] rather than a logical statement.  `query` is **not** here —
/// it carries a formula, so it becomes a `Conjecture` statement.
const DIRECTIVES: &[&str] = &["note", "time", "answer", "file"];

/// A parsed `.kif.tq` test: its logical content (as role-tagged statements) plus
/// the harness directives.
#[derive(Debug, Clone)]
pub struct TestCase {
    pub file_name: String,
    pub note: String,
    pub timeout: u32,
    /// The conjecture, as `Annotated { role: Conjecture, … }` (its `name` is the
    /// query's KIF text, for citations).  `None` if the file has no `(query …)`.
    pub query: Option<AstNode>,
    pub expected_proof: Option<bool>,        // true = yes, false = no
    pub expected_answer: Option<Vec<String>>,
    /// Hypotheses, each `Annotated { role: Hypothesis, … }`.
    pub axioms: Vec<AstNode>,
    pub extra_files: Vec<String>,
}

impl TestCase {
    /// The hypotheses rendered as newline-joined KIF — for string-based
    /// consumers (TPTP translation, sweep).  Byte-identical to the old
    /// `axioms.join("\n")` since each hypothesis is its bare formula.
    pub fn axiom_kif(&self) -> String {
        self.axioms.iter().map(|a| a.formula().to_string()).collect::<Vec<_>>().join("\n")
    }

    /// The conjecture's KIF text, if any.
    pub fn query_kif(&self) -> Option<String> {
        self.query.as_ref().map(|q| q.formula().to_string())
    }

    /// Assemble a test case from a classified document — `.tq` ([`parse_tq`])
    /// or a role-tagged TPTP parse.  Partition is by logical role:
    ///
    ///   * `Conjecture`        → the query (proved by refutation — negated, then
    ///     saturated against the KB);
    ///   * `NegatedConjecture` → the query too, but re-wrapped in `not` (see
    ///     [`renegate`]) so the prover's refutation negation restores the
    ///     original already-negated goal clause — no "already negated" flag to
    ///     thread through the prove path;
    ///   * `Hypothesis`        → force-included **support** (`axioms`);
    ///   * `Meta`              → harness directives.
    ///
    /// Every *other* statement (TPTP `axiom` / `plain` / `definition` / …) is
    /// **not** part of the test obligation — it is the background theory.  Those
    /// `DocItem`s are returned verbatim in the second tuple element for the
    /// caller (e.g. the SDK's TPTP loader) to ingest into the KB as ordinary,
    /// SInE-selectable axioms.  For a `.tq` source — which only ever emits
    /// `Conjecture` / `Hypothesis` — that leftover vec is empty.
    pub fn from_doc_items(items: &[DocItem], file_name: &str) -> (TestCase, Vec<DocItem>) {
        let mut tc = TestCase {
            file_name:       file_name.to_string(),
            note:            file_name.to_string(),
            timeout:         30,
            query:           None,
            expected_answer: None,
            expected_proof:  None,
            axioms:          Vec::new(),
            extra_files:     Vec::new(),
        };
        let mut leftover: Vec<DocItem> = Vec::new();
        for item in items {
            match item {
                DocItem::Stmt(node) => match node.role() {
                    Some(Role::Conjecture)        => tc.query = Some(node.clone()),
                    Some(Role::NegatedConjecture) => tc.query = Some(renegate(node, file_name)),
                    Some(Role::Hypothesis)        => tc.axioms.push(node.clone()),
                    // Background theory (`axiom`, `plain`, `definition`, …) is
                    // not part of the obligation — hand it back to be ingested
                    // as an ordinary, selectable KB axiom.  A role-less
                    // statement (bare KIF) is tagged `axiom` on the way out so
                    // downstream partitioning sees a uniform annotation.
                    None => leftover.push(DocItem::Stmt(annotate(
                        Role::Axiom, node.clone().strip_annotation(), file_name))),
                    Some(_) => leftover.push(item.clone()),
                },
                DocItem::Meta(m) => tc.apply_directive(m),
            }
        }
        (tc, leftover)
    }

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
    pub fn from_tptp(text: &str, name: &str)
        -> (TestCase, Vec<AstNode>, Vec<(Span, Box<dyn ParseError>)>)
    {
        let probe = Parser::Tptp {
            options: Some(TptpParseOptions { keep_conjectures: true, ..TptpParseOptions::none() }),
        };
        let (items, errors) = probe.parse(text, name);
        let (tc, leftover) = TestCase::from_doc_items(&items, name);
        let background = leftover.into_iter()
            .filter_map(|d| match d { DocItem::Stmt(n) => Some(n), DocItem::Meta(_) => None })
            .collect();
        (tc, background, errors)
    }

    /// Fold one harness directive into the test case.
    fn apply_directive(&mut self, m: &MetaNode) {
        match m.key.as_str() {
            "note" => if let Some(first) = m.args.first() {
                self.note = match first {
                    AstNode::Str { value, .. }   => value.trim_matches('"').to_string(),
                    AstNode::Symbol { name, .. } => name.clone(),
                    other                        => other.to_string(),
                };
            },
            "time" => if let Some(AstNode::Number { value, .. }) = m.args.first() {
                self.timeout = value.parse::<u32>().unwrap_or(30);
            },
            "answer" => if let Some(AstNode::Symbol { name, .. }) = m.args.first() {
                match name.to_lowercase().as_str() {
                    "yes" => self.expected_proof = Some(true),
                    "no"  => self.expected_proof = Some(false),
                    _ => {
                        self.expected_proof = Some(true);
                        let mut answers = vec![name.clone()];
                        for el in &m.args[1..] {
                            if let AstNode::Symbol { name, .. } = el { answers.push(name.clone()); }
                        }
                        self.expected_answer = Some(answers);
                    }
                }
            },
            "file" => if let Some(el) = m.args.first() {
                let fname = match el {
                    AstNode::Symbol { name, .. } => name.clone(),
                    AstNode::Str { value, .. }   => value.trim_matches('"').to_string(),
                    other                        => other.to_string(),
                };
                self.extra_files.push(fname);
            },
            _ => {}
        }
    }
}

/// Re-present an already-negated `negated_conjecture` (logically ¬C) as a
/// positive `Conjecture` by wrapping its formula in `not`.  The prover negates
/// the query exactly once during refutation, so the stored `(not ¬C)` becomes
/// `¬¬¬C ≡ ¬C` — the original goal clause — back in the refutation set.  This
/// keeps CNF `negated_conjecture` problems on the ordinary conjecture path
/// without an "already negated" flag threaded through `ask` → `prove`.
fn renegate(node: &AstNode, file: &str) -> AstNode {
    let negated = AstNode::List {
        elements: vec![
            AstNode::Operator { op: OpKind::Not, span: Span::synthetic() },
            node.formula().clone(),
        ],
        span: Span::synthetic(),
    };
    annotate(Role::Conjecture, negated, file)
}

/// Wrap `formula` as a top-level statement with `role`, tagging its source.
fn annotate(role: Role, formula: AstNode, file: &str) -> AstNode {
    let span = formula.span().clone();
    AstNode::Annotated {
        role,
        name:    None,
        source:  Some(crate::parse::ast::Source::Input(file.to_string())),
        formula: Box::new(formula),
        span,
    }
}

/// Parse a `.kif.tq` source into a classified document.  Reuses the KIF grammar
/// (`Parser::Kif`) for the raw nodes, then sorts each top-level form into a
/// [`DocItem`].  Parse errors are forwarded verbatim (positionally independent
/// of the returned items, like every other parser).
pub fn parse_tq(content: &str, file: &str)
    -> (Vec<DocItem>, Vec<(Span, KifParseError)>)
{
    let (tokens, tok_err) = kif::tokenize(&content, file);
    let (nodes, parse_err) = kif::parse(tokens, file);
    let items = nodes.into_iter().map(|n| classify(n, file)).collect();
    let mut errors = tok_err;
    errors.extend(parse_err);
    (items, errors)
}

/// Classify one raw KIF top-level node into a [`DocItem`].
fn classify(node: AstNode, file: &str) -> DocItem {
    if let AstNode::List { elements, span } = &node {
        if let Some(AstNode::Symbol { name, .. }) = elements.first() {
            match name.as_str() {
                // `(query F)` / `(ask F)` is formula-bearing → a Conjecture
                // statement whose `name` is the query's KIF text (for
                // proof-step citations).
                "query" | "ask" => if let Some(q) = elements.get(1) {
                    let qname = q.to_string();
                    let mut ann = annotate(Role::Conjecture, q.clone(), file);
                    if let AstNode::Annotated { name, .. } = &mut ann { *name = Some(qname); }
                    return DocItem::Stmt(ann);
                },
                // Harness directives carry no formula → `Meta`.
                k if DIRECTIVES.contains(&k) => {
                    return DocItem::Meta(MetaNode {
                        key:  k.to_string(),
                        args: elements[1..].to_vec(),
                        span: span.clone(),
                    });
                }
                _ => {}
            }
        }
    }
    DocItem::Stmt(annotate(Role::Hypothesis, node, file))
}

/// Parse a `.kif.tq` source straight into a [`TestCase`] — `parse_tq` followed
/// by [`TestCase::from_doc_items`].  Aborts on the first hard parse error.
pub fn parse_test_content(content: &str, file_name: &str) -> Result<TestCase, Diagnostic> {
    let (items, mut errors) = parse_tq(content, file_name);
    if !errors.is_empty() {
        let (_, err) = errors.remove(0);
        return Err(err.to_diagnostic());
    }
    // `.tq` sources emit only `Conjecture` / `Hypothesis`, so the leftover
    // (background-axiom) vec is always empty here — discard it.
    Ok(TestCase::from_doc_items(&items, file_name).0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_axioms_query_and_metadata() {
        let src = "(note \"a test\") (time 12) (answer yes) \
                   (subclass A B) (query (instance x B))";
        let tc = parse_test_content(src, "T.kif.tq").unwrap();
        assert_eq!(tc.note, "a test");
        assert_eq!(tc.timeout, 12);
        assert_eq!(tc.expected_proof, Some(true));
        // One hypothesis, role-tagged.
        assert_eq!(tc.axioms.len(), 1);
        assert!(matches!(tc.axioms[0].role(), Some(Role::Hypothesis)));
        // Query is a Conjecture-tagged statement.
        let q = tc.query.expect("query");
        assert!(matches!(q.role(), Some(Role::Conjecture)));
        assert_eq!(q.formula().to_string(), "(instance x B)");
    }

    // Role-aware partition (the TPTP path): `hypothesis` → support, `axiom`
    // (and other background roles) → leftover for the KB, `conjecture` → query.
    #[test]
    fn from_doc_items_partitions_by_role() {
        let f = |kif: &str| parse_tq(kif, "t").0.into_iter().next().unwrap();
        let k = |kif: &str| Parser::Kif.parse(kif, "t").0.into_iter().next().unwrap();
        let items = vec![
            k("(subclass A B)"),
            f("(instance x A)"),
            f("(ask (instance x B))"),
        ];
        let (tc, leftover) = TestCase::from_doc_items(&items, "t");
        // Only the hypothesis is force-included support.
        assert_eq!(tc.axioms.len(), 1);
        assert!(matches!(tc.axioms[0].role(), Some(Role::Hypothesis)));
        // The `axiom`-role statement is background theory → handed back.
        assert_eq!(leftover.len(), 1);
        assert!(matches!(
            leftover[0].as_stmt().and_then(|n| n.role()), Some(Role::Axiom)));
        assert_eq!(tc.query.unwrap().formula().to_string(), "(instance x B)");
    }

    // A `negated_conjecture` (already ¬C) becomes a `Conjecture` query wrapped
    // in an extra `not`, so the prover's refutation negation restores ¬C.
    #[test]
    fn negated_conjecture_is_re_negated_into_the_query() {
        let f = |kif: &str| Parser::Kif.parse(kif, "t").0.into_iter().next().unwrap();
        let neg = f("(not (mammal rex))");
        let s = neg.as_stmt().cloned().unwrap().strip_annotation();
        let items = vec![
            DocItem::Stmt(annotate(Role::NegatedConjecture, s, "t")),
        ];
        let (tc, leftover) = TestCase::from_doc_items(&items, "t");
        assert!(leftover.is_empty());
        let q = tc.query.expect("query");
        assert!(matches!(q.role(), Some(Role::Conjecture)));
        assert_eq!(q.formula().to_string(), "(not (not (mammal rex)))");
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
        assert!(matches!(tc.query.and_then(|q| q.role().cloned()), Some(Role::Conjecture)));
    }

    #[test]
    fn parse_tq_yields_docitems_with_meta_and_stmts() {
        let src = "(note \"a test\") (time 12) (subclass A B) (query (instance x B))";
        let (items, errors) = parse_tq(src, "T.kif.tq");
        assert!(errors.is_empty());
        // note + time → Meta; subclass → Stmt(Hypothesis); query → Stmt(Conjecture).
        let metas: Vec<&str> = items.iter()
            .filter_map(|i| i.as_meta().map(|m| m.key.as_str())).collect();
        assert_eq!(metas, vec!["note", "time"]);
        let roles: Vec<_> = items.iter()
            .filter_map(|i| i.as_stmt().and_then(|n| n.role())).cloned().collect();
        assert_eq!(roles, vec![Role::Hypothesis, Role::Conjecture]);
        // The `time` directive keeps its raw operand for the consumer.
        let time = items.iter().find_map(|i| i.as_meta().filter(|m| m.key == "time")).unwrap();
        assert!(matches!(time.args.first(), Some(AstNode::Number { value, .. }) if value == "12"));
    }
}
