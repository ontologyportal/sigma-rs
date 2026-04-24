// crates/sumo-kb/src/tptp/kif.rs
//
// Transforms Vampire TPTP formula strings back to SUO-KIF notation.
//
// Inverse of the TPTP encoding applied by tptp.rs:
//
//   s__holds(s__pred__m, t1, t2, ...)  ->  (pred t1_kif t2_kif ...)
//   s__Const                          ->  Const
//   Xn  (uppercase var)               ->  ?Xn
//   !  [X0,X1] : F                   ->  (forall (?X0 ?X1) F_kif)
//   ?  [X0,X1] : F                   ->  (exists (?X0 ?X1) F_kif)
//   F & G   /  F | G                 ->  (and ...) / (or ...)
//   ~F                               ->  (not F_kif)
//   F => G  /  F <=> G               ->  (=> ...) / (<=> ...)
//   t1 = t2  /  t1 != t2             ->  (equal ...) / (not (equal ...))
//   $false   /  $true                ->  false / true
//
// Gated under the `ask` feature (regex already available).

use crate::parse::ast::{AstNode, Span, OpKind};

fn dummy_span() -> Span { Span::point(String::new(), 0, 0, 0) }
fn sym(name: &str)  -> AstNode { AstNode::Symbol   { name: name.to_owned(), span: dummy_span() } }
fn op(o: OpKind)    -> AstNode { AstNode::Operator  { op: o, span: dummy_span() } }
fn lst(els: Vec<AstNode>) -> AstNode { AstNode::List { elements: els, span: dummy_span() } }

/// Convert a single Vampire/TPTP formula string to a SUO-KIF [`AstNode`].
///
/// Returns `None` when the formula cannot be parsed.
///
/// **Top-level universal quantifiers are stripped** (they're implicit
/// in SUO-KIF convention — a free uppercase variable is implicitly
/// universally quantified).  Any number of stacked leading `!` blocks
/// is removed, so `! [X0] : ! [X1] : body` and `! [X0,X1] : body` both
/// yield the same AST.  A `!` that occurs under another connective
/// (e.g. `! [X0] : (foo(X0) => ! [X1] : bar(X1))`) is preserved as a
/// visible `(forall (?X1) ...)` block.
///
/// Same-kind nested quantifiers that aren't at the top level are
/// collapsed by [`Fml::peel_same_quantifier`] during AST
/// conversion — `(exists (?X1) (exists (?X2) body))` becomes
/// `(exists (?X1 ?X2) body)`.
pub fn formula_to_ast(tptp: &str) -> Option<AstNode> {
    let tokens = tokenize(tptp.trim());
    let mut parser = Parser { tokens, pos: 0 };
    let mut current = parser.parse_formula()?;
    while let Fml::Forall(_, body) = current {
        current = *body;
    }
    Some(current.to_ast_node())
}

/// Convert a single Vampire/TPTP formula string to a flat SUO-KIF string.
///
/// Returns a best-effort KIF string; unrecognised fragments are kept verbatim.
/// For indented output, convert with [`formula_to_ast`] and call
/// [`AstNode::pretty_print`].
pub fn formula_to_kif(tptp: &str) -> String {
    match formula_to_ast(tptp) {
        Some(node) => node.flat(),
        None => format!("; [unparseable] {}", tptp),
    }
}

/// One step of a proof rendered in SUO-KIF.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct KifProofStep {
    /// Position in the proof (0-based).
    pub index:    usize,
    /// Human-readable rule name (e.g. "Axiom", "Resolution").
    pub rule:     String,
    /// Indices of the premises this step was derived from.
    pub premises: Vec<usize>,
    /// The formula for this step as a KIF AST, ready for pretty-printing.
    pub formula:  AstNode,
    /// Source [`SentenceId`] when this step traces directly back to an
    /// input axiom whose name Vampire preserved (requires
    /// `--output_axiom_names on`).  `None` for derived steps, for
    /// older Vampire builds, and for anonymous axioms.  Downstream
    /// consumers (e.g. proof-display in the CLI) should prefer this
    /// for O(1) source lookup when present and fall back to the
    /// canonical-hash path on [`crate::axiom_source::AxiomSourceIndex`]
    /// when `None` — the hash path is robust to alpha-renaming and
    /// quantifier-normalisation but requires a whole-KB scan.
    ///
    /// Serialisation: `#[serde(default, skip_serializing_if = …)]`
    /// keeps the JSON wire format compatible — old consumers that
    /// don't know about this field deserialize it as `None`; new
    /// consumers omit the field from the output when it's `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sid: Option<crate::types::SentenceId>,
}

