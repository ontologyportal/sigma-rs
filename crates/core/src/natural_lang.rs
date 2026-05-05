//! Natural-language rendering of SUO-KIF formulas via SUMO's
//! `format` and `termFormat` relations.
//!
//! This module backs the `--proof <LANGUAGE>` option on the `sumo ask`
//! CLI: once Vampire produces a proof, each step's AST is handed to
//! [`KnowledgeBase::render_formula`] which walks the tree, looks up
//! template strings in the chosen language, and substitutes argument
//! renderings in positional order.
//!
//! ## Template DSL
//!
//! Templates live in the KB as `(format ?LANG ?REL "...")` and
//! `(termFormat ?LANG ?SYM "...")` facts.  The notation we decode:
//!
//! | Marker       | Meaning                                                      |
//! |--------------|--------------------------------------------------------------|
//! | `%1` … `%9`  | Positional argument (1-indexed) rendered recursively.        |
//! | `&%Symbol`   | Cross-reference: render via `termFormat(Symbol, lang)` with a fallback to the raw name. |
//! | `%n`         | Negation placeholder: empty in positive context, `" not "` in negative. |
//! | `%n{TEXT}`   | Custom negation phrase: empty in positive context, `TEXT` in negative. |
//!
//! ## Structural cases (no `format` entry required)
//!
//! SUMO doesn't provide `format` strings for the logical connectives,
//! so we render them structurally:
//!
//! | KIF                        | Rendering                                       |
//! |----------------------------|-------------------------------------------------|
//! | `(and A B C)`              | `A and B and C`                                 |
//! | `(or A B C)`               | `A or B or C`                                   |
//! | `(not P)`                  | `P` with its negation context propagated, or `it is not the case that P` |
//! | `(=> A B)`                 | `if A then B`                                   |
//! | `(<=> A B)`                | `A if and only if B`                            |
//! | `(exists (?V …) B)`        | `there exists some ?V such that B`              |
//! | `(forall (?V …) B)`        | `for every ?V, B`                               |
//! | `(equal X Y)`              | `X equals Y` (unless `format` overrides it)     |
//!
//! ## Missing specifier handling
//!
//! When a relation lacks a `format` entry or an argument term lacks a
//! `termFormat` entry in the chosen language, the renderer **does not**
//! silently guess.  Missing symbols are collected in a
//! [`RenderReport::missing`] vector, and the caller can decide to fall
//! back to the bare KIF rendering with a warning listing the missing
//! specifiers.  The rendered string still contains a best-effort
//! rendering (bare symbol names where templates are missing) for
//! diagnostic purposes, but callers that want fidelity should check
//! `missing.is_empty()` and fall back otherwise.

#![cfg(feature = "ask")]

use std::collections::BTreeSet;

use crate::kb::KnowledgeBase;
use crate::parse::ast::AstNode;
use crate::parse::ast::OpKind;

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

impl KnowledgeBase {
    /// Render `formula` to natural language in `language` (e.g.
    /// `"EnglishLanguage"`).  See module docs for the template DSL.
    ///
    /// Plain (no ANSI colour escapes) — safe to embed in non-terminal
    /// sinks (logs, files, JSON).  Use [`render_formula_colored`] when
    /// the output is headed to a terminal.
    ///
    /// [`render_formula_colored`]: KnowledgeBase::render_formula_colored
    pub fn render_formula(&self, formula: &AstNode, language: &str) -> RenderReport {
        self.render_formula_impl(formula, language, /*coloured=*/ false)
    }

    /// Same as [`render_formula`] but wraps certain lexical classes in
    /// ANSI colour escapes so terminals can highlight them.  Classes:
    ///
    /// | Class                              | Colour        |
    /// |------------------------------------|---------------|
    /// | Variables (`?X`, `@Row`)           | magenta       |
    /// | `&%Symbol` cross-references        | bright blue   |
    /// | Negations (`%n`, `%n{…}`, "not …") | bright red    |
    /// | Structural operators (`and`, `or`, `if`/`then`, `equals`, `for every`, `there exists some`, `such that`, `if and only if`) | cyan |
    ///
    /// Plain English text from format templates and the SUMO
    /// `termFormat` table is left uncoloured — the point is to let the
    /// eye scan for the semantically-loaded pieces, not to paint every
    /// word.
    ///
    /// [`render_formula`]: KnowledgeBase::render_formula
    pub fn render_formula_colored(&self, formula: &AstNode, language: &str) -> RenderReport {
        self.render_formula_impl(formula, language, /*coloured=*/ true)
    }

