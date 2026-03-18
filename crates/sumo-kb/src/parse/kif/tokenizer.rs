// crates/sumo-kb/src/parse/kif/tokenizer.rs
// Ported verbatim from sumo-parser-core/src/tokenizer.rs.
// Only change: inline_colorization used for OpKind Display.

use core::fmt;
use serde::{Deserialize, Serialize};
use super::error::{ParseError, Span};

// ── OpKind ────────────────────────────────────────────────────────────────────

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

// ── Token types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    LParen,
    RParen,
    /// A regular symbol identifier (not an operator keyword).
    Symbol(String),
    /// A regular variable: `?name`
    Variable(String),
    /// A row variable: `@name`
    RowVariable(String),
    /// A string literal including surrounding double-quotes.
    Str(String),
    /// A numeric literal (integer or decimal).
    Number(String),
    /// A KIF logical operator keyword.
    Operator(OpKind),
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

// ── Tokenizer ─────────────────────────────────────────────────────────────────

pub struct Tokenizer<'src> {
    #[allow(dead_code)]
    src:    &'src str,
    chars:  std::str::CharIndices<'src>,
    peeked: Option<(usize, char)>,
    file:   String,
    line:   u32,
    col:    u32,
}

impl<'src> Tokenizer<'src> {
    fn new(src: &'src str, file: &str) -> Self {
        let mut chars = src.char_indices();
        let peeked = chars.next();
        Self { src, chars, peeked, file: file.to_owned(), line: 1, col: 1 }
    }

    fn span(&self) -> Span {
        Span { file: self.file.clone(), line: self.line, col: self.col, offset: self.chars.offset().saturating_sub(1) }
    }

    fn advance(&mut self) -> Option<char> {
        let cur = self.peeked.take();
        self.peeked = self.chars.next();
        if let Some((_, ch)) = cur {
            if ch == '\n' { self.line += 1; self.col = 1; } else { self.col += 1; }
            Some(ch)
        } else {
            None
        }
    }

    fn peek(&self) -> Option<char> { self.peeked.map(|(_, ch)| ch) }

    fn skip_line_comment(&mut self) {
        while let Some(ch) = self.peek() {
            self.advance();
            if ch == '\n' { break; }
        }
    }

    fn read_string(&mut self, start_span: Span) -> Result<Token, (Span, ParseError)> {
        let mut s = String::from('"');
        loop {
            match self.advance() {
                None => return Err((start_span.clone(), ParseError::UnterminatedString { span: start_span })),
                Some('"') => { s.push('"'); break; }
                Some(ch)  => s.push(ch),
            }
        }
        Ok(Token { kind: TokenKind::Str(s), span: start_span })
    }

    fn read_word(&mut self, first: char) -> String {
        let mut w = String::new();
        w.push(first);
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() || ch == '(' || ch == ')' || ch == '"' || ch == ';' { break; }
            self.advance();
            w.push(ch);
        }
        w
    }

    fn read_word_rest(&mut self) -> String {
        let mut w = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() || ch == '(' || ch == ')' || ch == '"' || ch == ';' { break; }
            self.advance();
            w.push(ch);
        }
        w
    }

    fn classify_word(w: String) -> TokenKind {
        match w.as_str() {
            "and"    => TokenKind::Operator(OpKind::And),
            "or"     => TokenKind::Operator(OpKind::Or),
            "not"    => TokenKind::Operator(OpKind::Not),
            "=>"     => TokenKind::Operator(OpKind::Implies),
            "<=>"    => TokenKind::Operator(OpKind::Iff),
            "equal"  => TokenKind::Operator(OpKind::Equal),
            "forall" => TokenKind::Operator(OpKind::ForAll),
            "exists" => TokenKind::Operator(OpKind::Exists),
            _ => if is_numeric(&w) { TokenKind::Number(w) } else { TokenKind::Symbol(w) },
        }
    }

    fn next_token(&mut self) -> Result<Option<Token>, (Span, ParseError)> {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() { self.advance(); } else { break; }
        }
        let span = self.span();
        let ch = match self.advance() { None => return Ok(None), Some(c) => c };
        match ch {
            ';'  => { self.skip_line_comment(); self.next_token() }
            '('  => Ok(Some(Token { kind: TokenKind::LParen, span })),
            ')'  => Ok(Some(Token { kind: TokenKind::RParen, span })),
            '"'  => Ok(Some(self.read_string(span)?)),
            '?'  => {
                let rest = self.read_word_rest();
                Ok(Some(Token { kind: TokenKind::Variable(format!("?{}", rest)), span }))
            }
            '@'  => {
                let rest = self.read_word_rest();
                Ok(Some(Token { kind: TokenKind::RowVariable(format!("@{}", rest)), span }))
            }
            _    => {
                let word = self.read_word(ch);
                let kind = Self::classify_word(word);
                // Symbols must start with a letter.  Numbers are handled by
                // classify_word already; operators like `=>` and `<=>` start
                // with punctuation but are matched explicitly above.  Any other
                // word that classifies as a Symbol but begins with a non-letter
                // (e.g. `_test`) is a tokenizer error.
                if matches!(&kind, TokenKind::Symbol(_)) && !ch.is_alphabetic() {
                    return Err((span.clone(), ParseError::UnexpectedChar { ch, span }));
                }
                Ok(Some(Token { kind, span }))
            }
        }
    }
}

