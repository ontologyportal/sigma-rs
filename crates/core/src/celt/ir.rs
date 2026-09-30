//! Logical form produced by the rules, and its rendering to KIF.
//!
//! A [`Drs`] is a discourse representation structure: bound variables plus
//! conditions.  Rules build these; [`Drs::to_ast`] closes them into a KIF
//! formula.  Assertions bind discourse referents to fresh Skolem constants
//! (`Book-1`) so every sentence is independently assertable and later
//! definite noun phrases can refer back; queries, negations and universals
//! bind variables instead.

use std::collections::HashMap;

use crate::parse::ast::{AstNode, OpKind};
use crate::parse::span::Span;

/// A KIF term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// A symbol: a KB constant or a Skolem constant.
    Const(String),
    /// A variable, with its leading `?`.
    Var(String),
    /// A string literal (unquoted).
    Str(String),
    /// A function application, e.g. `(WhenFn ?E)`.
    Fn(String, Vec<Term>),
}

impl Term {
    pub fn is_var(&self) -> bool {
        matches!(self, Term::Var(_))
    }

    pub fn to_ast(&self) -> AstNode {
        match self {
            Term::Const(s) => sym(s),
            Term::Var(v) => var_node(v),
            Term::Str(s) => AstNode::Str {
                value: format!("\"{s}\""),
                span: Span::synthetic(),
            },
            Term::Fn(f, args) => list(
                std::iter::once(sym(f))
                    .chain(args.iter().map(Term::to_ast))
                    .collect(),
            ),
        }
    }
}

/// A condition of a [`Drs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cond {
    /// `(pred args...)`
    Atom(String, Vec<Term>),
    /// `(not <drs>)`
    Not(Drs),
    /// `(forall (vars of antecedent) (=> antecedent consequent))`
    Implies(Drs, Drs),
}

/// Bound variables and conditions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drs {
    pub vars: Vec<String>,
    pub conds: Vec<Cond>,
}

impl Drs {
    pub fn atom(&mut self, pred: &str, args: Vec<Term>) {
        self.conds.push(Cond::Atom(pred.to_string(), args));
    }

    pub fn bind(&mut self, var: &str) {
        if !self.vars.iter().any(|v| v == var) {
            self.vars.push(var.to_string());
        }
    }

    pub fn extend(&mut self, other: Drs) {
        for v in other.vars {
            self.bind(&v);
        }
        self.conds.extend(other.conds);
    }

    /// The conditions as one formula, without quantifying the variables.
    pub fn body(&self) -> AstNode {
        let mut nodes: Vec<AstNode> = self.conds.iter().map(Cond::to_ast).collect();
        if nodes.len() == 1 {
            nodes.pop().unwrap_or_else(|| sym("true"))
        } else {
            list(std::iter::once(op(OpKind::And)).chain(nodes).collect())
        }
    }

    /// The closed formula: `(exists (vars) body)` or just `body`.
    pub fn to_ast(&self) -> AstNode {
        if self.vars.is_empty() {
            self.body()
        } else {
            list(vec![op(OpKind::Exists), var_list(&self.vars), self.body()])
        }
    }

    /// Top-level conjuncts of an assertion, each a separately tellable
    /// formula.  A DRS with bound variables stays one formula.
    pub fn conjuncts(&self) -> Vec<AstNode> {
        if self.vars.is_empty() {
            self.conds.iter().map(Cond::to_ast).collect()
        } else {
            vec![self.to_ast()]
        }
    }
}

impl Cond {
    pub fn to_ast(&self) -> AstNode {
        match self {
            Cond::Atom(pred, args) => list(
                std::iter::once(sym(pred))
                    .chain(args.iter().map(Term::to_ast))
                    .collect(),
            ),
            Cond::Not(d) => list(vec![op(OpKind::Not), d.to_ast()]),
            Cond::Implies(a, c) => {
                let imp = list(vec![op(OpKind::Implies), a.body(), c.to_ast()]);
                if a.vars.is_empty() {
                    imp
                } else {
                    list(vec![op(OpKind::ForAll), var_list(&a.vars), imp])
                }
            }
        }
    }
}

fn sym(name: &str) -> AstNode {
    AstNode::Symbol {
        name: name.to_string(),
        span: Span::synthetic(),
    }
}

fn op(kind: OpKind) -> AstNode {
    AstNode::Operator {
        op: kind,
        span: Span::synthetic(),
    }
}

fn list(elements: Vec<AstNode>) -> AstNode {
    AstNode::List {
        elements,
        span: Span::synthetic(),
    }
}

