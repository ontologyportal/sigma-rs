//! Tokenizer for SUO-KIF source.

use std::fmt::Display;

use super::super::Span;
use super::error::KifParseError;
use crate::parse::doc::CommentBlock;

// -- Token types ---------------------------------------------------------------

/// A KIF logical operator keyword.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpTok {
    And,
    Or,
    Not,
    Implies, // =>
    Iff,     // <=>
    Equal,   // equal
    ForAll,  // forall
    Exists,
}

impl OpTok {
    /// The canonical operator name for classification and display.
    pub fn name(&self) -> &'static str {
        match self {
            OpTok::And => "and",
            OpTok::Or => "or",
            OpTok::Not => "not",
            OpTok::Implies => "imp",
            OpTok::Iff => "iff",
            OpTok::Equal => "equal",
            OpTok::ForAll => "forall",
            OpTok::Exists => "exists",
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// A KIF source comment
    Comment(String),
}

impl Default for TokenKind {
    fn default() -> Self {
        Self::Symbol(String::new())
    }
}

impl TokenKind {
    /// Whether this token can appear as the head of a KIF s-expression.
    pub fn can_head(&self) -> bool {
        matches!(
            self,
            TokenKind::Symbol(_)
                | TokenKind::Variable(_)
                | TokenKind::RowVariable(_)
                | TokenKind::Operator(_)
        )
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
            TokenKind::Comment(comm) => write!(f, "; {}", comm),
        }
    }
}

/// A single token with its source span.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
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
    chars: std::str::CharIndices<'src>,
    peeked: Option<(usize, char)>,
    file: String,
    line: u32,
    col: u32,
    // Byte length of the source; closes the final span's end offset when the
    // tokenizer runs off the end of input.
    src_len: usize,
}

impl<'src> Tokenizer<'src> {
    fn new(src: &'src str, file: &str) -> Self {
        let mut chars = src.char_indices();
        let peeked = chars.next();
        Self {
            chars,
            peeked,
            file: file.to_owned(),
            line: 1,
            col: 1,
            src_len: src.len(),
        }
    }

    /// Current position as a zero-width point-span. The offset snaps to the
    /// next character's byte position (or end-of-input).
    fn point(&self) -> Span {
        let off = match self.peeked {
            Some((off, _)) => off,
            None => self.src_len,
        };
        Span::point(self.file.clone(), self.line, self.col, off)
    }

    /// Seal a span whose start was taken earlier by extending its
    /// end fields to the tokenizer's current position.
    fn seal(&self, mut start: Span) -> Span {
        let off = match self.peeked {
            Some((off, _)) => off,
            None => self.src_len,
        };
        start.end_line = self.line;
        start.end_col = self.col;
        start.end_offset = off;
        start
    }

    fn advance(&mut self) -> Option<char> {
        let cur = self.peeked.take();
        self.peeked = self.chars.next();
        if let Some((_, ch)) = cur {
            if ch == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            Some(ch)
        } else {
            None
        }
    }

    fn peek(&self) -> Option<char> {
        self.peeked.map(|(_, ch)| ch)
    }

    fn read_line_comment(&mut self, start_span: Span) -> Token {
        let mut comm = String::new();
        loop {
            match self.advance() {
                None | Some('\n') => {
                    break;
                }
                Some(ch) => comm.push(ch),
            }
        }
        let span = self.seal(start_span);
        Token {
            kind: TokenKind::Comment(comm.trim().to_string()),
            span,
        }
    }

    fn read_string(&mut self, start_span: Span) -> Result<Token, KifParseError> {
        let mut s = String::from('"');
        loop {
            match self.advance() {
                None => return Err(KifParseError::UnterminatedString { span: start_span }),
                Some('"') => {
                    s.push('"');
                    break;
                }
                Some(ch) => s.push(ch),
            }
        }
        let span = self.seal(start_span);
        Ok(Token {
            kind: TokenKind::Str(s),
            span,
        })
    }