/// Convert a sequence of `(formula, rule, premise_indices, source_name)`
/// tuples — as produced by both the TPTP and embedded prover paths — to
/// KIF steps.
///
/// The fourth element is the axiom's original TPTP name when Vampire
/// preserved it via `--output_axiom_names on` (e.g. `Some("kb_42")`);
/// `None` for derived steps or when the flag wasn't active.  When the
/// name matches our `kb_<sid>` convention, the numeric suffix is parsed
/// into [`KifProofStep::source_sid`] for direct source-axiom lookup.
/// Anything else — including Vampire's own anonymous axioms (`kb_anon_N`)
/// and names from prover backends that don't use our convention —
/// leaves `source_sid` as `None`.
pub fn proof_steps_to_kif(
    steps: &[(String, String, Vec<usize>, Option<String>)],
) -> Vec<KifProofStep> {
    steps
        .iter()
        .enumerate()
        .map(|(i, (formula, rule, premises, source_name))| KifProofStep {
            index:   i,
            rule:    rule.clone(),
            premises: premises.clone(),
            formula: formula_to_ast(formula)
                .unwrap_or_else(|| sym(&format!("; [unparseable] {}", formula))),
            source_sid: source_name
                .as_deref()
                .and_then(parse_kb_axiom_name),
        })
        .collect()
}

/// Parse an axiom name of the form `"kb_<digits>"` into a
/// [`SentenceId`](crate::types::SentenceId).  Anything else — including
/// `"kb_anon_0"` (Vampire's fallback for axioms we couldn't assign a
/// sid to) and names from other prover conventions — returns `None`.
fn parse_kb_axiom_name(name: &str) -> Option<crate::types::SentenceId> {
    name.strip_prefix("kb_")?.parse().ok()
}

// -- Tokens --------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String), // identifier, keyword ($false...), number-word
    LParen,       // (
    RParen,       // )
    LBrack,       // [
    RBrack,       // ]
    Comma,        // ,
    Colon,        // :
    Tilde,        // ~
    Bang,         // !  (forall)
    Question,     // ?  (exists)
    And,          // &
    Or,           // |
    Eq,           // =
    Neq,          // !=
    Implies,      // =>
    Iff,          // <=>
}

