// crates/core/src/parse/tptp/dis.rs
//
// TPTP emission dialect.  Emits an `AstNode` document as a homogeneous TPTP
// file in a chosen language (`cnf` / `fof` / `tff` / auto).  Untyped formula
// rendering (`emit_formula`) lives here; TFF (typed) emission routes through
// the translation layer and is wired in a later phase.
//
// "Frame, don't transform": CNF keeps only already-clausal statements (no
// clausification); FOF frames any untyped formula; non-conforming statements
// are dropped and reported in `EmitResult.dropped`.

use crate::parse::ast::{AstNode, OpKind, Role, Source, Span};
use crate::parse::dialect::{DroppedStmt, Emit, EmitResult, PrettyEmit, TptpLang};
use super::syntax;
use super::tokenizer::{tokenize, TokenKind};

/// ANSI-colourised TPTP text — tokenizes `text` (the real TPTP lexer, not an
/// ad-hoc regex) and re-emits it with each token's ORIGINAL bytes wrapped in
/// a colour keyed on its kind, splicing the untouched source back in between
/// tokens so whitespace, newlines, and comments survive verbatim (the
/// caller's chosen layout — flat or `styled`-wrapped — is unaffected).
/// Mirrors KIF's `Pretty` (`kif::dis`), adapted to TPTP's lexical shape;
/// unlike `Pretty`, this works over raw text rather than a parsed `AstNode`,
/// so it colourises a subprocess prover's verbatim transcript too, not just
/// document-reconstructed proofs.  Falls back to `text` unmodified on any
/// lex error — a readable plain proof beats a truncated/garbled highlight.
pub fn highlight(text: &str) -> String {
    use inline_colorization::*;

    let (tokens, errors) = tokenize(text, "highlight");
    if !errors.is_empty() {
        return text.to_string();
    }

    // Statement-framing / inference-annotation vocabulary: coloured as
    // keywords wherever it appears, since a flat token stream has no
    // parser state to confirm position (`fof(`, `..., axiom, ...`).  Real
    // predicate/function names in SUMO-derived proofs never collide with
    // this fixed set.
    const KEYWORDS: &[&str] = &[
        "fof", "cnf", "tff", "thf", "tcf", "include",
        "axiom", "hypothesis", "definition", "lemma", "conjecture",
        "negated_conjecture", "plain", "type", "unknown", "assumption",
        "theorem", "corollary", "fi_domain", "fi_functors", "fi_predicates",
        "inference", "file", "status", "introduced", "esa", "thm", "cth",
    ];

    let mut out = String::with_capacity(text.len() + tokens.len() * 8);
    let mut pos = 0usize;
    for t in &tokens {
        let (start, end) = (t.span.offset, t.span.end_offset);
        out.push_str(&text[pos..start]);
        let color = match &t.kind {
            TokenKind::LowerWord(w) if KEYWORDS.contains(&w.as_str()) => color_yellow,
            TokenKind::LowerWord(_) => color_bright_blue,
            TokenKind::UpperWord(_) => color_magenta,
            TokenKind::DollarWord(_) | TokenKind::DollarDollarWord(_) => color_bright_cyan,
            TokenKind::Integer(_) | TokenKind::Rational(_) | TokenKind::Real(_) => color_green,
            TokenKind::SingleQuoted(_) | TokenKind::DoubleQuoted(_) => color_green,
            TokenKind::Tilde | TokenKind::Bang | TokenKind::Question
            | TokenKind::Pipe | TokenKind::Ampersand | TokenKind::Equals
            | TokenKind::Operator(_) => color_cyan,
            _ => "",
        };
        if color.is_empty() {
            out.push_str(&text[start..end]);
        } else {
            out.push_str(color);
            out.push_str(&text[start..end]);
            out.push_str(color_reset);
        }
        pos = end;
    }
    out.push_str(&text[pos..]);
    out
}

/// Soft-wrap threshold for [`styled`] — mirrors `kif::dis::LINE_WIDTH`.  Forms
/// fitting in this many columns at their indent stay on one line; longer ones
/// break at the top connective, one operand per line.
const LINE_WIDTH: usize = 72;

/// The TPTP output dialect, configured with a target language.
pub(crate) struct TptpEmit {
    pub lang: TptpLang,
}

impl PrettyEmit for TptpEmit {
    fn emit_pretty(&self, node: &AstNode, indent: usize, color: bool) -> String {
        styled(node, indent, color, self.lang.is_typed())
    }
}

