use core::fmt;
use serde::{Deserialize, Serialize};
pub use super::Span;

/// Logical operators that are keywords in KIF.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpKind {
    And,
    Or,
    Not,
    Implies,
    Iff,
    Equal,
    ForAll,
    Exists,
}

impl OpKind {
    /// The KIF keyword for this operator (e.g. `"and"`, `"=>"`).
    pub fn name(&self) -> &'static str {
        match self {
            OpKind::And     => "and",
            OpKind::Or      => "or",
            OpKind::Not     => "not",
            OpKind::Implies => "=>",
            OpKind::Iff     => "<=>",
            OpKind::Equal   => "equal",
            OpKind::ForAll  => "forall",
            OpKind::Exists  => "exists",
        }
    }

    /// The fixed arity of this operator, or `0` for the variadic `And`/`Or`.
    pub fn arity(&self) -> usize {
        match self {
            OpKind::Not => 1,
            OpKind::And | OpKind::Or => 0,
            _ => 2
        }
    }

    /// Whether this operator is a quantifier (`ForAll` or `Exists`).
    pub fn is_quantifier(&self) -> bool {
        return matches!(self, OpKind::ForAll | OpKind::Exists)
    }
}

impl fmt::Display for OpKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// The logical status of a top-level statement within a problem or proof.
///
/// Carried by [`AstNode::Annotated`], never on a formula term. `Other`
/// preserves dialect-specific roles for round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Axiom,
    Hypothesis,
    Definition,
    Lemma,
    Conjecture,
    NegatedConjecture,
    /// Derived / no special status (TPTP `plain`).
    Plain,
    /// A TFF type-declaration statement (`tff(_, type, sym: T)`).
    Type,
    /// Any dialect-specific role that doesn't map to the above (e.g. TPTP
    /// `assumption`, `corollary`), holding the original role word.
    Other(String),
}

/// Provenance of a statement — where it came from. Populated for proof steps
/// and file-attributed input; absent (`Option<Source>` is `None`) for plain
/// input with no recorded origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// Read from an input file/problem (`file('problem')`).
    Input(String),
    /// Derived by an inference rule from named parent statements
    /// (`inference(rule, …, [parents])`).
    Inference { rule: String, parents: Vec<String> },
    /// Synthesized by the prover itself — background schemata, theory
    /// units — with no input formula or parent steps to cite
    /// (`introduced(mechanism)`).
    Introduced(String),
}

/// A node in the raw abstract syntax tree produced by the parser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AstNode {
    /// Corresponds to a sentence
    List     { elements: Vec<AstNode>, span: Span },
    /// Corresponds to a symbol
    Symbol   { name: String, span: Span },
    /// Corresponds to a variable
    Variable { name: String, span: Span },   // includes leading `?`
    /// Corresponds to a row variable
    RowVariable { name: String, span: Span }, // includes leading `@`
    /// Corresponds to a string literal
    Str      { value: String, span: Span },  // includes surrounding `"`
    /// Corresponds to a numerical literal
    Number   { value: String, span: Span },
    /// Corresponds to an operator
    Operator { op: OpKind, span: Span },
    /// A top-level statement wrapping a formula with its dialect-level
    /// metadata: logical `role`, optional `name`, and optional `source`
    /// provenance.
    ///
    /// Invariant: appears only at document top level, never inside a formula
    /// term. Term-level code strips it first via [`AstNode::strip_annotation`]
    /// / [`AstNode::formula`].
    Annotated {
        role:    Role,
        name:    Option<String>,
        source:  Option<Source>,
        formula: Box<AstNode>,
        span:    Span,
    },
}

impl AstNode {
    /// The source [`Span`] of this node.
    pub fn span(&self) -> &Span {
        match self {
            AstNode::List { span, .. }        => span,
            AstNode::Symbol { span, .. }      => span,
            AstNode::Variable { span, .. }    => span,
            AstNode::RowVariable { span, .. } => span,
            AstNode::Str { span, .. }         => span,
            AstNode::Number { span, .. }      => span,
            AstNode::Operator { span, .. }    => span,
            AstNode::Annotated { span, .. }   => span,
        }
    }