fn tokenize(src: &str) -> Vec<Tok> {
    let src: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < src.len() {
        match src[i] {
            ' ' | '\t' | '\n' | '\r' => { i += 1; }
            '(' => { out.push(Tok::LParen);   i += 1; }
            ')' => { out.push(Tok::RParen);   i += 1; }
            '[' => { out.push(Tok::LBrack);   i += 1; }
            ']' => { out.push(Tok::RBrack);   i += 1; }
            ',' => { out.push(Tok::Comma);    i += 1; }
            ':' => { out.push(Tok::Colon);    i += 1; }
            '~' => { out.push(Tok::Tilde);    i += 1; }
            '!' if i + 1 < src.len() && src[i + 1] == '=' => {
                out.push(Tok::Neq); i += 2;
            }
            '!' => { out.push(Tok::Bang);     i += 1; }
            '&' => { out.push(Tok::And);      i += 1; }
            '|' => { out.push(Tok::Or);       i += 1; }
            '?' => { out.push(Tok::Question); i += 1; }
            '<' if i + 2 < src.len() && src[i + 1] == '=' && src[i + 2] == '>' => {
                out.push(Tok::Iff); i += 3;
            }
            '=' if i + 1 < src.len() && src[i + 1] == '>' => {
                out.push(Tok::Implies); i += 2;
            }
            '=' => { out.push(Tok::Eq); i += 1; }
            _ if src[i].is_alphanumeric() || src[i] == '_' || src[i] == '$' => {
                let start = i;
                while i < src.len()
                    && (src[i].is_alphanumeric() || src[i] == '_' || src[i] == '$')
                {
                    i += 1;
                }
                out.push(Tok::Word(src[start..i].iter().collect()));
            }
            // TPTP single-quoted atoms: 'hello world', 'it\'s a string'.
            // Collect content between the quotes into a Word token so the
            // parser can unmangle it as a symbol.  Escaped quotes `\'` are
            // treated as a literal `'` character inside the atom.
            '\'' => {
                i += 1; // skip opening quote
                let mut atom = String::new();
                while i < src.len() {
                    if src[i] == '\\' && i + 1 < src.len() && src[i + 1] == '\'' {
                        atom.push('\'');
                        i += 2;
                    } else if src[i] == '\'' {
                        i += 1; // skip closing quote
                        break;
                    } else {
                        atom.push(src[i]);
                        i += 1;
                    }
                }
                if !atom.is_empty() {
                    out.push(Tok::Word(atom));
                }
            }
            _ => { i += 1; } // skip unexpected chars
        }
    }
    out
}

// -- AST -----------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Fml {
    False,
    True,
    /// Predicate application: `name(args...)` or bare `name` in formula position.
    Pred(String, Vec<Trm>),
    Eq(Trm, Trm),
    Neq(Trm, Trm),
    Not(Box<Fml>),
    And(Vec<Fml>),
    Or(Vec<Fml>),
    Implies(Box<Fml>, Box<Fml>),
    Iff(Box<Fml>, Box<Fml>),
    Forall(Vec<String>, Box<Fml>),
    Exists(Vec<String>, Box<Fml>),
}

#[derive(Debug, Clone)]
enum Trm {
    Var(String),           // uppercase -> TPTP variable
    Const(String),         // lowercase / s__ prefixed constant
    App(String, Vec<Trm>), // function application
}

// -- KIF string emitter (flat) -------------------------------------------------

impl Fml {
    /// Peel nested same-kind quantifiers starting at `self`, which
    /// must itself be [`Fml::Forall`] or [`Fml::Exists`].  Returns
    /// `(merged_vars, inner_body)`.
    ///
    /// `(exists (?X1) (exists (?X2) body))`
    ///     -> `(vars = [X1, X2], body = body)`
    ///
    /// `(forall (?X1) (forall (?X2) (exists (?X3) body)))`
    ///     -> `(vars = [X1, X2], body = (exists (?X3) body))`
    ///
    /// `(exists (?X1) (not (exists (?X2) body)))`
    ///     -> `(vars = [X1], body = (not (exists (?X2) body)))`
    ///
    /// The peel stops at the first non-matching formula, so mixed
    /// chains and any formula wrapping a nested quantifier (`not`,
    /// `and`, `=>`, ...) preserve their structure.  The inner
    /// quantifier inside the wrapping formula will be peeled on its
    /// own recursive pass.
    ///
    /// TPTP's proof transcripts emit each `?X` and `!X` as its own
    /// quantifier block (the CNF/skolemisation pipeline introduces
    /// them one at a time), so this collapse can shrink a 4-level
    /// nest back to a single `(exists (?X1 ?X2 ?X3 ?X4) ...)` —
    /// matching how a human would write the same formula by hand.
    fn peel_same_quantifier(&self) -> (Vec<String>, &Fml) {
        match self {
            Fml::Forall(vars, body) => {
                let mut merged = vars.clone();
                let mut current: &Fml = body;
                while let Fml::Forall(vs, inner) = current {
                    merged.extend(vs.iter().cloned());
                    current = inner;
                }
                (merged, current)
            }
            Fml::Exists(vars, body) => {
                let mut merged = vars.clone();
                let mut current: &Fml = body;
                while let Fml::Exists(vs, inner) = current {
                    merged.extend(vs.iter().cloned());
                    current = inner;
                }
                (merged, current)
            }
            // Callers guard the match arm, so this is unreachable in
            // practice.  Returning the trivial peel keeps the helper
            // total without an `unwrap` at every call site.
            _ => (Vec::new(), self),
        }
    }
}

