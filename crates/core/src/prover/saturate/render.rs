// crates/core/src/prover/saturate/render.rs
//
// Render native `PClause`s back to flat SUO-KIF text, reusing the crate's
// single KIF emitter (`parse::kif::dis::flat`) via a clause-local `AstNode`
// reconstruction. Mirrors `SyntacticLayer::sentence_to_ast`/`element_to_ast`
// (`syntactic/display.rs`), but resolves through the prover-local
// `AtomTable` first (clause literals can be interned-only, never promoted
// to the shared store) and remaps internal skolem symbol names
// (`sk_<root>_<n>` / `sk_g<root>_<n>`, see `clausify.rs`'s `SkolemCtx`) to
// short, readable `SkFnN`/`SkCN` names for output.

use std::collections::HashMap;

use crate::parse::kif::dis::flat;
use crate::parse::Span;
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, Literal, SymbolId};
use crate::AstNode;

use super::clause::{AtomId, AtomTable, PClause};

/// Per-`clausify`-call rename table: internal skolem symbol id -> short
/// output name. Functions (applied skolem terms) and constants (bare
/// skolem symbols) get independent counters, mirroring the applied/bare
/// split `skolemize` itself makes.
#[derive(Default)]
pub(crate) struct SkolemNames {
    names: HashMap<SymbolId, String>,
    fn_n: u32,
    c_n: u32,
}

impl SkolemNames {
    fn render(&mut self, id: SymbolId, is_fn: bool) -> String {
        if let Some(n) = self.names.get(&id) {
            return n.clone();
        }
        let n = if is_fn {
            self.fn_n += 1;
            format!("SkFn{}", self.fn_n)
        } else {
            self.c_n += 1;
            format!("SkC{}", self.c_n)
        };
        self.names.insert(id, n.clone());
        n
    }
}

fn is_skolem_name(name: &str) -> bool {
    name.starts_with("sk_")
}

/// Render one clause as flat SUO-KIF: a bare literal for a unit clause,
/// `(or lit1 lit2 ...)` otherwise. Negative literals are wrapped `(not
/// ...)`.
pub(crate) fn clause_to_kif(
    clause: &PClause,
    atoms: &AtomTable,
    syn: &SyntacticLayer,
    sk: &mut SkolemNames,
) -> String {
    let lits: Vec<String> = clause
        .lits
        .iter()
        .map(|lit| {
            let atom = atom_to_ast(lit.atom, atoms, syn, sk);
            if lit.pos {
                flat(&atom)
            } else {
                format!("(not {})", flat(&atom))
            }
        })
        .collect();
    match lits.as_slice() {
        [one] => one.clone(),
        many => format!("(or {})", many.join(" ")),
    }
}

fn atom_to_ast(
    id: AtomId,
    atoms: &AtomTable,
    syn: &SyntacticLayer,
    sk: &mut SkolemNames,
) -> AstNode {
    let span = Span::synthetic();
    let Some(sentence) = atoms.resolve(id, syn) else {
        return AstNode::Symbol {
            name: format!("sid: {}", id),
            span,
        };
    };
    let is_applied = sentence.elements.len() > 1;
    let elements = sentence
        .elements
        .iter()
        .enumerate()
        .map(|(i, el)| element_to_ast(el, atoms, syn, sk, is_applied && i == 0))
        .collect();
    AstNode::List { elements, span }
}

fn element_to_ast(
    el: &Element,
    atoms: &AtomTable,
    syn: &SyntacticLayer,
    sk: &mut SkolemNames,
    is_head: bool,
) -> AstNode {
    let span = Span::synthetic();
    match el {
        Element::Symbol(sym) => {
            let name = sym.name();
            let rendered = if is_skolem_name(&name) {
                sk.render(sym.id(), is_head)
            } else {
                name.to_string()
            };
            AstNode::Symbol {
                name: rendered,
                span,
            }
        }
        Element::Variable {
            name,
            is_row: false,
            ..
        } => AstNode::Variable {
            name: name.clone(),
            span,
        },
        Element::Variable {
            name, is_row: true, ..
        } => AstNode::RowVariable {
            name: name.clone(),
            span,
        },
        Element::Literal(Literal::Str(s)) => AstNode::Str {
            value: s.clone(),
            span,
        },
        Element::Literal(Literal::Number(n)) => AstNode::Number {
            value: n.clone(),
            span,
        },
        Element::Op(op) => AstNode::Operator {
            op: op.clone(),
            span,
        },
        Element::Sub(sub_sid) => atom_to_ast(*sub_sid, atoms, syn, sk),
    }
}
