// crates/sumo-kb/src/tptp/kif.rs
//
// Transforms Vampire TPTP formula strings back to SUO-KIF notation.
//
// Inverse of the TPTP encoding applied by tptp.rs:
//
//   s__holds(s__pred__m, t1, t2, …)  →  (pred t1_kif t2_kif …)
//   s__Const                          →  Const
//   Xn  (uppercase var)               →  ?Xn
//   !  [X0,X1] : F                   →  (forall (?X0 ?X1) F_kif)
//   ?  [X0,X1] : F                   →  (exists (?X0 ?X1) F_kif)
//   F & G   /  F | G                 →  (and …) / (or …)
//   ~F                               →  (not F_kif)
//   F => G  /  F <=> G               →  (=> …) / (<=> …)
//   t1 = t2  /  t1 != t2             →  (equal …) / (not (equal …))
//   $false   /  $true                →  false / true
//
// Gated under the `ask` feature (regex already available).

use crate::parse::kif::{AstNode, Span, OpKind};

fn dummy_span() -> Span { Span { file: String::new(), line: 0, col: 0, offset: 0 } }
fn sym(name: &str)  -> AstNode { AstNode::Symbol   { name: name.to_owned(), span: dummy_span() } }
fn op(o: OpKind)    -> AstNode { AstNode::Operator  { op: o, span: dummy_span() } }
fn lst(els: Vec<AstNode>) -> AstNode { AstNode::List { elements: els, span: dummy_span() } }

/// Convert a single Vampire/TPTP formula string to a SUO-KIF [`AstNode`].
///
/// Returns `None` when the formula cannot be parsed.
/// The top-level universal quantifier is stripped (implicit in SUO-KIF).
pub fn formula_to_ast(tptp: &str) -> Option<AstNode> {
    let tokens = tokenize(tptp.trim());
    let mut parser = Parser { tokens, pos: 0 };
    match parser.parse_formula() {
        Some(Fml::Forall(_, body)) => Some(body.to_ast_node()),
        Some(fml) => Some(fml.to_ast_node()),
        None => None,
    }
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
#[derive(Debug, Clone)]
pub struct KifProofStep {
    /// Position in the proof (0-based).
    pub index:    usize,
    /// Human-readable rule name (e.g. "Axiom", "Resolution").
    pub rule:     String,
    /// Indices of the premises this step was derived from.
    pub premises: Vec<usize>,
    /// The formula for this step as a KIF AST, ready for pretty-printing.
    pub formula:  AstNode,
}

/// Convert a sequence of `(formula_str, rule_label, premise_indices)` triples
/// — as produced by both the TPTP and embedded prover paths — to KIF steps.
pub fn proof_steps_to_kif(
    steps: &[(String, String, Vec<usize>)],
) -> Vec<KifProofStep> {
    steps
        .iter()
        .enumerate()
        .map(|(i, (formula, rule, premises))| KifProofStep {
            index:   i,
            rule:    rule.clone(),
            premises: premises.clone(),
            formula: formula_to_ast(formula)
                .unwrap_or_else(|| sym(&format!("; [unparseable] {}", formula))),
        })
        .collect()
}

// ── Tokens ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String), // identifier, keyword ($false…), number-word
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
            _ => { i += 1; } // skip unexpected chars
        }
    }
    out
}

// ── AST ───────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Fml {
    False,
    True,
    /// Predicate application: `name(args…)` or bare `name` in formula position.
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
    Var(String),           // uppercase → TPTP variable
    Const(String),         // lowercase / s__ prefixed constant
    App(String, Vec<Trm>), // function application
}

// ── KIF string emitter (flat) ─────────────────────────────────────────────────

impl Fml {
    fn to_kif(&self) -> String {
        match self {
            Fml::False => "false".into(),
            Fml::True  => "true".into(),

            // Core SUMO unwrapping: s__holds(s__pred__m, t1, t2, …) → (pred t1 t2 …)
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

            // Other predicates (including s__holds with 0 args — shouldn't happen, but safe)
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

            Fml::Forall(vars, body) => {
                let vlist = vars.iter().map(|v| format!("?{}", v)).collect::<Vec<_>>().join(" ");
                format!("(forall ({}) {})", vlist, body.to_kif())
            }
            Fml::Exists(vars, body) => {
                let vlist = vars.iter().map(|v| format!("?{}", v)).collect::<Vec<_>>().join(" ");
                format!("(exists ({}) {})", vlist, body.to_kif())
            }
        }
    }
}

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

    /// Used when this term appears as the first argument of `s__holds` —
    /// it's a mention constant like `s__pred__m` → return `pred`.
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

            Fml::Forall(vars, body) => {
                let var_list = lst(vars.iter()
                    .map(|v| AstNode::Variable { name: v.clone(), span: dummy_span() })
                    .collect());
                lst(vec![op(OpKind::ForAll), var_list, body.to_ast_node()])
            }
            Fml::Exists(vars, body) => {
                let var_list = lst(vars.iter()
                    .map(|v| AstNode::Variable { name: v.clone(), span: dummy_span() })
                    .collect());
                lst(vec![op(OpKind::Exists), var_list, body.to_ast_node()])
            }
        }
    }
}

// ── Name unmangling ───────────────────────────────────────────────────────────

/// Strip `s__` prefix and `__m` suffix from a TPTP symbol name.
fn unmangle(name: &str) -> String {
    let s = name.strip_prefix("s__").unwrap_or(name);
    let s = s.strip_suffix("__m").unwrap_or(s);
    // Number encoding: n__42 → 42, n__3_14 → 3.14 (underscores back to dots)
    if let Some(n) = s.strip_prefix("n__") {
        return n.replace('_', ".");
    }
    // String encoding: str__hello → "hello"
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

// ── Recursive descent parser ──────────────────────────────────────────────────

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
                self.advance();
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

// ── Tests ─────────────────────────────────────────────────────────────────────

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
}
