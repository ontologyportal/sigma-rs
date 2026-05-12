//! Natural-language rendering of SUO-KIF formulas via SUMO's `format` and
//! `termFormat` relations — a `SemanticLayer` capability.
//!
//! This is the engine behind `KnowledgeBase::render_formula` (kb/natural_lang.rs
//! keeps the thin wrapper).  It lives here because its only data is the
//! `format`/`termFormat` facts, which are semantic-layer documentation caches —
//! it never touches the prover, so it's available to every backend (and needs
//! no `ask` feature).
//!
//! ## Template DSL
//!
//! Templates live in the KB as `(format ?LANG ?REL "...")` and
//! `(termFormat ?LANG ?SYM "...")` facts.  Decoded markers:
//!
//! | Marker       | Meaning                                                      |
//! |--------------|--------------------------------------------------------------|
//! | `%1` … `%9`  | Positional argument (1-indexed) rendered recursively.        |
//! | `&%Symbol`   | Cross-reference: render via `termFormat(Symbol, lang)` with a fallback to the raw name. |
//! | `%n`         | Negation placeholder: empty in positive context, `" not "` in negative. |
//! | `%n{TEXT}`   | Custom negation phrase: empty in positive context, `TEXT` in negative. |
//!
//! Logical connectives have no `format` entry and render structurally
//! (`and`/`or`/`if…then`/`for every`/`there exists some`/…).  Missing `format`
//! or `termFormat` specifiers are collected in [`RenderReport::missing`] rather
//! than silently guessed.

use std::collections::BTreeSet;

use crate::parse::ast::{AstNode, OpKind};
use crate::semantics::consts::{FORMAT_RELATION, TERM_RELATION};
use crate::semantics::types::DocEntry;

use super::SemanticLayer;

/// Result of rendering a formula to natural language.
///
/// `rendered` is always populated with a best-effort rendering (missing
/// templates → bare symbol names).  Callers that require strict fidelity
/// should check `missing` before displaying the text.
#[derive(Debug, Clone, Default)]
pub struct RenderReport {
    /// The rendered natural-language string.
    pub rendered: String,
    /// Symbols that lacked a `format` or `termFormat` entry in the
    /// requested language.  Sorted and de-duplicated.
    pub missing: Vec<String>,
}

impl SemanticLayer {
    /// Every `(format <lang> <relation> "...")` entry for `relation`.
    pub(crate) fn format_string(&self, relation: &str, language: Option<&str>) -> Vec<DocEntry> {
        let Some(id) = self.syntactic.sym_id(relation) else { return Vec::new(); };
        self.documentation(id, language)
            .into_iter()
            .filter(|d| d.rel == FORMAT_RELATION.id())
            .collect()
    }

    /// Every `(termFormat <lang> <symbol> "...")` entry for `symbol`.
    pub(crate) fn term_format_named(&self, symbol: &str, language: Option<&str>) -> Vec<DocEntry> {
        let Some(id) = self.syntactic.sym_id(symbol) else { return Vec::new(); };
        self.documentation(id, language)
            .into_iter()
            .filter(|d| d.rel == TERM_RELATION.id())
            .collect()
    }

    /// Render `formula` to natural language in `language` (e.g.
    /// `"EnglishLanguage"`).  See module docs for the template DSL.  Plain (no
    /// ANSI escapes) — safe for logs / files / JSON.
    pub(crate) fn render_formula(&self, formula: &AstNode, language: &str) -> RenderReport {
        self.render_formula_impl(formula, language, /*coloured=*/ false)
    }

    /// Same as [`render_formula`](SemanticLayer::render_formula) but wraps
    /// variables, `&%Symbol` cross-references, negations, and structural
    /// operators in ANSI colour escapes for terminal output.
    pub(crate) fn render_formula_colored(&self, formula: &AstNode, language: &str) -> RenderReport {
        self.render_formula_impl(formula, language, /*coloured=*/ true)
    }