#[allow(dead_code)]
impl Fml {
    fn to_kif(&self) -> String {
        match self {
            Fml::False => "false".into(),
            Fml::True  => "true".into(),

            // Core SUMO unwrapping: s__holds(s__pred__m, t1, t2, ...) -> (pred t1 t2 ...)
            Fml::Pred(name, args)
                if name == "s__holds" && !args.is_empty() =>
            {
                let pred = args[0].as_pred_name();
                let rest: Vec<String> = args[1..].iter().map(Trm::to_kif).collect();
                if rest.is_empty() {
                    format!("({})", pred)
                } else {
                    format!("({} {})", pred, rest.join(" "))
                }
            }

            // Other predicates (including s__holds with 0 args -- shouldn't happen, but safe)
            Fml::Pred(name, args) => {
                let kif = unmangle(name);
                if args.is_empty() {
                    kif
                } else {
                    format!("({} {})", kif, args.iter().map(Trm::to_kif).collect::<Vec<_>>().join(" "))
                }
            }

            Fml::Eq(a, b)  => format!("(equal {} {})",            a.to_kif(), b.to_kif()),
            Fml::Neq(a, b) => format!("(not (equal {} {}))",      a.to_kif(), b.to_kif()),
            Fml::Not(f)    => format!("(not {})",                  f.to_kif()),

            Fml::And(fs) => format!("(and {})",  fs.iter().map(Fml::to_kif).collect::<Vec<_>>().join(" ")),
            Fml::Or(fs)  => format!("(or {})",   fs.iter().map(Fml::to_kif).collect::<Vec<_>>().join(" ")),

            Fml::Implies(a, b) => format!("(=> {} {})",  a.to_kif(), b.to_kif()),
            Fml::Iff(a, b)     => format!("(<=> {} {})", a.to_kif(), b.to_kif()),

            Fml::Forall(..) => {
                let (vars, body) = self.peel_same_quantifier();
                let vlist = vars.iter().map(|v| format!("?{}", v)).collect::<Vec<_>>().join(" ");
                format!("(forall ({}) {})", vlist, body.to_kif())
            }
            Fml::Exists(..) => {
                let (vars, body) = self.peel_same_quantifier();
                let vlist = vars.iter().map(|v| format!("?{}", v)).collect::<Vec<_>>().join(" ");
                format!("(exists ({}) {})", vlist, body.to_kif())
            }
        }
    }
}

#[allow(dead_code)]
impl Trm {
    fn to_kif(&self) -> String {
        match self {
            Trm::Var(name)        => format!("?{}", name),
            Trm::Const(name)      => unmangle(name),
            Trm::App(name, args)  => {
                let kif = unmangle_func(name);
                if args.is_empty() {
                    kif
                } else {
                    format!("({} {})", kif, args.iter().map(Trm::to_kif).collect::<Vec<_>>().join(" "))
                }
            }
        }
    }

    /// Used when this term appears as the first argument of `s__holds` --
    /// it's a mention constant like `s__pred__m` -> return `pred`.
    fn as_pred_name(&self) -> String {
        match self {
            Trm::Const(name) => unmangle(name),
            Trm::Var(name)   => format!("?{}", name),
            Trm::App(name, _) => unmangle(name),
        }
    }

    fn to_ast_node(&self) -> AstNode {
        match self {
            Trm::Var(name)       => AstNode::Variable { name: name.clone(), span: dummy_span() },
            Trm::Const(name)     => sym(&unmangle(name)),
            Trm::App(name, args) => {
                let head = sym(&unmangle_func(name));
                if args.is_empty() {
                    head
                } else {
                    let mut els = vec![head];
                    els.extend(args.iter().map(Trm::to_ast_node));
                    lst(els)
                }
            }
        }
    }
}

