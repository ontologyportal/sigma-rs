// crates/core/src/prover/proof/graphviz.rs
//
// Render a proof as a Graphviz DOT digraph. Pure string-building (a
// `dot_structures` AST serialized via `printer::DotPrinter`) — never touches
// `graphviz-rust`'s `exec`/`exec_dot` (which spawns a `dot` process and isn't
// wasm-compatible), so this is safe to call from any target, including
// wasm32 (see the crate's Cargo.toml comment on the `getrandom`/`wasm_js`
// build shim this still requires to compile there).

use graphviz_rust::dot_structures::{
    Attribute, Edge, EdgeTy, Graph, GraphAttributes, Id, Node, NodeId, Stmt, Vertex,
};
use graphviz_rust::printer::{DotPrinter, PrinterContext};

use crate::parse::kif::dis::AstKif;

use super::KifProofStep;

/// Render `proof` as a DOT digraph: one node per proof step (labelled
/// `N. [rule]` plus the flattened formula), one edge per premise pointing
/// from the premise step into the step it derives. Always produces a
/// syntactically valid graph — including when `proof` is empty — so the
/// output is safe to pipe straight into `dot`/`neato`/etc.
///
/// `status` is embedded verbatim in the graph's label (e.g. `"Theorem"` /
/// `"CounterSatisfiable"` / any prover status rendered to a string) — taken
/// as a plain `&str` rather than a concrete status enum so callers aren't
/// forced to depend on any particular status type.
pub fn render_graphviz(proof: &[KifProofStep], name: &str, status: &str) -> String {
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

    for step in proof {
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
/// escaping are the caller's responsibility (mirrors `dot_generator`'s `esc`
/// macro).
fn dot_escaped(s: &str) -> Id {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    Id::Escaped(format!("\"{escaped}\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::ast::AstNode;

    fn step(index: usize, rule: &str, premises: Vec<usize>) -> KifProofStep {
        KifProofStep {
            index,
            rule: rule.to_string(),
            premises,
            formula: AstNode::Symbol { name: "FALSE".to_string(), span: Default::default() },
            source_sid: None,
        }
    }

    #[test]
    fn empty_proof_is_still_a_valid_graph() {
        let dot = render_graphviz(&[], "test", "Theorem");
        assert!(dot.contains("digraph"));
        assert!(dot.contains("SZS status Theorem for test"));
    }

    #[test]
    fn one_edge_per_premise() {
        let proof = vec![step(0, "Axiom", vec![]), step(1, "Resolution", vec![0])];
        let dot = render_graphviz(&proof, "test", "Theorem");
        assert!(dot.contains("n0"));
        assert!(dot.contains("n1"));
        assert!(dot.contains("n0 -> n1") || dot.contains("n0->n1"));
    }

    #[test]
    fn escapes_newlines_and_quotes_in_labels() {
        // A rule name with an embedded quote shouldn't break the DOT output.
        let proof = vec![step(0, "Ax\"iom", vec![])];
        let dot = render_graphviz(&proof, "test", "Theorem");
        assert!(dot.contains("\\\""));
    }
}