    fn render_formula_impl(&self, formula: &AstNode, language: &str, coloured: bool) -> RenderReport {
        let mut ctx = RenderCtx {
            sem: self,
            language,
            missing: BTreeSet::new(),
            coloured,
        };
        let rendered = ctx.render(formula, /*negated=*/ false);
        RenderReport {
            rendered,
            missing: ctx.missing.into_iter().collect(),
        }
    }
}

// ANSI colour classes, aliased from `inline_colorization` so the mapping from
// *semantic class* (variable, linked symbol, negation, operator) to *palette
// colour* stays tunable from one place.  `ANSI_RESET` follows every open.
pub(crate) use inline_colorization::{
    color_bright_blue as ANSI_LINKED, // `&%Symbol` cross-references
    color_bright_red  as ANSI_NEG,    // negations
    color_cyan        as ANSI_OP,     // structural connectives
    color_magenta     as ANSI_VAR,    // variables
    color_reset       as ANSI_RESET,
};

struct RenderCtx<'a> {
    sem:      &'a SemanticLayer,
    language: &'a str,
    missing:  BTreeSet<String>,
    coloured: bool,
}

impl<'a> RenderCtx<'a> {
    /// Wrap `text` in an ANSI colour class when `self.coloured` is on.
    fn col(&self, text: &str, class: &str) -> String {
        if self.coloured {
            let mut out = String::with_capacity(text.len() + class.len() + ANSI_RESET.len());
            out.push_str(class);
            out.push_str(text);
            out.push_str(ANSI_RESET);
            out
        } else {
            text.to_owned()
        }
    }

    /// Entry point.  `negated` carries an enclosing `(not …)` into a relation's
    /// `%n` / `%n{…}` markers; it resets when descending into a sub-formula that
    /// is not a direct child of the `not`.
    fn render(&mut self, node: &AstNode, negated: bool) -> String {
        match node {
            AstNode::List { elements, .. } => self.render_list(elements, negated),
            AstNode::Symbol { name, .. }   => self.render_term_symbol(name),
            AstNode::Variable { name, .. } => self.col(&format!("?{}", name), ANSI_VAR),
            AstNode::RowVariable { name, .. } => self.col(&format!("@{}", name), ANSI_VAR),
            AstNode::Str { value, .. }     => value.clone(),
            AstNode::Number { value, .. }  => value.clone(),
            AstNode::Operator { op, .. }   => op.name().to_owned(),
            AstNode::Annotated { formula, .. } => self.render(formula, negated),
        }
    }

    fn render_list(&mut self, elements: &[AstNode], negated: bool) -> String {
        if elements.is_empty() {
            return "()".to_owned();
        }

        // Structural connectives come first — they don't use `format`.
        if let AstNode::Operator { op, .. } = &elements[0] {
            return self.render_operator(op.clone(), &elements[1..], negated);
        }

        // Relation / function application: look up `format(lang, head)`.
        let head_name = match &elements[0] {
            AstNode::Symbol { name, .. } => name.clone(),
            // Variable or literal in head position — no template lookup; flat.
            _ => {
                let head = self.render(&elements[0], false);
                let args = self.render_args(&elements[1..]);
                return format!("{}({})", head, args.join(", "));
            }
        };

        let args: Vec<AstNode> = elements[1..].to_vec();
        let fmt = self.sem.format_string(&head_name, Some(self.language));
        if let Some(entry) = fmt.into_iter().next() {
            return self.apply_template(&entry.text, &args, negated);
        }

        // No format template — render arguments via termFormat and emit
        // `<head>(a, b, c)`.
        self.missing.insert(format!("format:{}", head_name));
        let head_rendered = self.render_term_symbol(&head_name);
        let arg_strs: Vec<String> = args.iter().map(|a| self.render(a, false)).collect();
        format!("{}({})", head_rendered, arg_strs.join(", "))
    }

    fn render_args(&mut self, args: &[AstNode]) -> Vec<String> {
        args.iter().map(|a| self.render(a, false)).collect()
    }