    /// Read a single-quoted atom `'…'` into a `Symbol`, quotes retained
    /// (`'Socrates'` → `"'Socrates'"`). `\\` and `\'` escapes are kept verbatim.
    fn read_single_quoted(&mut self, start_span: Span) -> Result<Token, KifParseError> {
        let mut s = String::from('\'');
        loop {
            match self.advance() {
                None => return Err(KifParseError::UnterminatedString { span: start_span }),
                Some('\\') => {
                    s.push('\\');
                    if let Some(c) = self.advance() {
                        s.push(c);
                    }
                }
                Some('\'') => {
                    s.push('\'');
                    break;
                }
                Some(ch) => s.push(ch),
            }
        }
        let span = self.seal(start_span);
        Ok(Token {
            kind: TokenKind::Symbol(s),
            span,
        })
    }

    fn read_word(&mut self, first: char) -> String {
        let mut w = String::new();
        w.push(first);
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() || ch == '(' || ch == ')' || ch == '"' || ch == ';' {
                break;
            }
            self.advance();
            w.push(ch);
        }
        w
    }

    fn read_word_rest(&mut self) -> String {
        let mut w = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() || ch == '(' || ch == ')' || ch == '"' || ch == ';' {
                break;
            }
            self.advance();
            w.push(ch);
        }
        w
    }

    fn classify_word(w: String) -> TokenKind {
        match w.as_str() {
            "and" => TokenKind::Operator(OpTok::And),
            "or" => TokenKind::Operator(OpTok::Or),
            "not" => TokenKind::Operator(OpTok::Not),
            "=>" => TokenKind::Operator(OpTok::Implies),
            "<=>" => TokenKind::Operator(OpTok::Iff),
            "equal" => TokenKind::Operator(OpTok::Equal),
            "forall" => TokenKind::Operator(OpTok::ForAll),
            "exists" => TokenKind::Operator(OpTok::Exists),
            _ => {
                if is_numeric(&w) {
                    TokenKind::Number(w)
                } else {
                    TokenKind::Symbol(w)
                }
            }
        }
    }

    fn next_token(&mut self) -> Result<Option<Token>, KifParseError> {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.advance();
            } else {
                break;
            }
        }
        // Start position must be captured before consuming the first char.
        let start = self.point();
        let ch = match self.advance() {
            None => return Ok(None),
            Some(c) => c,
        };
        match ch {
            ';' => Ok(Some(self.read_line_comment(start))),
            '(' => {
                let span = self.seal(start);
                Ok(Some(Token {
                    kind: TokenKind::LParen,
                    span,
                }))
            }
            ')' => {
                let span = self.seal(start);
                Ok(Some(Token {
                    kind: TokenKind::RParen,
                    span,
                }))
            }
            '"' => Ok(Some(self.read_string(start)?)),
            '\'' => Ok(Some(self.read_single_quoted(start)?)),
            '?' => {
                let rest = self.read_word_rest();
                let span = self.seal(start);
                Ok(Some(Token {
                    kind: TokenKind::Variable(format!("?{}", rest)),
                    span,
                }))
            }
            '@' => {
                let rest = self.read_word_rest();
                let span = self.seal(start);
                Ok(Some(Token {
                    kind: TokenKind::RowVariable(format!("@{}", rest)),
                    span,
                }))
            }
            _ => {
                let word = self.read_word(ch);
                let kind = Self::classify_word(word);
                let span = self.seal(start);
                // Symbols must start with a letter; a Symbol beginning with a
                // non-letter (e.g. `_test`) is a tokenizer error.
                if matches!(&kind, TokenKind::Symbol(_)) && !ch.is_alphabetic() {
                    return Err(KifParseError::UnexpectedChar { ch, span });
                }
                Ok(Some(Token { kind, span }))
            }
        }
    }
}

fn is_numeric(s: &str) -> bool {
    let s = if let Some(stripped) = s.strip_prefix('-') {
        stripped
    } else {
        s
    };
    if s.is_empty() {
        return false;
    }
    let mut has_dot = false;
    for ch in s.chars() {
        if ch == '.' {
            if has_dot {
                return false;
            }
            has_dot = true;
        } else if !ch.is_ascii_digit() {
            return false;
        }
    }
    true
}

