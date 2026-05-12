//! Tokenizer for SUO-KIF source.

use std::fmt::Display;

use super::error::KifParseError;
use super::super::Span;

// -- Token types ---------------------------------------------------------------

/// A KIF logical operator keyword.
#[derive(Debug, Clone, PartialEq)]
pub enum OpTok {
    And,
    Or,
    Not,
    Implies,  // =>
    Iff,      // <=>
    Equal,    // equal
    ForAll,   // forall
    Exists,
}

impl OpTok {
    /// The canonical operator name for classification and display.
    pub fn name(&self) -> &'static str {
        match self {
            OpTok::And     => "and",
            OpTok::Or      => "or",
            OpTok::Not     => "not",
            OpTok::Implies => "imp",
            OpTok::Iff     => "iff",
            OpTok::Equal   => "equal",
            OpTok::ForAll  => "forall",
            OpTok::Exists  => "exists",
        }
    }
}

impl Display for OpTok {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpTok::And => write!(f, "and"),
            OpTok::Or => write!(f, "or"),
            OpTok::Not => write!(f, "not"),
            OpTok::Implies => write!(f, "=>"),
            OpTok::Iff => write!(f, "<=>"),
            OpTok::Equal => write!(f, "equal"),
            OpTok::ForAll => write!(f, "forall"),
            OpTok::Exists => write!(f, "exists"),
        }
    }
}

/// A lexical token class produced by the tokenizer.
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
    Operator(OpTok),
}

impl TokenKind {
    /// Whether this token can appear as the head of a KIF s-expression.
    pub fn can_head(&self) -> bool {
        match self {
            TokenKind::Symbol(_)
            | TokenKind::Variable(_)
            | TokenKind::RowVariable(_)
            | TokenKind::Operator(_) => return true,
            _ => return false
        }
    }
}

impl Display for TokenKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenKind::LParen => write!(f, "("),
            TokenKind::RParen => write!(f, ")"),
            TokenKind::Symbol(sym) => write!(f, "{}", sym),
            TokenKind::Variable(var) => write!(f, "{}", var),
            TokenKind::RowVariable(var) => write!(f, "{}", var),
            TokenKind::Str(str) => write!(f, "\"{}\"", str),
            TokenKind::Number(num) => write!(f, "{}", num),
            TokenKind::Operator(op_tok) => write!(f, "{}", op_tok),
        }
    }
}

/// A single token with its source span.
#[derive(Debug, Clone)]
pub struct Token {
    /// The token's lexical class.
    pub kind: TokenKind,
    /// The token's source span.
    pub span: Span,
}

impl Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind)
    }
}

// -- Tokenizer -----------------------------------------------------------------

/// Incremental tokenizer over a KIF source string.
pub struct Tokenizer<'src> {
    chars:  std::str::CharIndices<'src>,
    peeked: Option<(usize, char)>,
    file:   String,
    line:   u32,
    col:    u32,
    // Byte length of the source; closes the final span's end offset when the
    // tokenizer runs off the end of input.
    src_len: usize,
}

impl<'src> Tokenizer<'src> {
    fn new(src: &'src str, file: &str) -> Self {
        let mut chars = src.char_indices();
        let peeked = chars.next();
        Self {
            chars, peeked,
            file: file.to_owned(),
            line: 1, col: 1,
            src_len: src.len(),
        }
    }

    /// Current position as a zero-width point-span. The offset snaps to the
    /// next character's byte position (or end-of-input).
    fn point(&self) -> Span {
        let off = match self.peeked {
            Some((off, _)) => off,
            None           => self.src_len,
        };
        Span::point(self.file.clone(), self.line, self.col, off)
    }

    /// Seal a span whose start was taken earlier by extending its
    /// end fields to the tokenizer's current position.
    fn seal(&self, mut start: Span) -> Span {
        let off = match self.peeked {
            Some((off, _)) => off,
            None           => self.src_len,
        };
        start.end_line   = self.line;
        start.end_col    = self.col;
        start.end_offset = off;
        start
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

    fn read_string(&mut self, start_span: Span) -> Result<Token, (Span, KifParseError)> {
        let mut s = String::from('"');
        loop {
            match self.advance() {
                None => return Err((start_span.clone(), KifParseError::UnterminatedString { span: start_span })),
                Some('"') => { s.push('"'); break; }
                Some(ch)  => s.push(ch),
            }
        }
        let span = self.seal(start_span);
        Ok(Token { kind: TokenKind::Str(s), span })
    }

    /// Read a single-quoted atom `'…'` into a `Symbol`, quotes retained
    /// (`'Socrates'` → `"'Socrates'"`). `\\` and `\'` escapes are kept verbatim.
    fn read_single_quoted(&mut self, start_span: Span) -> Result<Token, (Span, KifParseError)> {
        let mut s = String::from('\'');
        loop {
            match self.advance() {
                None => return Err((start_span.clone(),
                    KifParseError::UnterminatedString { span: start_span })),
                Some('\\') => {
                    s.push('\\');
                    if let Some(c) = self.advance() { s.push(c); }
                }
                Some('\'') => { s.push('\''); break; }
                Some(ch)   => s.push(ch),
            }
        }
        let span = self.seal(start_span);
        Ok(Token { kind: TokenKind::Symbol(s), span })
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
            "and"    => TokenKind::Operator(OpTok::And),
            "or"     => TokenKind::Operator(OpTok::Or),
            "not"    => TokenKind::Operator(OpTok::Not),
            "=>"     => TokenKind::Operator(OpTok::Implies),
            "<=>"    => TokenKind::Operator(OpTok::Iff),
            "equal"  => TokenKind::Operator(OpTok::Equal),
            "forall" => TokenKind::Operator(OpTok::ForAll),
            "exists" => TokenKind::Operator(OpTok::Exists),
            _ => if is_numeric(&w) { TokenKind::Number(w) } else { TokenKind::Symbol(w) },
        }
    }