impl Fml {
    fn to_ast_node(&self) -> AstNode {
        match self {
            Fml::False => sym("false"),
            Fml::True  => sym("true"),

            Fml::Pred(name, args) if name == "s__holds" && !args.is_empty() => {
                let pred = sym(&args[0].as_pred_name());
                let mut els = vec![pred];
                els.extend(args[1..].iter().map(Trm::to_ast_node));
                lst(els)
            }

            Fml::Pred(name, args) => {
                let head = sym(&unmangle(name));
                if args.is_empty() {
                    head
                } else {
                    let mut els = vec![head];
                    els.extend(args.iter().map(Trm::to_ast_node));
                    lst(els)
                }
            }

            Fml::Eq(a, b)  => lst(vec![op(OpKind::Equal), a.to_ast_node(), b.to_ast_node()]),
            Fml::Neq(a, b) => lst(vec![op(OpKind::Not),
                                   lst(vec![op(OpKind::Equal), a.to_ast_node(), b.to_ast_node()])]),
            Fml::Not(f)    => lst(vec![op(OpKind::Not), f.to_ast_node()]),

            Fml::And(fs) => {
                let mut els = vec![op(OpKind::And)];
                els.extend(fs.iter().map(Fml::to_ast_node));
                lst(els)
            }
            Fml::Or(fs) => {
                let mut els = vec![op(OpKind::Or)];
                els.extend(fs.iter().map(Fml::to_ast_node));
                lst(els)
            }

            Fml::Implies(a, b) => lst(vec![op(OpKind::Implies), a.to_ast_node(), b.to_ast_node()]),
            Fml::Iff(a, b)     => lst(vec![op(OpKind::Iff),     a.to_ast_node(), b.to_ast_node()]),

            Fml::Forall(..) => {
                let (vars, body) = self.peel_same_quantifier();
                let var_list = lst(vars.iter()
                    .map(|v| AstNode::Variable { name: v.clone(), span: dummy_span() })
                    .collect());
                lst(vec![op(OpKind::ForAll), var_list, body.to_ast_node()])
            }
            Fml::Exists(..) => {
                let (vars, body) = self.peel_same_quantifier();
                let var_list = lst(vars.iter()
                    .map(|v| AstNode::Variable { name: v.clone(), span: dummy_span() })
                    .collect());
                lst(vec![op(OpKind::Exists), var_list, body.to_ast_node()])
            }
        }
    }
}

// -- Name unmangling -----------------------------------------------------------

/// Strip `s__` prefix and `__m` suffix from a TPTP symbol name.
fn unmangle(name: &str) -> String {
    let s = name.strip_prefix("s__").unwrap_or(name);
    let s = s.strip_suffix("__m").unwrap_or(s);
    // Number encoding: n__42 -> 42, n__3_14 -> 3.14, n__neg_42 -> -42.
    // `neg_` prefix encodes the minus sign; underscores elsewhere encode dots.
    if let Some(n) = s.strip_prefix("n__") {
        if let Some(pos) = n.strip_prefix("neg_") {
            return format!("-{}", pos.replace('_', "."));
        }
        return n.replace('_', ".");
    }
    // String encoding: str__hello -> "hello"
    if let Some(content) = s.strip_prefix("str__") {
        return format!("\"{}\"", content);
    }
    s.to_owned()
}

/// Like [`unmangle`] but also strips `__op` suffix for operator-as-function encodings.
fn unmangle_func(name: &str) -> String {
    let s = unmangle(name);
    s.strip_suffix("__op").unwrap_or(&s).to_owned()
}

// -- Recursive descent parser --------------------------------------------------

