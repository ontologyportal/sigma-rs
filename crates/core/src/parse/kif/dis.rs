// crates/core/src/parse/kif/dis.rs
//
// KIF emission dialect — the single home for AstNode → KIF rendering.
//
// KIF has no statement-level framing ("everything is an axiom"), so a statement
// is just its formula rendered as KIF.  Three views are provided, all here:
//
//   * `flat`       — compact one-line `(op a b)`; also the `Display` impl.
//   * `format_plain` (styled, color=false) — indented, width-wrapped, ASCII;
//     always re-parseable.
//   * `pretty_print` (styled, color=true)  — same layout, ANSI-coloured for
//     terminals.
//
// `pretty_print` and `format_plain` share one width-wrapping implementation
// (`styled`), differing only in leaf colourisation.  The [`AstKif`] extension
// trait re-exposes the three as methods on `AstNode` so callers `use` it and
// keep `node.flat()` / `node.pretty_print(0)` syntax; `ast.rs` itself carries
// no rendering logic.

use core::fmt;
use inline_colorization::*;

use crate::parse::ast::AstNode;
use crate::parse::dialect::{Emit, PrettyEmit};

/// Soft-wrap threshold: forms fitting in this many columns at their indent stay
/// on one line; longer ones break with each argument indented two further.
const LINE_WIDTH: usize = 72;

// -- the rendering logic ------------------------------------------------------

/// Compact flat KIF — `(op a b)` with no extra spaces.
pub(crate) fn flat(node: &AstNode) -> String {
    match node {
        AstNode::List { elements, .. } => {
            if elements.is_empty() { return "()".into(); }
            format!("({})", elements.iter().map(flat).collect::<Vec<_>>().join(" "))
        }
        AstNode::Symbol { name, .. }      => name.clone(),
        AstNode::Variable { name, .. }    => format!("?{}", name),
        AstNode::RowVariable { name, .. } => format!("@{}", name),
        AstNode::Str { value, .. }
        | AstNode::Number { value, .. }   => value.clone(),
        AstNode::Operator { op, .. }      => op.name().to_owned(),
        AstNode::Annotated { formula, .. } => flat(formula),
    }
}

/// Indented, width-wrapped rendering.  `color` toggles ANSI leaf colourisation;
/// the layout (line breaking, quantifier-on-head-line rule) is identical either
/// way, so the plain and coloured renderers can never drift.
pub(crate) fn styled(node: &AstNode, indent: usize, color: bool) -> String {
    // Statement wrapper: render its formula (annotation framing is `Emit`'s job).
    if let AstNode::Annotated { formula, .. } = node {
        return styled(formula, indent, color);
    }
    let leaf = |n: &AstNode| if color { Pretty(n).to_string() } else { flat(n) };

    let f = flat(node);
    if indent + f.len() <= LINE_WIDTH {
        return leaf(node);
    }
    match node {
        AstNode::List { elements, .. } if elements.len() >= 2 => {
            let pad  = " ".repeat(indent + 2);
            let head = styled(&elements[0], 0, color);

            // Quantifier rule: `(forall|exists VARS BODY...)` keeps the variable
            // list on the head line, grouping the binding with its operator.
            if is_quantifier_head(&elements[0]) && elements.len() >= 3 {
                let vars = leaf(&elements[1]);
                let body: Vec<String> = elements[2..].iter()
                    .map(|e| format!("{}{}", pad, styled(e, indent + 2, color)))
                    .collect();
                return format!("({} {}\n{})", head, vars, body.join("\n"));
            }

            let args: Vec<String> = elements[1..].iter()
                .map(|e| format!("{}{}", pad, styled(e, indent + 2, color)))
                .collect();
            format!("({}\n{})", head, args.join("\n"))
        }
        _ => leaf(node),
    }
}

/// `true` iff `head` is an `Operator` quantifier (`forall` / `exists`).
fn is_quantifier_head(head: &AstNode) -> bool {
    matches!(head, AstNode::Operator { op, .. } if op.is_quantifier())
}

// -- extension trait: keep `node.flat()` / `.pretty_print()` / `.format_plain()`

/// KIF rendering methods on [`AstNode`].  `use` this (re-exported at the crate
/// root as `sigmakee_rs_core::AstKif`) where the method syntax is wanted; the
/// implementation lives here, not on the type.
pub trait AstKif {
    fn flat(&self) -> String;
    fn pretty_print(&self, indent: usize) -> String;
    fn format_plain(&self, indent: usize) -> String;
}

impl AstKif for AstNode {
    fn flat(&self) -> String { flat(self) }
    // The two styled views route through the `PrettyEmit` dialect seam (whose
    // sole implementation is `KifEmit`), so there is one rendering path: AstKif
    // is just the method-syntax facade over it.
    fn pretty_print(&self, indent: usize) -> String { KifEmit.emit_pretty(self, indent, true) }
    fn format_plain(&self, indent: usize) -> String { KifEmit.emit_pretty(self, indent, false) }
}