    fn next_token(&mut self) -> Result<Option<Token>, (Span, KifParseError)> {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() { self.advance(); } else { break; }
        }
        // Start position must be captured before consuming the first char.
        let start = self.point();
        let ch = match self.advance() { None => return Ok(None), Some(c) => c };
        match ch {
            ';'  => { self.skip_line_comment(); self.next_token() }
            '('  => { let span = self.seal(start); Ok(Some(Token { kind: TokenKind::LParen, span })) }
            ')'  => { let span = self.seal(start); Ok(Some(Token { kind: TokenKind::RParen, span })) }
            '"'  => Ok(Some(self.read_string(start)?)),
            '\'' => Ok(Some(self.read_single_quoted(start)?)),
            '?'  => {
                let rest = self.read_word_rest();
                let span = self.seal(start);
                Ok(Some(Token { kind: TokenKind::Variable(format!("?{}", rest)), span }))
            }
            '@'  => {
                let rest = self.read_word_rest();
                let span = self.seal(start);
                Ok(Some(Token { kind: TokenKind::RowVariable(format!("@{}", rest)), span }))
            }
            _    => {
                let word = self.read_word(ch);
                let kind = Self::classify_word(word);
                let span = self.seal(start);
                // Symbols must start with a letter; a Symbol beginning with a
                // non-letter (e.g. `_test`) is a tokenizer error.
                if matches!(&kind, TokenKind::Symbol(_)) && !ch.is_alphabetic() {
                    return Err((span.clone(), KifParseError::UnexpectedChar { ch, span }));
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
pub fn tokenize(src: &str, file: &str) -> (Vec<Token>, Vec<(Span, KifParseError)>) {
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
    crate::log!(Trace, "sigmakee_rs_core::tokenizer", format!("tokenized {} tokens, {} errors from '{}'", tokens.len(), errors.len(), file));
    (tokens, errors)
}

// -- Tests ---------------------------------------------------------------------

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
    fn single_quoted_atom_is_a_symbol_quotes_retained() {
        // A TPTP atomic word the conjecture round-trip emits into KIF;
        // quotes retained so it matches the TPTP-ingested axiom symbol.
        let kinds = toks("(p 's__attribute(a,b)' 'with spaces')");
        assert_eq!(kinds[0], TokenKind::LParen);
        assert!(matches!(&kinds[2], TokenKind::Symbol(s) if s == "'s__attribute(a,b)'"),
            "got {:?}", kinds[2]);
        assert!(matches!(&kinds[3], TokenKind::Symbol(s) if s == "'with spaces'"),
            "got {:?}", kinds[3]);
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
        assert!(matches!(&kinds[1], TokenKind::Operator(OpTok::Implies)));
        assert!(matches!(&kinds[3], TokenKind::Operator(OpTok::Iff)));
        assert!(matches!(&kinds[5], TokenKind::Operator(OpTok::And)));
        assert!(matches!(&kinds[7], TokenKind::Operator(OpTok::Or)));
        assert!(matches!(&kinds[9], TokenKind::Operator(OpTok::Not)));
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
        assert!(matches!(&errors[0].1, KifParseError::UnexpectedChar { ch: '_', .. }));
    }

    // -- Span end-position coverage ------------------------------------------

    #[test]
    fn spans_cover_token_width() {
        // Byte offsets are [start, end); `byte_len` matches token textual width.
        let (tokens, _) = tokenize("(subclass Human Animal)", "test");
        assert_eq!(tokens.len(), 5);
        // `(`  at offset 0 .. 1
        assert_eq!(tokens[0].span.offset,     0);
        assert_eq!(tokens[0].span.end_offset, 1);
        // `subclass`  at offset 1 .. 9
        assert_eq!(tokens[1].span.offset,     1);
        assert_eq!(tokens[1].span.end_offset, 9);
        assert_eq!(tokens[1].span.byte_len(), "subclass".len());
        // `Human`  at offset 10 .. 15
        assert_eq!(tokens[2].span.offset,     10);
        assert_eq!(tokens[2].span.end_offset, 15);
        // `Animal`  at offset 16 .. 22
        assert_eq!(tokens[3].span.offset,     16);
        assert_eq!(tokens[3].span.end_offset, 22);
        // `)`  at offset 22 .. 23
        assert_eq!(tokens[4].span.offset,     22);
        assert_eq!(tokens[4].span.end_offset, 23);
    }

    #[test]
    fn string_span_includes_quotes() {
        let (tokens, _) = tokenize("\"hi\"", "test");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].span.offset,     0);
        assert_eq!(tokens[0].span.end_offset, 4);
    }

    #[test]
    fn variable_span_includes_question_mark() {
        let (tokens, _) = tokenize("?Foo", "test");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].span.byte_len(), 4);
    }

    #[test]
    fn spans_track_line_breaks() {
        let (tokens, _) = tokenize("(a\n  b)", "test");
        // tokens: ( a b )
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[1].span.line,     1);          // `a` on line 1
        assert_eq!(tokens[1].span.end_line, 1);
        assert_eq!(tokens[2].span.line,     2);          // `b` on line 2
        assert_eq!(tokens[2].span.end_line, 2);
        assert_eq!(tokens[2].span.col,      3);          // indented 2 cols
    }
}
