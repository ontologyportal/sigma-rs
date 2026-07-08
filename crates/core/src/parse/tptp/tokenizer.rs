//! TPTP tokenizer.
//!
//! Operator tokens are resolved into the local [`TptpOpTok`] enum; the parser
//! maps them onto its own AST nodes.

use std::fmt::Display;

use super::error::TptpParseError;
use super::super::Span;
use super::super::doc::MetaNode;
use super::super::ast::AstNode;

/// TPTP multi-character connective tokens.
///
/// Compound punctuation sequences the tokenizer collapses into a single token.
/// Single-character punctuation (`|`, `&`, `~`, `=`, `!`, `?`, `^`, `@`) stays
/// as its own [`TokenKind`] variant so the parser can distinguish quantifier
/// introducers from connectives.
#[derive(Debug, Clone, PartialEq)]
pub enum TptpOpTok {
    Implies,      // =>
    RevImplies,   // <=
    Iff,          // <=>
    Xor,          // <~>
    Nor,          // ~|
    Nand,         // ~&
    NotEqual,     // !=
    // THF only
    Choice,       // @+
    Description,  // @-
}

impl Display for TptpOpTok {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TptpOpTok::Choice => write!(f, "=>"),
            TptpOpTok::Description => write!(f, "@-"),
            TptpOpTok::Iff => write!(f, "<=>"),
            TptpOpTok::Implies => write!(f, "=>"),
            TptpOpTok::Nand => write!(f, "~&"),
            TptpOpTok::Nor => write!(f, "~|"),
            TptpOpTok::NotEqual => write!(f, "!="),
            TptpOpTok::RevImplies => write!(f, "<="),
            TptpOpTok::Xor => write!(f, "<~>"),
        }
    }
}

// Token types

/// The lexical category of a single TPTP token.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Punctuation
    LParen,    // (
    RParen,    // )
    LBracket,  // [
    RBracket,  // ]
    Comma,     // ,
    Dot,       // .
    Colon,     // :
    Semicolon, // ;  (sequent separator in TFF)

    // Single-character operators
    /// `|` — disjunction.
    Pipe,
    /// `&` — conjunction.
    Ampersand,
    /// `~` — negation prefix.
    Tilde,
    /// `=` — equality predicate.
    Equals,
    /// `!` — universal quantifier.
    Bang,
    /// `?` — existential quantifier.
    Question,
    /// `^` — lambda (THF).
    Caret,
    /// `@` — apply (THF).
    At,
    /// `>` — type-arrow in TFF/THF.
    TypeArrow,

    // Multi-character connective operators
    /// A compound punctuation connective; see [`TptpOpTok`].
    Operator(TptpOpTok),

    // Identifiers
    /// `lower_word` — starts with `[a-z]`, continues `[a-zA-Z0-9_]`. Also the
    /// syntactic category for functor and predicate names.
    LowerWord(String),
    /// `upper_word` — starts with `[A-Z]`, continues `[a-zA-Z0-9_]`. Variables
    /// in FOF/TFF/THF are always upper-words.
    UpperWord(String),
    /// `$lower_word` — a defined word such as `$true`, `$false`, `$ite`.
    DollarWord(String),
    /// `$$lower_word` — a system word; solver-specific extension point.
    DollarDollarWord(String),

    // Literals
    /// `'sq_char*'` — single-quoted atom; the surrounding quotes are retained
    /// in the string.
    SingleQuoted(String),
    /// `"sq_char*"` — double-quoted string (a distinct-object).
    DoubleQuoted(String),
    /// Integer literal (decimal, no leading zeros except `0` itself).
    Integer(String),
    /// Rational literal `p/q`.
    Rational(String),
    /// Real literal — decimal point or exponent notation.
    Real(String),
}

