// crates/sumo-kb/src/parser.rs
// Ported verbatim from sumo-parser-core/src/parser.rs.
// Only change: import paths updated to crate-local modules.

use core::fmt;
use inline_colorization::*;
use crate::error::{ParseError, Span};
use crate::tokenizer::{OpKind, Token, TokenKind};

// ── AST ───────────────────────────────────────────────────────────────────────

/// A node in the raw abstract syntax tree produced by the parser.
#[derive(Debug, Clone)]
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
}

/// Plain KIF display — output is always re-parseable (no ANSI codes).
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
            other => write!(f, "{}", other),
        }
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

pub struct Parser {
    tokens: Vec<Token>,
    pos:    usize,
    file:   String,
}

impl Parser {
    fn new(tokens: Vec<Token>, file: &str) -> Self {
        Self { tokens, pos: 0, file: file.to_owned() }
    }

    fn peek(&self) -> Option<&Token> { self.tokens.get(self.pos) }

    fn advance(&mut self) -> Option<&Token> {
        let tok = self.tokens.get(self.pos);
        if tok.is_some() { self.pos += 1; }
        tok
    }

    fn eof_span(&self) -> Span {
        if let Some(t) = self.tokens.last() { t.span.clone() }
        else { Span { file: self.file.clone(), line: 1, col: 1, offset: 0 } }
    }

    fn parse_node(&mut self) -> Result<AstNode, (Span, ParseError)> {
        let tok = match self.peek() {
            None => return Err((self.eof_span(), ParseError::UnexpectedEof { span: self.eof_span() })),
            Some(t) => t,
        };
        match &tok.kind {
            TokenKind::LParen => {
                let start_span = tok.span.clone();
                self.advance();
                let mut elements = Vec::new();
                loop {
                    match self.peek() {
                        None => return Err((start_span.clone(), ParseError::UnbalancedParens { span: start_span })),
                        Some(t) if matches!(t.kind, TokenKind::RParen) => { self.advance(); break; }
                        _ => elements.push(self.parse_node()?),
                    }
                }
                Ok(AstNode::List { elements, span: start_span })
            }
            TokenKind::RParen => {
                let span = tok.span.clone();
                self.advance();
                Err((span.clone(), ParseError::UnbalancedParens { span }))
            }
            TokenKind::Symbol(name) => {
                let node = AstNode::Symbol { name: name.clone(), span: tok.span.clone() };
                self.advance(); Ok(node)
            }
            TokenKind::Variable(name) => {
                let name = name.trim_start_matches('?').to_string();
                let node = AstNode::Variable { name, span: tok.span.clone() };
                self.advance(); Ok(node)
            }
            TokenKind::RowVariable(name) => {
                let name = name.trim_start_matches('@').to_string();
                let node = AstNode::RowVariable { name, span: tok.span.clone() };
                self.advance(); Ok(node)
            }
            TokenKind::Str(s) => {
                let node = AstNode::Str { value: s.clone(), span: tok.span.clone() };
                self.advance(); Ok(node)
            }
            TokenKind::Number(n) => {
                let node = AstNode::Number { value: n.clone(), span: tok.span.clone() };
                self.advance(); Ok(node)
            }
            TokenKind::Operator(op) => {
                let node = AstNode::Operator { op: op.clone(), span: tok.span.clone() };
                self.advance(); Ok(node)
            }
        }
    }

    fn parse_all(&mut self) -> (Vec<AstNode>, Vec<(Span, ParseError)>) {
        let mut nodes  = Vec::new();
        let mut errors = Vec::new();
        while self.peek().is_some() {
            match self.parse_node() {
                Ok(node) => nodes.push(node),
                Err(e)   => { errors.push(e); self.advance(); }
            }
        }
        (nodes, errors)
    }
}

/// Parse `tokens` into a list of top-level AST nodes.
pub fn parse(tokens: Vec<Token>, file: &str) -> (Vec<AstNode>, Vec<(Span, ParseError)>) {
    let mut parser = Parser::new(tokens, file);
    parser.parse_all()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::tokenize;

    fn parse_kif(src: &str) -> Vec<AstNode> {
        let (tokens, _) = tokenize(src, "test");
        let (nodes, errors) = parse(tokens, "test");
        assert!(errors.is_empty(), "parse errors: {:?}", errors);
        nodes
    }

    #[test]
    fn simple_list() {
        let nodes = parse_kif("(subclass Human Animal)");
        assert_eq!(nodes.len(), 1);
        assert!(matches!(&nodes[0], AstNode::List { elements, .. } if elements.len() == 3));
    }

    #[test]
    fn nested_list() {
        let nodes = parse_kif("(=> (instance ?X Human) (instance ?X Animal))");
        assert_eq!(nodes.len(), 1);
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(&elements[0], AstNode::Operator { op: OpKind::Implies, .. }));
            assert!(matches!(&elements[1], AstNode::List { .. }));
            assert!(matches!(&elements[2], AstNode::List { .. }));
        } else { panic!("expected List"); }
    }

    #[test]
    fn multiple_top_level() {
        let nodes = parse_kif("(foo a) (bar b)");
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn variables_and_literals() {
        let nodes = parse_kif("(lessThan ?X 42)");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(&elements[1], AstNode::Variable { name, .. } if name == "X"));
            assert!(matches!(&elements[2], AstNode::Number { value, .. } if value == "42"));
        }
    }
}