/// `AstNode::Variable` stores the name without its `?`.
fn var_node(v: &str) -> AstNode {
    AstNode::Variable {
        name: v.trim_start_matches('?').to_string(),
        span: Span::synthetic(),
    }
}

fn var_list(vars: &[String]) -> AstNode {
    list(vars.iter().map(|v| var_node(v)).collect())
}

/// A discourse referent introduced by an earlier sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Referent {
    pub term: String,
    pub class: String,
    pub sentence: usize,
}

/// Document-level state: minted constants and referents for definite
/// reference across sentences.
#[derive(Debug, Default)]
pub struct Discourse {
    counters: HashMap<String, usize>,
    pub referents: Vec<Referent>,
    pub names: Vec<String>,
    next_var: HashMap<String, usize>,
    pub sentence: usize,
}

impl Discourse {
    /// A fresh Skolem constant for an instance of `class` (`Book-1`).
    /// `taken` reports names already in use by the KB.
    pub fn skolem(&mut self, class: &str, taken: impl Fn(&str) -> bool) -> String {
        loop {
            let n = self.counters.entry(class.to_string()).or_insert(0);
            *n += 1;
            let name = format!("{class}-{n}");
            if !taken(&name) {
                self.referents.push(Referent {
                    term: name.clone(),
                    class: class.to_string(),
                    sentence: self.sentence,
                });
                return name;
            }
        }
    }

    /// A fresh variable `?<prefix><n>`.
    pub fn var(&mut self, prefix: &str) -> String {
        let n = self.next_var.entry(prefix.to_string()).or_insert(0);
        *n += 1;
        format!("?{prefix}{n}")
    }

    /// Referents whose class is `class` or a subclass of it, most recent
    /// first.
    pub fn matching(&self, class: &str, is_sub: impl Fn(&str, &str) -> bool) -> Vec<&Referent> {
        let mut out: Vec<&Referent> = self
            .referents
            .iter()
            .filter(|r| r.class == class || is_sub(&r.class, class))
            .collect();
        out.reverse();
        out
    }

    /// Reset variable numbering at the start of a sentence.
    pub fn begin_sentence(&mut self, index: usize) {
        self.sentence = index;
        self.next_var.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_exists_and_negation() {
        let mut d = Drs::default();
        d.bind("?E");
        d.atom(
            "instance",
            vec![Term::Var("?E".into()), Term::Const("Reading".into())],
        );
        d.atom(
            "agent",
            vec![Term::Var("?E".into()), Term::Const("Mike".into())],
        );
        assert_eq!(
            d.to_ast().to_string(),
            "(exists (?E) (and (instance ?E Reading) (agent ?E Mike)))"
        );
        let n = Cond::Not(d.clone());
        assert!(n.to_ast().to_string().starts_with("(not (exists (?E)"));
    }

    #[test]
    fn renders_implication_with_forall() {
        let mut a = Drs::default();
        a.bind("?X");
        a.atom(
            "instance",
            vec![Term::Var("?X".into()), Term::Const("Human".into())],
        );
        let mut c = Drs::default();
        c.atom(
            "attribute",
            vec![Term::Var("?X".into()), Term::Const("Mortal".into())],
        );
        assert_eq!(
            Cond::Implies(a, c).to_ast().to_string(),
            "(forall (?X) (=> (instance ?X Human) (attribute ?X Mortal)))"
        );
    }

    #[test]
    fn conjuncts_split_only_ground_assertions() {
        let mut d = Drs::default();
        d.atom(
            "instance",
            vec![Term::Const("Rex".into()), Term::Const("Dog".into())],
        );
        d.atom(
            "attribute",
            vec![Term::Const("Rex".into()), Term::Const("Loud".into())],
        );
        assert_eq!(d.conjuncts().len(), 2);
        d.bind("?X");
        assert_eq!(d.conjuncts().len(), 1);
    }

    #[test]
    fn skolems_skip_taken_names_and_resolve_by_class() {
        let mut disc = Discourse::default();
        let a = disc.skolem("Book", |n| n == "Book-1");
        assert_eq!(a, "Book-2");
        let b = disc.skolem("Book", |_| false);
        assert_eq!(b, "Book-3");
        let m = disc.matching("Book", |_, _| false);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].term, "Book-3");
        assert_eq!(disc.var("X"), "?X1");
        disc.begin_sentence(1);
        assert_eq!(disc.var("X"), "?X1");
    }
}