    fn render_formula_impl(
        &self,
        formula:  &AstNode,
        language: &str,
        coloured: bool,
    ) -> RenderReport {
        let mut ctx = RenderCtx {
            kb: self,
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

// ANSI colour classes, re-exported from the `inline_colorization`
// crate used throughout the workspace.  Aliased here so the mapping
// from *semantic class* (variable, linked symbol, negation, operator)
// to *palette colour* stays easy to tune from a single place.
//
// `ANSI_RESET` must follow every open — we always emit matched pairs
// and never nest, so a single `color_reset` closes the innermost open.
use inline_colorization::{
    color_bright_blue as ANSI_LINKED, // `&%Symbol` cross-references
    color_bright_red  as ANSI_NEG,    // negations
    color_cyan        as ANSI_OP,     // structural connectives
    color_magenta     as ANSI_VAR,    // variables
    color_reset       as ANSI_RESET,
};

struct RenderCtx<'a> {
    kb:       &'a KnowledgeBase,
    language: &'a str,
    missing:  BTreeSet<String>,
    coloured: bool,
}

impl<'a> RenderCtx<'a> {
    /// Wrap `text` in an ANSI colour class when `self.coloured` is on.
    /// When off, returns a plain owned copy so call sites can use the
    /// result uniformly.
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

    /// Entry point.  `negated` carries an enclosing `(not …)` into a
    /// relation's `%n` / `%n{…}` markers.  The flag is reset to
    /// `false` whenever we descend into a sub-formula that is itself
    /// not a direct child of the `not`.
    fn render(&mut self, node: &AstNode, negated: bool) -> String {
        match node {
            AstNode::List { elements, .. } => self.render_list(elements, negated),
            AstNode::Symbol { name, .. }   => self.render_term_symbol(name),
            AstNode::Variable { name, .. } => self.col(&format!("?{}", name), ANSI_VAR),
            // Row variables (`@args`) are rare in user-facing formulas
            // and only survive into clausal proofs as raw names.
            AstNode::RowVariable { name, .. } => self.col(&format!("@{}", name), ANSI_VAR),
            AstNode::Str { value, .. }     => value.clone(),
            AstNode::Number { value, .. }  => value.clone(),
            // Bare operators in head-less position — unusual but show
            // their KIF name rather than crashing.
            AstNode::Operator { op, .. }   => op.name().to_owned(),
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
            // Variable or literal in head position — no template
            // lookup is possible; fall back to a flat rendering.
            _ => {
                let head = self.render(&elements[0], false);
                let args = self.render_args(&elements[1..]);
                return format!("{}({})", head, args.join(", "));
            }
        };

        let args: Vec<AstNode> = elements[1..].to_vec();
        let fmt = self.kb.format_string(&head_name, Some(self.language));
        if let Some(entry) = fmt.into_iter().next() {
            return self.apply_template(&entry.text, &args, negated);
        }

        // No format template — try to at least render arguments via
        // termFormat and emit `<head>(a, b, c)`.
        self.missing.insert(format!("format:{}", head_name));
        let head_rendered = self.render_term_symbol(&head_name);
        let arg_strs: Vec<String> = args.iter().map(|a| self.render(a, false)).collect();
        format!("{}({})", head_rendered, arg_strs.join(", "))
    }

    fn render_args(&mut self, args: &[AstNode]) -> Vec<String> {
        args.iter().map(|a| self.render(a, false)).collect()
    }

    /// Render `(op args...)` where `op` is a built-in logical
    /// connective — no `format` entry is expected for these.
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
                // Propagate negation one level into the child *if* that
                // child is a relation call that will actually consume
                // `%n`.  Otherwise, wrap with "it is not the case that".
                if let Some(child) = args.first() {
                    if let AstNode::List { elements, .. } = child {
                        if let Some(AstNode::Symbol { name, .. }) = elements.first() {
                            let fmt = self.kb.format_string(name, Some(self.language));
                            if let Some(entry) = fmt.into_iter().next() {
                                if entry.text.contains("%n") {
                                    let inner_args: Vec<AstNode> = elements[1..].to_vec();
                                    return self.apply_template(&entry.text, &inner_args, /*negated=*/ true);
                                }
                            }
                        }
                    }
                    // No template / template lacks %n: fall back.
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
                let body_txt = body.map(|b| self.render(&b, false))
                    .unwrap_or_default();
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

    /// Substitute `%1`-`%9`, `&%Symbol`, `%n`, `%n{…}` in `template`
    /// against `args`.  Unknown markers are left literal so users can
    /// spot template bugs.
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
                                // Template referenced %N but axiom
                                // supplied too few args — keep the
                                // literal so the bug is visible.
                                out.push('%');
                                out.push(c as char);
                            }
                            i += 2;
                        }
                        b'n' => {
                            // `%n{...}` uses custom text; plain `%n`
                            // becomes " not ".
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
                                // Keep the leading/trailing spaces *outside*
                                // the colour region so the surrounding text
                                // in the template stays uncoloured.
                                out.push(' ');
                                out.push_str(&self.col("not", ANSI_NEG));
                                out.push(' ');
                            }
                            i += 2;
                        }
                        _ => {
                            // Unknown `%X`: leave literal.
                            out.push('%');
                            out.push(c as char);
                            i += 2;
                        }
                    }
                }
                b'&' if i + 1 < bytes.len() && bytes[i + 1] == b'%' => {
                    // `&%Identifier` -> termFormat lookup.  These are
                    // explicit cross-references in the template DSL, so
                    // they get the "linked symbol" highlight in
                    // coloured output.
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
                        // `&%` with no identifier — literal.
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

    /// Render a term/relation name: try `termFormat` in the chosen
    /// language, fall back to the raw name.  Records a miss in
    /// `self.missing` when the lookup fails.
    fn render_term_symbol(&mut self, name: &str) -> String {
        // Logical constants: Vampire's refutations terminate in `$false`,
        // which the KIF translator surfaces as a bare `false` symbol.
        // Similar story for `true` in saturation-style outputs.  Neither
        // appears in SUMO's `termFormat` axioms (they're not ontology
        // terms — they're the truth values of FOL itself), so we'd
        // otherwise flag them as "missing specs" and fall back to KIF
        // for the step.  Intercept the lookup and render them literally
        // without recording a miss.
        match name {
            "false" | "False" | "$false" => return "false".to_owned(),
            "true"  | "True"  | "$true"  => return "true".to_owned(),
            _ => {}
        }

        // SUMO uses `termFormat` primarily for readable common-noun
        // forms; the bare name is a safe fallback for things like
        // instances that naturally read as proper nouns (e.g. `UnitedStates`).
        let entries = self.kb.term_format(name, Some(self.language));
        if let Some(e) = entries.into_iter().next() {
            return e.text;
        }
        self.missing.insert(format!("termFormat:{}", name));
        name.to_owned()
    }
}