// -- dialect trait impls ------------------------------------------------------

/// The KIF output dialect.  Stateless (no per-format options).
pub(crate) struct KifEmit;

impl Emit for KifEmit {
    fn emit_formula(&self, f: &AstNode) -> String {
        styled(f, 0, false) // canonical re-parseable KIF
    }
    fn emit_statement(&self, stmt: &AstNode) -> Result<String, String> {
        // KIF carries no role/name framing — emit the formula. Never drops.
        Ok(self.emit_formula(stmt.formula()))
    }
}

impl PrettyEmit for KifEmit {
    fn emit_pretty(&self, node: &AstNode, indent: usize, color: bool) -> String {
        styled(node, indent, color)
    }
}

// -- Display / Pretty (canonical flat KIF; live with the rest of KIF emission)-

impl fmt::Display for AstNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&flat(self))
    }
}

/// Colourised flat display wrapper for [`AstNode`] (terminal/log output).
pub(crate) struct Pretty<'a>(pub &'a AstNode);

impl fmt::Display for Pretty<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            AstNode::List { elements, .. } => {
                f.write_str("(")?;
                let mut first = true;
                for el in elements {
                    if !first { f.write_str(" ")?; }
                    first = false;
                    write!(f, "{}", Pretty(el))?;
                }
                f.write_str(")")
            }
            AstNode::Operator { op, .. } =>
                write!(f, "{color_cyan}{}{color_reset}", op.name()),
            AstNode::Number { value, .. }
            | AstNode::Str { value, .. } => write!(f, "{color_green}{}{color_reset}", value),
            AstNode::Variable { .. }
            | AstNode::RowVariable { .. } => write!(f, "{color_magenta}{}{color_reset}", flat(self.0)),
            AstNode::Symbol { name, .. } => {
                if name.chars().next().map_or(false, |c| c.is_lowercase()) {
                    write!(f, "{color_bright_blue}{}{color_reset}", name)
                } else {
                    write!(f, "{color_yellow}{}{color_reset}", name)
                }
            }
            AstNode::Annotated { formula, .. } => write!(f, "{}", Pretty(formula)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::ast::{Role, Span};
    use crate::parse::dialect::Emitter;

    fn parse_one(src: &str) -> AstNode {
        let doc = crate::parse::parse_document("t", src, crate::Parser::Kif);
        assert!(doc.parse_errors.is_empty(), "parse errors: {:?}", doc.parse_errors);
        doc.ast.into_iter().next().unwrap().as_stmt().cloned().unwrap()
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut in_esc = false;
        for c in s.chars() {
            if c == '\x1B' { in_esc = true; continue; }
            if in_esc { if c == 'm' { in_esc = false; } continue; }
            out.push(c);
        }
        out
    }

    #[test]
    fn display_has_no_internal_padding() {
        assert_eq!(parse_one("(and (instance Foo Bar) (instance Foo Baz))").to_string(),
            "(and (instance Foo Bar) (instance Foo Baz))");
    }

    #[test]
    fn format_plain_inlines_quantifier_vars() {
        let n = parse_one(
            "(exists (?A ?B) (and (member ?A ?P) (member ?B ?P) \
             (not (equal ?A ?B)) (instance ?A SomeLongClassName)))");
        let out = n.format_plain(0);
        assert_eq!(out.lines().next().unwrap(), "(exists (?A ?B)");
        assert!(out.lines().nth(1).unwrap().starts_with("  "));
    }

    #[test]
    fn pretty_print_matches_plain_layout_ignoring_color() {
        let n = parse_one(
            "(and (instance Foo Bar) (instance Foo Baz) \
             (instance Foo VeryLongClassName) (instance Foo AnotherLong))");
        assert_eq!(strip_ansi(&n.pretty_print(0)), n.format_plain(0));
    }

    #[test]
    fn kif_emit_matches_format_plain() {
        let n = parse_one("(exists (?A ?B) (and (member ?A ?P) (instance ?A SomeLongClassName)))");
        let r = Emitter::Kif.emit_one(&n);
        assert_eq!(r.text.trim_end(), n.format_plain(0));
        assert!(r.is_complete());
    }

    #[test]
    fn kif_strips_annotation_framing() {
        let inner = parse_one("(instance Foo Bar)");
        let ann = AstNode::Annotated {
            role: Role::Conjecture, name: Some("c".into()), source: None,
            formula: Box::new(inner.clone()), span: Span::default(),
        };
        assert_eq!(Emitter::Kif.emit_one(&ann).text.trim_end(), inner.format_plain(0));
        assert_eq!(ann.flat(), inner.flat());
    }
}
