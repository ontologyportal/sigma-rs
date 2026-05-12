//! KIF token-to-AST parser.
use super::error::KifParseError;
use super::tokenizer::{Token, TokenKind, OpTok};

use super::super::{AstNode, OpKind, Span};

/// Recursive-descent parser over a stream of KIF tokens.
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
        else { Span::point(self.file.clone(), 1, 1, 0) }
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
                // Span of the closing `)`, so the list span can cover the
                // whole `( ... )` range.
                let close_span: Span = loop {
                    match self.peek() {
                        Some(t) if matches!(t.kind, TokenKind::RParen) && idx == 0 => {
                            return Err((start_span.clone(), KifParseError::EmptySentence { span: start_span.clone() }));
                        }
                        Some(t) if matches!(t.kind, TokenKind::RParen) && idx > 0 => {
                            let close = t.span.clone();
                            self.advance();
                            break close;
                        }
                        None => {
                            return Err((start_span.clone(), KifParseError::UnbalancedParens { span: start_span }))
                        },
                        Some(Token { kind: TokenKind::Operator(op_tok), span, .. }) if idx > 0 => {
                            // Alphanumeric operator keywords in argument position are plain
                            // symbols (e.g. `(instance equal BinaryRelation)`). The
                            // non-alphanumeric operators `=>` and `<=>` are not valid symbol
                            // names, so they are a parse error here.
                            match op_tok {
                                OpTok::And | OpTok::Or | OpTok::Not
                                | OpTok::Equal | OpTok::ForAll | OpTok::Exists => {
                                    let sym_name = op_tok.name().to_string();
                                    let sym_span = span.clone();
                                    self.advance();
                                    elements.push(AstNode::Symbol { name: sym_name, span: sym_span });
                                }
                                OpTok::Implies | OpTok::Iff => {
                                    let op_str = op_tok.to_string();
                                    return Err((span.clone(), KifParseError::OperatorOutOfPosition {
                                        op: op_str,
                                        span: span.clone(),
                                    }));
                                }
                            }
                        },
                        Some(t) if idx == 0 &&  !t.kind.can_head() => {
                            return Err((t.span.clone(), KifParseError::FirstTerm { span: t.span.clone() }));
                        },
                        _ => elements.push(self.parse_node()?),
                    }
                    idx += 1;
                };

                // Single-term sentence: `(Foo)` or `(and)` — a head with no arguments.
                // Variable-headed single-element lists are exempt: they are valid
                // quantifier variable lists, e.g. `(?X)` in `(forall (?X) body)`.
                if elements.len() == 1
                    && !matches!(elements[0], AstNode::Variable { .. } | AstNode::RowVariable { .. })
                {
                    return Err((start_span.clone(), KifParseError::SingleTermSentence {
                        span: start_span,
                    }));
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

                // Operator arity validation: enforce canonical arities for the
                // eight built-in logical operators.
                //
                // | Operator          | Required arity |
                // |-------------------|----------------|
                // | `not`             | exactly 1      |
                // | `=>`, `<=>`, `equal` | exactly 2   |
                // | `and`, `or`       | at least 2     |
                // | `forall`, `exists`| already checked above (QuantifierArg) |
                if let Some(AstNode::Operator { op, span: op_span }) = elements.first() {
                    let n_args  = elements.len() - 1; // elements[0] is the operator
                    let op_name = op.name().to_string();
                    let op_span = op_span.clone();
                    let arity_err: Option<&'static str> = match op {
                        OpKind::Not => (n_args != 1).then_some("exactly 1"),
                        OpKind::Implies | OpKind::Iff | OpKind::Equal
                            => (n_args != 2).then_some("exactly 2"),
                        OpKind::And | OpKind::Or
                            => (n_args < 2).then_some("at least 2"),
                        // ForAll / Exists are handled by the QuantifierArg check above.
                        OpKind::ForAll | OpKind::Exists => None,
                    };
                    if let Some(expected) = arity_err {
                        return Err((op_span.clone(), KifParseError::OperatorArityMismatch {
                            op:       op_name,
                            expected: expected.to_string(),
                            actual:   n_args,
                            span:     op_span,
                        }));
                    }
                }

                // The list span runs from `(` through `)`.
                let full_span = start_span.join(&close_span);
                Ok(AstNode::List { elements, span: full_span })
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
            TokenKind::Operator(op_tok) => {
                let op: OpKind = match op_tok {
                    OpTok::And => OpKind::And,
                    OpTok::Or => OpKind::Or,
                    OpTok::Iff => OpKind::Iff,
                    OpTok::Implies => OpKind::Implies,
                    OpTok::Not => OpKind::Not,
                    OpTok::Equal => OpKind::Equal,
                    OpTok::ForAll => OpKind::ForAll,
                    OpTok::Exists => OpKind::Exists,
                };
                let node = AstNode::Operator { op, span: tok.span.clone() };
                self.advance(); Ok(node)
            }
        }
    }

    fn parse_all(&mut self) -> (Vec<AstNode>, Vec<(Span, KifParseError)>) {
        let mut nodes  = Vec::new();
        let mut errors = Vec::new();
        while self.peek().is_some() {
            // If parse_node consumed tokens before failing, do not advance
            // again -- that would skip the opening token of the next sentence.
            let pos_before = self.pos;
            match self.parse_node() {
                Ok(node) => {
                    nodes.push(node);
                }
                Err(e) => {
                    errors.push(e);
                    // Only skip a token when parse_node made no progress.
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
    fn operator_keyword_in_argument_position_parses_as_symbol() {
        // `and` in argument position is a plain symbol, not a connective.
        // SUMO uses this in axioms like `(instance and Connective)`.
        let nodes = parse_kif("(instance and Foo)");
        assert_eq!(nodes.len(), 1);
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert_eq!(elements.len(), 3);
            assert!(matches!(&elements[1], AstNode::Symbol { name, .. } if name == "and"));
        } else { panic!("expected List"); }
    }

    #[test]
    fn equal_in_argument_position_parses_as_symbol() {
        // Core SUMO Merge.kif axiom: `(instance equal BinaryRelation)`.
        // `equal` at position > 0 must be a symbol, not the equality operator.
        let nodes = parse_kif("(instance equal BinaryRelation)");
        assert_eq!(nodes.len(), 1);
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert_eq!(elements.len(), 3);
            assert!(matches!(&elements[1], AstNode::Symbol { name, .. } if name == "equal"));
            assert!(matches!(&elements[2], AstNode::Symbol { name, .. } if name == "BinaryRelation"));
        } else { panic!("expected List"); }
    }

    #[test]
    fn operator_keyword_in_nested_argument_position_parses_as_symbol() {
        // Operator keyword as a non-head element of an inner list.
        let nodes = parse_kif("(=> (P ?X) (Q and ?Y))");
        assert_eq!(nodes.len(), 1);
        // Inner `(Q and ?Y)` should parse as a 3-element list with symbol `and`.
        if let AstNode::List { elements, .. } = &nodes[0] {
            if let AstNode::List { elements: inner, .. } = &elements[2] {
                assert_eq!(inner.len(), 3);
                assert!(matches!(&inner[1], AstNode::Symbol { name, .. } if name == "and"));
            } else { panic!("expected inner List"); }
        } else { panic!("expected outer List"); }
    }

    #[test]
    fn operator_keyword_at_head_is_still_an_operator() {
        // Operators in head position (idx == 0) must still parse as logical operators.
        let nodes = parse_kif("(equal ?X ?Y)");
        assert_eq!(nodes.len(), 1);
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(&elements[0], AstNode::Operator { op: OpKind::Equal, .. }));
        } else { panic!("expected List"); }
    }

    #[test]
    fn single_term_sentence_is_an_error() {
        // `(Foo)` — a symbol head with no arguments — is not valid SUMO KIF.
        let errs = parse_errors("(Foo)");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::SingleTermSentence { .. })),
            "expected SingleTermSentence, got: {:?}", errs
        );
    }

    #[test]
    fn zero_arg_and_is_an_error() {
        // `(and)` — zero arguments to `and` — is not valid.
        let errs = parse_errors("(and)");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::SingleTermSentence { .. })),
            "expected SingleTermSentence for (and), got: {:?}", errs
        );
    }

    #[test]
    fn zero_arg_or_is_an_error() {
        // `(or)` — zero arguments to `or` — is not valid.
        let errs = parse_errors("(or)");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::SingleTermSentence { .. })),
            "expected SingleTermSentence for (or), got: {:?}", errs
        );
    }

    #[test]
    fn quantifier_var_list_with_single_var_is_valid() {
        // `(?X)` inside a forall is a legal single-variable var-list; the
        // single-term check must not fire for Variable-headed 1-element lists.
        let nodes = parse_kif("(forall (?X) (instance ?X Human))");
        assert_eq!(nodes.len(), 1, "forall with single-var list must parse: {nodes:?}");
    }

    #[test]
    fn implies_in_argument_position_is_an_error() {
        // `=>` is not a valid SUMO or TPTP symbol name; it must not silently
        // become a symbol when used in argument position.
        let errs = parse_errors("(instance => BinaryRelation)");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::OperatorOutOfPosition { op, .. } if op == "=>")),
            "expected OperatorOutOfPosition for `=>`, got: {:?}", errs
        );
    }

    #[test]
    fn iff_in_argument_position_is_an_error() {
        // `<=>` is not a valid SUMO or TPTP symbol name.
        let errs = parse_errors("(instance <=> BinaryRelation)");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::OperatorOutOfPosition { op, .. } if op == "<=>")),
            "expected OperatorOutOfPosition for `<=>`, got: {:?}", errs
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
        // `(forall)` with no arguments at all — caught as a single-term
        // sentence before the QuantifierArg check even runs.
        let errs = parse_errors("(forall)");
        assert!(
            errs.iter().any(|e| matches!(e,
                KifParseError::SingleTermSentence { .. } | KifParseError::QuantifierArg { .. }
            )),
            "expected SingleTermSentence or QuantifierArg, got: {:?}", errs
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

    // -- Operator arity validation tests -------------------------------------

    /// Helper: extract only `OperatorArityMismatch` errors.
    fn arity_errors(src: &str) -> Vec<KifParseError> {
        parse_errors(src)
            .into_iter()
            .filter(|e| matches!(e, KifParseError::OperatorArityMismatch { .. }))
            .collect()
    }

    #[test]
    fn not_too_many_args_is_error() {
        // `(not X Y)` — `not` requires exactly 1 argument.
        let errs = arity_errors("(not X Y)");
        assert!(!errs.is_empty(), "expected OperatorArityMismatch for (not X Y)");
        if let KifParseError::OperatorArityMismatch { op, expected, actual, .. } = &errs[0] {
            assert_eq!(op, "not");
            assert!(expected.contains("1"), "expected '1' in message, got '{expected}'");
            assert_eq!(*actual, 2);
        }
    }

    #[test]
    fn not_zero_args_caught_by_single_term_check() {
        // `(not)` — caught by SingleTermSentence before arity check.
        let errs = parse_errors("(not)");
        assert!(
            errs.iter().any(|e| matches!(e, KifParseError::SingleTermSentence { .. })),
            "expected SingleTermSentence for (not), got: {:?}", errs
        );
    }

    #[test]
    fn not_one_arg_is_valid() {
        let nodes = parse_kif("(not (instance ?X Dog))");
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn implies_one_arg_is_error() {
        // `(=> A)` — `=>` requires exactly 2 arguments.
        let errs = arity_errors("(=> A)");
        assert!(!errs.is_empty(), "expected OperatorArityMismatch for (=> A)");
        if let KifParseError::OperatorArityMismatch { op, expected, actual, .. } = &errs[0] {
            assert_eq!(op, "=>");
            assert!(expected.contains("2"), "expected '2' in message, got '{expected}'");
            assert_eq!(*actual, 1);
        }
    }

    #[test]
    fn implies_three_args_is_error() {
        // `(=> A B C)` — `=>` requires exactly 2 arguments.
        let errs = arity_errors("(=> A B C)");
        assert!(!errs.is_empty(), "expected OperatorArityMismatch for (=> A B C)");
        if let KifParseError::OperatorArityMismatch { op, actual, .. } = &errs[0] {
            assert_eq!(op, "=>");
            assert_eq!(*actual, 3);
        }
    }

    #[test]
    fn implies_two_args_is_valid() {
        let nodes = parse_kif("(=> (instance ?X Dog) (instance ?X Animal))");
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn iff_one_arg_is_error() {
        let errs = arity_errors("(<=> A)");
        assert!(!errs.is_empty(), "expected OperatorArityMismatch for (<=> A)");
        if let KifParseError::OperatorArityMismatch { op, actual, .. } = &errs[0] {
            assert_eq!(op, "<=>");
            assert_eq!(*actual, 1);
        }
    }

    #[test]
    fn equal_one_arg_is_error() {
        let errs = arity_errors("(equal A)");
        assert!(!errs.is_empty(), "expected OperatorArityMismatch for (equal A)");
        if let KifParseError::OperatorArityMismatch { op, actual, .. } = &errs[0] {
            assert_eq!(op, "equal");
            assert_eq!(*actual, 1);
        }
    }

    #[test]
    fn and_one_arg_is_error() {
        // `(and X)` — `and` requires at least 2 arguments.
        let errs = arity_errors("(and X)");
        assert!(!errs.is_empty(), "expected OperatorArityMismatch for (and X)");
        if let KifParseError::OperatorArityMismatch { op, expected, actual, .. } = &errs[0] {
            assert_eq!(op, "and");
            assert!(expected.contains("2"), "expected '2' in message, got '{expected}'");
            assert_eq!(*actual, 1);
        }
    }

    #[test]
    fn and_two_args_is_valid() {
        let nodes = parse_kif("(and (P ?X) (Q ?X))");
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn and_three_args_is_valid() {
        // `and` allows any number >= 2.
        let nodes = parse_kif("(and (P ?X) (Q ?X) (R ?X))");
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn or_one_arg_is_error() {
        let errs = arity_errors("(or X)");
        assert!(!errs.is_empty(), "expected OperatorArityMismatch for (or X)");
        if let KifParseError::OperatorArityMismatch { op, actual, .. } = &errs[0] {
            assert_eq!(op, "or");
            assert_eq!(*actual, 1);
        }
    }

    #[test]
    fn or_two_args_is_valid() {
        let nodes = parse_kif("(or (P ?X) (Q ?X))");
        assert_eq!(nodes.len(), 1);
    }

    // -- Span coverage -------------------------------------------------------

    #[test]
    fn list_span_covers_full_sentence() {
        let src = "(subclass Human Animal)";
        let nodes = parse_kif(src);
        let span = nodes[0].span();
        // `(` starts at byte 0, `)` ends at byte 23 (exclusive).
        assert_eq!(span.offset,     0);
        assert_eq!(span.end_offset, src.len());
        assert_eq!(span.byte_len(), src.len());
    }

    #[test]
    fn nested_list_span_covers_nested_parens() {
        let src = "(=> (P ?X) (Q ?X))";
        let nodes = parse_kif(src);
        let outer = nodes[0].span();
        assert_eq!(outer.offset,     0);
        assert_eq!(outer.end_offset, src.len());

        // Inner (P ?X) starts at byte 4, ends at 10 (exclusive).
        if let AstNode::List { elements, .. } = &nodes[0] {
            let inner_p = elements[1].span();
            assert_eq!(inner_p.offset,     4);
            assert_eq!(inner_p.end_offset, 10);
        } else { panic!("expected List"); }
    }
}
