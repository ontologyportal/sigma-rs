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
use crate::parse::doc::{CommentBlock, DocItem};

/// Soft-wrap threshold: forms fitting in this many columns at their indent stay
/// on one line; longer ones break with each argument indented two further.
const LINE_WIDTH: usize = 72;

/// Compact flat KIF — `(op a b)` with no extra spaces.
pub(crate) fn flat(node: &AstNode) -> String {
    match node {
        AstNode::List { elements, .. } => {
            if elements.is_empty() {
                return "()".into();
            }
            format!(
                "({})",
                elements.iter().map(flat).collect::<Vec<_>>().join(" ")
            )
        }
        AstNode::Symbol { name, .. } => name.clone(),
        AstNode::Variable { name, .. } => format!("?{}", name),
        AstNode::RowVariable { name, .. } => format!("@{}", name),
        AstNode::Str { value, .. } | AstNode::Number { value, .. } => value.clone(),
        AstNode::Operator { op, .. } => op.name().to_owned(),
        AstNode::Annotated { formula, .. } => flat(formula),
    }
}

/// Indented, width-wrapped rendering.  `color` toggles ANSI leaf colourisation;
/// the layout is identical either way, so the plain and coloured renderers can
/// never drift.
pub(crate) fn styled(node: &AstNode, indent: usize, color: bool) -> String {
    let mut none: &[CommentBlock] = &[];
    styled_c(node, indent, color, "", &mut none)
}

/// The one styled-rendering implementation, threading an optional cursor of
/// [`CommentBlock`]s to interleave (see [`format_forms`]).  `rem` must be
/// span-ordered with every block before `node`'s span already drained; blocks
/// interior to `node` are consumed and re-emitted near their source position.
/// With an empty cursor the output is byte-identical to the historical
/// comment-free layout.  `src` is the original source text, consulted only to
/// tell a trailing comment (same line as the element before it) from a
/// standalone one; it is unused when `rem` is empty.
fn styled_c(
    node: &AstNode,
    indent: usize,
    color: bool,
    src: &str,
    rem: &mut &[CommentBlock],
) -> String {
    // Statement wrapper: render its formula (annotation framing is `Emit`'s job).
    if let AstNode::Annotated { formula, .. } = node {
        return styled_c(formula, indent, color, src, rem);
    }
    let leaf = |n: &AstNode| {
        if color {
            Pretty(n).to_string()
        } else {
            flat(n)
        }
    };

    let end_off = node.span().end_offset;
    // Comments inside this node's source extent.  Anything remaining after
    // the body is emitted trails the closing paren.
    let has_interior = rem.first().is_some_and(|c| c.span.offset < end_off);

    let AstNode::List { elements, .. } = node else {
        return leaf(node);
    };
    if elements.len() < 2 {
        let mut out = leaf(node);
        append_trailing_comments(&mut out, indent, drain_before(rem, end_off));
        return out;
    }

    // The one argument slot allowed to sit inline with the head, if any.
    // `query` gets `not`'s exemption: a `.tq` `(query <formula>)` directive
    // conventionally holds its formula on the same line.
    let inline_idx = if (is_quantifier_head(&elements[0]) && elements.len() >= 3)
        || ((is_not_head(&elements[0]) || is_query_head(&elements[0])) && elements.len() == 2)
    {
        Some(1) // the sole argument
    } else {
        None
    };

    // Render the inline slot eagerly — if it itself needs to break (e.g. `not`
    // wrapping a compound `and`), that cascades: the parent can no longer stay
    // on one line either, even though the inline exemption still holds.
    // (Safe with the comment cursor: when interior comments exist the broken
    // path below is forced, so nothing the recursion drains is discarded.)
    let inline_rendered = inline_idx.map(|idx| styled_c(&elements[idx], indent, color, src, rem));

    let forces_break = has_interior
        || inline_rendered.as_deref().is_some_and(|s| s.contains('\n'))
        || elements
            .iter()
            .enumerate()
            .skip(1)
            .any(|(i, e)| Some(i) != inline_idx && is_compound(e));

    let f = flat(node);
    if !forces_break && indent + f.len() <= LINE_WIDTH {
        return leaf(node);
    }

    let pad = " ".repeat(indent + 2);
    let head = styled_c(&elements[0], 0, color, src, rem);

    let (prefix, body_start) = match (inline_idx, inline_rendered) {
        (Some(idx), Some(rendered)) => (format!("({} {}", head, rendered), idx + 1),
        _ => (format!("({}", head), 1),
    };

    let mut body: Vec<String> = Vec::new();
    let mut prev_elem_end: Option<usize> = None;
    for e in &elements[body_start..] {
        for c in drain_before(rem, e.span().offset) {
            emit_interior_comment(&mut body, &pad, src, prev_elem_end, &c);
        }
        body.push(format!(
            "{}{}",
            pad,
            styled_c(e, indent + 2, color, src, rem)
        ));
        prev_elem_end = Some(e.span().end_offset);
    }

    let mut out = if body.is_empty() {
        format!("{prefix})")
    } else {
        format!("{prefix}\n{})", body.join("\n"))
    };
    // Comments between the last element and the closing paren re-emit after
    // the close: gluing `)` onto a comment line would swallow it.
    append_trailing_comments(&mut out, indent, drain_before(rem, end_off));
    out
}