struct Parser {
    tokens: Vec<Tok>,
    pos:    usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> { self.tokens.get(self.pos) }
    fn advance(&mut self) { self.pos += 1; }
    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == Some(tok) { self.advance(); true } else { false }
    }

    fn parse_formula(&mut self) -> Option<Fml> { self.parse_iff() }

    fn parse_iff(&mut self) -> Option<Fml> {
        let lhs = self.parse_implies()?;
        if self.peek() == Some(&Tok::Iff) {
            self.advance();
            let rhs = self.parse_implies()?;
            return Some(Fml::Iff(Box::new(lhs), Box::new(rhs)));
        }
        Some(lhs)
    }

    fn parse_implies(&mut self) -> Option<Fml> {
        let lhs = self.parse_or()?;
        if self.peek() == Some(&Tok::Implies) {
            self.advance();
            let rhs = self.parse_or()?;
            return Some(Fml::Implies(Box::new(lhs), Box::new(rhs)));
        }
        Some(lhs)
    }

    fn parse_or(&mut self) -> Option<Fml> {
        let first = self.parse_and()?;
        if self.peek() != Some(&Tok::Or) { return Some(first); }
        let mut parts = vec![first];
        while self.peek() == Some(&Tok::Or) {
            self.advance();
            parts.push(self.parse_and()?);
        }
        Some(Fml::Or(parts))
    }

    fn parse_and(&mut self) -> Option<Fml> {
        let first = self.parse_unary()?;
        if self.peek() != Some(&Tok::And) { return Some(first); }
        let mut parts = vec![first];
        while self.peek() == Some(&Tok::And) {
            self.advance();
            parts.push(self.parse_unary()?);
        }
        Some(Fml::And(parts))
    }

    fn parse_unary(&mut self) -> Option<Fml> {
        match self.peek()? {
            Tok::Tilde => {
                self.advance();
                Some(Fml::Not(Box::new(self.parse_unary()?)))
            }
            Tok::Bang => {
                self.advance();
                let vars = self.parse_var_list()?;
                self.eat(&Tok::Colon);
                let body = self.parse_unary()?;
                Some(Fml::Forall(vars, Box::new(body)))
            }
            Tok::Question => {
                // peek ahead: if next is LBrack, it's an existential; else a term
                if self.tokens.get(self.pos + 1) == Some(&Tok::LBrack) {
                    self.advance();
                    let vars = self.parse_var_list()?;
                    self.eat(&Tok::Colon);
                    let body = self.parse_unary()?;
                    Some(Fml::Exists(vars, Box::new(body)))
                } else {
                    self.parse_primary()
                }
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_var_list(&mut self) -> Option<Vec<String>> {
        self.eat(&Tok::LBrack);
        let mut vars = Vec::new();
        loop {
            match self.peek()? {
                Tok::Word(w) => { vars.push(w.clone()); self.advance(); }
                _ => break,
            }
            // TFF annotates variables with sorts: `V__X: $int`.
            // Eat `: <sort-word>` when present so the annotation does not
            // corrupt the rest of the parse.  The sort is discarded here
            // because KIF variables are untyped at the AST level.
            if self.eat(&Tok::Colon) {
                if matches!(self.peek(), Some(Tok::Word(_))) {
                    self.advance();
                }
            }
            if !self.eat(&Tok::Comma) { break; }
        }
        self.eat(&Tok::RBrack);
        Some(vars)
    }

    fn parse_primary(&mut self) -> Option<Fml> {
        match self.peek()? {
            Tok::LParen => {
                self.advance();
                let inner = self.parse_formula()?;
                self.eat(&Tok::RParen);
                Some(inner)
            }
            _ => {
                // Parse a term first; then decide if it's equality or a predicate.
                let t = self.parse_term()?;
                match self.peek() {
                    Some(Tok::Eq) => {
                        self.advance();
                        let rhs = self.parse_term()?;
                        Some(Fml::Eq(t, rhs))
                    }
                    Some(Tok::Neq) => {
                        self.advance();
                        let rhs = self.parse_term()?;
                        Some(Fml::Neq(t, rhs))
                    }
                    _ => {
                        // Treat the term as a predicate atom.
                        match t {
                            Trm::App(name, args) => Some(Fml::Pred(name, args)),
                            Trm::Const(name)     => Some(Fml::Pred(name, vec![])),
                            Trm::Var(name) if name == "$false" => Some(Fml::False),
                            Trm::Var(name) if name == "$true"  => Some(Fml::True),
                            Trm::Var(_)                        => None,
                        }
                    }
                }
            }
        }
    }

    fn parse_term(&mut self) -> Option<Trm> {
        let name = match self.peek()? {
            Tok::Word(w) => { let w = w.clone(); self.advance(); w }
            Tok::Question => {
                // `?` in term position is the prefix of a TPTP variable name
                // (`?X` style, used in some output formats).  Peek ahead before
                // advancing: if the token after `?` is not a Word we cannot form
                // a variable name and must return None *without* consuming `?`,
                // so the caller sees the stream unchanged.
                if !matches!(self.tokens.get(self.pos + 1), Some(Tok::Word(_))) {
                    return None;
                }
                self.advance(); // consume `?`
                match self.peek()? {
                    Tok::Word(w) => { let w = w.clone(); self.advance(); w }
                    _ => return None,
                }
            }
            _ => return None,
        };
        // Detect variables: TPTP variables start with uppercase.
        let first_char = name.chars().next().unwrap_or('_');
        if first_char.is_uppercase() {
            return Some(Trm::Var(name));
        }
        // Special constants
        if name == "$false" { return Some(Trm::Var("$false".into())); }
        if name == "$true"  { return Some(Trm::Var("$true".into()));  }
        // Function application?
        if self.peek() == Some(&Tok::LParen) {
            self.advance();
            let mut args = Vec::new();
            loop {
                match self.peek() {
                    Some(Tok::RParen) | None => { self.advance(); break; }
                    _ => {
                        args.push(self.parse_term()?);
                        self.eat(&Tok::Comma);
                    }
                }
            }
            return Some(Trm::App(name, args));
        }
        Some(Trm::Const(name))
    }
}

// -- Tests ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn kif(tptp: &str) -> String { formula_to_kif(tptp) }

    #[test]
    fn simple_predicate() {
        assert_eq!(kif("s__holds(s__likes__m,s__John,s__Mary)"),
                   "(likes John Mary)");
    }

    #[test]
    fn negated_predicate() {
        assert_eq!(kif("~s__holds(s__likes__m,s__John,s__Mary)"),
                   "(not (likes John Mary))");
    }

    #[test]
    fn variable() {
        assert_eq!(kif("s__holds(s__likes__m,s__John,X0)"),
                   "(likes John ?X0)");
    }

    #[test]
    fn forall_top_level_stripped() {
        // Outermost forall is implicit in SUO-KIF and should be stripped.
        assert_eq!(
            kif("! [X0] : s__holds(s__likes__m,s__John,X0)"),
            "(likes John ?X0)"
        );
    }

    #[test]
    fn forall_nested_kept() {
        // forall inside an implication is NOT stripped.
        assert_eq!(
            kif("! [X0] : (s__holds(s__foo__m,X0) => ! [X1] : s__holds(s__bar__m,X1))"),
            "(=> (foo ?X0) (forall (?X1) (bar ?X1)))"
        );
    }

    #[test]
    fn exists() {
        assert_eq!(
            kif("? [X0,X1] : s__holds(s__likes__m,X0,X1)"),
            "(exists (?X0 ?X1) (likes ?X0 ?X1))"
        );
    }

    #[test]
    fn or_clause() {
        assert_eq!(
            kif("~s__holds(s__instance__m,X0,s__Carrying) | ~s__holds(s__agent__m,X0,s__John)"),
            "(or (not (instance ?X0 Carrying)) (not (agent ?X0 John)))"
        );
    }

    #[test]
    fn implies() {
        assert_eq!(
            kif("s__holds(s__instance__m,X0,s__Carrying) => s__holds(s__instance__m,X0,s__Transfer)"),
            "(=> (instance ?X0 Carrying) (instance ?X0 Transfer))"
        );
    }

    #[test]
    fn equality() {
        assert_eq!(kif("s__Circle = X0"), "(equal Circle ?X0)");
    }

    #[test]
    fn disequality() {
        assert_eq!(kif("s__Circle != X0"), "(not (equal Circle ?X0))");
    }

    #[test]
    fn false_literal() {
        assert_eq!(kif("$false"), "false");
    }

    #[test]
    fn negated_conjecture_sp04() {
        let tptp = "~? [X0,X1] : (s__holds(s__instance__m,X0,s__Carrying) & s__holds(s__agent__m,X0,s__John) & s__holds(s__instance__m,X1,s__Flower) & s__holds(s__objectTransferred__m,X0,X1))";
        assert_eq!(
            kif(tptp),
            "(not (exists (?X0 ?X1) (and (instance ?X0 Carrying) (agent ?X0 John) (instance ?X1 Flower) (objectTransferred ?X0 ?X1))))"
        );
    }

    #[test]
    fn nested_exists_collapsed() {
        // Vampire's CNF pipeline emits nested single-variable
        // quantifiers; the translator should merge same-kind ones
        // into a single variable list.
        let tptp = "! [X0] : (s__holds(s__instance__m,X0,s__Pair) => \
                    ? [X1] : ? [X2] : (s__holds(s__member__m,X1,X0) \
                                        & s__holds(s__member__m,X2,X0) \
                                        & X1 != X2))";
        assert_eq!(
            kif(tptp),
            "(=> (instance ?X0 Pair) \
              (exists (?X1 ?X2) \
                (and (member ?X1 ?X0) (member ?X2 ?X0) (not (equal ?X1 ?X2)))))"
        );
    }

    #[test]
    fn stacked_top_level_foralls_all_stripped() {
        // Two leading `!` blocks at the top are both stripped —
        // SUO-KIF convention leaves a free `?X` implicitly
        // universally quantified, so the output has no `forall`
        // wrapper at all.
        let tptp = "! [X0] : ! [X1] : (s__holds(s__sameRow__m,X0,X1) \
                                        => s__holds(s__sameRow__m,X1,X0))";
        assert_eq!(
            kif(tptp),
            "(=> (sameRow ?X0 ?X1) (sameRow ?X1 ?X0))"
        );
    }

    #[test]
    fn nested_forall_inside_implies_collapsed() {
        // When `forall` appears under another connective it's kept
        // as a visible block — and stacked same-kind nests under
        // the connective merge into one variable list.
        let tptp = "! [X0] : (s__holds(s__foo__m,X0) => \
                     ! [X1] : ! [X2] : s__holds(s__bar__m,X1,X2))";
        assert_eq!(
            kif(tptp),
            "(=> (foo ?X0) (forall (?X1 ?X2) (bar ?X1 ?X2)))"
        );
    }

    #[test]
    fn mixed_quantifier_chain_not_collapsed() {
        // `?` nested directly inside `!` must NOT collapse — they're
        // different kinds and merging would change semantics.
        // (`formula_to_ast` strips only the outermost `!`, so the
        // inner one is preserved; the inner `?` under it becomes the
        // stop point.)
        let tptp = "! [X0] : (s__holds(s__instance__m,X0,s__Top) => \
                    ! [X1] : ? [X2] : s__holds(s__foo__m,X0,X1,X2))";
        assert_eq!(
            kif(tptp),
            "(=> (instance ?X0 Top) \
              (forall (?X1) (exists (?X2) (foo ?X0 ?X1 ?X2))))"
        );
    }

    #[test]
    fn nested_exists_under_not_not_collapsed_with_outer() {
        // The inner `exists` is wrapped in `not`, so the outer `exists`
        // peels only its own vars; the inner stays intact.  Matches
        // the user-reported Pair example.
        let tptp = "? [X1] : ? [X2] : (s__holds(s__foo__m,X1,X2) & ~? [X3] : s__holds(s__bar__m,X3))";
        assert_eq!(
            kif(tptp),
            "(exists (?X1 ?X2) (and (foo ?X1 ?X2) (not (exists (?X3) (bar ?X3)))))"
        );
    }
}