/// Tokenize `src` and return all tokens plus any hard errors encountered.
/// Tokenization continues after an error to collect as many issues as possible.
pub fn tokenize(src: &str, file: &str) -> (Vec<Token>, Vec<KifParseError>) {
    let mut tok = Tokenizer::new(src, file);
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    loop {
        match tok.next_token() {
            Ok(None) => break,
            Ok(Some(t)) => tokens.push(t),
            Err(e) => errors.push(e),
        }
    }
    crate::log!(
        Trace,
        "sigmakee_rs_core::tokenizer",
        format!(
            "tokenized {} tokens, {} errors from '{}'",
            tokens.len(),
            errors.len(),
            file
        )
    );
    (tokens, errors)
}

/// [`tokenize`], with `;` comments dropped from the stream -- the historical
/// comment-free view, for consumers that treat comments as pure whitespace
/// and want no [`TokenKind::Comment`] entries to skip over.  Spans and errors
/// are unaffected; only the comment tokens are omitted.
pub fn tokenize_without_comments(src: &str, file: &str) -> (Vec<Token>, Vec<KifParseError>) {
    let (mut tokens, errors) = tokenize(src, file);
    tokens.retain(|t| !matches!(t.kind, TokenKind::Comment(_)));
    (tokens, errors)
}