impl Emit for TptpEmit {
    fn emit_formula(&self, f: &AstNode) -> String {
        tptp_formula(f)
    }

    fn emit_statement(&self, stmt: &AstNode) -> Result<String, String> {
        let lang = match self.lang {
            TptpLang::Auto => if is_clause(stmt.formula()) { TptpLang::Cnf } else { TptpLang::Fof },
            l => l,
        };
        frame_stmt(stmt, 1, lang)
    }

    fn emit_document(&self, doc: &[AstNode]) -> EmitResult {
        // Resolve `Auto` over the whole document: CNF iff every statement is a
        // clause, else FOF (the universal untyped fallback).
        let lang = match self.lang {
            TptpLang::Auto => if doc.iter().all(|s| is_clause(s.formula())) {
                TptpLang::Cnf
            } else {
                TptpLang::Fof
            },
            l => l,
        };
        let mut out = EmitResult::default();
        // TFF requires every symbol to be declared.  We type the whole document
        // monomorphically over `$i` (individuals), `$o` (predicates' result),
        // emitting one `tff(_, type, …)` per symbol as a preamble.  This is
        // sound but unsorted; lifting SUMO sorts from instance-guards is a
        // future enhancement.
        if lang.is_typed() {
            out.text.push_str(&tff_type_preamble(doc));
        }
        for (i, stmt) in doc.iter().enumerate() {
            match frame_stmt(stmt, i + 1, lang) {
                Ok(t)  => { out.text.push_str(&t); out.text.push('\n'); }
                Err(r) => out.dropped.push(DroppedStmt {
                    name: stmt_name_or(stmt, i + 1).into(), reason: r,
                }),
            }
        }
        out
    }
}

// -- TFF monomorphic ($i) symbol typing ---------------------------------------

/// One symbol's TFF signature, keyed by its emitted (TPTP) name.
#[derive(PartialEq, Eq)]
enum TffKind { Pred(usize), Func(usize), Const }

/// Collect every symbol used across `doc` and emit a `tff(_, type, …)`
/// declaration for each, monomorphically typed over `$i`.  Predicates get
/// `($i * … ) > $o`, functions `($i * … ) > $i`, constants `$i`.  Equality and
/// the `$true`/`$false` constants are built-in and need no declaration.
fn tff_type_preamble(doc: &[AstNode]) -> String {
    use std::collections::BTreeMap;
    let mut sigs: BTreeMap<String, TffKind> = BTreeMap::new();
    for stmt in doc {
        collect_formula_sigs(stmt.formula(), &mut sigs);
    }
    let mut out = String::new();
    for (i, (name, kind)) in sigs.iter().enumerate() {
        let sig = match kind {
            TffKind::Pred(0)          => "$o".to_string(),
            TffKind::Pred(n)          => format!("{} > $o", arrow_domain(*n)),
            TffKind::Func(n)          => format!("{} > $i", arrow_domain(*n)),
            TffKind::Const            => "$i".to_string(),
        };
        out.push_str(&format!("tff(ty{i}, type, {name}: {sig}).\n"));
    }
    out
}

/// `$i` for arity 1, `($i * $i * …)` for arity ≥ 2.
fn arrow_domain(arity: usize) -> String {
    if arity == 1 {
        "$i".to_string()
    } else {
        format!("({})", std::iter::repeat("$i").take(arity).collect::<Vec<_>>().join(" * "))
    }
}