    /// Render `(op args...)` for a built-in logical connective — no `format`
    /// entry is expected for these.
    fn render_operator(&mut self, op: OpKind, args: &[AstNode], negated: bool) -> String {
        match op {
            OpKind::And => {
                let parts = self.render_args(args);
                if parts.is_empty() { return "true".into(); }
                if parts.len() == 1 { return parts.into_iter().next().unwrap(); }
                let sep = format!(" {} ", self.col("and", ANSI_OP));
                parts.join(&sep)
            }
            OpKind::Or => {
                let parts = self.render_args(args);
                if parts.is_empty() { return "false".into(); }
                if parts.len() == 1 { return parts.into_iter().next().unwrap(); }
                let sep = format!(" {} ", self.col("or", ANSI_OP));
                parts.join(&sep)
            }
            OpKind::Not => {
                // Propagate negation one level into the child *if* that child is
                // a relation call that will actually consume `%n`.  Otherwise
                // wrap with "it is not the case that".
                if let Some(child) = args.first() {
                    if let AstNode::List { elements, .. } = child {
                        if let Some(AstNode::Symbol { name, .. }) = elements.first() {
                            let fmt = self.sem.format_string(name, Some(self.language));
                            if let Some(entry) = fmt.into_iter().next() {
                                if entry.text.contains("%n") {
                                    let inner_args: Vec<AstNode> = elements[1..].to_vec();
                                    return self.apply_template(&entry.text, &inner_args, /*negated=*/ true);
                                }
                            }
                        }
                    }
                    let inner = self.render(child, negated);
                    let lead = self.col("it is not the case that", ANSI_NEG);
                    return format!("{} {}", lead, inner);
                }
                "not ()".into()
            }
            OpKind::Implies => match args {
                [a, b] => format!(
                    "{} {} {} {}",
                    self.col("if", ANSI_OP),
                    self.render(a, false),
                    self.col("then", ANSI_OP),
                    self.render(b, false),
                ),
                _ => format!("(=> {})", self.render_args(args).join(" ")),
            },
            OpKind::Iff => match args {
                [a, b] => format!(
                    "{} {} {}",
                    self.render(a, false),
                    self.col("if and only if", ANSI_OP),
                    self.render(b, false),
                ),
                _ => format!("(<=> {})", self.render_args(args).join(" ")),
            },
            OpKind::Equal => match args {
                [a, b] => format!(
                    "{} {} {}",
                    self.render(a, false),
                    self.col("equals", ANSI_OP),
                    self.render(b, false),
                ),
                _ => format!("(equal {})", self.render_args(args).join(" ")),
            },
            OpKind::ForAll | OpKind::Exists => {
                let lead_raw = if matches!(op, OpKind::Exists) {
                    "there exists some"
                } else {
                    "for every"
                };
                let lead = self.col(lead_raw, ANSI_OP);
                let (var_names, body) = extract_quantifier_vars_and_body(args);
                let vars_txt = if var_names.is_empty() {
                    String::new()
                } else {
                    var_names.into_iter()
                        .map(|v| self.col(&format!("?{}", v), ANSI_VAR))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let body_txt = body.map(|b| self.render(&b, false)).unwrap_or_default();
                let connector = if matches!(op, OpKind::Exists) {
                    self.col("such that", ANSI_OP)
                } else {
                    ",".to_owned()
                };
                if vars_txt.is_empty() {
                    body_txt
                } else {
                    format!("{} {} {} {}", lead, vars_txt, connector, body_txt).trim().to_string()
                }
            }
        }
    }

    /// Substitute `%1`-`%9`, `&%Symbol`, `%n`, `%n{…}` in `template` against
    /// `args`.  Unknown markers are left literal so template bugs are visible.
    fn apply_template(&mut self, template: &str, args: &[AstNode], negated: bool) -> String {
        let bytes = template.as_bytes();
        let mut out = String::with_capacity(template.len() + 16);
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'%' if i + 1 < bytes.len() => {
                    let c = bytes[i + 1];
                    match c {
                        b'1'..=b'9' => {
                            let idx = (c - b'0') as usize - 1;
                            if let Some(arg) = args.get(idx) {
                                out.push_str(&self.render(arg, false));
                            } else {
                                out.push('%');
                                out.push(c as char);
                            }
                            i += 2;
                        }
                        b'n' => {
                            if i + 2 < bytes.len() && bytes[i + 2] == b'{' {
                                if let Some(close) = find_matching(&bytes[i + 3..], b'}') {
                                    let text = &template[i + 3..i + 3 + close];
                                    if negated {
                                        out.push_str(&self.col(text, ANSI_NEG));
                                    }
                                    i += 3 + close + 1;
                                    continue;
                                }
                            }
                            if negated {
                                out.push(' ');
                                out.push_str(&self.col("not", ANSI_NEG));
                                out.push(' ');
                            }
                            i += 2;
                        }
                        _ => {
                            out.push('%');
                            out.push(c as char);
                            i += 2;
                        }
                    }
                }
                b'&' if i + 1 < bytes.len() && bytes[i + 1] == b'%' => {
                    let start = i + 2;
                    let end = start + bytes[start..]
                        .iter()
                        .take_while(|&&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                        .count();
                    if end > start {
                        let sym = &template[start..end];
                        let rendered = self.render_term_symbol(sym);
                        out.push_str(&self.col(&rendered, ANSI_LINKED));
                        i = end;
                    } else {
                        out.push('&');
                        i += 1;
                    }
                }
                _ => {
                    out.push(bytes[i] as char);
                    i += 1;
                }
            }
        }
        // Collapse runs of whitespace introduced by empty `%n` substitutions.
        collapse_whitespace(&out)
    }