/// Collect the comment tokens in `tokens` into consolidated
/// [`CommentBlock`]s, in source order.
///
/// Consecutive comments merge into one block when the next comment starts on
/// the line immediately after the block's last line AND no significant
/// (non-comment) token appeared between them in the stream.  A blank line or
/// any intervening code starts a new block.
pub fn comment_blocks(tokens: &[Token]) -> Vec<CommentBlock> {
    let mut blocks: Vec<CommentBlock> = Vec::new();
    // True while the last token seen was the tail of the open block -- any
    // significant token breaks the run.
    let mut run_open = false;
    // Start line of the open block's LAST comment.  Adjacency is judged
    // against this, not `span.end_line`: a comment token's span swallows its
    // terminating newline, so its end position sits on the following line.
    let mut last_line = 0u32;
    for tok in tokens {
        let TokenKind::Comment(text) = &tok.kind else {
            run_open = false;
            continue;
        };
        match blocks.last_mut() {
            Some(block) if run_open && tok.span.line == last_line + 1 => {
                block.text.push('\n');
                block.text.push_str(text);
                block.span.end_line = tok.span.end_line;
                block.span.end_col = tok.span.end_col;
                block.span.end_offset = tok.span.end_offset;
            }
            _ => blocks.push(CommentBlock {
                text: text.clone(),
                span: tok.span.clone(),
            }),
        }
        run_open = true;
        last_line = tok.span.line;
    }
    blocks
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
        assert!(
            matches!(&kinds[2], TokenKind::Symbol(s) if s == "'s__attribute(a,b)'"),
            "got {:?}",
            kinds[2]
        );
        assert!(
            matches!(&kinds[3], TokenKind::Symbol(s) if s == "'with spaces'"),
            "got {:?}",
            kinds[3]
        );
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
    fn line_comment_ingested() {
        let kinds = toks("; this is a comment\n(foo)");
        assert!(matches!(&kinds[0], TokenKind::Comment(s) if s == "this is a comment"));
        assert_eq!(kinds.len(), 4);
    }

    #[test]
    fn end_of_line_comment_ingested() {
        let kinds = toks("(foo) ; this is a comment");
        assert!(matches!(&kinds[3], TokenKind::Comment(s) if s == "this is a comment"));
        assert_eq!(kinds.len(), 4);
    }

    #[test]
    fn tokenize_without_comments_restores_the_comment_free_stream() {
        let src = "; header\n(subclass Dog ; inline\n Mammal) ; trailing";
        let (with, errs_a) = tokenize(src, "test");
        let (without, errs_b) = tokenize_without_comments(src, "test");
        assert!(errs_a.is_empty() && errs_b.is_empty());
        assert!(without
            .iter()
            .all(|t| !matches!(t.kind, TokenKind::Comment(_))));
        // Exactly the non-comment tokens survive, order and spans untouched.
        let significant: Vec<_> = with
            .iter()
            .filter(|t| !matches!(t.kind, TokenKind::Comment(_)))
            .map(|t| (t.kind.clone(), t.span.offset))
            .collect();
        let stripped: Vec<_> = without
            .iter()
            .map(|t| (t.kind.clone(), t.span.offset))
            .collect();
        assert_eq!(significant, stripped);
    }

    #[test]
    fn comment_blocks_merge_consecutive_lines() {
        let (tokens, errors) = tokenize("; line one\n; line two\n(foo)", "test");
        assert!(errors.is_empty());
        let blocks = comment_blocks(&tokens);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "line one\nline two");
        assert_eq!(blocks[0].span.line, 1);
        // The block span covers both comment lines (a comment token's span
        // swallows its terminating newline, so end_line may sit one past).
        assert!(blocks[0].span.end_line >= 2);
    }

    #[test]
    fn comment_blocks_split_on_blank_line() {
        let (tokens, _) = tokenize("; first\n\n; second", "test");
        let blocks = comment_blocks(&tokens);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "first");
        assert_eq!(blocks[1].text, "second");
    }

    #[test]
    fn comment_blocks_split_on_intervening_code() {
        // The comments sit on consecutive lines, but `(foo)` intervenes in
        // the token stream -- they must not merge.
        let (tokens, _) = tokenize("; header\n(foo) ; trailing", "test");
        let blocks = comment_blocks(&tokens);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "header");
        assert_eq!(blocks[1].text, "trailing");
    }

    #[test]
    fn comment_blocks_trailing_then_full_line_merge() {
        // A trailing comment and a full-line comment directly under it are
        // adjacent with nothing between them, so they form one block; the
        // span start records that the block began inline (col > 1).
        let (tokens, _) = tokenize("(foo) ; explains foo\n; and continues", "test");
        let blocks = comment_blocks(&tokens);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "explains foo\nand continues");
        assert!(blocks[0].span.col > 1);
    }

    #[test]
    fn invalid_symbol_start() {
        // Symbols must begin with a letter; `_test` should produce an error.
        let (_, errors) = tokenize("_test", "test");
        assert!(!errors.is_empty(), "expected tokenizer error for '_test'");
        assert!(matches!(
            &errors[0],
            KifParseError::UnexpectedChar { ch: '_', .. }
        ));
    }

    // -- Span end-position coverage ------------------------------------------

    #[test]
    fn spans_cover_token_width() {
        // Byte offsets are [start, end); `byte_len` matches token textual width.
        let (tokens, _) = tokenize("(subclass Human Animal)", "test");
        assert_eq!(tokens.len(), 5);
        // `(`  at offset 0 .. 1
        assert_eq!(tokens[0].span.offset, 0);
        assert_eq!(tokens[0].span.end_offset, 1);
        // `subclass`  at offset 1 .. 9
        assert_eq!(tokens[1].span.offset, 1);
        assert_eq!(tokens[1].span.end_offset, 9);
        assert_eq!(tokens[1].span.byte_len(), "subclass".len());
        // `Human`  at offset 10 .. 15
        assert_eq!(tokens[2].span.offset, 10);
        assert_eq!(tokens[2].span.end_offset, 15);
        // `Animal`  at offset 16 .. 22
        assert_eq!(tokens[3].span.offset, 16);
        assert_eq!(tokens[3].span.end_offset, 22);
        // `)`  at offset 22 .. 23
        assert_eq!(tokens[4].span.offset, 22);
        assert_eq!(tokens[4].span.end_offset, 23);
    }

    #[test]
    fn string_span_includes_quotes() {
        let (tokens, _) = tokenize("\"hi\"", "test");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].span.offset, 0);
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
        assert_eq!(tokens[1].span.line, 1); // `a` on line 1
        assert_eq!(tokens[1].span.end_line, 1);
        assert_eq!(tokens[2].span.line, 2); // `b` on line 2
        assert_eq!(tokens[2].span.end_line, 2);
        assert_eq!(tokens[2].span.col, 3); // indented 2 cols
    }
}