/// Pop and return every block in `rem` starting before byte offset `off`.
fn drain_before(rem: &mut &[CommentBlock], off: usize) -> Vec<CommentBlock> {
    let n = rem.iter().take_while(|c| c.span.offset < off).count();
    let (taken, rest) = rem.split_at(n);
    *rem = rest;
    taken.to_vec()
}

/// Re-flow a comment's words into `; `-prefixed lines at `indent`, greedily
/// filled so no line exceeds [`LINE_WIDTH`] columns (indent and marker
/// included).  A word longer than the budget still gets a line of its own --
/// the width is a soft target, never a reason to split a word.
fn wrap_comment(text: &str, indent: usize) -> String {
    let prefix = format!("{}; ", " ".repeat(indent));
    let budget = LINE_WIDTH.saturating_sub(prefix.len()).max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for w in text.split_whitespace() {
        if !cur.is_empty() && cur.len() + 1 + w.len() > budget {
            lines.push(format!("{prefix}{cur}"));
            cur.clear();
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(format!("{prefix}{cur}").trim_end().to_string());
    }
    lines.join("\n")
}

/// Append a comment to `out`'s current (last) line as ` ; words...`, filling
/// up to [`LINE_WIDTH`] columns; overflow words continue on fresh
/// `indent`-prefixed comment lines via [`wrap_comment`].  When not even the
/// first word fits on the current line, the whole comment drops to its own
/// line(s) below instead.
fn append_comment_to_line(out: &mut String, indent: usize, text: &str) {
    let mut words = text.split_whitespace().peekable();
    if words.peek().is_none() {
        out.push_str(" ;");
        return;
    }
    let mut cur_len = out.rsplit('\n').next().map_or(0, str::len);
    let mut on_line = false;
    while let Some(&w) = words.peek() {
        let extra = if on_line { 1 + w.len() } else { 3 + w.len() };
        if cur_len + extra > LINE_WIDTH {
            break;
        }
        out.push_str(if on_line { " " } else { " ; " });
        out.push_str(w);
        cur_len += extra;
        on_line = true;
        words.next();
    }
    let rest: Vec<&str> = words.collect();
    if !rest.is_empty() {
        out.push('\n');
        out.push_str(&wrap_comment(&rest.join(" "), indent));
    }
}

/// Emit one interior comment block into `body`: filled onto the previous
/// element's line when it trailed it in the source (no newline between),
/// otherwise as its own `pad`-indented, width-wrapped line(s).
fn emit_interior_comment(
    body: &mut Vec<String>,
    pad: &str,
    src: &str,
    prev_elem_end: Option<usize>,
    block: &CommentBlock,
) {
    let trails_prev = prev_elem_end.is_some_and(|pe| {
        let (lo, hi) = (pe.min(src.len()), block.span.offset.min(src.len()));
        lo <= hi && !src[lo..hi].contains('\n')
    });
    match body.last_mut() {
        Some(last) if trails_prev => append_comment_to_line(last, pad.len(), &block.text),
        _ => body.push(wrap_comment(&block.text, pad.len())),
    }
}

/// Append comment blocks after a rendered form: filled onto the closing line
/// (`...) ; comment`) up to the width budget, overflow wrapping at `indent`.
fn append_trailing_comments(out: &mut String, indent: usize, blocks: Vec<CommentBlock>) {
    if blocks.is_empty() {
        return;
    }
    let joined = blocks
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    append_comment_to_line(out, indent, &joined);
}

// -- Document-level formatting (comment-preserving) ---------------------------

/// Pretty-print a whole parsed KIF or `.kif.tq` document -- the round-trip
/// formatter.
///
/// Every root item is re-emitted through the canonical styled layout, with
/// the document's [`CommentBlock`]s re-interleaved by source position:
/// standalone blocks keep their place between items, trailing comments stay
/// on their item's line, and comments inside a form are woven back in at the
/// nearest argument boundary.  Blank-line grouping between top-level items
/// is preserved (one blank line where the source had any).  `.tq` items
/// round-trip too: `Meta` directives re-emit as their `(key args...)` form
/// and a `Conjecture`-role statement as `(query <formula>)`.
///
/// Returns `None` when the document has hard parse errors -- re-emitting a
/// partial AST would silently drop the malformed fragments.
///
/// The output is guaranteed re-parseable; parsing it back yields the same
/// root fingerprints (comments never enter the AST).
pub fn format_document(doc: &crate::parse::ParsedDocument) -> Option<String> {
    if doc.has_errors() {
        return None;
    }
    let items: Vec<&DocItem> = doc.ast.iter().collect();
    Some(format_forms(&doc.text, &items, &doc.comments))
}

/// Reconstruct the source-level form a [`DocItem`] renders as.  A bare or
/// `Hypothesis`-annotated statement is its formula (styled unwraps the
/// annotation); a `Conjecture` statement re-wraps as `(query <formula>)` and
/// a `Meta` directive as `(key args...)` -- both synthesized as plain lists
/// carrying the original spans, so the ordinary styled layout and comment
/// interleaving apply unchanged.
fn display_node(item: &DocItem) -> AstNode {
    let wrap = |head: &str, args: Vec<AstNode>, span: &crate::parse::Span| AstNode::List {
        elements: std::iter::once(AstNode::Symbol {
            name: head.to_string(),
            span: span.clone(),
        })
        .chain(args)
        .collect(),
        span: span.clone(),
    };
    match item {
        DocItem::Stmt(n) => match n {
            AstNode::Annotated { formula, span, .. }
                if n.role() == Some(&crate::parse::ast::Role::Conjecture) =>
            {
                wrap("query", vec![(**formula).clone()], span)
            }
            _ => n.clone(),
        },
        DocItem::Meta(m) => wrap(&m.key, m.args.clone(), &m.span),
    }
}

/// The engine behind [`format_document`], usable on a subset of items (range
/// formatting): re-emit `items` with `comments` re-interleaved by source
/// position.  `text` is the ORIGINAL source the spans index into, consulted
/// for line-adjacency (blank-line grouping, trailing-comment placement); both
/// slices must be span-ordered.
pub fn format_forms(text: &str, items: &[&DocItem], comments: &[CommentBlock]) -> String {
    let roots: Vec<AstNode> = items.iter().map(|i| display_node(i)).collect();
    // Route each comment either into the root whose source extent contains
    // it (interior) or into the top-level interleave.
    let mut interior: Vec<Vec<CommentBlock>> = vec![Vec::new(); roots.len()];
    let mut top: Vec<&CommentBlock> = Vec::new();
    for c in comments {
        match roots
            .iter()
            .position(|r| c.span.offset >= r.span().offset && c.span.offset < r.span().end_offset)
        {
            Some(i) => interior[i].push(c.clone()),
            None => top.push(c),
        }
    }

    enum Item<'a> {
        Root(usize, &'a AstNode),
        Comment(&'a CommentBlock),
    }
    let mut ordered: Vec<(usize, Item)> = roots
        .iter()
        .enumerate()
        .map(|(i, r)| (r.span().offset, Item::Root(i, r)))
        .collect();
    ordered.extend(top.into_iter().map(|c| (c.span.offset, Item::Comment(c))));
    ordered.sort_by_key(|(off, _)| *off);

    let mut out = String::new();
    // End offset of the previous item, plus whether it was a comment (a
    // comment token's span swallows its terminating newline, so one newline
    // of separation is implicit).
    let mut prev: Option<(usize, bool)> = None;
    for (start, item) in ordered {
        // Separator: 0 = same source line (trailing placement), 1 = adjacent
        // lines, 2 = blank-line separated.
        let separation = prev.map(|(prev_end, prev_was_comment)| {
            let (lo, hi) = (prev_end.min(text.len()), start.min(text.len()));
            let gap_newlines = if lo <= hi {
                text[lo..hi].matches('\n').count()
            } else {
                0
            };
            let effective = gap_newlines + usize::from(prev_was_comment);
            if effective == 0 && !prev_was_comment {
                0
            } else if effective >= 2 {
                2
            } else {
                1
            }
        });
        match separation {
            Some(2) => out.push_str("\n\n"),
            Some(1) => out.push('\n'),
            _ => {}
        }
        let (end, is_comment) = match item {
            Item::Root(i, node) => {
                let mut rem: &[CommentBlock] = &interior[i];
                out.push_str(&styled_c(node, 0, false, text, &mut rem));
                debug_assert!(rem.is_empty(), "interior comments not fully drained");
                (node.span().end_offset, false)
            }
            Item::Comment(c) => {
                if separation == Some(0) {
                    // Trailing on the previous form's line, width-budgeted.
                    append_comment_to_line(&mut out, 0, &c.text);
                } else {
                    out.push_str(&wrap_comment(&c.text, 0));
                }
                (c.span.end_offset, true)
            }
        };
        prev = Some((end, is_comment));
    }
    out
}

/// `true` iff `head` is an `Operator` quantifier (`forall` / `exists`).
fn is_quantifier_head(head: &AstNode) -> bool {
    matches!(head, AstNode::Operator { op, .. } if op.is_quantifier())
}

/// `true` iff `head` is the `not` operator.
fn is_not_head(head: &AstNode) -> bool {
    matches!(head, AstNode::Operator { op, .. } if op.name() == "not")
}

/// `true` iff `head` is the `.tq` `query` directive symbol.
fn is_query_head(head: &AstNode) -> bool {
    matches!(head, AstNode::Symbol { name, .. } if name == "query")
}

/// `true` iff `node` is a non-empty list — i.e. would itself open a paren,
/// so inlining it next to a sibling's open paren would violate the
/// no-two-opens-per-line rule.
fn is_compound(node: &AstNode) -> bool {
    matches!(node, AstNode::List { elements, .. } if !elements.is_empty())
}

/// KIF rendering methods on [`AstNode`].  `use` this (re-exported at the crate
/// root as `sigmakee_rs_core::AstKif`) where the method syntax is wanted; the
/// implementation lives here, not on the type.
pub trait AstKif {
    fn flat(&self) -> String;
    fn pretty_print(&self, indent: usize) -> String;
    fn format_plain(&self, indent: usize) -> String;
}

impl AstKif for AstNode {
    fn flat(&self) -> String {
        flat(self)
    }
    // The two styled views route through the `PrettyEmit` dialect seam (whose
    // sole implementation is `KifEmit`), so there is one rendering path: AstKif
    // is just the method-syntax facade over it.
    fn pretty_print(&self, indent: usize) -> String {
        KifEmit.emit_pretty(self, indent, true)
    }
    fn format_plain(&self, indent: usize) -> String {
        KifEmit.emit_pretty(self, indent, false)
    }
}

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
                    if !first {
                        f.write_str(" ")?;
                    }
                    first = false;
                    write!(f, "{}", Pretty(el))?;
                }
                f.write_str(")")
            }
            AstNode::Operator { op, .. } => write!(f, "{color_cyan}{}{color_reset}", op.name()),
            AstNode::Number { value, .. } | AstNode::Str { value, .. } => {
                write!(f, "{color_green}{}{color_reset}", value)
            }
            AstNode::Variable { .. } | AstNode::RowVariable { .. } => {
                write!(f, "{color_magenta}{}{color_reset}", flat(self.0))
            }
            AstNode::Symbol { name, .. } => {
                if name.chars().next().is_some_and(|c| c.is_lowercase()) {
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
mod format_document_tests {
    use super::format_document;
    use crate::parse::{parse_document, Parser};

    fn fmt(src: &str) -> String {
        let doc = parse_document("t", src, Parser::Kif { options: None });
        assert!(!doc.has_errors(), "errors: {:?}", doc.parse_errors);
        format_document(&doc).expect("clean doc formats")
    }

    /// Formatting must be a true round trip: same fingerprints, same comments.
    fn assert_roundtrip(src: &str) -> String {
        assert_roundtrip_with(src, || Parser::Kif { options: None })
    }

    fn assert_roundtrip_with(src: &str, parser: impl Fn() -> Parser) -> String {
        let before = parse_document("t", src, parser());
        let out = format_document(&before).expect("clean doc formats");
        let after = parse_document("t", out.as_str(), parser());
        assert!(!after.has_errors(), "reparse errors on:\n{out}");
        assert_eq!(
            before.root_hashes, after.root_hashes,
            "logical content changed:\n{out}"
        );
        // Word sequence, not raw text: the formatter re-flows comment lines
        // to the width budget, so line breaks inside a block may legally
        // move; the words and their order must survive exactly.
        let words = |d: &crate::parse::ParsedDocument| -> Vec<String> {
            d.comments
                .iter()
                .flat_map(|c| c.text.split_whitespace().map(str::to_string))
                .collect()
        };
        assert_eq!(
            words(&before),
            words(&after),
            "comment words lost or reordered:\n{out}"
        );
        // The width budget: no comment-bearing line may exceed it (except an
        // unbreakable single word).
        for line in out.lines() {
            if line.contains(';') && line.len() > 72 {
                let tail = &line[line.find(';').unwrap()..];
                assert!(
                    tail.split_whitespace().count() <= 2,
                    "comment line over 72 columns:\n{line}"
                );
            }
        }
        out
    }

    #[test]
    fn comment_free_output_matches_historical_layout() {
        assert_eq!(
            fmt("(subclass    Human   Animal)\n\n(subclass Dog Mammal)"),
            "(subclass Human Animal)\n\n(subclass Dog Mammal)"
        );
    }

    #[test]
    fn standalone_and_attached_comments_keep_their_grouping() {
        // The two short header lines re-flow into ONE filled line (the fixed
        // width is a fill target, not just a ceiling); grouping around the
        // forms is unchanged.
        let out = assert_roundtrip(
            "; header block\n; second line\n(subclass Dog Mammal)\n\n; separate\n\n(subclass Cat Mammal)",
        );
        assert_eq!(
            out,
            "; header block second line\n(subclass Dog Mammal)\n\n; separate\n\n(subclass Cat Mammal)"
        );
    }

    #[test]
    fn long_comment_block_wraps_at_72_columns() {
        let src = "; alpha bravo charlie delta echo foxtrot golf hotel india juliett kilo lima mike november oscar papa quebec romeo sierra tango\n(subclass Dog Mammal)";
        let out = assert_roundtrip(src);
        let comment_lines: Vec<&str> = out.lines().filter(|l| l.starts_with("; ")).collect();
        assert!(comment_lines.len() >= 2, "long comment must wrap:\n{out}");
        for l in &comment_lines {
            assert!(l.len() <= 72, "line over budget: {l}");
        }
        // Greedy fill: every line but the last could not take the next word.
        assert!(
            comment_lines[0].len() > 50,
            "first line under-filled: {:?}",
            comment_lines[0]
        );
    }

    #[test]
    fn long_trailing_comment_spills_to_wrapped_lines() {
        let src = "(=>\n  (instance ?X Dog) ; a very long explanation that cannot possibly fit on the remainder of this line because it keeps going and going\n  (instance ?X Mammal))";
        let out = assert_roundtrip(src);
        for l in out.lines() {
            if l.trim_start().starts_with(';') {
                assert!(l.len() <= 72, "wrapped line over budget: {l}");
            }
        }
        // The spill continues as indented comment lines inside the form.
        assert!(
            out.contains("\n  ; "),
            "expected wrapped interior lines:\n{out}"
        );
    }

    #[test]
    fn trailing_comment_stays_on_its_line() {
        let out =
            assert_roundtrip("(subclass Dog Mammal) ; dogs are mammals\n(subclass Cat Mammal)");
        assert_eq!(
            out,
            "(subclass Dog Mammal) ; dogs are mammals\n(subclass Cat Mammal)"
        );
    }

    #[test]
    fn interior_comments_weave_into_the_broken_layout() {
        // "premise" and the full-line comment under it consolidated into one
        // block at tokenize time (adjacent lines, nothing between); the fill
        // flows the whole block onto the element's line since it fits the
        // width budget.
        let src =
            "(=>\n  (instance ?X Dog) ; premise\n  ; conclusion next\n  (instance ?X Mammal))";
        let out = assert_roundtrip(src);
        assert_eq!(
            out,
            "(=>\n  (instance ?X Dog) ; premise conclusion next\n  (instance ?X Mammal))"
        );
    }

    #[test]
    fn standalone_interior_comment_keeps_its_own_line() {
        // A blank line detaches the comment from the element above, so it is
        // its own block and stays on its own indented line.
        let src = "(=>\n  (instance ?X Dog)\n\n  ; conclusion next\n  (instance ?X Mammal))";
        let out = assert_roundtrip(src);
        assert_eq!(
            out,
            "(=>\n  (instance ?X Dog)\n  ; conclusion next\n  (instance ?X Mammal))"
        );
    }

    #[test]
    fn interior_comment_forces_a_break_on_a_short_form() {
        // Without the comment this fits on one line; with it, the layout must
        // break so the comment has a place to live -- and stay re-parseable.
        let out = assert_roundtrip("(subclass Dog ; the class\n Mammal)");
        assert_eq!(out, "(subclass\n  Dog ; the class\n  Mammal)");
    }

    #[test]
    fn comment_before_close_moves_after_it() {
        let out = assert_roundtrip("(=>\n  (instance ?X Dog)\n  (instance ?X Mammal)\n  ; why\n)");
        assert!(out.ends_with(") ; why"), "got:\n{out}");
    }

    #[test]
    fn tq_document_roundtrips_directives_query_and_comments() {
        let src = "; suite header\n(note \"dogs are mortal\")\n(time 30)\n(instance Rex Dog) ; the pet\n(query (instance Rex Mammal))\n(answer yes)\n(file \"Merge.kif\")";
        let out = assert_roundtrip_with(src, || Parser::Tq);
        assert_eq!(
            out,
            "; suite header\n(note \"dogs are mortal\")\n(time 30)\n(instance Rex Dog) ; the pet\n(query (instance Rex Mammal))\n(answer yes)\n(file \"Merge.kif\")"
        );
    }

    #[test]
    fn tq_ask_alias_canonicalizes_to_query() {
        let out = assert_roundtrip_with("(ask (instance Rex Dog))", || Parser::Tq);
        assert_eq!(out, "(query (instance Rex Dog))");
    }

    #[test]
    fn error_document_refuses_to_format() {
        let doc = parse_document("t", "(subclass Dog", Parser::Kif { options: None });
        assert!(format_document(&doc).is_none());
    }

    #[test]
    fn footer_comment_survives() {
        let out = assert_roundtrip("(subclass Dog Mammal)\n; the end");
        assert_eq!(out, "(subclass Dog Mammal)\n; the end");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::ast::{Role, Span};
    use crate::parse::dialect::Emitter;

    fn parse_one(src: &str) -> AstNode {
        let doc = crate::parse::parse_document("t", src, crate::Parser::Kif { options: None });
        assert!(
            doc.parse_errors.is_empty(),
            "parse errors: {:?}",
            doc.parse_errors
        );
        doc.ast
            .into_iter()
            .next()
            .unwrap()
            .as_stmt()
            .cloned()
            .unwrap()
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut in_esc = false;
        for c in s.chars() {
            if c == '\x1B' {
                in_esc = true;
                continue;
            }
            if in_esc {
                if c == 'm' {
                    in_esc = false;
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    #[test]
    fn display_has_no_internal_padding() {
        assert_eq!(
            parse_one("(and (instance Foo Bar) (instance Foo Baz))").to_string(),
            "(and (instance Foo Bar) (instance Foo Baz))"
        );
    }

    #[test]
    fn format_plain_inlines_quantifier_vars() {
        let n = parse_one(
            "(exists (?A ?B) (and (member ?A ?P) (member ?B ?P) \
             (not (equal ?A ?B)) (instance ?A SomeLongClassName)))",
        );
        let out = n.format_plain(0);
        assert_eq!(out.lines().next().unwrap(), "(exists (?A ?B)");
        assert!(out.lines().nth(1).unwrap().starts_with("  "));
    }

    #[test]
    fn pretty_print_matches_plain_layout_ignoring_color() {
        let n = parse_one(
            "(and (instance Foo Bar) (instance Foo Baz) \
             (instance Foo VeryLongClassName) (instance Foo AnotherLong))",
        );
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
            role: Role::Conjecture,
            name: Some("c".into()),
            source: None,
            formula: Box::new(inner.clone()),
            span: Span::default(),
        };
        assert_eq!(
            Emitter::Kif.emit_one(&ann).text.trim_end(),
            inner.format_plain(0)
        );
        assert_eq!(ann.flat(), inner.flat());
    }

    /// Count consecutive `((` runs on one line — i.e. two opens landing back
    /// to back with nothing but whitespace between them. The `not`/quantifier
    /// exemptions produce exactly one `(head (arg` pattern each, which this
    /// same check would also flag if it looked at *all* adjacent opens rather
    /// than back-to-back ones — so instead we assert the general rule
    /// directly: strip every allowed inline pair first, then no `(` may be
    /// immediately followed (modulo whitespace) by another `(` on the same
    /// line.
    fn assert_no_stacked_opens(text: &str) {
        for line in text.lines() {
            let trimmed = line.trim_start();
            // Skip the one inline pair a quantifier var-list or `not` may
            // introduce right after the head symbol.
            let rest = if let Some(after) = trimmed
                .strip_prefix("(forall (")
                .or_else(|| trimmed.strip_prefix("(exists ("))
            {
                after
            } else if let Some(after) = trimmed.strip_prefix("(not (") {
                after
            } else if let Some(after) = trimmed.strip_prefix('(') {
                after
            } else {
                trimmed
            };
            assert!(
                !rest.trim_start().starts_with('('),
                "stacked opens on line: {line:?}"
            );
        }
    }

    #[test]
    fn no_two_opens_share_a_line_outside_quantifier_and_not() {
        let cases = [
            "(forall (?X ?Y) (=> (instance ?X Human) (instance ?Y Human)))",
            "(not (instance ?A Human))",
            "(not (and (instance ?A Human) (instance ?B Human)))",
            "(and (instance Foo Bar) (instance Foo Baz))",
            "(exists (?X) (P ?X))",
            "(forall (?X) (=> (and (P ?X) (Q ?X)) (R ?X)))",
        ];
        for c in cases {
            let n = parse_one(c);
            let out = n.format_plain(0);
            assert_no_stacked_opens(&out);
            // Round-trips: re-parsing the pretty-printed form yields the same
            // canonical flat KIF as the original.
            let reparsed = parse_one(&out);
            assert_eq!(reparsed.flat(), n.flat(), "not re-parseable:\n{out}");
        }
    }

    #[test]
    fn quantifier_body_always_breaks_even_when_short() {
        let n = parse_one("(forall (?X) (instance ?X Entity))");
        assert_eq!(n.format_plain(0), "(forall (?X)\n  (instance ?X Entity))");
    }

    #[test]
    fn not_keeps_compound_argument_inline() {
        let n = parse_one("(not (instance ?A Human))");
        assert_eq!(n.format_plain(0), "(not (instance ?A Human))");
    }

    #[test]
    fn and_with_compound_args_always_breaks() {
        let n = parse_one("(and (instance Foo Bar) (instance Foo Baz))");
        assert_eq!(
            n.format_plain(0),
            "(and\n  (instance Foo Bar)\n  (instance Foo Baz))"
        );
    }
}
