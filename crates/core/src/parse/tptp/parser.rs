// TPTP token-stream → AST parser.
//
// What is dropped
//   Language keyword    fof / cnf / tff / thf / tcf
//   Formula name        ax1, my_lemma, …
//   Formula role        axiom, conjecture, negated_conjecture, …
//   Annotations         source, useful_info
//   Variable types      X : $i  →  just X  (TFF/THF)
//
// Connective desugaring
//   A <= B    →  (=> B A)
//   A <~> B   →  (not (iff A B))
//   A ~| B    →  (not (or  A B))
//   A ~& B    →  (not (and A B))
//   A != B    →  (not (equal A B))
//
// Unsupported constructs
//   THF operators  ^  @+  @-  @   →  TptpParseError::UnexpectedToken
//   include directives             →  TptpParseError::UnsupportedInclude
//
// Symbol remapping
//   TPTP problems generated from typed/polymorphic sources (e.g. Sigma/SUMO)
//   encode the original symbol name inside a mangled identifier using `__` as
//   a separator.  The three remapping options decode these names back:
//
//   remap_term_symbols
//     Applied to symbols in *argument / constant* position (no `(` follows).
//     `.*__NAME`  →  `NAME`
//     e.g. `sK0__bob`  →  `bob`
//
//   remap_mention_symbols
//     Applied to symbols in *argument / constant* position (no `(` follows).
//     `.*__NAME__.*`  →  `NAME`
//     e.g. `sK0__bob`  →  `bob`
//
//   remap_functional_symbols
//     Applied to symbols in *head / predicate / function* position (`(` follows).
//     Same prefix strip, no arity handling.
//     e.g. `s__subclassOf`  →  `subclassOf`
//
//   remap_functional_polymorphism
//     Applied to head-position symbols.  Strips both the leading prefix and
//     any trailing `__\d+` type-arity segments.  Supersedes
//     `remap_functional_symbols` when both flags are set.
//     e.g. `s__subclassOf__1En`  →  `subclassOf`
//          `esk1_2__f__1__2`   →  `f`
//
//   Dollar-words (`$true`, `$false`, `$$domain`, …) are never remapped.

use super::error::TptpParseError;
use super::tokenizer::{Token, TokenKind, TptpOpTok};
use super::super::{AstNode, OpKind, Span};
use super::super::ast::{Role, Source};

// Internal helpers

/// Classification of the binary tail that may follow a unitary formula.
#[derive(Debug)]
enum BinaryTail {
    Or,
    And,
    NonAssoc(TptpOpTok),
}

/// Classification of a unitary-formula head token.
#[derive(Debug)]
enum UnitaryHead {
    LParen,
    Tilde,
    ForAll, // !
    Exists, // ?
    Thf,    // ^  @+  @-  @  — unsupported
    Atomic,
    Eof,
}

#[derive(Debug, Clone)]
/// Options for the TPTP parser
pub struct TptpParseOptions {
    /// Remap SUMO decorated symbol terms to their original forms
    pub remap_term_symbols: bool,
    /// Remap SUMO decorated relations to their original forms
    pub remap_formula_symbols: bool,
    /// Remap SUMO relation expansions (polymorphic and variable arity) to their original forms
    pub remap_formula_expansions: bool,
    /// Expect that input is formula only, meaning input is not wrapped in
    /// TPTP sentence (e.g. the input is not `fof(<formula>,...)`, rather
    /// just `<formula>`
    pub formulas_only: bool,
    /// Emit `conjecture`-role formulas (as `Annotated { role: Conjecture, … }`)
    /// so callers can partition them from axioms by role. Off by default.
    pub keep_conjectures: bool,
}

impl TptpParseOptions {
    /// Options with all remapping and behavior flags disabled.
    #[allow(dead_code)]
    pub fn none() -> Self {
        Self {
            remap_term_symbols: false,
            remap_formula_symbols: false,
            remap_formula_expansions: false,
            formulas_only: false,
            keep_conjectures: false,
        }
    }
}

impl Default for TptpParseOptions {
    fn default() -> Self {
        Self {
            remap_term_symbols: true,
            remap_formula_symbols: true,
            remap_formula_expansions: true,
            formulas_only: false,
            keep_conjectures: false,
        }
    }
}

// Parser

/// TPTP token-stream parser producing AST nodes.
pub struct TptpParser {
    tokens: Vec<Token>,
    pos: usize,
    file: String,
    options: TptpParseOptions,
}

impl TptpParser {
    fn new(tokens: Vec<Token>, file: &str, options: Option<TptpParseOptions>) -> Self {
        Self {
            tokens,
            pos: 0,
            file: file.to_owned(),
            options: options.unwrap_or_default(),
        }
    }

    // ── Stream primitives ─────────────────────────────────────────────────

    /// Get the current token (if not at EOF) but DO NOT consume it
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    /// Get the current token type but DO NOT consume it
    fn peek_kind(&self) -> Option<&TokenKind> {
        self.peek().map(|t| &t.kind)
    }

    /// Return the current token and consume it, advancing to the next.
    fn advance(&mut self) -> Option<&Token> {
        let tok = self.tokens.get(self.pos);
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn eof_span(&self) -> Span {
        if let Some(t) = self.tokens.last() {
            t.span.clone()
        } else {
            Span::point(self.file.clone(), 1, 1, 0)
        }
    }

    /// Peek the current token's span
    fn current_span(&self) -> Span {
        match self.peek() {
            Some(t) => t.span.clone(),
            None => self.eof_span(),
        }
    }

    /// Kind of the current token, cloned. Returns `Dot` as an EOF sentinel.
    fn current_kind(&self) -> TokenKind {
        match self.peek() {
            Some(t) => t.kind.clone(),
            None => TokenKind::Dot,
        }
    }

    fn expect(&mut self, expected: &TokenKind) -> Result<Span, (Span, TptpParseError)> {
        match self.peek() {
            None => {
                let sp = self.eof_span();
                Err((sp.clone(), TptpParseError::UnexpectedEof { span: sp }))
            }
            Some(t) if &t.kind == expected => {
                let span = t.span.clone();
                self.advance();
                Ok(span)
            }
            Some(t) => {
                let found = t.kind.clone();
                let sp = t.span.clone();
                Err((
                    sp.clone(),
                    TptpParseError::UnexpectedToken { found, span: sp },
                ))
            }
        }
    }

    // Symbol remapping
    //
    // Three independent options govern functor remapping; they compose in
    // pipeline order:
    //
    //   remap_functional_multiarity    — strip "function arity" segments.
    //     A segment is a single-group arity tag: exactly one (\d+[A-Za-z]+).
    //     e.g. `2Fn`, `3Op`  →  stripped.
    //
    //   remap_functional_polymorphism  — strip "type arity" segments.
    //     A segment is a multi-group arity tag: (\d+[A-Za-z]+) repeated 2+.
    //     e.g. `0En1In2In`, `1En2In`  →  stripped.
    //
    //   remap_functional_symbols       — strip the leading namespace prefix.
    //     Everything before (and including) the last remaining `__`.

    /// Strip TPTP SUMO prefix (`s__` or `p__`).
    fn strip_sumo_prefix(name: &str) -> &str {
        if name.starts_with("s__") || name.starts_with("p__") {
            &name[3..]
        } else {
            name
        }
    }

    /// Strip TPTP SUMO mention suffix
    fn strip_sumo_mention_suffix(name: &str) -> &str {
        if !name.ends_with("__m") {
            name
        } else {
            &name[..name.len() - 3]
        }
    }

    /// `true` iff `tail` is one or more `<digits><uppercase><optional
    /// lowercase>` segments — the SUMO polymorphic type-abbreviation encoding
    /// after a `__` separator (regex `(\d+[A-EG-Z][a-z]?)+`). The uppercase
    /// letter must not be 'F': that is reserved for the arity marker "Fn".
    fn is_poly_type_segments(tail: &str) -> bool {
        let b = tail.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let digits_start = i;
            while i < b.len() && b[i].is_ascii_digit() { i += 1; }
            if i == digits_start || i == b.len() { return false; }
            if !(b[i].is_ascii_uppercase() && b[i] != b'F') { return false; }
            i += 1;
            if i < b.len() && b[i].is_ascii_lowercase() { i += 1; }
        }
        !tail.is_empty()
    }

