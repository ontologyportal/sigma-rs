use core::fmt;
use inline_colorization::*;
use serde::{Deserialize, Serialize};

/// Source location (1-based line and column).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Span {
    pub file:   String,
    pub line:   u32,
    pub col:    u32,
    pub offset: usize,
}

impl std::fmt::Display for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.col)
    }
}

/// Logical operators that are keywords in KIF.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpKind {
    And,
    Or,
    Not,
    Implies,  // =>
    Iff,      // <=>
    Equal,    // equal
    ForAll,   // forall
    Exists,   // exists
}

impl OpKind {
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
}

impl fmt::Display for OpKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// A node in the raw abstract syntax tree produced by the parser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AstNode {
    List     { elements: Vec<AstNode>, span: Span },
    Symbol   { name: String, span: Span },
    Variable { name: String, span: Span },   // includes leading `?`
    RowVariable { name: String, span: Span }, // includes leading `@`
    Str      { value: String, span: Span },  // includes surrounding `"`
    Number   { value: String, span: Span },
    Operator { op: OpKind, span: Span },
}

impl AstNode {
    pub fn span(&self) -> &Span {
        match self {
            AstNode::List { span, .. }        => span,
            AstNode::Symbol { span, .. }      => span,
            AstNode::Variable { span, .. }    => span,
            AstNode::RowVariable { span, .. } => span,
            AstNode::Str { span, .. }         => span,
            AstNode::Number { span, .. }      => span,
            AstNode::Operator { span, .. }    => span,
        }
    }

    /// Compact flat KIF string -- `(op arg1 arg2)` without extra spaces.
    pub fn flat(&self) -> String {
        match self {
            AstNode::List { elements, .. } => {
                if elements.is_empty() { return "()".into(); }
                format!("({})", elements.iter().map(AstNode::flat).collect::<Vec<_>>().join(" "))
            }
            AstNode::Symbol { name, .. }      => name.clone(),
            AstNode::Variable { name, .. }    => format!("?{}", name),
            AstNode::RowVariable { name, .. } => format!("@{}", name),
            AstNode::Str { value, .. }
            | AstNode::Number { value, .. }   => value.clone(),
            AstNode::Operator { op, .. }      => op.name().to_owned(),
        }
    }

    /// Indented KIF pretty-printer.
    ///
    /// Expressions that fit within 72 columns at `indent` are kept on one line.
    /// Longer ones break so that the operator stays on the opening line and
    /// each argument is placed on its own line indented two spaces further.
    pub fn pretty_print(&self, indent: usize) -> String {
        const LINE_WIDTH: usize = 72;
        let flat = self.flat();
        if indent + flat.len() <= LINE_WIDTH {
            return Pretty(self).to_string();
        }
        match self {
            AstNode::List { elements, .. } if elements.len() >= 2 => {
                let pad  = " ".repeat(indent + 2);
                let head = elements[0].pretty_print(0);
                let args: Vec<String> = elements[1..].iter()
                    .map(|e| format!("{}{}", pad, e.pretty_print(indent + 2)))
                    .collect();
                format!("({}\n{})", head, args.join("\n"))
            }
            _ => Pretty(self).to_string(),
        }
    }
}

/// Plain KIF display -- output is always re-parseable (no ANSI codes).
/// Use [`Pretty`] for colourised terminal/log output.
impl fmt::Display for AstNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AstNode::List { elements, .. } => {
                write!(f, "( ")?;
                for el in elements { write!(f, "{} ", el)?; }
                write!(f, ")")
            }
            AstNode::Symbol { name, .. }        => write!(f, "{}", name),
            AstNode::Variable { name, .. }      => write!(f, "?{}", name),
            AstNode::RowVariable { name, .. }   => write!(f, "@{}", name),
            AstNode::Str { value, .. }
            | AstNode::Number { value, .. }     => write!(f, "{}", value),
            AstNode::Operator { op, .. }        => write!(f, "{}", op.name()),
        }
    }
}

/// Colourised display wrapper for [`AstNode`].
///
/// Use this for terminal output and log messages where ANSI colour is
/// desirable.  Operators are rendered in cyan.  For output that must be
/// fed back into the parser or KB, use plain [`Display`] / [`to_string`].
pub struct Pretty<'a>(pub &'a AstNode);

impl fmt::Display for Pretty<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            AstNode::List { elements, .. } => {
                write!(f, "( ")?;
                for el in elements { write!(f, "{} ", Pretty(el))?; }
                write!(f, ")")
            }
            AstNode::Operator { op, .. } =>
                write!(f, "{color_cyan}{}{color_reset}", op.name()),
            AstNode::Number { value, ..}
            | AstNode::Str { value, ..} => write!(f, "{color_green}{}{color_reset}", value),
            AstNode::Variable { ..}
            | AstNode::RowVariable { .. } => write!(f, "{color_magenta}{}{color_reset}", self.0.flat()),
            AstNode::Symbol { name, .. } => {
                if name.chars().next().map_or(false, |c| c.is_lowercase()) {
                    write!(f, "{color_bright_blue}{}{color_reset}", name)
                } else {
                    write!(f, "{color_yellow}{}{color_reset}", name)
                }
            }
        }
    }
}