/// Extract bound variable names from `(forall (?V1 ?V2 …) body)` /
/// `(exists …)` argument list.  Returns (names, body_ast).  The body
/// is cloned because the caller owns the render context.
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

/// Return the byte offset of the next byte equal to `target`, ignoring
/// any nested `{…}` groups.  Used to parse `%n{TEXT}` without getting
/// confused by a `}` inside `TEXT`.
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

/// Collapse consecutive spaces into a single space and trim ends.
/// Conservative — preserves newlines and tabs — so complex templates
/// with intentional spacing stay readable.
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

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KnowledgeBase;

    /// Load a tiny in-memory KB with just enough `format` /
    /// `termFormat` to exercise the template DSL.
    fn kb_with_english_templates(extras: &str) -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let kif = format!(r#"
            (termFormat EnglishLanguage Dog "dog")
            (termFormat EnglishLanguage Animal "animal")
            (termFormat EnglishLanguage Fido "fido")
            (termFormat EnglishLanguage Juno "juno")
            (termFormat EnglishLanguage attribute "attribute")
            (termFormat EnglishLanguage instance "instance")
            (termFormat EnglishLanguage subclass "subclass")
            (termFormat EnglishLanguage Friendly "friendly")
            (format EnglishLanguage instance "%1 is %n an instance of %2")
            (format EnglishLanguage subclass "%1 is %n a subclass of %2")
            (format EnglishLanguage attribute "%1 has %n the attribute %2")
            (format EnglishLanguage likes "%1 %n{{does not}} &%likes %2")
            (termFormat EnglishLanguage likes "likes")
            {extras}
        "#);
        let r = kb.load_kif(&kif, "tests.kif", Some("tests.kif"));
        assert!(r.ok, "load failed: {:?}", r.errors);
        kb.make_session_axiomatic("tests.kif");
        kb
    }

    fn parse_kif_formula(kif: &str) -> AstNode {
        use crate::parse::parse_document;
        let doc = parse_document("test", kif);
        assert!(!doc.has_errors(), "parse errors: {:?}", doc.diagnostics);
        doc.ast.into_iter().next().expect("at least one root")
    }

    #[test]
    fn positive_binary_predicate() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(instance Fido Dog)");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert_eq!(r.rendered, "fido is an instance of dog");
        assert!(r.missing.is_empty(), "unexpected misses: {:?}", r.missing);
    }

    #[test]
    fn negated_n_marker_becomes_not() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(not (instance Fido Dog))");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert_eq!(r.rendered, "fido is not an instance of dog");
        assert!(r.missing.is_empty(), "unexpected misses: {:?}", r.missing);
    }

    #[test]
    fn custom_negation_phrase() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(not (likes Fido Juno))");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert!(r.rendered.contains("does not"), "got: {}", r.rendered);
        assert!(r.rendered.contains("likes"), "got: {}", r.rendered);
    }

    #[test]
    fn cross_reference_uses_termformat() {
        // The `instance` template references `attribute` nowhere, but
        // we can smoke-test cross-reference with a bespoke format
        // string injected via `extras`.
        let kb = kb_with_english_templates(
            r#"(format EnglishLanguage hasAttr "%1 has the &%attribute %2")
               (termFormat EnglishLanguage hasAttr "has attribute")"#
        );
        let f = parse_kif_formula("(hasAttr Fido Friendly)");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert_eq!(r.rendered, "fido has the attribute friendly");
    }

    #[test]
    fn conjunction_rendering() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(and (instance Fido Dog) (instance Fido Animal))");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert_eq!(
            r.rendered,
            "fido is an instance of dog and fido is an instance of animal"
        );
    }

    #[test]
    fn implication_rendering() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(=> (instance Fido Dog) (instance Fido Animal))");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert_eq!(
            r.rendered,
            "if fido is an instance of dog then fido is an instance of animal"
        );
    }

    #[test]
    fn quantifier_rendering() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(forall (?X) (instance ?X Animal))");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert_eq!(
            r.rendered,
            "for every ?X , ?X is an instance of animal"
        );
    }

    #[test]
    fn missing_format_records_miss_and_falls_back() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(undefinedRel Fido Dog)");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert!(r.missing.iter().any(|m| m.starts_with("format:")
            && m.contains("undefinedRel")),
            "expected format:undefinedRel miss, got: {:?}", r.missing);
        // Best-effort rendering still includes the args.
        assert!(r.rendered.contains("fido"), "got: {}", r.rendered);
        assert!(r.rendered.contains("dog"),  "got: {}", r.rendered);
    }

    #[test]
    fn missing_termformat_records_miss() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(instance UnknownThing Dog)");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert!(
            r.missing.iter().any(|m| m == "termFormat:UnknownThing"),
            "expected termFormat:UnknownThing miss, got: {:?}", r.missing
        );
    }

    #[test]
    fn logical_constants_render_without_being_flagged_missing() {
        // `$true` / `$false` can't be lexed by the KIF parser (`$` is
        // not a legal KIF character), so they only reach the renderer
        // if an upstream component synthesises them — we cover them
        // defensively in `render_term_symbol` and exercise that code
        // path by constructing `AstNode::Symbol` directly below.  The
        // bare `true` / `false` / `True` / `False` cases *are*
        // parseable and represent what actually shows up in Vampire
        // refutation proofs.
        let kb = kb_with_english_templates("");
        for name in ["true", "false", "True", "False"] {
            let f = parse_kif_formula(name);
            let r = kb.render_formula(&f, "EnglishLanguage");
            let expected = name.to_lowercase();
            assert_eq!(
                r.rendered, expected,
                "expected {} → {:?}, got {:?}", name, expected, r.rendered
            );
            assert!(
                !r.missing.iter().any(|m| m.contains(name)),
                "logical constant `{}` should not register as missing: {:?}",
                name, r.missing,
            );
        }
        // Direct-construction path for the `$`-prefixed variants.
        use crate::parse::ast::Span;
        for name in ["$true", "$false"] {
            let node = AstNode::Symbol {
                name: name.to_string(),
                span: Span::point("test".to_string(), 0, 0, 0),
            };
            let r = kb.render_formula(&node, "EnglishLanguage");
            let expected = name.trim_start_matches('$').to_lowercase();
            assert_eq!(
                r.rendered, expected,
                "expected {} → {:?}, got {:?}", name, expected, r.rendered
            );
            assert!(r.missing.is_empty(),
                "logical constant `{}` should not register as missing: {:?}",
                name, r.missing,
            );
        }
    }

    // -- Colour rendering --------------------------------------------------
    //
    // `render_formula_colored` wraps lexical classes in ANSI escapes.
    // We assert presence-and-placement of each class (variable, linked
    // symbol via &%, negation, operator) rather than exact bytes, so
    // the palette can shift without breaking these tests.  The colour
    // codes are the module constants — we check for both the opening
    // and the matching `ANSI_RESET` to catch unbalanced emission.

    /// Strip every `\x1b[…m` sequence from a rendered string.  Gives
    /// back the "plain" view so we can assert structural text stays
    /// intact regardless of coloration.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
                // Skip CSI sequence: ESC [ ... m
                let mut j = i + 2;
                while j < bytes.len() && bytes[j] != b'm' { j += 1; }
                i = j + 1;
                continue;
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    #[test]
    fn coloured_stripped_equals_plain_render() {
        // The coloured output must reduce to the uncoloured output
        // when ANSI escapes are removed — guards against accidentally
        // dropping text while colouring.  Checked across a mix of
        // no-colour (pure atom), colour-expected, and edge cases.
        let kb = kb_with_english_templates("");
        let cases = [
            "(instance Fido Dog)",                              // no colour expected
            "(not (instance Fido Dog))",                        // negation
            "(and (instance Fido Dog) (instance Fido Animal))", // connective
            "(=> (instance Fido Dog) (instance Fido Animal))",  // if/then
            "(forall (?X) (instance ?X Animal))",               // variables + connective
            "(not (likes Fido Juno))",                          // custom negation
        ];
        for kif in cases {
            let f = parse_kif_formula(kif);
            let plain = kb.render_formula(&f, "EnglishLanguage").rendered;
            let coloured = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
            assert_eq!(
                strip_ansi(&coloured), plain,
                "colour stripping must recover plain output for {}", kif
            );
        }
    }

    #[test]
    fn formulas_with_colourable_elements_actually_gain_colour() {
        // At least one ANSI open must appear whenever the formula
        // contains any of: variable, `&%`, negation, or connective.
        // A bare `(predicate arg arg)` with no colourable elements
        // intentionally returns the same string as the plain path —
        // covered by a separate assertion below.
        let kb = kb_with_english_templates("");
        for kif in [
            "(not (instance Fido Dog))",
            "(and (instance Fido Dog) (instance Fido Animal))",
            "(=> (instance Fido Dog) (instance Fido Animal))",
            "(forall (?X) (instance ?X Animal))",
            "(not (likes Fido Juno))",
        ] {
            let f = parse_kif_formula(kif);
            let coloured = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
            assert!(
                coloured.contains("\x1b["),
                "expected ANSI colour in {}: {:?}", kif, coloured
            );
        }

        // Conversely, a bare predicate with no variables, `&%`, or
        // connectives has nothing to colour — coloured == plain.
        let f = parse_kif_formula("(instance Fido Dog)");
        let plain    = kb.render_formula(&f, "EnglishLanguage").rendered;
        let coloured = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
        assert_eq!(plain, coloured,
            "bare predicate with no colourable class should render identically");
    }

    #[test]
    fn variables_are_wrapped_in_magenta() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(forall (?X) (instance ?X Animal))");
        let r = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
        // `?X` appears twice (once in the quantifier list, once in the
        // body).  Each occurrence must be wrapped in a matched pair.
        let magenta_opens = r.matches(super::ANSI_VAR).count();
        let resets        = r.matches(super::ANSI_RESET).count();
        assert!(magenta_opens >= 2, "expected ≥2 magenta opens, got {}: {:?}", magenta_opens, r);
        assert!(resets >= magenta_opens, "resets must cover opens: {:?}", r);
    }

    #[test]
    fn connectives_are_wrapped_in_cyan() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula(
            "(and (instance Fido Dog) (=> (instance Fido Dog) (instance Fido Animal)))",
        );
        let r = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
        // `and`, `if`, `then` should all be coloured.
        assert!(r.contains(&format!("{}and{}", super::ANSI_OP, super::ANSI_RESET)),
            "missing cyan `and`: {:?}", r);
        assert!(r.contains(&format!("{}if{}", super::ANSI_OP, super::ANSI_RESET)),
            "missing cyan `if`: {:?}", r);
        assert!(r.contains(&format!("{}then{}", super::ANSI_OP, super::ANSI_RESET)),
            "missing cyan `then`: {:?}", r);
    }

    #[test]
    fn negation_n_marker_is_wrapped_in_red() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(not (instance Fido Dog))");
        let r = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
        assert!(r.contains(&format!("{}not{}", super::ANSI_NEG, super::ANSI_RESET)),
            "missing red `not`: {:?}", r);
    }

    #[test]
    fn custom_negation_phrase_is_wrapped_in_red() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(not (likes Fido Juno))");
        let r = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
        assert!(r.contains(&format!("{}does not{}", super::ANSI_NEG, super::ANSI_RESET)),
            "missing red `does not`: {:?}", r);
    }

    #[test]
    fn cross_reference_is_wrapped_in_bright_blue() {
        let kb = kb_with_english_templates(
            r#"(format EnglishLanguage hasAttr "%1 has the &%attribute %2")
               (termFormat EnglishLanguage hasAttr "has attribute")"#
        );
        let f = parse_kif_formula("(hasAttr Fido Friendly)");
        let r = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
        assert!(r.contains(&format!("{}attribute{}", super::ANSI_LINKED, super::ANSI_RESET)),
            "missing bright-blue `attribute`: {:?}", r);
    }

    #[test]
    fn it_is_not_the_case_fallback_is_red() {
        // A template-less predicate wrapped in `not` triggers the
        // "it is not the case that …" path, which must be red.
        let kb = kb_with_english_templates(
            r#"(format EnglishLanguage bareRel "%1 bareRel %2")
               (termFormat EnglishLanguage bareRel "bare rel")"#,
        );
        let f = parse_kif_formula("(not (bareRel Fido Dog))");
        let r = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
        assert!(
            r.contains(&format!("{}it is not the case that{}", super::ANSI_NEG, super::ANSI_RESET)),
            "missing red `it is not the case that`: {:?}", r
        );
    }

    #[test]
    fn missing_language_reports_everything() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(instance Fido Dog)");
        let r = kb.render_formula(&f, "KlingonLanguage");
        // Every symbol should be missing in Klingon.
        assert!(r.missing.iter().any(|m| m == "format:instance"));
        assert!(r.missing.iter().any(|m| m == "termFormat:Fido"));
        assert!(r.missing.iter().any(|m| m == "termFormat:Dog"));
    }
}
