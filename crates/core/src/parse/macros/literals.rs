use super::super::{AstNode, Parser, Span};

/// Decode `n__` (numeric) and `str__` (string) symbol encodings
/// introduced by the TPTP encoder, walking the full subtree.
///
/// Encodings:
///   `n__42`       → `42`
///   `n__3_14`     → `3.14`   (underscores encode decimal points)
///   `n__neg_42`   → `-42`    (`neg_` prefix encodes the minus sign)
///   `str__hello`  → `"hello"`
///
/// Only applies to TPTP input; KIF input never contains these encodings.
pub fn decode_tptp_literals(node: &mut AstNode, parser: &Parser) {
    if !matches!(parser, Parser::Tptp { .. }) {
        return;
    }
    decode_tptp_literals_inner(node);
}

fn decode_tptp_literals_inner(node: &mut AstNode) {
    match node {
        AstNode::Symbol { name, span } => {
            if let Some(replacement) = decode_encoded_literal(name, span) {
                *node = replacement;
            }
        }
        AstNode::List { elements, .. } => {
            for el in elements.iter_mut() {
                decode_tptp_literals_inner(el);
            }
        }
        _ => {}
    }
}

fn decode_encoded_literal(name: &str, span: &Span) -> Option<AstNode> {
    if name == "$true"  { return Some(AstNode::Symbol { name: "True".to_owned(),  span: span.clone() }); }
    if name == "$false" { return Some(AstNode::Symbol { name: "False".to_owned(), span: span.clone() }); }
    if let Some(n) = name.strip_prefix("n__") {
        let value = if let Some(pos) = n.strip_prefix("neg_") {
            format!("-{}", pos.replace('_', "."))
        } else {
            n.replace('_', ".")
        };
        return Some(AstNode::Number { value, span: span.clone() });
    }
    if let Some(content) = name.strip_prefix("str__") {
        // `AstNode::Str` stores the value with surrounding double-quotes,
        // matching the KIF tokenizer convention.
        return Some(AstNode::Str { value: format!("\"{}\"", content), span: span.clone() });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::ast::AstNode;

    fn dummy_span() -> Span {
        Span::default()
    }

    fn tptp_parser() -> Parser {
        Parser::Tptp { options: None }
    }

    fn dummy_symbol(name: &str) -> AstNode {
        AstNode::Symbol { name: name.to_owned(), span: dummy_span() }
    }

    // ── decode_encoded_literal ────────────────────────────────────────────

    #[test]
    fn dollar_true_becomes_symbol_true() {
        let result = decode_encoded_literal("$true", &dummy_span());
        assert!(matches!(result, Some(AstNode::Symbol { name, .. }) if name == "True"));
    }

    #[test]
    fn dollar_false_becomes_symbol_false() {
        let result = decode_encoded_literal("$false", &dummy_span());
        assert!(matches!(result, Some(AstNode::Symbol { name, .. }) if name == "False"));
    }

    #[test]
    fn n_integer_becomes_number() {
        let result = decode_encoded_literal("n__42", &dummy_span());
        assert!(matches!(result, Some(AstNode::Number { value, .. }) if value == "42"));
    }

    #[test]
    fn n_decimal_becomes_number() {
        let result = decode_encoded_literal("n__3_14", &dummy_span());
        assert!(matches!(result, Some(AstNode::Number { value, .. }) if value == "3.14"));
    }

    #[test]
    fn n_negative_becomes_number() {
        let result = decode_encoded_literal("n__neg_42", &dummy_span());
        assert!(matches!(result, Some(AstNode::Number { value, .. }) if value == "-42"));
    }

    #[test]
    fn n_negative_decimal_becomes_number() {
        let result = decode_encoded_literal("n__neg_3_14", &dummy_span());
        assert!(matches!(result, Some(AstNode::Number { value, .. }) if value == "-3.14"));
    }

    #[test]
    fn str_encoding_becomes_str_node() {
        let result = decode_encoded_literal("str__hello", &dummy_span());
        // Value includes surrounding double-quotes to match the KIF tokenizer convention.
        assert!(matches!(result, Some(AstNode::Str { value, .. }) if value == "\"hello\""));
    }

    #[test]
    fn plain_symbol_unchanged() {
        assert!(decode_encoded_literal("Bob", &dummy_span()).is_none());
        assert!(decode_encoded_literal("subclassOf", &dummy_span()).is_none());
    }

    // ── decode_tptp_literals (tree walk) ─────────────────────────────────

    #[test]
    fn skipped_for_kif_parser() {
        let mut node = dummy_symbol("$true");
        decode_tptp_literals(&mut node, &Parser::Kif);
        // Must be unchanged — macro is TPTP-only.
        assert!(matches!(node, AstNode::Symbol { name, .. } if name == "$true"));
    }

    #[test]
    fn literal_at_root_replaced() {
        let mut node = dummy_symbol("$false");
        decode_tptp_literals(&mut node, &tptp_parser());
        assert!(matches!(node, AstNode::Symbol { name, .. } if name == "False"));
    }

    #[test]
    fn literal_in_list_replaced() {
        let mut node = AstNode::List {
            elements: vec![
                dummy_symbol("p"),
                dummy_symbol("n__7"),
            ],
            span: dummy_span(),
        };
        decode_tptp_literals(&mut node, &tptp_parser());
        if let AstNode::List { elements, .. } = node {
            assert!(matches!(&elements[0], AstNode::Symbol { name, .. } if name == "p"));
            assert!(matches!(&elements[1], AstNode::Number { value, .. } if value == "7"));
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn literals_in_nested_list_replaced() {
        // (pred (n__1) (str__hello) $true)
        let mut node = AstNode::List {
            elements: vec![
                dummy_symbol("pred"),
                dummy_symbol("n__1"),
                dummy_symbol("str__hello"),
                dummy_symbol("$true"),
            ],
            span: dummy_span(),
        };
        decode_tptp_literals(&mut node, &tptp_parser());
        if let AstNode::List { elements, .. } = node {
            assert!(matches!(&elements[1], AstNode::Number { value, .. } if value == "1"));
            assert!(matches!(&elements[2], AstNode::Str    { value, .. } if value == "\"hello\""));
            assert!(matches!(&elements[3], AstNode::Symbol { name,  .. } if name  == "True"));
        } else {
            panic!("expected List");
        }
    }
}