    /// Strip a trailing polymorphic type-abbreviation suffix after the last `__`.
    fn strip_polymorphic_suffix(name: &str) -> &str {
        match name.rfind("__") {
            Some(idx) if Self::is_poly_type_segments(&name[idx + 2..]) => &name[..idx],
            _ => name,
        }
    }

    /// Remove variable arity suffixes (should be called after stripping the polymorphic suffix)
    fn strip_arity_suffix(name: &str) -> &str {
        // Require at least one character before __NNFn so that a bare
        // "__NNFn" string (with no base name) is left unchanged.
        let Some(idx) = name.rfind("__") else { return name };
        if idx == 0 { return name; }
        let tail = &name[idx + 2..];
        match tail.strip_suffix("Fn") {
            Some(d) if !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()) => &name[..idx],
            _ => name,
        }
    }

    /// Remap a symbol in *term* position.
    fn remap_term<'a>(&self, name: &'a str, is_dollar: bool) -> &'a str {
        if is_dollar || !self.options.remap_term_symbols || !name.contains("__") {
            return name;
        }
        let stripped = Self::strip_sumo_prefix(name);
        Self::strip_sumo_mention_suffix(stripped)
    }

    /// Remove a trailing `__N` suffix where N is one or more ASCII digits only
    /// (no `Fn` suffix).  This handles names like `pred__1` → `pred`.
    ///
    /// Only strips when there is at least one character before the `__`.
    fn strip_simple_arity_suffix(name: &str) -> &str {
        if let Some(idx) = name.rfind("__") {
            if idx > 0 {
                let tail = &name[idx + 2..];
                if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
                    return &name[..idx];
                }
            }
        }
        name
    }

    /// Remap a symbol in *head / predicate / function* position.
    fn remap_functor<'a>(&self, name: &'a str, is_dollar: bool) -> &'a str {
        if is_dollar || !name.contains("__") {
            return name;
        }
        let stripped = if self.options.remap_formula_symbols {
            Self::strip_sumo_prefix(name)
        } else {
            name
        };

        if self.options.remap_formula_expansions {
            let stripped = Self::strip_polymorphic_suffix(stripped);
            let stripped = Self::strip_arity_suffix(stripped);
            Self::strip_simple_arity_suffix(stripped)
        } else {
            stripped
        }
    }

    // ── Lookahead classifiers ─────────────────────────────────────────────

    fn peek_unitary_head(&self) -> (UnitaryHead, Span) {
        match self.tokens.get(self.pos) {
            None => (UnitaryHead::Eof, self.eof_span()),
            Some(t) => {
                let head = match &t.kind {
                    TokenKind::LParen => UnitaryHead::LParen,
                    TokenKind::Tilde => UnitaryHead::Tilde,
                    TokenKind::Bang => UnitaryHead::ForAll,
                    TokenKind::Question => UnitaryHead::Exists,
                    TokenKind::Caret
                    | TokenKind::At
                    | TokenKind::Operator(TptpOpTok::Choice)
                    | TokenKind::Operator(TptpOpTok::Description) => UnitaryHead::Thf,
                    _ => UnitaryHead::Atomic,
                };
                (head, t.span.clone())
            }
        }
    }

    fn peek_binary_tail(&self) -> Option<(BinaryTail, Span)> {
        match self.tokens.get(self.pos) {
            None => None,
            Some(t) => match &t.kind {
                TokenKind::Pipe => Some((BinaryTail::Or, t.span.clone())),
                TokenKind::Ampersand => Some((BinaryTail::And, t.span.clone())),
                TokenKind::Operator(op) => {
                    let nonassoc = matches!(
                        op,
                        TptpOpTok::Implies
                            | TptpOpTok::RevImplies
                            | TptpOpTok::Iff
                            | TptpOpTok::Xor
                            | TptpOpTok::Nor
                            | TptpOpTok::Nand
                    );
                    if nonassoc {
                        Some((BinaryTail::NonAssoc(op.clone()), t.span.clone()))
                    } else {
                        None
                    }
                }
                _ => None,
            },
        }
    }

    // ── AST node builders ─────────────────────────────────────────────────

    fn op_node(op: OpKind, span: Span) -> AstNode {
        AstNode::Operator { op, span }
    }

    fn make_unary(op: OpKind, op_span: Span, inner: AstNode) -> AstNode {
        let span = op_span.join(inner.span());
        AstNode::List {
            elements: vec![Self::op_node(op, op_span), inner],
            span,
        }
    }

    fn make_binary(op: OpKind, op_span: Span, lhs: AstNode, rhs: AstNode) -> AstNode {
        let span = lhs.span().join(rhs.span());
        AstNode::List {
            elements: vec![Self::op_node(op, op_span), lhs, rhs],
            span,
        }
    }

    fn make_nary(op: OpKind, op_span: Span, args: Vec<AstNode>) -> AstNode {
        debug_assert!(!args.is_empty());
        let span = args
            .first()
            .unwrap()
            .span()
            .join(args.last().unwrap().span());
        let mut elements = vec![Self::op_node(op, op_span)];
        elements.extend(args);
        AstNode::List { elements, span }
    }

    // ── Connective desugaring ─────────────────────────────────────────────

    fn desugar_binary(op: TptpOpTok, op_span: Span, lhs: AstNode, rhs: AstNode) -> AstNode {
        match op {
            TptpOpTok::Implies => Self::make_binary(OpKind::Implies, op_span, lhs, rhs),
            TptpOpTok::RevImplies => Self::make_binary(OpKind::Implies, op_span, rhs, lhs),
            TptpOpTok::Iff => Self::make_binary(OpKind::Iff, op_span, lhs, rhs),
            TptpOpTok::Xor => {
                let iff = Self::make_binary(OpKind::Iff, op_span.clone(), lhs, rhs);
                Self::make_unary(OpKind::Not, op_span, iff)
            }
            TptpOpTok::Nor => {
                let or = Self::make_binary(OpKind::Or, op_span.clone(), lhs, rhs);
                Self::make_unary(OpKind::Not, op_span, or)
            }
            TptpOpTok::Nand => {
                let and = Self::make_binary(OpKind::And, op_span.clone(), lhs, rhs);
                Self::make_unary(OpKind::Not, op_span, and)
            }
            TptpOpTok::NotEqual | TptpOpTok::Choice | TptpOpTok::Description => {
                unreachable!("desugar_binary called with non-binary connective {:?}", op)
            }
        }
    }

    // ── Skip helpers ──────────────────────────────────────────────────────

    fn skip_to_dot(&mut self) {
        let mut depth: i32 = 0;
        loop {
            match self.peek_kind() {
                None => break,
                Some(TokenKind::LParen | TokenKind::LBracket) => {
                    depth += 1;
                    self.advance();
                }
                Some(TokenKind::RParen | TokenKind::RBracket) => {
                    if depth > 0 {
                        depth -= 1;
                    }
                    self.advance();
                }
                Some(TokenKind::Dot) if depth == 0 => {
                    self.advance();
                    break;
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    fn skip_type(&mut self) -> Result<(), (Span, TptpParseError)> {
        let mut depth: i32 = 0;
        loop {
            match self.peek_kind() {
                None => {
                    let sp = self.eof_span();
                    return Err((sp.clone(), TptpParseError::UnexpectedEof { span: sp }));
                }
                Some(TokenKind::LParen) => {
                    depth += 1;
                    self.advance();
                }
                Some(TokenKind::RParen) if depth > 0 => {
                    depth -= 1;
                    self.advance();
                }
                Some(TokenKind::Comma | TokenKind::RBracket) if depth == 0 => break,
                _ => {
                    self.advance();
                }
            }
        }
        Ok(())
    }

    fn skip_annotations(&mut self) -> Result<(), (Span, TptpParseError)> {
        let mut depth: i32 = 0;
        loop {
            match self.peek_kind() {
                None => {
                    let sp = self.eof_span();
                    return Err((sp.clone(), TptpParseError::UnexpectedEof { span: sp }));
                }
                Some(TokenKind::LParen) => {
                    depth += 1;
                    self.advance();
                }
                Some(TokenKind::RParen) if depth > 0 => {
                    depth -= 1;
                    self.advance();
                }
                Some(TokenKind::RParen) => break,
                _ => {
                    self.advance();
                }
            }
        }
        Ok(())
    }

    // ── Top-level sentence ────────────────────────────────────────────────

    fn parse_top_level(&mut self) -> Result<Option<AstNode>, (Span, TptpParseError)> {
        if self.options.formulas_only {
            return Ok(Some(self.parse_formula()?));
        }

        let (kw, kw_span) = match self.tokens.get(self.pos) {
            Some(t) => match &t.kind {
                TokenKind::LowerWord(w) => (w.clone(), t.span.clone()),
                _ => {
                    let found = t.kind.clone();
                    let sp = t.span.clone();
                    return Err((
                        sp.clone(),
                        TptpParseError::UnexpectedToken { found, span: sp },
                    ));
                }
            },
            None => {
                let sp = self.eof_span();
                return Err((sp.clone(), TptpParseError::UnexpectedEof { span: sp }));
            }
        };

        match kw.as_str() {
            // `tff` is accepted but its typing is dropped; the body parses as
            // untyped `fof`. `thf`/`tcf` are rejected outright.
            "fof" | "cnf" | "tff" => self.parse_annotated_formula(),
            "thf" | "tcf" => Err((
                kw_span.clone(),
                TptpParseError::UnsupportedLanguage { span: kw_span, lang: kw },
            )),
            "include" => Err((
                kw_span.clone(),
                TptpParseError::UnsupportedInclude { span: kw_span },
            )),
            _ => {
                let found = self.current_kind();
                Err((
                    kw_span.clone(),
                    TptpParseError::UnexpectedToken {
                        found,
                        span: kw_span,
                    },
                ))
            }
        }
    }

    fn parse_annotated_formula(&mut self) -> Result<Option<AstNode>, (Span, TptpParseError)> {
        self.advance(); // consume language keyword
        self.expect(&TokenKind::LParen)?;

        let stmt_name: String = match self.peek_kind() {
            Some(TokenKind::LowerWord(w) | TokenKind::UpperWord(w)
                 | TokenKind::SingleQuoted(w) | TokenKind::Integer(w)) => {
                let n = w.clone();
                self.advance();
                n
            }
            _ => {
                let found = self.current_kind();
                let sp = self.current_span();
                return Err((sp.clone(), TptpParseError::UnexpectedToken { found, span: sp }));
            }
        };

        self.expect(&TokenKind::Comma)?;

        // Map the formula role to a `Role`. Roles whose body is not a parseable
        // formula (`type`, anything unknown) are skipped (`Ok(None)`).
        // `conjecture` is dropped unless `keep_conjectures` is set.
        let role: Role = match self.peek_kind() {
            Some(TokenKind::LowerWord(word)) => {
                let role = match word.as_str() {
                    "axiom"              => Role::Axiom,
                    "plain"              => Role::Plain,
                    "hypothesis"         => Role::Hypothesis,
                    "definition"         => Role::Definition,
                    "lemma"              => Role::Lemma,
                    "negated_conjecture" => Role::NegatedConjecture,
                    "theorem" | "corollary" => Role::Other(word.clone()),
                    "conjecture" if self.options.keep_conjectures => Role::Conjecture,
                    _ => return Ok(None),
                };
                self.advance();
                role
            }
            _ => {
                let found = self.current_kind();
                let sp = self.current_span();
                return Err((sp.clone(), TptpParseError::UnexpectedToken { found, span: sp }));
            }
        };

        self.expect(&TokenKind::Comma)?;

        let formula = self.parse_formula()?;

        if matches!(self.peek_kind(), Some(TokenKind::Comma)) {
            self.advance();
            self.skip_annotations()?;
        }

        self.expect(&TokenKind::RParen)?;
        self.expect(&TokenKind::Dot)?;

        let span = formula.span().clone();
        Ok(Some(AstNode::Annotated {
            role,
            name:    Some(stmt_name),
            source:  Some(Source::Input(self.file.clone())),
            formula: Box::new(formula),
            span,
        }))
    }

    // ── Formula ───────────────────────────────────────────────────────────

    fn parse_formula(&mut self) -> Result<AstNode, (Span, TptpParseError)> {
        let lhs = self.parse_unitary_formula()?;

        match self.peek_binary_tail() {
            None => Ok(lhs),

            Some((BinaryTail::Or, op_span)) => {
                self.advance();
                let mut args = vec![lhs];
                loop {
                    args.push(self.parse_unitary_formula()?);
                    if matches!(self.peek_kind(), Some(TokenKind::Pipe)) {
                        self.advance();
                    } else {
                        break;
                    }
                }
                Ok(Self::make_nary(OpKind::Or, op_span, args))
            }

            Some((BinaryTail::And, op_span)) => {
                self.advance();
                let mut args = vec![lhs];
                loop {
                    args.push(self.parse_unitary_formula()?);
                    if matches!(self.peek_kind(), Some(TokenKind::Ampersand)) {
                        self.advance();
                    } else {
                        break;
                    }
                }
                Ok(Self::make_nary(OpKind::And, op_span, args))
            }

            Some((BinaryTail::NonAssoc(op_tok), op_span)) => {
                self.advance();
                let rhs = self.parse_unitary_formula()?;
                Ok(Self::desugar_binary(op_tok, op_span, lhs, rhs))
            }
        }
    }

    // ── Unitary formula ───────────────────────────────────────────────────

    fn parse_unitary_formula(&mut self) -> Result<AstNode, (Span, TptpParseError)> {
        let (head, head_span) = self.peek_unitary_head();

        match head {
            UnitaryHead::Eof => Err((
                head_span.clone(),
                TptpParseError::UnexpectedEof { span: head_span },
            )),
            UnitaryHead::LParen => {
                self.advance();
                let inner = self.parse_formula()?;
                self.expect(&TokenKind::RParen)?;
                Ok(inner)
            }
            UnitaryHead::Tilde => {
                self.advance();
                let inner = self.parse_unitary_formula()?;
                Ok(Self::make_unary(OpKind::Not, head_span, inner))
            }
            UnitaryHead::ForAll => self.parse_quantified_formula(OpKind::ForAll, head_span),
            UnitaryHead::Exists => self.parse_quantified_formula(OpKind::Exists, head_span),
            UnitaryHead::Thf => {
                let found = self.current_kind();
                Err((
                    head_span.clone(),
                    TptpParseError::UnexpectedToken {
                        found,
                        span: head_span,
                    },
                ))
            }
            UnitaryHead::Atomic => self.parse_atomic_formula(),
        }
    }

    // ── Quantified formula ────────────────────────────────────────────────

    fn parse_quantified_formula(
        &mut self,
        quant: OpKind,
        quant_span: Span,
    ) -> Result<AstNode, (Span, TptpParseError)> {
        self.advance();
        self.expect(&TokenKind::LBracket)?;

        let var_list_span = self.current_span();
        let mut vars: Vec<AstNode> = Vec::new();

        loop {
            match self.tokens.get(self.pos) {
                Some(t) if matches!(t.kind, TokenKind::UpperWord(_)) => {
                    let name = match &t.kind {
                        TokenKind::UpperWord(n) => n.clone(),
                        _ => unreachable!(),
                    };
                    let span = t.span.clone();
                    self.advance();

                    if matches!(self.peek_kind(), Some(TokenKind::Colon)) {
                        self.advance();
                        self.skip_type()?;
                    }

                    vars.push(AstNode::Variable { name, span });
                }
                _ => {
                    let found = self.current_kind();
                    let sp = self.current_span();
                    return Err((
                        sp.clone(),
                        TptpParseError::UnexpectedToken { found, span: sp },
                    ));
                }
            }

            match self.peek_kind() {
                Some(TokenKind::Comma) => {
                    self.advance();
                }
                Some(TokenKind::RBracket) => break,
                _ => {
                    let found = self.current_kind();
                    let sp = self.current_span();
                    return Err((
                        sp.clone(),
                        TptpParseError::UnexpectedToken { found, span: sp },
                    ));
                }
            }
        }

        if vars.is_empty() {
            return Err((
                var_list_span.clone(),
                TptpParseError::EmptyQuantifierList {
                    span: var_list_span,
                },
            ));
        }

        self.expect(&TokenKind::RBracket)?;
        self.expect(&TokenKind::Colon)?;

        let body = self.parse_unitary_formula()?;
        let var_span = vars
            .first()
            .unwrap()
            .span()
            .join(vars.last().unwrap().span());
        let var_list = AstNode::List {
            elements: vars,
            span: var_span,
        };
        let full_span = quant_span.join(body.span());

        Ok(AstNode::List {
            elements: vec![Self::op_node(quant, quant_span), var_list, body],
            span: full_span,
        })
    }

    // ── Atomic formula ────────────────────────────────────────────────────

    fn parse_atomic_formula(&mut self) -> Result<AstNode, (Span, TptpParseError)> {
        let lhs = self.parse_term()?;

        let infix: Option<(bool, Span)> = match self.tokens.get(self.pos) {
            Some(t) => match &t.kind {
                TokenKind::Equals => Some((false, t.span.clone())),
                TokenKind::Operator(TptpOpTok::NotEqual) => Some((true, t.span.clone())),
                _ => None,
            },
            None => None,
        };

        match infix {
            None => Ok(lhs),
            Some((is_neq, op_span)) => {
                self.advance();
                let rhs = self.parse_term()?;
                let eq = Self::make_binary(OpKind::Equal, op_span.clone(), lhs, rhs);
                if is_neq {
                    Ok(Self::make_unary(OpKind::Not, op_span, eq))
                } else {
                    Ok(eq)
                }
            }
        }
    }

    // ── Term ──────────────────────────────────────────────────────────────

    fn parse_term(&mut self) -> Result<AstNode, (Span, TptpParseError)> {
        match self.tokens.get(self.pos) {
            // Variable: upper-word — never remapped.
            Some(t) if matches!(t.kind, TokenKind::UpperWord(_)) => {
                let name = match &t.kind {
                    TokenKind::UpperWord(n) => n.clone(),
                    _ => unreachable!(),
                };
                let span = t.span.clone();
                self.advance();
                Ok(AstNode::Variable { name, span })
            }

            // Functor or constant: lower-word, single-quoted, $-word, $$-word.
            Some(t)
                if matches!(
                    t.kind,
                    TokenKind::LowerWord(_)
                        | TokenKind::SingleQuoted(_)
                        | TokenKind::DollarWord(_)
                        | TokenKind::DollarDollarWord(_)
                ) =>
            {
                let is_dollar = matches!(
                    t.kind,
                    TokenKind::DollarWord(_) | TokenKind::DollarDollarWord(_)
                );
                let raw_name = match &t.kind {
                    TokenKind::LowerWord(s)
                    | TokenKind::SingleQuoted(s)
                    | TokenKind::DollarWord(s)
                    | TokenKind::DollarDollarWord(s) => s.clone(),
                    _ => unreachable!(),
                };
                let head_span = t.span.clone();
                self.advance();

                if matches!(self.peek_kind(), Some(TokenKind::LParen)) {
                    // ── Head position: functor / predicate ──────────────
                    let name = self.remap_functor(&raw_name, is_dollar).to_string();
                    self.advance();
                    let head = AstNode::Symbol {
                        name,
                        span: head_span.clone(),
                    };
                    let mut elements = vec![head];

                    if !matches!(self.peek_kind(), Some(TokenKind::RParen)) {
                        loop {
                            elements.push(self.parse_term()?);
                            match self.peek_kind() {
                                Some(TokenKind::Comma) => {
                                    self.advance();
                                }
                                Some(TokenKind::RParen) => break,
                                _ => {
                                    let found = self.current_kind();
                                    let sp = self.current_span();
                                    return Err((
                                        sp.clone(),
                                        TptpParseError::UnexpectedToken { found, span: sp },
                                    ));
                                }
                            }
                        }
                    }

                    let close = self.expect(&TokenKind::RParen)?;
                    Ok(AstNode::List {
                        elements,
                        span: head_span.join(&close),
                    })
                } else {
                    // ── Argument / constant position ────────────────────
                    let name = self.remap_term(&raw_name, is_dollar).to_string();
                    Ok(AstNode::Symbol {
                        name,
                        span: head_span,
                    })
                }
            }

            // Numeric literals — never remapped.
            Some(t)
                if matches!(
                    t.kind,
                    TokenKind::Integer(_) | TokenKind::Rational(_) | TokenKind::Real(_)
                ) =>
            {
                let value = match &t.kind {
                    TokenKind::Integer(s) | TokenKind::Rational(s) | TokenKind::Real(s) => {
                        s.clone()
                    }
                    _ => unreachable!(),
                };
                let span = t.span.clone();
                self.advance();
                Ok(AstNode::Number { value, span })
            }

            // Double-quoted distinct object — never remapped.
            Some(t) if matches!(t.kind, TokenKind::DoubleQuoted(_)) => {
                let value = match &t.kind {
                    TokenKind::DoubleQuoted(s) => s.clone(),
                    _ => unreachable!(),
                };
                let span = t.span.clone();
                self.advance();
                Ok(AstNode::Str { value, span })
            }

            _ => {
                let found = self.current_kind();
                let sp = self.current_span();
                Err((
                    sp.clone(),
                    TptpParseError::UnexpectedToken { found, span: sp },
                ))
            }
        }
    }

    // ── Driver ────────────────────────────────────────────────────────────

    fn parse_all(&mut self) -> (Vec<AstNode>, Vec<(Span, TptpParseError)>) {
        let mut nodes = Vec::new();
        let mut errors = Vec::new();
        while self.peek().is_some() {
            match self.parse_top_level() {
                Ok(None) => self.skip_to_dot(),
                Ok(Some(node)) => nodes.push(node),
                Err(e) => {
                    errors.push(e);
                    self.skip_to_dot();
                }
            }
        }
        (nodes, errors)
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

pub fn parse(
    tokens: Vec<Token>,
    file: &str,
    options: Option<TptpParseOptions>,
) -> (Vec<AstNode>, Vec<(Span, TptpParseError)>) {
    TptpParser::new(tokens, file, options).parse_all()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::tokenizer::tokenize;
    use super::*;
    use crate::parse::ast::OpKind;

    // ── Helpers ───────────────────────────────────────────────────────────

    /// Parse with default options (all remapping enabled).
    fn parse_tptp(src: &str) -> Vec<AstNode> {
        let (tokens, _) = tokenize(src, "test");
        let (nodes, errors) = parse(tokens, "test", None);
        assert!(errors.is_empty(), "unexpected parse errors: {:?}", errors);
        // These helpers assert on formula shape; unwrap the statement framing.
        nodes.into_iter().map(|n| n.strip_annotation()).collect()
    }

    /// Parse with all remapping disabled.
    fn parse_tptp_raw(src: &str) -> Vec<AstNode> {
        let (tokens, _) = tokenize(src, "test");
        let opts = TptpParseOptions::none();
        let (nodes, errors) = parse(tokens, "test", Some(opts));
        assert!(errors.is_empty(), "unexpected parse errors: {:?}", errors);
        // These helpers assert on formula shape; unwrap the statement framing.
        nodes.into_iter().map(|n| n.strip_annotation()).collect()
    }

    /// Check that if `formulas_only` is set to true, parse will parse bare formulas
    fn parse_tptp_formula(src: &str) -> Vec<AstNode> {
        let (tokens, _) = tokenize(src, "test");
        let opts = TptpParseOptions {
            formulas_only: true,
            ..TptpParseOptions::default()
        };
        let (nodes, errors) = parse(tokens, "test", Some(opts));
        assert!(errors.is_empty(), "unexpected parse errors: {:?}", errors);
        // These helpers assert on formula shape; unwrap the statement framing.
        nodes.into_iter().map(|n| n.strip_annotation()).collect()
    }

    /// Parse with custom options.
    fn parse_tptp_opts(src: &str, opts: TptpParseOptions) -> Vec<AstNode> {
        let (tokens, _) = tokenize(src, "test");
        let (nodes, errors) = parse(tokens, "test", Some(opts));
        assert!(errors.is_empty(), "unexpected parse errors: {:?}", errors);
        // These helpers assert on formula shape; unwrap the statement framing.
        nodes.into_iter().map(|n| n.strip_annotation()).collect()
    }

    fn parse_errors(src: &str) -> Vec<TptpParseError> {
        let (tokens, _) = tokenize(src, "test");
        let (_, errors) = parse(tokens, "test", None);
        errors.into_iter().map(|(_, e)| e).collect()
    }

    fn parse_errors_opts(src: &str, opts: TptpParseOptions) -> Vec<TptpParseError> {
        let (tokens, _) = tokenize(src, "test");
        let (_, errors) = parse(tokens, "test", Some(opts));
        errors.into_iter().map(|(_, e)| e).collect()
    }

    // ── Unit tests for the remapping helpers ──────────────────────────────

    #[test]
    fn strip_sumo_prefix_basic() {
        assert_eq!(TptpParser::strip_sumo_prefix("s__bob"), "bob");
        assert_eq!(TptpParser::strip_sumo_prefix("s__subclassOf"), "subclassOf");
        assert_eq!(TptpParser::strip_sumo_prefix("bob"), "bob");
        assert_eq!(TptpParser::strip_sumo_prefix("a__c"), "a__c");
    }

    #[test]
    fn strip_mention_suffix_basic() {
        // strip_sumo_mention_suffix only removes the `__m` suffix; the `s__`
        // prefix is left intact (stripped by strip_sumo_prefix when appropriate).
        assert_eq!(TptpParser::strip_sumo_mention_suffix("s__bob__m"), "s__bob");
        assert_eq!(
            TptpParser::strip_sumo_mention_suffix("s__subclassOf__m"),
            "s__subclassOf"
        );
        assert_eq!(TptpParser::strip_sumo_mention_suffix("bob"), "bob");
        assert_eq!(TptpParser::strip_sumo_mention_suffix("s__c"), "s__c");
    }

    #[test]
    fn strip_polymorphic_suffix_basic() {
        // Plain digit suffix — not an arity tag; strip_prefix gives the digit segment.
        assert_eq!(
            TptpParser::strip_polymorphic_suffix("s__subclassOf__0En1En"),
            "s__subclassOf"
        );
        assert_eq!(
            TptpParser::strip_polymorphic_suffix("s__ListFn__6Fn__0En1Ra2En3In4In5Re6Re"),
            "s__ListFn__6Fn"
        );
        assert_eq!(TptpParser::strip_polymorphic_suffix("s__f"), "s__f");
        assert_eq!(TptpParser::strip_polymorphic_suffix("f"), "f");
        assert_eq!(
            TptpParser::strip_polymorphic_suffix("sK0__Human__garbage"),
            "sK0__Human__garbage"
        );
        assert_eq!(
            TptpParser::strip_polymorphic_suffix("s__parent__6Fn"),
            "s__parent__6Fn"
        );
    }

    #[test]
    fn strip_arity_suffix_basic() {
        assert_eq!(
            TptpParser::strip_arity_suffix("s__parent__1Fn"),
            "s__parent"
        );
        assert_eq!(
            TptpParser::strip_arity_suffix("s__subclassOf__10Fn"),
            "s__subclassOf"
        );
        assert_eq!(
            TptpParser::strip_arity_suffix("s__ListFn__6Fn__0En1Ra2En3In4In5Re6Re"),
            "s__ListFn__6Fn__0En1Ra2En3In4In5Re6Re"
        );
        assert_eq!(TptpParser::strip_arity_suffix("s__f"), "s__f");
        assert_eq!(
            TptpParser::strip_arity_suffix("sK0__Human__garbage"),
            "sK0__Human__garbage"
        );
        assert_eq!(TptpParser::strip_arity_suffix("f"), "f");
        assert_eq!(TptpParser::strip_arity_suffix("__6Fn"), "__6Fn"); // multi-group
    }

    #[test]
    fn dollar_words_never_remapped() {
        // $true and $$domain must survive unchanged regardless of options.
        let nodes = parse_tptp("fof(f, axiom, $true).");
        assert!(matches!(&nodes[0], AstNode::Symbol { name, .. } if name == "$true"));

        let nodes2 = parse_tptp("fof(f, axiom, p__pred__1($$domain)).");
        if let AstNode::List { elements, .. } = &nodes2[0] {
            // Head remapped, dollar arg untouched.
            assert!(matches!(&elements[0], AstNode::Symbol { name, .. } if name == "pred"));
            assert!(matches!(&elements[1], AstNode::Symbol { name, .. } if name == "$$domain"));
        }
    }

    // ── remap_term_symbols ────────────────────────────────────────────────

    #[test]
    fn term_remap_constant_position() {
        // `sK0__bob` used as a constant argument → `bob`
        let nodes = parse_tptp("fof(f, axiom, p(s__bob)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(
                matches!(&elements[1], AstNode::Symbol { name, .. } if name == "bob"),
                "expected 'bob', got {:?}",
                elements[1]
            );
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn term_remap_multiple_constants() {
        let nodes = parse_tptp("fof(f, axiom, s__instance(s__likes__m, s__Entity)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(&elements[0], AstNode::Symbol { name, .. } if name == "instance"));
            assert!(matches!(&elements[1], AstNode::Symbol { name, .. } if name == "likes"));
            assert!(matches!(&elements[2], AstNode::Symbol { name, .. } if name == "Entity"));
        }
    }

    #[test]
    fn term_remap_no_prefix_unchanged() {
        // Symbol with no `__` must not change.
        let nodes = parse_tptp("fof(f, axiom, p(alice)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(&elements[1], AstNode::Symbol { name, .. } if name == "alice"));
        }
    }

    #[test]
    fn term_remap_disabled_preserves_mangled_name() {
        let nodes = parse_tptp_opts(
            "fof(f, axiom, p(s__bob)).",
            TptpParseOptions {
                remap_term_symbols: false,
                ..TptpParseOptions::default()
            },
        );
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(
                matches!(&elements[1], AstNode::Symbol { name, .. } if name == "s__bob"),
                "term remap disabled: expected mangled name, got {:?}",
                elements[1]
            );
        }
    }

    // ── remap_functional_symbols ──────────────────────────────────────────

    #[test]
    fn functor_remap_head_position() {
        // `s__subclassOf(X, Y)` → head becomes `subclassOf`
        let nodes = parse_tptp("fof(f, axiom, s__subclass(X, Y)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(
                matches!(&elements[0], AstNode::Symbol { name, .. } if name == "subclass"),
                "expected 'subclassOf', got {:?}",
                elements[0]
            );
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn functor_remap_nested_application() {
        // Nested: `s__f(s__g(X))` → `(f (g X))`
        let nodes = parse_tptp("fof(f, axiom, s__f(s__g(X))).");
        if let AstNode::List {
            elements: outer, ..
        } = &nodes[0]
        {
            assert!(matches!(&outer[0], AstNode::Symbol { name, .. } if name == "f"));
            if let AstNode::List {
                elements: inner, ..
            } = &outer[1]
            {
                assert!(matches!(&inner[0], AstNode::Symbol { name, .. } if name == "g"));
            } else {
                panic!("expected inner List");
            }
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn functor_remap_disabled_preserves_mangled_head() {
        let nodes = parse_tptp_opts(
            "fof(f, axiom, s__pred(a)).",
            TptpParseOptions {
                remap_formula_symbols: false,
                ..TptpParseOptions::default()
            },
        );
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(
                matches!(&elements[0], AstNode::Symbol { name, .. } if name == "s__pred"),
                "functor remap disabled: expected mangled head, got {:?}",
                elements[0]
            );
        }
    }

    // ── remap_functional_polymorphism ─────────────────────────────────────

    #[test]
    fn poly_remap_strips_encoded_arity_suffix() {
        // Encoded arity tag — matches (\d+[A-Za-z]+)+.
        let nodes = parse_tptp("fof(f, axiom, s__subclass__2Fn(X, Y)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(
                matches!(&elements[0], AstNode::Symbol { name, .. } if name == "subclass"),
                "expected 'subclass', got {:?}",
                elements[0]
            );
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn poly_remap_strips_multiple_encoded_arity_segments() {
        // Two encoded arity tags: `ns__foo__1I__2E`
        let nodes = parse_tptp("fof(f, axiom, s__foo__1Fn__0En1En2En(X)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(&elements[0], AstNode::Symbol { name, .. } if name == "foo"));
        }
    }

    #[test]
    fn poly_remap_encoded_arity_tag() {
        // Sigma/SUMO style: `s__MeasureFn__0En1In2En(X)` → head `MeasureFn`
        let nodes = parse_tptp("fof(f, axiom, s__MeasureFn__0En1In2En(X)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(
                matches!(&elements[0], AstNode::Symbol { name, .. } if name == "MeasureFn"),
                "expected 'MeasureFn', got {:?}",
                elements[0]
            );
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn poly_remap_no_arity_same_as_functional() {
        // No arity segment: same result as remap_functional_symbols.
        let nodes = parse_tptp("fof(f, axiom, s__pred(X)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(&elements[0], AstNode::Symbol { name, .. } if name == "pred"));
        }
    }

    #[test]
    fn poly_strips_multi_group_not_single_group() {
        // `s__func__0En1In` has a multi-group poly tag; poly strips it.
        let nodes_poly = parse_tptp_opts(
            "fof(f, axiom, s__func__0En1In(X)).",
            TptpParseOptions {
                remap_formula_expansions: false,
                ..TptpParseOptions::default()
            },
        );
        if let AstNode::List { elements, .. } = &nodes_poly[0] {
            assert!(
                matches!(&elements[0], AstNode::Symbol { name, .. } if name == "func__0En1In"),
                "poly on: expected '__func', got {:?}",
                elements[0]
            );
        }

        // With formula-symbol remap off, the multi-group tag is NOT stripped.
        let nodes_nopoly = parse_tptp_opts(
            "fof(f, axiom, s__func__0En1In(X)).",
            TptpParseOptions {
                remap_term_symbols: true,
                remap_formula_symbols: false,
                remap_formula_expansions: true,
                formulas_only: false,
                keep_conjectures: false,
        },
        );
        if let AstNode::List { elements, .. } = &nodes_nopoly[0] {
            assert!(
                matches!(&elements[0], AstNode::Symbol { name, .. } if name == "s__func"),
                "poly off: expected last segment 's__func', got {:?}",
                elements[0]
            );
        }
    }

    #[test]
    fn poly_remap_disabled_preserves_arity_in_name() {
        // All remapping flags off: name untouched.
        let nodes = parse_tptp_raw("fof(f, axiom, s__subclassOf__2(X)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(
                matches!(&elements[0], AstNode::Symbol { name, .. } if name == "s__subclassOf__2"),
                "all remap off: expected raw name, got {:?}",
                elements[0]
            );
        }
    }

    #[test]
    fn all_three_functor_options_fully_strip() {
        // All options on: `s__ListFn__2Fn__0En1In2In` → `ListFn`.
        let nodes = parse_tptp("fof(f, axiom, s__ListFn__2Fn__0En1In2In(X)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(
                matches!(&elements[0], AstNode::Symbol { name, .. } if name == "ListFn"),
                "expected 'ListFn', got {:?}",
                elements[0]
            );
        }
    }

    // ── Interaction: mixed formula ─────────────────────────────────────────

    #[test]
    fn realistic_sigma_style_formula() {
        // A formula that looks like what Sigma/SUMO encoders produce.
        // Encoded arity tag `__1I` (1 intensional param).
        // `![X]: s__instance__1I(X, s__Human) => s__instance__1I(X, s__Animal)`
        let src = "fof(ax, axiom, \
            ![X]: (s__instance__1I(X, s__Human) => s__instance__1I(X, s__Animal))).";
        let nodes = parse_tptp(src);
        // Outer: ForAll
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(
                &elements[0],
                AstNode::Operator {
                    op: OpKind::ForAll,
                    ..
                }
            ));
            // Body: Implies
            if let AstNode::List { elements: body, .. } = &elements[2] {
                assert!(matches!(
                    &body[0],
                    AstNode::Operator {
                        op: OpKind::Implies,
                        ..
                    }
                ));
                // LHS of implies: instance(X, Human)
                if let AstNode::List { elements: lhs, .. } = &body[1] {
                    assert!(
                        matches!(&lhs[0], AstNode::Symbol { name, .. } if name == "instance"),
                        "expected 'instance' (poly-remapped), got {:?}",
                        lhs[0]
                    );
                    assert!(
                        matches!(&lhs[2], AstNode::Symbol { name, .. } if name == "Human"),
                        "expected 'Human' (term-remapped), got {:?}",
                        lhs[2]
                    );
                } else {
                    panic!("expected implies LHS list");
                }
            } else {
                panic!("expected body list");
            }
        } else {
            panic!("expected ForAll list");
        }
    }

    // ── Existing tests (unchanged) ────────────────────────────────────────

    #[test]
    fn fof_wrapper_stripped() {
        let nodes = parse_tptp("fof(ax1, axiom, p(a)).");
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn cnf_wrapper_stripped() {
        let nodes = parse_tptp("cnf(cl1, plain, p(X)).");
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn multiple_formulas() {
        let nodes = parse_tptp("fof(a, axiom, p(a)). fof(b, axiom, q(b)).");
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn annotations_skipped() {
        let nodes = parse_tptp("fof(ax1, axiom, p(a), inference(modus_ponens,[],[])).");
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn constant_symbol() {
        let nodes = parse_tptp("fof(f, axiom, p(a)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(&elements[0], AstNode::Symbol { name, .. } if name == "p"));
            assert!(matches!(&elements[1], AstNode::Symbol { name, .. } if name == "a"));
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn variable_in_term() {
        let nodes = parse_tptp("fof(f, axiom, p(X)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(&elements[1], AstNode::Variable { name, .. } if name == "X"));
        }
    }

    #[test]
    fn nested_functor() {
        let nodes = parse_tptp("fof(f, axiom, p(f(a, b))).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(&elements[1], AstNode::List { .. }));
        }
    }

    #[test]
    fn single_quoted_functor() {
        let nodes = parse_tptp("fof(f, axiom, 'sos'(a)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(&elements[0], AstNode::Symbol { name, .. } if name == "'sos'"));
        }
    }

    #[test]
    fn dollar_atoms() {
        let nodes = parse_tptp("fof(f, axiom, $true).");
        assert!(matches!(&nodes[0], AstNode::Symbol { name, .. } if name == "$true"));
    }

    #[test]
    fn number_literal() {
        let nodes = parse_tptp("fof(f, axiom, p(42)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(&elements[1], AstNode::Number { value, .. } if value == "42"));
        }
    }

    #[test]
    fn string_distinct_object() {
        let nodes = parse_tptp("fof(f, axiom, p(\"hello\")).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(&elements[1], AstNode::Str { .. }));
        }
    }

    #[test]
    fn conjunction() {
        let nodes = parse_tptp("fof(f, axiom, p(a) & q(b)).");
        assert!(matches!(&nodes[0], AstNode::List { elements, .. }
            if matches!(&elements[0], AstNode::Operator { op: OpKind::And, .. })));
    }

    #[test]
    fn disjunction() {
        let nodes = parse_tptp("fof(f, axiom, p(a) | q(b)).");
        assert!(matches!(&nodes[0], AstNode::List { elements, .. }
            if matches!(&elements[0], AstNode::Operator { op: OpKind::Or, .. })));
    }

    #[test]
    fn flat_or_chain() {
        let nodes = parse_tptp("fof(f, axiom, p(a) | q(b) | r(c)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(
                &elements[0],
                AstNode::Operator { op: OpKind::Or, .. }
            ));
            assert_eq!(elements.len(), 4);
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn flat_and_chain() {
        let nodes = parse_tptp("fof(f, axiom, p(a) & q(b) & r(c)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(
                &elements[0],
                AstNode::Operator {
                    op: OpKind::And,
                    ..
                }
            ));
            assert_eq!(elements.len(), 4);
        }
    }

    #[test]
    fn implication() {
        let nodes = parse_tptp("fof(f, axiom, p(a) => q(b)).");
        assert!(matches!(&nodes[0], AstNode::List { elements, .. }
            if matches!(&elements[0], AstNode::Operator { op: OpKind::Implies, .. })));
    }

    #[test]
    fn reverse_implication_swaps_operands() {
        let nodes = parse_tptp("fof(f, axiom, p(a) <= q(b)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(
                &elements[0],
                AstNode::Operator {
                    op: OpKind::Implies,
                    ..
                }
            ));
            assert!(matches!(&elements[1], AstNode::List { elements: e, .. }
                if matches!(&e[0], AstNode::Symbol { name, .. } if name == "q")));
            assert!(matches!(&elements[2], AstNode::List { elements: e, .. }
                if matches!(&e[0], AstNode::Symbol { name, .. } if name == "p")));
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn iff() {
        let nodes = parse_tptp("fof(f, axiom, p(a) <=> q(b)).");
        assert!(matches!(&nodes[0], AstNode::List { elements, .. }
            if matches!(&elements[0], AstNode::Operator { op: OpKind::Iff, .. })));
    }

    #[test]
    fn xor_desugars_to_not_iff() {
        let nodes = parse_tptp("fof(f, axiom, p(a) <~> q(b)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(
                &elements[0],
                AstNode::Operator {
                    op: OpKind::Not,
                    ..
                }
            ));
            if let AstNode::List {
                elements: inner, ..
            } = &elements[1]
            {
                assert!(matches!(
                    &inner[0],
                    AstNode::Operator {
                        op: OpKind::Iff,
                        ..
                    }
                ));
            } else {
                panic!();
            }
        }
    }

    #[test]
    fn nor_desugars_to_not_or() {
        let nodes = parse_tptp("fof(f, axiom, p(a) ~| q(b)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(
                &elements[0],
                AstNode::Operator {
                    op: OpKind::Not,
                    ..
                }
            ));
            if let AstNode::List {
                elements: inner, ..
            } = &elements[1]
            {
                assert!(matches!(
                    &inner[0],
                    AstNode::Operator { op: OpKind::Or, .. }
                ));
            } else {
                panic!();
            }
        }
    }

    #[test]
    fn nand_desugars_to_not_and() {
        let nodes = parse_tptp("fof(f, axiom, p(a) ~& q(b)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(
                &elements[0],
                AstNode::Operator {
                    op: OpKind::Not,
                    ..
                }
            ));
            if let AstNode::List {
                elements: inner, ..
            } = &elements[1]
            {
                assert!(matches!(
                    &inner[0],
                    AstNode::Operator {
                        op: OpKind::And,
                        ..
                    }
                ));
            } else {
                panic!();
            }
        }
    }

    #[test]
    fn negation() {
        let nodes = parse_tptp("fof(f, axiom, ~p(a)).");
        assert!(matches!(&nodes[0], AstNode::List { elements, .. }
            if matches!(&elements[0], AstNode::Operator { op: OpKind::Not, .. })));
    }

    #[test]
    fn double_negation() {
        let nodes = parse_tptp("fof(f, axiom, ~~p(a)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(
                &elements[0],
                AstNode::Operator {
                    op: OpKind::Not,
                    ..
                }
            ));
            assert!(matches!(&elements[1], AstNode::List { elements: e, .. }
                if matches!(&e[0], AstNode::Operator { op: OpKind::Not, .. })));
        }
    }

    #[test]
    fn equality() {
        let nodes = parse_tptp("fof(f, axiom, a = b).");
        assert!(matches!(&nodes[0], AstNode::List { elements, .. }
            if matches!(&elements[0], AstNode::Operator { op: OpKind::Equal, .. })));
    }

    #[test]
    fn disequality_desugars_to_not_equal() {
        let nodes = parse_tptp("fof(f, axiom, a != b).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(
                &elements[0],
                AstNode::Operator {
                    op: OpKind::Not,
                    ..
                }
            ));
            if let AstNode::List {
                elements: inner, ..
            } = &elements[1]
            {
                assert!(matches!(
                    &inner[0],
                    AstNode::Operator {
                        op: OpKind::Equal,
                        ..
                    }
                ));
            } else {
                panic!();
            }
        }
    }

    #[test]
    fn universal_quantifier() {
        let nodes = parse_tptp("fof(f, axiom, ![X]: p(X)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(
                &elements[0],
                AstNode::Operator {
                    op: OpKind::ForAll,
                    ..
                }
            ));
            assert!(matches!(&elements[1], AstNode::List { .. }));
            assert_eq!(elements.len(), 3);
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn existential_quantifier() {
        let nodes = parse_tptp("fof(f, axiom, ?[X]: p(X)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(
                &elements[0],
                AstNode::Operator {
                    op: OpKind::Exists,
                    ..
                }
            ));
        }
    }

    #[test]
    fn multi_var_quantifier() {
        let nodes = parse_tptp("fof(f, axiom, ![X,Y,Z]: p(X,Y,Z)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            if let AstNode::List { elements: vars, .. } = &elements[1] {
                assert_eq!(vars.len(), 3);
                assert!(matches!(&vars[0], AstNode::Variable { name, .. } if name == "X"));
                assert!(matches!(&vars[2], AstNode::Variable { name, .. } if name == "Z"));
            } else {
                panic!("expected var list");
            }
        }
    }

    #[test]
    fn tff_typed_var_stripped() {
        let nodes = parse_tptp("tff(f, axiom, ![X: $i]: p(X)).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            if let AstNode::List { elements: vars, .. } = &elements[1] {
                assert_eq!(vars.len(), 1);
                assert!(matches!(&vars[0], AstNode::Variable { name, .. } if name == "X"));
            }
        }
    }

    #[test]
    fn quantifier_body_is_unitary() {
        let nodes = parse_tptp("fof(f, axiom, ![X]: p(X) & q(X)).");
        assert!(
            matches!(&nodes[0], AstNode::List { elements, .. }
            if matches!(&elements[0], AstNode::Operator { op: OpKind::And, .. })),
            "outer node should be And, not ForAll"
        );
    }

    #[test]
    fn parens_override_scope() {
        let nodes = parse_tptp("fof(f, axiom, ![X]: (p(X) & q(X))).");
        if let AstNode::List { elements, .. } = &nodes[0] {
            assert!(matches!(
                &elements[0],
                AstNode::Operator {
                    op: OpKind::ForAll,
                    ..
                }
            ));
            assert!(matches!(&elements[2], AstNode::List { elements: e, .. }
                if matches!(&e[0], AstNode::Operator { op: OpKind::And, .. })));
        }
    }

    #[test]
    fn include_is_an_error() {
        let errs = parse_errors("include('axioms.ax').");
        assert!(errs
            .iter()
            .any(|e| matches!(e, TptpParseError::UnsupportedInclude { .. })));
    }

    #[test]
    fn error_does_not_abort_remaining_formulas() {
        let src = "include('extra.ax'). fof(a, axiom, p(a)).";
        let (tokens, _) = tokenize(src, "test");
        let (nodes, errors) = parse(tokens, "test", None);
        assert!(!errors.is_empty());
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn annotated_carries_role_and_name() {
        let (tokens, _) = tokenize(
            "fof(myax, axiom, p(a)). cnf(c1, negated_conjecture, q(b)).", "f");
        let (nodes, errs) = parse(tokens, "f", None);
        assert!(errs.is_empty(), "{:?}", errs);
        assert_eq!(nodes.len(), 2);
        match &nodes[0] {
            AstNode::Annotated { role, name, .. } => {
                assert_eq!(role, &Role::Axiom);
                assert_eq!(name.as_deref(), Some("myax"));
            }
            n => panic!("expected Annotated, got {:?}", n),
        }
        assert!(matches!(&nodes[1], AstNode::Annotated { role: Role::NegatedConjecture, .. }));
    }

    #[test]
    fn conjecture_kept_only_with_opt_in() {
        let src = "fof(g, conjecture, p(a)).";
        // Default: conjecture is dropped (axioms-only ingest).
        let (tokens, _) = tokenize(src, "f");
        let (nodes, _) = parse(tokens, "f", None);
        assert!(nodes.is_empty(), "conjecture should be dropped by default");
        // Opt-in: kept as `Annotated { role: Conjecture }` (no marker).
        let (tokens, _) = tokenize(src, "f");
        let (nodes, _) = parse(tokens, "f",
            Some(TptpParseOptions { keep_conjectures: true, ..TptpParseOptions::none() }));
        assert!(matches!(&nodes[0], AstNode::Annotated { role: Role::Conjecture, .. }));
    }

    #[test]
    fn bad_formula_does_not_abort_next() {
        let src = "fof(a, axiom, !!). fof(b, axiom, q(b)).";
        let (tokens, _) = tokenize(src, "test");
        let (nodes, errors) = parse(tokens, "test", None);
        assert!(!errors.is_empty());
        assert_eq!(nodes.len(), 1);
        assert!(matches!(nodes[0].formula(), AstNode::List { elements, .. }
            if matches!(&elements[0], AstNode::Symbol { name, .. } if name == "q")));
    }

    #[test]
    fn thf_is_an_unsupported_language() {
        // `thf` is rejected at the language keyword — before its (also
        // unsupported) lambda body is even reached.
        let errs = parse_errors("thf(f, axiom, ^[X]: p(X)).");
        assert!(errs.iter().any(|e| matches!(
            e,
            TptpParseError::UnsupportedLanguage { lang, .. } if lang == "thf"
        )), "expected UnsupportedLanguage(thf), got {errs:?}");
    }

    #[test]
    fn tcf_is_an_unsupported_language() {
        let errs = parse_errors("tcf(f, axiom, ![X]: p(X)).");
        assert!(errs.iter().any(|e| matches!(
            e,
            TptpParseError::UnsupportedLanguage { lang, .. } if lang == "tcf"
        )), "expected UnsupportedLanguage(tcf), got {errs:?}");
    }

    #[test]
    fn tff_parses_as_untyped_fof() {
        // A `tff` statement with a typed binder loads as if it were plain fof:
        // the `: $i` annotation is dropped, leaving a bare universally
        // quantified formula identical to the `fof` parse.
        // Compare flat KIF (span-free): the `: $i` shifts byte offsets but the
        // logical structure must be identical.
        let tff = parse_tptp("tff(f, axiom, ![X: $i]: p(X)).");
        let fof = parse_tptp("fof(f, axiom, ![X]: p(X)).");
        assert_eq!(tff[0].to_string(), fof[0].to_string());
        assert_eq!(fof[0].to_string(), "(forall (?X) (p ?X))");
    }

    #[test]
    fn binary_span_covers_both_operands() {
        let src = "fof(f, axiom, p(a) & q(b)).";
        let nodes = parse_tptp(src);
        let span = nodes[0].span();
        assert!(span.offset < span.end_offset);
        assert_eq!(span.byte_len(), "p(a) & q(b)".len());
    }

    #[test]
    fn quantifier_span_starts_at_bang() {
        let src = "fof(f, axiom, ![X]: p(X)).";
        let nodes = parse_tptp(src);
        assert_eq!(nodes[0].span().offset, 14);
    }

    #[test]
    fn formula_only_errors() {
        let src = "![X]: p(X) & g(X, Y)";
        let errs = parse_errors(src);
        assert!(errs
            .iter()
            .any(|e| matches!(e, TptpParseError::UnexpectedToken { .. })));
    }

    #[test]
    fn formula_when_formula_only_errors() {
        let src = "fof(f, axiom, ![X]: p(X) & g(X, Y)).";
        let errs = parse_errors_opts(
            src,
            TptpParseOptions {
                formulas_only: true,
                ..TptpParseOptions::default()
            },
        );
        assert!(errs
            .iter()
            .any(|e| matches!(e, TptpParseError::UnexpectedToken { .. })));
    }

    #[test]
    fn formula_only() {
        let src = "![X]: (p(X) & g(X, Y))";
        let nodes = parse_tptp_formula(src);
        assert_eq!(nodes.len(), 1);
        let tl_elements = match &nodes[0] {
            AstNode::List { elements, .. } => elements,
            other => panic!(
                "expected top-level List (quantified formula), got {:?}",
                other
            ),
        };
        assert!(tl_elements.len() == 3);
        match &tl_elements[0] {
            AstNode::Operator { op: OpKind::ForAll, .. } => {
                // OK
            }
            other => panic!(
                "Missing top-level forall operator, got {:?}",
                other
            ),
        }
        assert!(matches!(&tl_elements[1], AstNode::List { elements, .. } if elements.len() == 1 && matches!(&elements[0], AstNode::Variable { name, .. } if name == "X")), "Quantifier variable sentence wrong");
        
    }
}