impl Display for TokenKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenKind::Ampersand => write!(f, "&"),
            TokenKind::At => write!(f, "@"),
            TokenKind::Bang => write!(f, "!"),
            TokenKind::Caret => write!(f, "^"),
            TokenKind::Colon => write!(f, ":"),
            TokenKind::Comma => write!(f, ","),
            TokenKind::DollarDollarWord(word) => write!(f, "$${}", word),
            TokenKind::DollarWord(word) => write!(f, "${}", word),
            TokenKind::Dot => write!(f, "."),
            TokenKind::DoubleQuoted(word) => write!(f, "\"{}\"", word),
            TokenKind::Equals => write!(f, "="),
            TokenKind::Integer(int) => write!(f, "{}", int),
            TokenKind::LBracket => write!(f, "["),
            TokenKind::LParen => write!(f, "("),
            TokenKind::LowerWord(word) => write!(f, "{}", word),
            TokenKind::Operator(op) => write!(f, "{}", op),
            TokenKind::Pipe => write!(f, "|"),
            TokenKind::Question => write!(f, "?"),
            TokenKind::RBracket => write!(f, "]"),
            TokenKind::RParen => write!(f, ")"),
            TokenKind::Rational(rat) => write!(f, "{}", rat),
            TokenKind::Real(real) => write!(f, "{}", real),
            TokenKind::Semicolon => write!(f, ";"),
            TokenKind::SingleQuoted(word) => write!(f, "'{}'", word),
            TokenKind::Tilde => write!(f, "~"),
            TokenKind::TypeArrow => write!(f, ">"),
            TokenKind::UpperWord(word) => write!(f, "{}", word),
        }
    }
}

/// A lexed TPTP token together with its source span.
#[derive(Debug, Clone)]
pub(crate) struct Token {
    /// The token's classification.
    pub kind: TokenKind,
    /// Source location spanning the token's characters.
    pub span: Span,
}

impl Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind)
    }
}

// Tokenizer