fn is_numeric(s: &str) -> bool {
    let s = if s.starts_with('-') { &s[1..] } else { s };
    if s.is_empty() { return false; }
    let mut has_dot = false;
    for ch in s.chars() {
        if ch == '.' { if has_dot { return false; } has_dot = true; }
        else if !ch.is_ascii_digit() { return false; }
    }
    true
}

/// Tokenize `src` and return all tokens plus any hard errors encountered.
/// Tokenization continues after an error to collect as many issues as possible.
pub fn tokenize(src: &str, file: &str) -> (Vec<Token>, Vec<(Span, ParseError)>) {
    let mut tok = Tokenizer::new(src, file);
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    loop {
        match tok.next_token() {
            Ok(None)    => break,
            Ok(Some(t)) => tokens.push(t),
            Err(e)      => errors.push(e),
        }
    }
    log::trace!(target: "sumo_kb::tokenizer", "tokenized {} tokens, {} errors from '{}'",
        tokens.len(), errors.len(), file);
    (tokens, errors)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<TokenKind> {
        let (tokens, errors) = tokenize(src, "test");
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
        tokens.into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn parens() {
        assert_eq!(toks("()"), vec![TokenKind::LParen, TokenKind::RParen]);
    }

    #[test]
    fn symbol() {
        let kinds = toks("(subclass Human Animal)");
        assert!(matches!(&kinds[1], TokenKind::Symbol(s) if s == "subclass"));
        assert!(matches!(&kinds[2], TokenKind::Symbol(s) if s == "Human"));
    }

    #[test]
    fn operators() {
        let kinds = toks("(=> (<=> (and (or (not)))))");
        assert!(matches!(&kinds[1], TokenKind::Operator(OpKind::Implies)));
        assert!(matches!(&kinds[3], TokenKind::Operator(OpKind::Iff)));
        assert!(matches!(&kinds[5], TokenKind::Operator(OpKind::And)));
        assert!(matches!(&kinds[7], TokenKind::Operator(OpKind::Or)));
        assert!(matches!(&kinds[9], TokenKind::Operator(OpKind::Not)));
    }

    #[test]
    fn variables() {
        let kinds = toks("?X @ROW");
        assert!(matches!(&kinds[0], TokenKind::Variable(s) if s == "?X"));
        assert!(matches!(&kinds[1], TokenKind::RowVariable(s) if s == "@ROW"));
    }

    #[test]
    fn numbers() {
        let kinds = toks("42 3.14 -1");
        assert!(matches!(&kinds[0], TokenKind::Number(s) if s == "42"));
        assert!(matches!(&kinds[1], TokenKind::Number(s) if s == "3.14"));
        assert!(matches!(&kinds[2], TokenKind::Number(s) if s == "-1"));
    }

    #[test]
    fn string_literal() {
        let kinds = toks("\"hello world\"");
        assert!(matches!(&kinds[0], TokenKind::Str(s) if s == "\"hello world\""));
    }

    #[test]
    fn comment_skipped() {
        let kinds = toks("; this is a comment\n(foo)");
        assert_eq!(kinds.len(), 3);
    }

    #[test]
    fn invalid_symbol_start() {
        // Symbols must begin with a letter; `_test` should produce an error.
        let (_, errors) = tokenize("_test", "test");
        assert!(!errors.is_empty(), "expected tokenizer error for '_test'");
        assert!(matches!(&errors[0].1, ParseError::UnexpectedChar { ch: '_', .. }));
    }
}