/// Walk a node in **formula** position, recording predicate symbols and
/// descending into terms for function/constant symbols.
fn collect_formula_sigs(node: &AstNode, sigs: &mut std::collections::BTreeMap<String, TffKind>) {
    match node {
        AstNode::Annotated { formula, .. } => collect_formula_sigs(formula, sigs),
        AstNode::Symbol { name, .. } if name != "FALSE" =>
            { sigs.entry(syntax::lower_word(name)).or_insert(TffKind::Pred(0)); }
        AstNode::List { elements, .. } => {
            let Some(head) = elements.first() else { return };
            let args = &elements[1..];
            match head {
                AstNode::Operator { op, .. } => match op {
                    OpKind::Not | OpKind::And | OpKind::Or | OpKind::Implies | OpKind::Iff =>
                        args.iter().for_each(|a| collect_formula_sigs(a, sigs)),
                    OpKind::Equal => args.iter().for_each(|a| collect_term_sigs(a, sigs)),
                    OpKind::ForAll | OpKind::Exists =>
                        if let Some(body) = args.get(1) { collect_formula_sigs(body, sigs) },
                },
                AstNode::Symbol { name, .. } => {
                    sigs.insert(syntax::lower_word(name), TffKind::Pred(args.len()));
                    args.iter().for_each(|a| collect_term_sigs(a, sigs));
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Walk a node in **term** position, recording function/constant symbols.
fn collect_term_sigs(node: &AstNode, sigs: &mut std::collections::BTreeMap<String, TffKind>) {
    match node {
        AstNode::Symbol { name, .. } =>
            { sigs.entry(syntax::lower_word(name)).or_insert(TffKind::Const); }
        AstNode::List { elements, .. } => {
            let Some(AstNode::Symbol { name, .. }) = elements.first() else { return };
            let args = &elements[1..];
            sigs.insert(syntax::lower_word(name), TffKind::Func(args.len()));
            args.iter().for_each(|a| collect_term_sigs(a, sigs));
        }
        _ => {}
    }
}

/// Frame one statement in a concrete language (`lang` must be resolved, not
/// `Auto`).  `idx` supplies a default name when the statement is unnamed.
fn frame_stmt(stmt: &AstNode, idx: usize, lang: TptpLang) -> Result<String, String> {
    let (role, name, source, formula) = match stmt {
        AstNode::Annotated { role, name, source, formula, .. } =>
            (role.clone(), name.clone(), source.clone(), formula.as_ref()),
        other => (Role::Axiom, None, None, other),
    };
    let name = name.unwrap_or_else(|| format!("a{idx}"));

    let kw = match lang {
        TptpLang::Cnf => {
            if !is_clause(formula) {
                return Err("non-clausal formula cannot be emitted as cnf".into());
            }
            "cnf"
        }
        TptpLang::Fof => "fof",
        TptpLang::Tff => "tff",
        TptpLang::Auto => unreachable!("Auto is resolved before frame_stmt"),
    };
    // FOF/TFF formulas must be closed — TPTP permits free variables only in
    // cnf clauses, where they are implicitly universal.  KIF-convention ASTs
    // leave universal variables free (derived proof clauses; top-level
    // foralls stripped for display), so close them with an explicit
    // universal binder here.
    let closed;
    let formula = match lang {
        TptpLang::Fof | TptpLang::Tff => {
            let mut free = Vec::new();
            free_var_names(formula, &mut Vec::new(), &mut free);
            if free.is_empty() {
                formula
            } else {
                closed = universal_closure(formula, free);
                &closed
            }
        }
        _ => formula,
    };

    // TFF quantifier binders carry an explicit sort (`![X: $i]`); every other
    // language renders untyped binders.  Symbol typing is monomorphic over `$i`
    // (declared in the document preamble), so bodies are otherwise identical.
    // `styled` (width-wrapped) rather than the always-flat `render_formula`:
    // short formulas render identically either way, long ones wrap instead of
    // producing one unreadable line — see `styled`'s doc comment.
    let body = styled(formula, 2, false, lang.is_typed());

    // A `Source` (provenance) becomes the optional 4th TPTP argument; without
    // one the statement is the bare 3-arg form.
    let source_suffix = source.map(|src| format!(", {}", render_source(&src))).unwrap_or_default();

    Ok(if body.contains('\n') {
        format!("{kw}({}, {},\n  {}{}).", name, role_word(&role), body, source_suffix)
    } else {
        format!("{kw}({}, {}, {}{}).", name, role_word(&role), body, source_suffix)
    })
}

/// Free-variable names of `node` in first-appearance order, respecting
/// `forall`/`exists` binders (a variable bound by an enclosing quantifier
/// is not free in its body).
fn free_var_names(node: &AstNode, bound: &mut Vec<String>, out: &mut Vec<String>) {
    match node {
        AstNode::Variable { name, .. } | AstNode::RowVariable { name, .. } => {
            if !bound.iter().any(|b| b == name) && !out.iter().any(|o| o == name) {
                out.push(name.clone());
            }
        }
        AstNode::Annotated { formula, .. } => free_var_names(formula, bound, out),
        AstNode::List { elements, .. } => {
            let is_quant = matches!(elements.first(),
                Some(AstNode::Operator { op: OpKind::ForAll | OpKind::Exists, .. }));
            if !is_quant {
                for e in elements { free_var_names(e, bound, out); }
                return;
            }
            let depth = bound.len();
            match elements.get(1) {
                Some(AstNode::List { elements: vs, .. }) => {
                    for v in vs {
                        if let AstNode::Variable { name, .. }
                             | AstNode::RowVariable { name, .. } = v {
                            bound.push(name.clone());
                        }
                    }
                }
                Some(AstNode::Variable { name, .. })
                | Some(AstNode::RowVariable { name, .. }) => bound.push(name.clone()),
                _ => {}
            }
            for body in elements.iter().skip(2) {
                free_var_names(body, bound, out);
            }
            bound.truncate(depth);
        }
        _ => {}
    }
}

/// `formula` wrapped in one explicit universal binder over `names` — the
/// closure [`frame_stmt`] applies before framing an open formula as FOF/TFF.
fn universal_closure(formula: &AstNode, names: Vec<String>) -> AstNode {
    let sp = Span::synthetic;
    AstNode::List {
        elements: vec![
            AstNode::Operator { op: OpKind::ForAll, span: sp() },
            AstNode::List {
                elements: names.into_iter()
                    .map(|name| AstNode::Variable { name, span: sp() })
                    .collect(),
                span: sp(),
            },
            formula.clone(),
        ],
        span: sp(),
    }
}

/// TPTP annotation-source term for a [`Source`].  Inputs cite
/// `file('<path>')`; inferences cite `inference(<rule>, [status(<s>)],
/// [<parents>])` — `cth` (counter-theorem) for `negate_conjecture`, `esa`
/// (equisatisfiable) for `cnf_transformation` (clausification skolemizes,
/// so the clauses are not theorems of their parent), else `thm`.
fn render_source(src: &Source) -> String {
    match src {
        Source::Input(f) => format!("file('{f}')"),
        Source::Introduced(mechanism) => format!("introduced({})", syntax::lower_word(mechanism)),
        Source::Inference { rule, parents } => {
            let status = match rule.as_str() {
                "negate_conjecture" => "cth",
                "cnf_transformation" => "esa",
                _ => "thm",
            };
            format!("inference({}, [status({status})], [{}])",
                syntax::lower_word(rule), parents.join(","))
        }
    }
}

fn stmt_name_or(stmt: &AstNode, idx: usize) -> String {
    match stmt {
        AstNode::Annotated { name: Some(n), .. } => n.clone(),
        _ => format!("a{idx}"),
    }
}

/// TPTP role keyword for a [`Role`].
fn role_word(role: &Role) -> String {
    match role {
        Role::Axiom             => "axiom".into(),
        Role::Hypothesis        => "hypothesis".into(),
        Role::Definition        => "definition".into(),
        Role::Lemma             => "lemma".into(),
        Role::Conjecture        => "conjecture".into(),
        Role::NegatedConjecture => "negated_conjecture".into(),
        Role::Plain             => "plain".into(),
        Role::Type              => "type".into(),
        Role::Other(s)          => s.clone(),
    }
}

// -- clause shape detection (for CNF conformance / Auto resolution) -----------

/// A clause: a (possibly unit) disjunction of literals, no quantifiers/other
/// connectives.
pub(crate) fn is_clause(f: &AstNode) -> bool {
    match f {
        AstNode::List { elements, .. }
            if matches!(elements.first(), Some(AstNode::Operator { op: OpKind::Or, .. })) =>
            elements[1..].iter().all(is_literal),
        _ => is_literal(f),
    }
}

fn is_literal(f: &AstNode) -> bool {
    match f {
        AstNode::List { elements, .. }
            if matches!(elements.first(), Some(AstNode::Operator { op: OpKind::Not, .. })) =>
            elements.len() == 2 && is_atom(&elements[1]),
        _ => is_atom(f),
    }
}

fn is_atom(f: &AstNode) -> bool {
    match f {
        AstNode::List { elements, .. } => match elements.first() {
            // Equality is an atom; any other logical connective is not.
            Some(AstNode::Operator { op, .. }) => matches!(op, OpKind::Equal),
            Some(_) => true,  // predicate / function application
            None    => false,
        },
        AstNode::Symbol { .. } => true, // propositional constant
        _ => false,
    }
}

/// AST → untyped TPTP formula text (FOF/CNF).  Symbols are TPTP-legal (else
/// single-quoted) and variables upper-cased / armored.  All token spellings
/// come from [`syntax`], the layer shared with the typed (`trans`) emitter.
pub(crate) fn tptp_formula(node: &AstNode) -> String {
    render_formula(node, false)
}

/// AST → TPTP formula text.  When `typed`, quantifier binders carry an explicit
/// `: $i` sort (TFF); otherwise binders are untyped (FOF/CNF).  Everything else
/// is identical, so the two languages share one renderer.
fn render_formula(node: &AstNode, typed: bool) -> String {
    let rec = |n: &AstNode| render_formula(n, typed);
    match node {
        AstNode::Symbol { name, .. } if name == "FALSE" => syntax::FALSE.to_string(),
        AstNode::Symbol { name, .. } => syntax::lower_word(name),
        AstNode::Variable { name, .. } | AstNode::RowVariable { name, .. } => syntax::variable(name),
        AstNode::Number { value, .. } => value.clone(),
        AstNode::Str { value, .. } => format!("\"{value}\""),
        AstNode::Operator { .. } => syntax::TRUE.to_string(), // bare op — unreachable
        AstNode::Annotated { formula, .. } => rec(formula),
        AstNode::List { elements, .. } => {
            let Some(head) = elements.first() else { return syntax::TRUE.to_string() };
            let args = &elements[1..];
            match head {
                AstNode::Operator { op, .. } => match op {
                    OpKind::Not => format!("({} {})", syntax::NOT, rec(&args[0])),
                    OpKind::And | OpKind::Or => {
                        let con = if matches!(op, OpKind::And) { syntax::AND } else { syntax::OR };
                        format!("({})", args.iter().map(rec).collect::<Vec<_>>().join(con))
                    }
                    OpKind::Implies => format!("({}{}{})", rec(&args[0]), syntax::IMPLIES, rec(&args[1])),
                    OpKind::Iff     => format!("({}{}{})", rec(&args[0]), syntax::IFF, rec(&args[1])),
                    OpKind::Equal   => format!("({}{}{})", rec(&args[0]), syntax::EQ, rec(&args[1])),
                    OpKind::ForAll | OpKind::Exists => {
                        let q = if matches!(op, OpKind::ForAll) { syntax::FORALL } else { syntax::EXISTS };
                        let (vars, body) = quantifier_parts(args, typed);
                        format!("({q} [{}] : {})", vars.join(","), body)
                    }
                },
                _ => {
                    let h = rec(head);
                    if args.is_empty() {
                        h
                    } else {
                        format!("{h}({})", args.iter().map(rec).collect::<Vec<_>>().join(","))
                    }
                }
            }
        }
    }
}

/// `((?X ?Y) body)` argument shape of KIF quantifiers.  When `typed`, each bound
/// variable is annotated `X: $i` (monomorphic TFF sorting).
fn quantifier_parts(args: &[AstNode], typed: bool) -> (Vec<String>, String) {
    let bind = |name: &str| if typed { format!("{}: $i", syntax::variable(name)) } else { syntax::variable(name) };
    let mut vars = Vec::new();
    if let Some(AstNode::List { elements, .. }) = args.first() {
        for v in elements {
            if let AstNode::Variable { name, .. } = v {
                vars.push(bind(name));
            }
        }
    } else if let Some(AstNode::Variable { name, .. }) = args.first() {
        vars.push(bind(name));
    }
    let body = args.get(1).map(|b| render_formula(b, typed)).unwrap_or_else(|| "$true".to_string());
    if vars.is_empty() {
        vars.push(if typed { "X__: $i".to_string() } else { "X__".to_string() });
    }
    (vars, body)
}

/// Indented, width-wrapped TPTP formula rendering — the [`PrettyEmit`]
/// counterpart to the always-flat [`render_formula`].  `color` is accepted
/// for parity with `kif::dis::styled` but unused for now: TPTP has no leaf
/// colourisation defined yet, so plain and "coloured" output are identical.
///
/// Short forms (fit in [`LINE_WIDTH`] at their indent) render exactly like
/// [`render_formula`]. Longer ones break at the top connective, one operand
/// per line, continuation lines indented under the opening `(`:
///
/// ```text
/// ( (instance ?X Human) => (mortal ?X) )     -- short: one line
///
/// ( (instance ?X Human)
/// & (instance ?X Mammal)
/// & (mortal ?X) )                            -- long: one conjunct per line
/// ```
///
/// Predicate/function application arguments (`pred(a,b,c)`) are never
/// wrapped — TPTP's infix connectives are where deep nesting piles up
/// (mirroring KIF's prefix-list problem), argument lists rarely do.
fn styled(node: &AstNode, indent: usize, _color: bool, typed: bool) -> String {
    if let AstNode::Annotated { formula, .. } = node {
        return styled(formula, indent, _color, typed);
    }
    let flat = render_formula(node, typed);
    if indent + flat.len() <= LINE_WIDTH {
        return flat;
    }
    let AstNode::List { elements, .. } = node else { return flat };
    let Some(AstNode::Operator { op, .. }) = elements.first() else { return flat };
    let args = &elements[1..];
    let pad  = " ".repeat(indent);
    let pad2 = " ".repeat(indent + 2);
    let rec  = |n: &AstNode| styled(n, indent + 2, _color, typed);

    match op {
        OpKind::Not => format!("(~\n{pad2}{})", rec(&args[0])),
        OpKind::And | OpKind::Or => {
            let sym = if matches!(op, OpKind::And) { "&" } else { "|" };
            let mut parts = args.iter().map(rec);
            let first = parts.next().unwrap_or_else(|| syntax::TRUE.to_string());
            let rest: String = parts.map(|p| format!("\n{pad}{sym} {p}")).collect();
            format!("({first}{rest})")
        }
        OpKind::Implies | OpKind::Iff | OpKind::Equal => {
            let sym = match op {
                OpKind::Implies => "=>",
                OpKind::Iff     => "<=>",
                _               => "=",
            };
            format!("({}\n{pad}{sym} {})", rec(&args[0]), rec(&args[1]))
        }
        OpKind::ForAll | OpKind::Exists => {
            let q = if matches!(op, OpKind::ForAll) { syntax::FORALL } else { syntax::EXISTS };
            let (vars, _) = quantifier_parts(args, typed);
            let body = args.get(1).map(rec).unwrap_or_else(|| syntax::TRUE.to_string());
            format!("({q} [{}] :\n{pad2}{body})", vars.join(","))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::ast::Span;
    use crate::parse::dialect::Emitter;

    fn parse_one(src: &str) -> AstNode {
        let doc = crate::parse::parse_document("t", src, crate::Parser::Kif);
        assert!(doc.parse_errors.is_empty(), "parse errors: {:?}", doc.parse_errors);
        doc.ast.into_iter().next().unwrap().as_stmt().cloned().unwrap()
    }

    fn ann(role: Role, name: &str, f: AstNode) -> AstNode {
        AstNode::Annotated { role, name: Some(name.into()), source: None,
            formula: Box::new(f), span: Span::default() }
    }

    #[test]
    fn fof_frames_quantified_formula() {
        // A KIF-convention open formula (free ?X = implicitly universal) is
        // explicitly closed on framing: fof formulas must have no free
        // variables (GDV rejects them).
        let f = parse_one("(=> (instance ?X Human) (mortal ?X))");
        let r = Emitter::Tptp(TptpLang::Fof).emit_one(&ann(Role::Axiom, "a1", f));
        assert_eq!(r.text.trim_end(),
            "fof(a1, axiom, (! [X] : (instance(X,'Human') => mortal(X)))).");
        assert!(r.is_complete());
    }

    #[test]
    fn fof_closure_skips_bound_vars_and_keeps_order() {
        // Only genuinely free variables are closed over — ?Y is bound by the
        // inner exists and must not reappear in the outer binder.  Free vars
        // bind in first-appearance order.
        let f = parse_one("(=> (p ?X ?Z) (exists (?Y) (q ?X ?Y)))");
        let r = Emitter::Tptp(TptpLang::Fof).emit_one(&ann(Role::Plain, "f7", f));
        assert_eq!(r.text.trim_end(),
            "fof(f7, plain, (! [X,Z] : (p(X,Z) => (? [Y] : q(X,Y))))).");
    }

    #[test]
    fn cnf_keeps_free_variables_open() {
        // cnf clauses are the one TPTP form where free variables are legal
        // (implicitly universal) — no closure there.
        let clause = parse_one("(or (p ?X) (not (q ?X)))");
        let r = Emitter::Tptp(TptpLang::Cnf).emit_one(&ann(Role::Axiom, "c1", clause));
        assert!(r.text.contains("p(X) | (~ q(X))") && !r.text.contains("! ["), "{}", r.text);
    }

    #[test]
    fn cnf_keeps_clauses_drops_quantified() {
        let clause = parse_one("(or (p ?X) (not (q ?X)))");
        let quant  = parse_one("(forall (?X) (p ?X))");
        let doc = vec![ann(Role::Axiom, "c1", clause), ann(Role::Axiom, "c2", quant)];
        let r = Emitter::Tptp(TptpLang::Cnf).emit(&doc);
        assert!(r.text.starts_with("cnf(c1, axiom,") && r.text.contains("p(X) | (~ q(X))"),
            "{}", r.text);
        assert_eq!(r.dropped.len(), 1);
        assert_eq!(r.dropped[0].name.as_deref(), Some("c2"));
    }

    #[test]
    fn auto_picks_cnf_when_all_clausal_else_fof() {
        let c1 = ann(Role::Axiom, "c1", parse_one("(p ?X)"));
        let c2 = ann(Role::Axiom, "c2", parse_one("(or (p ?X) (q ?X))"));
        let all_clausal = Emitter::Tptp(TptpLang::Auto).emit(&[c1.clone(), c2.clone()]);
        assert!(all_clausal.text.contains("cnf(c1") && all_clausal.text.contains("cnf(c2"),
            "{}", all_clausal.text);

        let quant = ann(Role::Conjecture, "g", parse_one("(forall (?X) (p ?X))"));
        let mixed = Emitter::Tptp(TptpLang::Auto).emit(&[c1, quant]);
        assert!(mixed.text.contains("fof(c1") && mixed.text.contains("fof(g, conjecture"),
            "{}", mixed.text);
        assert!(mixed.is_complete());
    }

    fn ann_src(role: Role, name: &str, source: Source, f: AstNode) -> AstNode {
        AstNode::Annotated { role, name: Some(name.into()), source: Some(source),
            formula: Box::new(f), span: Span::default() }
    }

    #[test]
    fn source_becomes_fourth_argument() {
        // An input axiom cites `file('...')`; a derived step cites an inference
        // with status `thm`; a `negate_conjecture` step uses status `cth`.
        let input = ann_src(Role::Axiom, "f1",
            Source::Input("p.kif".into()), parse_one("(p ?X)"));
        let derived = ann_src(Role::Plain, "f3",
            Source::Inference { rule: "resolution".into(),
                parents: vec!["f1".into(), "f2".into()] }, parse_one("(q a)"));
        let negc = ann_src(Role::NegatedConjecture, "f2",
            Source::Inference { rule: "negate_conjecture".into(), parents: vec![] },
            parse_one("(not (q a))"));
        let r = Emitter::Tptp(TptpLang::Fof).emit(&[input, derived, negc]);
        let lines: Vec<&str> = r.text.lines().collect();
        assert_eq!(lines[0], "fof(f1, axiom, (! [X] : p(X)), file('p.kif')).");
        assert_eq!(lines[1],
            "fof(f3, plain, q(a), inference(resolution, [status(thm)], [f1,f2])).");
        assert_eq!(lines[2],
            "fof(f2, negated_conjecture, (~ q(a)), inference(negate_conjecture, [status(cth)], [])).");
    }

    #[test]
    fn tff_emits_typed_binders_and_type_preamble() {
        // A document emits a `$i`-monomorphic type preamble (one decl per
        // symbol, sorted by emitted name) then the framed statements; binders
        // carry an explicit `: $i` sort.
        let ax = ann(Role::Axiom, "a1", parse_one("(forall (?X) (=> (human ?X) (mortal ?X)))"));
        let hy = ann(Role::Hypothesis, "a2", parse_one("(human socrates)"));
        let r = Emitter::Tptp(TptpLang::Tff).emit(&[ax, hy]);
        assert!(r.is_complete(), "dropped: {:?}", r.dropped);
        // Type preamble: human/1 and mortal/1 are predicates; socrates is a const.
        assert!(r.text.contains("tff(ty0, type, human: $i > $o)."), "{}", r.text);
        assert!(r.text.contains("type, mortal: $i > $o)."), "{}", r.text);
        assert!(r.text.contains("type, socrates: $i)."), "{}", r.text);
        // Framed statements with typed binder.
        assert!(r.text.contains("tff(a1, axiom, (! [X: $i] : (human(X) => mortal(X))))."), "{}", r.text);
        assert!(r.text.contains("tff(a2, hypothesis, human(socrates))."), "{}", r.text);
    }

    #[test]
    fn tff_function_and_equality_typing() {
        // `f` is a function ($i>$i), `c` a constant, equality is built-in (no decl).
        let ax = ann(Role::Axiom, "a1", parse_one("(equal (f c) c)"));
        let r = Emitter::Tptp(TptpLang::Tff).emit(&[ax]);
        assert!(r.text.contains("type, f: $i > $i)."), "{}", r.text);
        assert!(r.text.contains("type, c: $i)."), "{}", r.text);
        assert!(!r.text.contains("'='"), "equality must not be declared: {}", r.text);
        assert!(r.text.contains("tff(a1, axiom, (f(c) = c))."), "{}", r.text);
    }

    #[test]
    fn short_formula_pretty_matches_flat() {
        // Under LINE_WIDTH, `emit_pretty` and the flat `Emit` renderer agree —
        // no gratuitous wrapping of short forms.
        let f = parse_one("(=> (instance ?X Human) (mortal ?X))");
        let pretty = TptpEmit { lang: TptpLang::Fof }.emit_pretty(&f, 0, false);
        assert_eq!(pretty, "(instance(X,'Human') => mortal(X))");
        assert!(!pretty.contains('\n'));
    }

    #[test]
    fn long_conjunction_wraps_one_conjunct_per_line() {
        let f = parse_one(
            "(and (instanceOfSomeVeryLongPredicateName ?X ?Y ?Z) \
                  (anotherVeryLongPredicateNameHereToo ?A ?B ?C) \
                  (yetAnotherLongPredicateNameForTestingWrap ?D))",
        );
        let pretty = TptpEmit { lang: TptpLang::Fof }.emit_pretty(&f, 0, false);
        let lines: Vec<&str> = pretty.lines().collect();
        assert_eq!(lines.len(), 3, "expected one conjunct per line:\n{pretty}");
        assert!(lines[0].starts_with('('), "{pretty}");
        assert!(lines[1].trim_start().starts_with('&'), "{pretty}");
        assert!(lines[2].trim_start().starts_with('&') && lines[2].ends_with(')'), "{pretty}");
    }

    #[test]
    fn long_formula_in_a_framed_statement_indents_under_the_frame() {
        // `frame_stmt` (driven by `Emit::emit_statement`/`emit_document`) picks
        // up the wrap automatically — proof/document output never needs a
        // separate pretty-only call site.
        let f = parse_one(
            "(and (instanceOfSomeVeryLongPredicateName ?X ?Y ?Z) \
                  (anotherVeryLongPredicateNameHereToo ?A ?B ?C) \
                  (yetAnotherLongPredicateNameForTestingWrap ?D))",
        );
        let r = Emitter::Tptp(TptpLang::Fof).emit_one(&ann(Role::Axiom, "a1", f));
        assert!(r.is_complete());
        assert!(r.text.starts_with("fof(a1, axiom,\n  ("), "{}", r.text);
        assert!(r.text.trim_end().ends_with(")).") , "{}", r.text);
    }

    #[test]
    fn long_quantified_formula_wraps_body_under_the_binder() {
        let f = parse_one(
            "(forall (?X) (=> (instanceOfSomeVeryLongPredicateName ?X) \
                              (anotherVeryLongPredicateNameHereToo ?X)))",
        );
        let pretty = TptpEmit { lang: TptpLang::Fof }.emit_pretty(&f, 0, false);
        assert!(pretty.starts_with("(! [X] :\n  ("), "{pretty}");
        assert!(pretty.trim_end().ends_with("))"), "{pretty}");
    }

    #[test]
    fn pretty_output_is_still_valid_tptp_when_reparsed_flat() {
        // Multi-line output isn't just for humans — collapse whitespace and
        // it must still be byte-identical to the flat renderer's formula
        // (same tokens, same order), so anything parsing TPTP downstream
        // (a prover, a round-trip test) sees the same formula either way.
        let f = parse_one(
            "(and (instanceOfSomeVeryLongPredicateName ?X ?Y ?Z) \
                  (anotherVeryLongPredicateNameHereToo ?A ?B ?C) \
                  (yetAnotherLongPredicateNameForTestingWrap ?D))",
        );
        let pretty = TptpEmit { lang: TptpLang::Fof }.emit_pretty(&f, 0, false);
        let collapsed: String = pretty.split_whitespace().collect::<Vec<_>>().join(" ");
        let flat = tptp_formula(&f);
        let flat_collapsed: String = flat.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(collapsed, flat_collapsed);
    }
}