/// Streaming lexer over TPTP source that yields [`Token`]s.
pub struct Tokenizer<'src> {
    chars:   std::str::CharIndices<'src>,
    peeked:  Option<(usize, char)>,
    file:    String,
    line:    u32,
    col:     u32,
    src_len: usize,
    /// Header pragma comments recognized while skipping `%`-comments (today:
    /// `% Status : <word>`) — the side-channel that lets the SZS expected
    /// outcome ride out of the tokenizer alongside the token stream, without
    /// giving line comments a token of their own.
    metas:   Vec<MetaNode>,
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
            metas: Vec::new(),
        }
    }

    // ── Position helpers ──────────────────────────────────────────

    fn point(&self) -> Span {
        let off = self.peeked.map(|(o, _)| o).unwrap_or(self.src_len);
        Span::point(self.file.clone(), self.line, self.col, off)
    }

    fn seal(&self, mut start: Span) -> Span {
        let off = self.peeked.map(|(o, _)| o).unwrap_or(self.src_len);
        start.end_line   = self.line;
        start.end_col    = self.col;
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

    // ── Comment skipping ──────────────────────────────────────────

    /// Skip a `%`-style line comment (TPTP) — identical to KIF `;` comments.
    /// `start` is the comment's opening `%` position, so a recognized header
    /// pragma (`% Status : Theorem`) gets a span covering the whole line.
    fn skip_line_comment(&mut self, start: Span) {
        let mut text = String::new();
        while let Some(ch) = self.peek() {
            self.advance();
            if ch == '\n' {
                break;
            }
            text.push(ch);
        }
        self.record_status_pragma(&text, start);
    }

    /// Recognize a `Status : <word>` TPTP header pragma inside one `%`
    /// comment's text (the leading `%` is already stripped) and, if found,
    /// push a `status` [`MetaNode`] onto the side channel — the SDK's SZS
    /// grading path reads this back off the parsed document.
    fn record_status_pragma(&mut self, text: &str, span: Span) {
        let Some(rest) = text.trim_start().strip_prefix("Status") else { return };
        let Some(word) = rest.trim_start().strip_prefix(':') else { return };
        let Some(status) = word.split_whitespace().next() else { return };
        self.metas.push(MetaNode {
            key:  "status".into(),
            args: vec![AstNode::Symbol { name: status.to_string(), span: span.clone() }],
            span,
        });
    }

    /// Skip a `/* … */` block comment (TPTP extension).
    /// Returns an error if EOF is reached before `*/`.
    fn skip_block_comment(
        &mut self,
        start: Span,
    ) -> Result<(), (Span, TptpParseError)> {
        loop {
            match self.advance() {
                None => {
                    return Err((
                        start.clone(),
                        TptpParseError::UnterminatedBlockComment { span: start },
                    ))
                }
                Some('*') if self.peek() == Some('/') => {
                    self.advance(); // consume '/'
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    // ── String / quoted-atom readers ─────────────────────────────

    /// Read a single-quoted atom `'…'` (TPTP *single_quoted*).
    /// The outer quotes are retained: `'Socrates'` → `"'Socrates'"`.
    fn read_single_quoted(
        &mut self,
        start: Span,
    ) -> Result<Token, (Span, TptpParseError)> {
        let mut s = String::from('\'');
        loop {
            match self.advance() {
                None => {
                    return Err((
                        start.clone(),
                        TptpParseError::UnterminatedString { span: start },
                    ))
                }
                Some('\\') => {
                    // Escape: only `\\` and `\'` are valid in TPTP.
                    match self.advance() {
                        Some(esc @ ('\\' | '\'')) => {
                            s.push('\\');
                            s.push(esc);
                        }
                        Some(bad) => {
                            let sp = self.seal(start.clone());
                            return Err((
                                sp.clone(),
                                TptpParseError::InvalidEscape { ch: bad, span: sp },
                            ));
                        }
                        None => {
                            return Err((
                                start.clone(),
                                TptpParseError::UnterminatedString { span: start },
                            ))
                        }
                    }
                }
                Some('\'') => {
                    s.push('\'');
                    break;
                }
                Some(ch) => s.push(ch),
            }
        }
        let span = self.seal(start);
        Ok(Token { kind: TokenKind::SingleQuoted(s), span })
    }

    /// Read a double-quoted string `"…"` (TPTP *double_quoted*).
    fn read_double_quoted(
        &mut self,
        start: Span,
    ) -> Result<Token, (Span, TptpParseError)> {
        let mut s = String::from('"');
        loop {
            match self.advance() {
                None => {
                    return Err((
                        start.clone(),
                        TptpParseError::UnterminatedString { span: start },
                    ))
                }
                Some('\\') => {
                    match self.advance() {
                        Some(esc @ ('\\' | '"')) => {
                            s.push('\\');
                            s.push(esc);
                        }
                        Some(bad) => {
                            let sp = self.seal(start.clone());
                            return Err((
                                sp.clone(),
                                TptpParseError::InvalidEscape { ch: bad, span: sp },
                            ));
                        }
                        None => {
                            return Err((
                                start.clone(),
                                TptpParseError::UnterminatedString { span: start },
                            ))
                        }
                    }
                }
                Some('"') => {
                    s.push('"');
                    break;
                }
                Some(ch) => s.push(ch),
            }
        }
        let span = self.seal(start);
        Ok(Token { kind: TokenKind::DoubleQuoted(s), span })
    }

    // ── Word / number readers ─────────────────────────────────────

    /// Read the *rest* of an alphanumeric word (after the first char has
    /// already been consumed).  Stops at whitespace, brackets, or TPTP
    /// punctuation.
    fn read_word_rest(&mut self) -> String {
        let mut w = String::new();
        while let Some(ch) = self.peek() {
            if is_word_continue(ch) {
                self.advance();
                w.push(ch);
            } else {
                break;
            }
        }
        w
    }

    /// Read the continuation of a numeric literal after the first digit (or
    /// leading `-`).  Consumes digits plus the characters that can appear
    /// inside TPTP rational (`/`) and real (`.`, `e`/`E`, and the `+`/`-`
    /// exponent sign) literals.
    fn read_number_rest(&mut self) -> String {
        let mut w = String::new();
        while let Some(ch) = self.peek() {
            match ch {
                '0'..='9' | '.' | '/' => {
                    self.advance();
                    w.push(ch);
                }
                'e' | 'E' => {
                    self.advance();
                    w.push(ch);
                    // Consume optional exponent sign.
                    if matches!(self.peek(), Some('+') | Some('-')) {
                        let sign = self.advance().unwrap();
                        w.push(sign);
                    }
                }
                _ => break,
            }
        }
        w
    }

    /// Classify a complete lower-word or numeric string into a `TokenKind`.
    ///
    /// Language keywords (`fof`, `cnf`, `tff`, …) surface as `LowerWord`; the
    /// parser distinguishes them by position.
    fn classify_lower(w: String) -> TokenKind {
        if is_numeric_str(&w) {
            classify_number(w)
        } else {
            TokenKind::LowerWord(w)
        }
    }

    // ── Core dispatch ─────────────────────────────────────────────

    fn next_token(&mut self) -> Result<Option<Token>, (Span, TptpParseError)> {
        // Skip whitespace.
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.advance();
            } else {
                break;
            }
        }

        let start = self.point();
        let ch = match self.advance() {
            None    => return Ok(None),
            Some(c) => c,
        };

        match ch {
            // ── Comments ──────────────────────────────────────────
            '%' => {
                self.skip_line_comment(self.seal(start.clone()));
                self.next_token()
            }
            '/' if self.peek() == Some('*') => {
                self.advance(); // consume '*'
                self.skip_block_comment(start)?;
                self.next_token()
            }

            // ── Parentheses / brackets ────────────────────────────
            '(' => Ok(Some(Token { kind: TokenKind::LParen,    span: self.seal(start) })),
            ')' => Ok(Some(Token { kind: TokenKind::RParen,    span: self.seal(start) })),
            '[' => Ok(Some(Token { kind: TokenKind::LBracket,  span: self.seal(start) })),
            ']' => Ok(Some(Token { kind: TokenKind::RBracket,  span: self.seal(start) })),

            // ── Mundane punctuation ───────────────────────────────
            ',' => Ok(Some(Token { kind: TokenKind::Comma,     span: self.seal(start) })),
            '.' => Ok(Some(Token { kind: TokenKind::Dot,       span: self.seal(start) })),
            ':' => Ok(Some(Token { kind: TokenKind::Colon,     span: self.seal(start) })),
            ';' => Ok(Some(Token { kind: TokenKind::Semicolon, span: self.seal(start) })),

            // ── Simple connectives ────────────────────────────────
            '|' => Ok(Some(Token { kind: TokenKind::Pipe,      span: self.seal(start) })),
            '&' => Ok(Some(Token { kind: TokenKind::Ampersand, span: self.seal(start) })),
            '^' => Ok(Some(Token { kind: TokenKind::Caret,     span: self.seal(start) })),

            // ── `~`  →  `~`, `~|` (NOR), or `~&` (NAND) ─────────
            '~' => {
                let kind = match self.peek() {
                    Some('|') => { self.advance(); TokenKind::Operator(TptpOpTok::Nor)  }
                    Some('&') => { self.advance(); TokenKind::Operator(TptpOpTok::Nand) }
                    _         => TokenKind::Tilde,
                };
                Ok(Some(Token { kind, span: self.seal(start) }))
            }

            // ── `=`  →  `=` (equality) or `=>` (implication) ────
            '=' => {
                let kind = if self.peek() == Some('>') {
                    self.advance();
                    TokenKind::Operator(TptpOpTok::Implies)
                } else {
                    TokenKind::Equals
                };
                Ok(Some(Token { kind, span: self.seal(start) }))
            }

            // ── `!`  →  `!` or `!=` ──────────────────────────────
            '!' => {
                let kind = if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::Operator(TptpOpTok::NotEqual)
                } else {
                    TokenKind::Bang
                };
                Ok(Some(Token { kind, span: self.seal(start) }))
            }

            // ── `?`  →  bare `?` (existential) ───────────────────
            '?' => Ok(Some(Token { kind: TokenKind::Question, span: self.seal(start) })),

            // ── `@`  →  `@`, `@+` (choice), `@-` (description) ──
            '@' => {
                let kind = match self.peek() {
                    Some('+') => { self.advance(); TokenKind::Operator(TptpOpTok::Choice)      }
                    Some('-') => { self.advance(); TokenKind::Operator(TptpOpTok::Description) }
                    _         => TokenKind::At,
                };
                Ok(Some(Token { kind, span: self.seal(start) }))
            }

            // ── `>`  →  type-arrow in TFF/THF  ────────────────────
            '>' => Ok(Some(Token { kind: TokenKind::TypeArrow, span: self.seal(start) })),

            // ── `<`  →  `<=>` (iff), `<=` (rev-implies), `<~>` (xor) ─
            '<' => {
                let kind = match self.peek() {
                    Some('=') => {
                        self.advance();
                        if self.peek() == Some('>') {
                            self.advance();
                            TokenKind::Operator(TptpOpTok::Iff)
                        } else {
                            TokenKind::Operator(TptpOpTok::RevImplies)
                        }
                    }
                    Some('~') => {
                        self.advance();
                        if self.peek() == Some('>') {
                            self.advance();
                            TokenKind::Operator(TptpOpTok::Xor)
                        } else {
                            let sp = self.seal(start.clone());
                            return Err((sp.clone(), TptpParseError::UnexpectedChar { ch: '~', span: sp }));
                        }
                    }
                    _ => {
                        let sp = self.seal(start.clone());
                        return Err((sp.clone(), TptpParseError::UnexpectedChar { ch: '<', span: sp }));
                    }
                };
                Ok(Some(Token { kind, span: self.seal(start) }))
            }

            // ── `$` words — defined and system ───────────────────
            '$' => {
                if self.peek() == Some('$') {
                    // `$$lower_word`
                    self.advance();
                    match self.peek() {
                        Some(c) if c.is_lowercase() => {
                            self.advance();
                            let rest = self.read_word_rest();
                            let span = self.seal(start);
                            let name = format!("$${}{}",  c, rest);
                            Ok(Some(Token { kind: TokenKind::DollarDollarWord(name), span }))
                        }
                        _ => {
                            let sp = self.seal(start.clone());
                            Err((sp.clone(), TptpParseError::UnexpectedChar { ch: '$', span: sp }))
                        }
                    }
                } else {
                    // `$lower_word`
                    match self.peek() {
                        Some(c) if c.is_lowercase() => {
                            self.advance();
                            let rest = self.read_word_rest();
                            let span = self.seal(start);
                            let name = format!("${}{}",  c, rest);
                            Ok(Some(Token { kind: TokenKind::DollarWord(name), span }))
                        }
                        _ => {
                            let sp = self.seal(start.clone());
                            Err((sp.clone(), TptpParseError::UnexpectedChar { ch: '$', span: sp }))
                        }
                    }
                }
            }

            // ── Quoted atoms and strings ──────────────────────────
            '\'' => self.read_single_quoted(start).map(Some),
            '"'  => self.read_double_quoted(start).map(Some),

            // ── Numbers and identifiers ───────────────────────────
            c => {
                // Upper-word → variable.
                if c.is_uppercase() {
                    let rest = self.read_word_rest();
                    let name = format!("{}{}", c, rest);
                    let span = self.seal(start);
                    return Ok(Some(Token { kind: TokenKind::UpperWord(name), span }));
                }

                // Lower-word: alphanumeric words starting with a letter.
                if c.is_lowercase() {
                    let rest = self.read_word_rest();
                    let word = format!("{}{}", c, rest);
                    let kind = Self::classify_lower(word);
                    let span = self.seal(start);
                    return Ok(Some(Token { kind, span }));
                }

                // Integer, rational, real, or signed number.
                if c.is_ascii_digit() || c == '-' {
                    let rest = self.read_number_rest();
                    let word = format!("{}{}", c, rest);
                    let kind = Self::classify_lower(word);
                    let span = self.seal(start);
                    return Ok(Some(Token { kind, span }));
                }

                // Anything else is an unexpected character.
                let sp = self.seal(start.clone());
                Err((sp.clone(), TptpParseError::UnexpectedChar { ch: c, span: sp }))
            }
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Characters that may *continue* (but not start) an alphanumeric word.
fn is_word_continue(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Classify a numeric string as a rational, real, or integer literal.
fn classify_number(s: String) -> TokenKind {
    if s.contains('/') {
        return TokenKind::Rational(s);
    }
    if s.contains('.') || s.contains('e') || s.contains('E') {
        return TokenKind::Real(s);
    }
    TokenKind::Integer(s)
}

/// Returns true if `s` could be any kind of numeric literal.
fn is_numeric_str(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    if !chars.next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        return false;
    }
    chars.all(|c| c.is_ascii_digit() || matches!(c, '.' | '/' | 'e' | 'E' | '+' | '-'))
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Tokenize TPTP source `src` from virtual file `file`.
///
/// Returns the tokens and any errors. Errors do not abort tokenization; they
/// accumulate so the caller can report multiple problems at once.
pub fn tokenize(src: &str, file: &str) -> (Vec<Token>, Vec<(Span, TptpParseError)>) {
    let (tokens, errors, _metas) = tokenize_with_meta(src, file);
    (tokens, errors)
}

/// Like [`tokenize`], but also returns header pragma [`MetaNode`]s recognized
/// while skipping `%`-comments (today: `% Status : <word>`) — the TPTP
/// document parser folds these into its `Vec<DocItem>` output alongside the
/// parsed statements.
pub fn tokenize_with_meta(src: &str, file: &str)
    -> (Vec<Token>, Vec<(Span, TptpParseError)>, Vec<MetaNode>)
{
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

    crate::log!(Trace,
        "sigmakee_rs_core::tptp_tokenizer",
        format!(
            "tokenized {} tokens, {} errors from '{}'",
            tokens.len(), errors.len(), file
        )
    );

    (tokens, errors, tok.metas)
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

    // ── Basic punctuation ────────────────────────────────────────

    #[test]
    fn parens_and_brackets() {
        assert_eq!(
            toks("()[]"),
            vec![
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::LBracket,
                TokenKind::RBracket,
            ]
        );
    }

    #[test]
    fn comma_dot_colon() {
        let kinds = toks("a , b . c : d");
        assert!(matches!(&kinds[1], TokenKind::Comma));
        assert!(matches!(&kinds[3], TokenKind::Dot));
        assert!(matches!(&kinds[5], TokenKind::Colon));
    }

    // ── Identifiers ──────────────────────────────────────────────

    #[test]
    fn lower_word() {
        let kinds = toks("subclass");
        assert!(matches!(&kinds[0], TokenKind::LowerWord(s) if s == "subclass"));
    }

    #[test]
    fn upper_word_is_variable() {
        let kinds = toks("X Y_1 Foo");
        assert!(matches!(&kinds[0], TokenKind::UpperWord(s) if s == "X"));
        assert!(matches!(&kinds[1], TokenKind::UpperWord(s) if s == "Y_1"));
        assert!(matches!(&kinds[2], TokenKind::UpperWord(s) if s == "Foo"));
    }

    #[test]
    fn dollar_words() {
        let kinds = toks("$true $false $ite $$domain");
        assert!(matches!(&kinds[0], TokenKind::DollarWord(s) if s == "$true"));
        assert!(matches!(&kinds[1], TokenKind::DollarWord(s) if s == "$false"));
        assert!(matches!(&kinds[2], TokenKind::DollarWord(s) if s == "$ite"));
        assert!(matches!(&kinds[3], TokenKind::DollarDollarWord(s) if s == "$$domain"));
    }

    // ── Quoted forms ─────────────────────────────────────────────

    #[test]
    fn single_quoted_atom() {
        let kinds = toks("'Socrates'");
        assert!(matches!(&kinds[0], TokenKind::SingleQuoted(s) if s == "'Socrates'"));
    }

    #[test]
    fn double_quoted_string() {
        let kinds = toks("\"hello world\"");
        assert!(matches!(&kinds[0], TokenKind::DoubleQuoted(s) if s == "\"hello world\""));
    }

    #[test]
    fn single_quoted_with_escape() {
        let kinds = toks(r"'it\'s'");
        assert!(matches!(&kinds[0], TokenKind::SingleQuoted(s) if s == r"'it\'s'"));
    }

    // ── Numbers ──────────────────────────────────────────────────

    #[test]
    fn integers() {
        let kinds = toks("0 42 -7");
        assert!(matches!(&kinds[0], TokenKind::Integer(s) if s == "0"));
        assert!(matches!(&kinds[1], TokenKind::Integer(s) if s == "42"));
        assert!(matches!(&kinds[2], TokenKind::Integer(s) if s == "-7"));
    }

    #[test]
    fn rational() {
        let kinds = toks("3/4");
        assert!(matches!(&kinds[0], TokenKind::Rational(s) if s == "3/4"));
    }

    #[test]
    fn real_decimal() {
        let kinds = toks("3.14 1.0e10");
        assert!(matches!(&kinds[0], TokenKind::Real(s) if s == "3.14"));
        assert!(matches!(&kinds[1], TokenKind::Real(s) if s == "1.0e10"));
    }

    // ── Connective operators ─────────────────────────────────────

    #[test]
    fn binary_connectives() {
        let kinds = toks("| & ~");
        assert_eq!(kinds[0], TokenKind::Pipe);
        assert_eq!(kinds[1], TokenKind::Ampersand);
        assert_eq!(kinds[2], TokenKind::Tilde);
    }

    #[test]
    fn compound_connectives() {
        let kinds = toks("<=> <= <~> ~| ~& !=");
        assert!(matches!(&kinds[0], TokenKind::Operator(TptpOpTok::Iff)));
        assert!(matches!(&kinds[1], TokenKind::Operator(TptpOpTok::RevImplies)));
        assert!(matches!(&kinds[2], TokenKind::Operator(TptpOpTok::Xor)));
        assert!(matches!(&kinds[3], TokenKind::Operator(TptpOpTok::Nor)));
        assert!(matches!(&kinds[4], TokenKind::Operator(TptpOpTok::Nand)));
        assert!(matches!(&kinds[5], TokenKind::Operator(TptpOpTok::NotEqual)));
    }

    #[test]
    fn equality() {
        let kinds = toks("a = b");
        assert_eq!(kinds[1], TokenKind::Equals);
    }

    #[test]
    fn implication() {
        let kinds = toks("=>");
        assert!(matches!(&kinds[0], TokenKind::Operator(TptpOpTok::Implies)));
        // Plain `=` must still work when not followed by `>`.
        let kinds2 = toks("a = b");
        assert_eq!(kinds2[1], TokenKind::Equals);
    }

    #[test]
    fn quantifiers_and_lambda() {
        let kinds = toks("! ? ^");
        assert_eq!(kinds[0], TokenKind::Bang);
        assert_eq!(kinds[1], TokenKind::Question);
        assert_eq!(kinds[2], TokenKind::Caret);
    }

    #[test]
    fn thf_apply_and_choice() {
        let kinds = toks("@ @+ @-");
        assert_eq!(kinds[0], TokenKind::At);
        assert!(matches!(&kinds[1], TokenKind::Operator(TptpOpTok::Choice)));
        assert!(matches!(&kinds[2], TokenKind::Operator(TptpOpTok::Description)));
    }

    // ── Comments ─────────────────────────────────────────────────

    #[test]
    fn line_comment_skipped() {
        // A `%` comment should vanish entirely.
        let kinds = toks("% this is a TPTP comment\nfoo");
        assert_eq!(kinds.len(), 1);
        assert!(matches!(&kinds[0], TokenKind::LowerWord(s) if s == "foo"));
    }

    #[test]
    fn block_comment_skipped() {
        let kinds = toks("/* block */ bar");
        assert_eq!(kinds.len(), 1);
        assert!(matches!(&kinds[0], TokenKind::LowerWord(s) if s == "bar"));
    }

    #[test]
    fn unterminated_block_comment_is_error() {
        let (_, errors) = tokenize("/* oops", "test");
        assert!(!errors.is_empty());
        assert!(matches!(&errors[0].1, TptpParseError::UnterminatedBlockComment { .. }));
    }

    // ── A real-ish TPTP formula ───────────────────────────────────

    #[test]
    fn fof_header() {
        // fof(name, axiom, formula).
        let kinds = toks("fof(ax1, axiom, ![X]: p(X)).");
        // fof ( ax1 , axiom , ! [ X ] : p ( X ) ) .
        //  0   1  2  3   4   5 6  7 8  9 10 11 12 13
        assert!(matches!(&kinds[0],  TokenKind::LowerWord(s) if s == "fof"));
        assert_eq!(kinds[1],  TokenKind::LParen);
        assert!(matches!(&kinds[2],  TokenKind::LowerWord(s) if s == "ax1"));
        assert_eq!(kinds[3],  TokenKind::Comma);
        assert!(matches!(&kinds[4],  TokenKind::LowerWord(s) if s == "axiom"));
        assert_eq!(kinds[5],  TokenKind::Comma);
        assert_eq!(kinds[6],  TokenKind::Bang);
        assert_eq!(kinds[7],  TokenKind::LBracket);
        assert!(matches!(&kinds[8],  TokenKind::UpperWord(s) if s == "X"));
        assert_eq!(kinds[9],  TokenKind::RBracket);
        assert_eq!(kinds[10], TokenKind::Colon);
        assert!(matches!(&kinds[11], TokenKind::LowerWord(s) if s == "p"));
        assert_eq!(kinds[12], TokenKind::LParen);
        assert!(matches!(&kinds[13], TokenKind::UpperWord(s) if s == "X"));
        assert_eq!(kinds[14], TokenKind::RParen);
        assert_eq!(kinds[15], TokenKind::RParen);
        assert_eq!(kinds[16], TokenKind::Dot);
    }

    // ── Span coverage ────────────────────────────────────────────

    #[test]
    fn spans_cover_token_width() {
        let (tokens, _) = tokenize("fof(a)", "test");
        // fof ( a )
        assert_eq!(tokens[0].span.offset,     0);
        assert_eq!(tokens[0].span.end_offset, 3);
        assert_eq!(tokens[1].span.offset,     3);
        assert_eq!(tokens[1].span.end_offset, 4);
        assert_eq!(tokens[2].span.offset,     4);
        assert_eq!(tokens[2].span.end_offset, 5);
        assert_eq!(tokens[3].span.offset,     5);
        assert_eq!(tokens[3].span.end_offset, 6);
    }

    #[test]
    fn spans_track_line_breaks() {
        let (tokens, _) = tokenize("fof\n  bar", "test");
        assert_eq!(tokens[0].span.line, 1);
        assert_eq!(tokens[1].span.line, 2);
        assert_eq!(tokens[1].span.col,  3);
    }

    #[test]
    fn compound_connective_span() {
        // `<=>` at offset 0 should span 3 bytes.
        let (tokens, _) = tokenize("<=>", "test");
        assert_eq!(tokens[0].span.offset,     0);
        assert_eq!(tokens[0].span.end_offset, 3);
    }

    // ── Status pragma capture ─────────────────────────────────────

    #[test]
    fn captures_status_pragma_from_header_comment() {
        let src = "% File     : PUZ001+1\n% Status   : Theorem\nfof(a, axiom, p).\n";
        let (_, errors, metas) = tokenize_with_meta(src, "test");
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
        assert_eq!(metas.len(), 1, "exactly one status pragma recognized");
        assert_eq!(metas[0].key, "status");
        assert!(matches!(&metas[0].args[0], AstNode::Symbol { name, .. } if name == "Theorem"));
    }

    #[test]
    fn ignores_ordinary_comments_without_status() {
        let src = "% just a regular comment\nfof(a, axiom, p).\n";
        let (_, _, metas) = tokenize_with_meta(src, "test");
        assert!(metas.is_empty());
    }
}