    /// Render a term/relation name: try `termFormat` in the chosen language,
    /// fall back to the raw name.  Records a miss when the lookup fails.
    fn render_term_symbol(&mut self, name: &str) -> String {
        // Logical constants aren't ontology terms (no `termFormat`) — render
        // literally without recording a miss.
        match name {
            "false" | "False" | "$false" => return "false".to_owned(),
            "true"  | "True"  | "$true"  => return "true".to_owned(),
            "FALSE" => return "a contradiction".to_owned(),
            _ => {}
        }
        // Proof-local skolem witnesses (`sk0`, …) aren't ontology terms either.
        if name.strip_prefix("sk")
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        {
            return name.to_owned();
        }

        let entries = self.sem.term_format_named(name, Some(self.language));
        if let Some(e) = entries.into_iter().next() {
            return e.text;
        }
        self.missing.insert(format!("termFormat:{}", name));
        name.to_owned()
    }
}

/// Extract bound variable names from `(forall (?V1 ?V2 …) body)` /
/// `(exists …)` argument list.  Returns `(names, body_ast)`.
fn extract_quantifier_vars_and_body(args: &[AstNode]) -> (Vec<String>, Option<AstNode>) {
    let mut names = Vec::new();
    let body = if args.len() >= 2 {
        if let AstNode::List { elements: vars, .. } = &args[0] {
            for e in vars {
                if let AstNode::Variable { name, .. } = e {
                    names.push(name.clone());
                }
            }
        }
        Some(args[1].clone())
    } else if args.len() == 1 {
        Some(args[0].clone())
    } else {
        None
    };
    (names, body)
}

/// Byte offset of the next `target`, ignoring nested `{…}` groups (so `%n{TEXT}`
/// parses without tripping on a `}` inside `TEXT`).
fn find_matching(bytes: &[u8], target: u8) -> Option<usize> {
    let mut depth: usize = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'{' { depth += 1; }
        else if b == target {
            if depth == 0 { return Some(i); }
            depth -= 1;
        }
    }
    None
}

/// Collapse consecutive spaces into a single space and trim ends.  Conservative
/// — preserves newlines/tabs so templates with intentional spacing stay readable.
fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c == ' ' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim().to_owned()
}