    /// The inner formula of an [`AstNode::Annotated`] statement, or `self` for a
    /// bare formula.
    pub fn formula(&self) -> &AstNode {
        match self {
            AstNode::Annotated { formula, .. } => formula,
            other => other,
        }
    }

    /// The [`Role`] of an [`AstNode::Annotated`] statement, if any.
    pub fn role(&self) -> Option<&Role> {
        match self {
            AstNode::Annotated { role, .. } => Some(role),
            _ => None,
        }
    }

    /// Consume any top-level [`AstNode::Annotated`] wrapper and return the inner
    /// formula by value, or return `self` unchanged for a bare formula.
    pub fn strip_annotation(self) -> AstNode {
        match self {
            AstNode::Annotated { formula, .. } => *formula,
            other => other,
        }
    }

}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_is_zero_width() {
        let s = Span::point("f".into(), 1, 1, 0);
        assert!(s.is_point());
        assert_eq!(s.byte_len(), 0);
        assert_eq!(s.end_offset, s.offset);
    }

    #[test]
    fn join_covers_both() {
        let a = Span { file: "f".into(), line: 1, col: 1, offset: 0, end_line: 1, end_col: 4, end_offset: 3 };
        let b = Span { file: "f".into(), line: 1, col: 5, offset: 4, end_line: 1, end_col: 9, end_offset: 8 };
        let j = a.join(&b);
        assert_eq!(j.offset,     0);
        assert_eq!(j.end_offset, 8);
        assert_eq!(j.col,        1);
        assert_eq!(j.end_col,    9);
    }

    #[test]
    fn join_is_order_insensitive() {
        let a = Span { file: "f".into(), line: 2, col: 3, offset: 10, end_line: 2, end_col: 7, end_offset: 14 };
        let b = Span { file: "f".into(), line: 1, col: 1, offset:  0, end_line: 1, end_col: 2, end_offset:  1 };
        let ab = a.join(&b);
        let ba = b.join(&a);
        assert_eq!(ab.offset,     ba.offset);
        assert_eq!(ab.end_offset, ba.end_offset);
    }

    fn parse_one(src: &str) -> AstNode {
        let doc = crate::parse::parse_document("t", src, crate::Parser::Kif);
        assert!(doc.parse_errors.is_empty(), "parse errors: {:?}", doc.parse_errors);
        doc.ast.into_iter().next().expect("at least one node").as_stmt().cloned().expect("a stmt item")
    }

    #[test]
    fn annotated_helpers_and_passthrough() {
        use crate::parse::kif::dis::AstKif;
        let inner = parse_one("(represents ?STRING ?USER)");
        let ann = AstNode::Annotated {
            role:    Role::Conjecture,
            name:    Some("c1".into()),
            source:  None,
            formula: Box::new(inner.clone()),
            span:    Span::default(),
        };
        assert_eq!(ann.role(), Some(&Role::Conjecture));
        assert_eq!(ann.formula().to_string(), inner.to_string());
        assert_eq!(ann.clone().strip_annotation().to_string(), inner.to_string());
        assert_eq!(ann.to_string(), inner.to_string());           // Display
        assert_eq!(ann.format_plain(0), inner.format_plain(0));    // AstKif
        assert_eq!(inner.role(), None);
        assert_eq!(inner.formula().to_string(), inner.to_string());
    }

    #[test]
    fn display_elides_point_and_same_col() {
        let p = Span::point("f".into(), 3, 5, 42);
        assert_eq!(p.to_string(), "f:3:5");

        let inline = Span { file: "f".into(), line: 3, col: 5, offset: 42, end_line: 3, end_col: 11, end_offset: 48 };
        assert_eq!(inline.to_string(), "f:3:5-11");

        let multi = Span { file: "f".into(), line: 3, col: 5, offset: 42, end_line: 4, end_col: 2, end_offset: 60 };
        assert_eq!(multi.to_string(), "f:3:5-4:2");
    }
}