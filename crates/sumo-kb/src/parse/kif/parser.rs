// crates/sumo-kb/src/parse/kif/parser.rs
use super::error::KifParseError;
use super::tokenizer::{Token, TokenKind};

use crate::parse::ast::{AstNode, OpKind, Span};

// Parser

pub struct KifParser {
    tokens: Vec<Token>,
    pos:    usize,
    file:   String,
}

impl KifParser {
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

    fn parse_node(&mut self) -> Result<AstNode, (Span, KifParseError)> {
        let tok = match self.peek() {
            None => return Err((self.eof_span(), KifParseError::UnexpectedEof { span: self.eof_span() })),
            Some(t) => t,
        };
        match &tok.kind {
            TokenKind::LParen => {
                let start_span = tok.span.clone();
                self.advance();
                let mut elements = Vec::new();
                let mut idx = 0;
                loop {
                    match self.peek() {
                        Some(t) if matches!(t.kind, TokenKind::RParen) && idx == 0 => { 
                            return Err((start_span.clone(), KifParseError::EmptySentence { span: start_span.clone() }));
                        }
                        Some(t) if matches!(t.kind, TokenKind::RParen) && idx > 0 => { self.advance(); break; }
                        None => {
                            return Err((start_span.clone(), KifParseError::UnbalancedParens { span: start_span }))
                        },
                        Some(Token { kind: TokenKind::Operator(op), span, .. }) if idx > 0 => { 
                            return Err((span.clone(), KifParseError::OperatorOutOfPosition {
                                op: op.name().to_string(),
                                span: span.clone(),
                            }));
                        },
                        Some(t) if idx == 0 &&  !t.kind.can_head() => {
                            return Err((t.span.clone(), KifParseError::FirstTerm { span: t.span.clone() }));
                        },
                        _ => elements.push(self.parse_node()?),
                    }
                    idx += 1;
                }

                // QuantifierArg: `(forall VAR_LIST BODY)` and `(exists VAR_LIST BODY)`.
                //   * VAR_LIST must be a parenthesised list -- not a bare variable.
                //   * Every element of VAR_LIST must be a plain or row variable.
                if matches!(
                    elements.first(),
                    Some(AstNode::Operator { op: OpKind::ForAll | OpKind::Exists, .. })
                ) {
                    match elements.get(1) {
                        Some(AstNode::List { elements: var_els, .. }) => {
                            for el in var_els {
                                if !matches!(el, AstNode::Variable { .. } | AstNode::RowVariable { .. }) {
                                    return Err((el.span().clone(), KifParseError::QuantifierArg {
                                        span: el.span().clone(),
                                    }));
                                }
                            }
                        }
                        Some(other) => {
                            return Err((other.span().clone(), KifParseError::QuantifierArg {
                                span: other.span().clone(),
                            }));
                        }
                        None => {
                            return Err((start_span.clone(), KifParseError::QuantifierArg {
                                span: start_span.clone(),
                            }));
                        }
                    }
                }

                Ok(AstNode::List { elements, span: start_span })
            }
            TokenKind::RParen => {
                let span = tok.span.clone();
                self.advance();
                Err((span.clone(), KifParseError::UnbalancedParens { span }))
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

    fn parse_all(&mut self) -> (Vec<AstNode>, Vec<(Span, KifParseError)>) {
        let mut nodes  = Vec::new();
        let mut errors = Vec::new();
        while self.peek().is_some() {
            // Remember the position so we know whether parse_node consumed any
            // tokens before failing.  If it did (e.g. a complete but structurally
            // invalid list), we must NOT advance again -- doing so would skip the
            // opening token of the next well-formed sentence.
            let pos_before = self.pos;
            match self.parse_node() {
                Ok(node) => {
                    // Top-level-only structural checks.  These cannot be caught
                    // inside `parse_node` because sub-lists are allowed to be
                    // empty (e.g. the variable list of a bare `forall` with no
                    // bound vars) and to start with non-symbol elements.
                    nodes.push(node);
                }
                Err(e) => {
                    errors.push(e);
                    // Only skip a token when parse_node made no progress -- i.e.
                    // the error occurred before any token was consumed.  If the
                    // parser consumed tokens and then found a structural problem
                    // (OperatorOutOfPosition, QuantifierArg) the stream is already
                    // positioned at the start of the next sentence.
                    if self.pos == pos_before {
                        self.advance();
                    }
                }
            }
        }
        (nodes, errors)
    }
}

/// Parse `tokens` into a list of top-level AST nodes.
pub fn parse(tokens: Vec<Token>, file: &str) -> (Vec<AstNode>, Vec<(Span, KifParseError)>) {
    let mut parser = KifParser::new(tokens, file);
    parser.parse_all()
}

// Tests


#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tokenizer::tokenize;
    use crate::parse::ast::OpKind;

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

    // -- Structural validation tests -------------------------------------------

    fn parse_errors(src: &str) -> Vec<KifParseError> {
        let (tokens, _) = tokenize(src, "test");
        let (_, errors) = parse(tokens, "test");
        errors.into_iter().map(|(_, e)| e).collect()
    }

    #[test]
    fn empty_sentence_is_an_error() {
        let errs = parse_errors("()");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::EmptySentence { .. })),
            "expected EmptySentence, got: {:?}", errs
        );
    }

    #[test]
    fn empty_sentence_does_not_abort_remaining_sentences() {
        // The empty list should be rejected but the valid sentence after it
        // must still be parsed.
        let (tokens, _) = tokenize("() (subclass Human Animal)", "test");
        let (nodes, errors) = parse(tokens, "test");
        assert!(errors.iter().any(|e| matches!(e.1, KifParseError::EmptySentence { .. })));
        assert_eq!(nodes.len(), 1, "the valid sentence after () should survive");
    }

    #[test]
    fn first_term_number_is_an_error() {
        let errs = parse_errors("(42 ?X)");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::FirstTerm { .. })),
            "expected FirstTerm, got: {:?}", errs
        );
    }

    #[test]
    fn first_term_string_is_an_error() {
        let errs = parse_errors("(\"hello\" ?X)");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::FirstTerm { .. })),
            "expected FirstTerm, got: {:?}", errs
        );
    }

    #[test]
    fn first_term_nested_list_is_an_error() {
        // A sub-list as the head of a top-level sentence is invalid.
        let errs = parse_errors("((P ?X) Foo)");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::FirstTerm { .. })),
            "expected FirstTerm, got: {:?}", errs
        );
    }

    #[test]
    fn operator_out_of_position_is_an_error() {
        // `and` in argument position is invalid.
        let errs = parse_errors("(instance and Foo)");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::OperatorOutOfPosition { .. })),
            "expected OperatorOutOfPosition, got: {:?}", errs
        );
    }

    #[test]
    fn operator_out_of_position_in_nested_list() {
        // Even inside a sub-list, operators must be the head.
        let errs = parse_errors("(=> (P ?X) (Q and ?Y))");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::OperatorOutOfPosition { .. })),
            "expected OperatorOutOfPosition, got: {:?}", errs
        );
    }

    #[test]
    fn quantifier_arg_bare_variable_is_an_error() {
        // `(forall ?X body)` -- variable list must be wrapped in parens.
        let errs = parse_errors("(forall ?X (instance ?X Human))");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::QuantifierArg { .. })),
            "expected QuantifierArg, got: {:?}", errs
        );
    }

    #[test]
    fn quantifier_arg_non_variable_in_list_is_an_error() {
        // A symbol inside the variable list is invalid.
        let errs = parse_errors("(forall (?X Human) (instance ?X Human))");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::QuantifierArg { .. })),
            "expected QuantifierArg, got: {:?}", errs
        );
    }

    #[test]
    fn quantifier_arg_missing_var_list_is_an_error() {
        // `(forall)` with no arguments at all.
        let errs = parse_errors("(forall)");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::QuantifierArg { .. })),
            "expected QuantifierArg, got: {:?}", errs
        );
    }

    #[test]
    fn valid_forall_still_parses() {
        // Sanity-check: well-formed forall must not trigger QuantifierArg.
        let nodes = parse_kif("(forall (?X) (instance ?X Human))");
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn valid_forall_with_row_variable() {
        // Row variables in the quantifier list are allowed.
        let nodes = parse_kif("(forall (@ROW) (P @ROW))");
        assert_eq!(nodes.len(), 1);
    }
}
