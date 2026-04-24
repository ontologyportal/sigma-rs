use core::fmt;
use inline_colorization::*;
use serde::{Deserialize, Serialize};

/// Soft wrap threshold for the AST pretty-printers.  Expressions that
/// fit within this many columns at their target indent are kept on
/// one line; longer ones break across lines with each argument
/// indented two columns further.  Shared by [`AstNode::pretty_print`]
/// (ANSI-coloured terminal output) and [`AstNode::format_plain`]
/// (plain-text output) so a change to the wrapping width stays
/// consistent across both sinks.
const LINE_WIDTH: usize = 72;

/// Source range (1-based line / column, byte offset at both ends).
///
/// `line` / `col` / `offset` describe the START of the range (backward
/// compatible with older single-point spans).  `end_line` / `end_col` /
/// `end_offset` describe the END (exclusive byte offset, one past the
/// last byte of the token or node).  For point-spans (no known width)
/// `end_*` equals the start fields — the helpers
/// [`Span::point`] and [`Span::is_point`] make that explicit.
///
/// The byte offsets are half-open `[offset, end_offset)` — i.e.
/// `end_offset - offset == byte_len` and a zero-width span has
/// `end_offset == offset`.  LSP consumers want this exact semantic
/// when mapping to `Range { start, end }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Span {
    pub file:       String,
    /// Start line (1-based).
    pub line:       u32,
    /// Start column (1-based).
    pub col:        u32,
    /// Start byte offset (0-based, inclusive).
    pub offset:     usize,
    /// End line (1-based).  Point-spans have `end_line == line`.
    pub end_line:   u32,
    /// End column (1-based, exclusive -- one past the last byte).
    pub end_col:    u32,
    /// End byte offset (0-based, exclusive).
    pub end_offset: usize,
}

impl Span {
    /// Construct a zero-width span at a single position.  Useful when
    /// only a start position is known (e.g. synthetic / placeholder
    /// spans).
    pub fn point(file: String, line: u32, col: u32, offset: usize) -> Self {
        Self {
            file,
            line, col, offset,
            end_line:   line,
            end_col:    col,
            end_offset: offset,
        }
    }

    /// Sentinel span for synthesised Elements that have no source
    /// origin (CNF clausifier output, macro expansions, test
    /// fixtures, rehydrated-from-LMDB sentences).  Position queries
    /// (`element_at_offset`, goto, hover) treat these as invisible
    /// so bogus ranges never leak to downstream tooling.
    ///
    /// The sentinel is a distinctive `file` tag; actual positions
    /// are zero.  Equality is by `file` only -- any span with
    /// `file == "<synthetic>"` is synthetic.
    pub fn synthetic() -> Self {
        Self {
            file:       "<synthetic>".to_string(),
            line:       0, col: 0, offset: 0,
            end_line:   0, end_col: 0, end_offset: 0,
        }
    }

    /// True if this span has no real source location.  Covers both
    /// [`Span::synthetic`] (`<synthetic>`) and LMDB-rehydrated
    /// placeholders (`<lmdb:N>`) -- every caller of this predicate
    /// wants to treat "no real origin" the same way regardless of
    /// which sentinel was used.
    pub fn is_synthetic(&self) -> bool {
        self.file == "<synthetic>" || self.file.starts_with("<lmdb:")
    }

    /// True if the span covers zero bytes (a point span).
    pub fn is_point(&self) -> bool {
        self.offset == self.end_offset
    }

    /// Byte length of the span.
    pub fn byte_len(&self) -> usize {
        self.end_offset.saturating_sub(self.offset)
    }

    /// Combine two spans into one that covers both.  The resulting
    /// span's file is taken from `self`; if the two differ the call
    /// still returns `self`'s file unchanged -- merging across files
    /// is meaningless.
    pub fn join(&self, other: &Span) -> Span {
        let (s_line, s_col, s_off) =
            if (self.line, self.col) <= (other.line, other.col) {
                (self.line, self.col, self.offset)
            } else {
                (other.line, other.col, other.offset)
            };
        let (e_line, e_col, e_off) =
            if (self.end_line, self.end_col) >= (other.end_line, other.end_col) {
                (self.end_line, self.end_col, self.end_offset)
            } else {
                (other.end_line, other.end_col, other.end_offset)
            };
        Span {
            file:       self.file.clone(),
            line:       s_line,
            col:        s_col,
            offset:     s_off,
            end_line:   e_line,
            end_col:    e_col,
            end_offset: e_off,
        }
    }
}

impl std::fmt::Display for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Keep the common case compact: point-spans and same-line
        // ranges elide the redundant end.
        if self.is_point() || (self.line == self.end_line && self.col == self.end_col) {
            write!(f, "{}:{}:{}", self.file, self.line, self.col)
        } else if self.line == self.end_line {
            write!(f, "{}:{}:{}-{}", self.file, self.line, self.col, self.end_col)
        } else {
            write!(f, "{}:{}:{}-{}:{}", self.file, self.line, self.col, self.end_line, self.end_col)
        }
    }
}

/// Logical operators that are keywords in KIF.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpKind {
    And,
    Or,
    Not,
    Implies,  // =>
    Iff,      // <=>
    Equal,    // equal
    ForAll,   // forall
    Exists,   // exists
}

impl OpKind {
    pub fn name(&self) -> &'static str {
        match self {
            OpKind::And     => "and",
            OpKind::Or      => "or",
            OpKind::Not     => "not",
            OpKind::Implies => "=>",
            OpKind::Iff     => "<=>",
            OpKind::Equal   => "equal",
            OpKind::ForAll  => "forall",
            OpKind::Exists  => "exists",
        }
    }
}

impl fmt::Display for OpKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// A node in the raw abstract syntax tree produced by the parser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AstNode {
    List     { elements: Vec<AstNode>, span: Span },
    Symbol   { name: String, span: Span },
    Variable { name: String, span: Span },   // includes leading `?`
    RowVariable { name: String, span: Span }, // includes leading `@`
    Str      { value: String, span: Span },  // includes surrounding `"`
    Number   { value: String, span: Span },
    Operator { op: OpKind, span: Span },
}

impl AstNode {
    pub fn span(&self) -> &Span {
        match self {
            AstNode::List { span, .. }        => span,
            AstNode::Symbol { span, .. }      => span,
            AstNode::Variable { span, .. }    => span,
            AstNode::RowVariable { span, .. } => span,
            AstNode::Str { span, .. }         => span,
            AstNode::Number { span, .. }      => span,
            AstNode::Operator { span, .. }    => span,
        }
    }

    /// Compact flat KIF string -- `(op arg1 arg2)` without extra spaces.
    pub fn flat(&self) -> String {
        match self {
            AstNode::List { elements, .. } => {
                if elements.is_empty() { return "()".into(); }
                format!("({})", elements.iter().map(AstNode::flat).collect::<Vec<_>>().join(" "))
            }
            AstNode::Symbol { name, .. }      => name.clone(),
            AstNode::Variable { name, .. }    => format!("?{}", name),
            AstNode::RowVariable { name, .. } => format!("@{}", name),
            AstNode::Str { value, .. }
            | AstNode::Number { value, .. }   => value.clone(),
            AstNode::Operator { op, .. }      => op.name().to_owned(),
        }
    }

    /// Indented, ANSI-colored KIF pretty-printer for terminal output.
    ///
    /// Expressions that fit within 72 columns at `indent` are kept on one line.
    /// Longer ones break so that the operator stays on the opening line and
    /// each argument is placed on its own line indented two spaces further.
    ///
    /// **Contains ANSI escape codes.**  Downstream tools emitting to
    /// non-terminal sinks (LSP edits, file writes, JSON reports)
    /// should use [`format_plain`](Self::format_plain) instead --
    /// `pretty_print`'s output is only re-parseable by KIF tooling
    /// that strips the escapes first.
    pub fn pretty_print(&self, indent: usize) -> String {
        let flat = self.flat();
        if indent + flat.len() <= LINE_WIDTH {
            return Pretty(self).to_string();
        }
        match self {
            AstNode::List { elements, .. } if elements.len() >= 2 => {
                let pad  = " ".repeat(indent + 2);
                let head = elements[0].pretty_print(0);
                let args: Vec<String> = elements[1..].iter()
                    .map(|e| format!("{}{}", pad, e.pretty_print(indent + 2)))
                    .collect();
                format!("({}\n{})", head, args.join("\n"))
            }
            _ => Pretty(self).to_string(),
        }
    }

    /// Indented plain-text pretty-printer.  Identical line-width
    /// breaking to [`pretty_print`] but produces ASCII-only output
    /// with no ANSI colour escapes -- always safe to round-trip
    /// through the KIF parser.
    ///
    /// Use cases: LSP formatting, file emission, anything where
    /// the output isn't going to a colour-aware terminal.  The
    /// `sumo man` CLI and proof-trace printers use
    /// [`pretty_print`] because their destination is a terminal.
    pub fn format_plain(&self, indent: usize) -> String {
        let flat = self.flat();
        if indent + flat.len() <= LINE_WIDTH {
            return flat;
        }
        match self {
            AstNode::List { elements, .. } if elements.len() >= 2 => {
                let pad  = " ".repeat(indent + 2);
                let head = elements[0].format_plain(0);
                let args: Vec<String> = elements[1..].iter()
                    .map(|e| format!("{}{}", pad, e.format_plain(indent + 2)))
                    .collect();
                format!("({}\n{})", head, args.join("\n"))
            }
            _ => flat,
        }
    }
}

/// Plain KIF display -- output is always re-parseable (no ANSI codes).
/// Use [`Pretty`] for colourised terminal/log output.
impl fmt::Display for AstNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AstNode::List { elements, .. } => {
                write!(f, "( ")?;
                for el in elements { write!(f, "{} ", el)?; }
                write!(f, ")")
            }
            AstNode::Symbol { name, .. }        => write!(f, "{}", name),
            AstNode::Variable { name, .. }      => write!(f, "?{}", name),
            AstNode::RowVariable { name, .. }   => write!(f, "@{}", name),
            AstNode::Str { value, .. }
            | AstNode::Number { value, .. }     => write!(f, "{}", value),
            AstNode::Operator { op, .. }        => write!(f, "{}", op.name()),
        }
    }
}

/// Colourised display wrapper for [`AstNode`].
///
/// Use this for terminal output and log messages where ANSI colour is
/// desirable.  Operators are rendered in cyan.  For output that must be
/// fed back into the parser or KB, use plain [`Display`] / [`to_string`].
pub struct Pretty<'a>(pub &'a AstNode);

impl fmt::Display for Pretty<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            AstNode::List { elements, .. } => {
                write!(f, "( ")?;
                for el in elements { write!(f, "{} ", Pretty(el))?; }
                write!(f, ")")
            }
            AstNode::Operator { op, .. } =>
                write!(f, "{color_cyan}{}{color_reset}", op.name()),
            AstNode::Number { value, ..}
            | AstNode::Str { value, ..} => write!(f, "{color_green}{}{color_reset}", value),
            AstNode::Variable { ..}
            | AstNode::RowVariable { .. } => write!(f, "{color_magenta}{}{color_reset}", self.0.flat()),
            AstNode::Symbol { name, .. } => {
                if name.chars().next().map_or(false, |c| c.is_lowercase()) {
                    write!(f, "{color_bright_blue}{}{color_reset}", name)
                } else {
                    write!(f, "{color_yellow}{}{color_reset}", name)
                }
            }
        }
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_is_zero_width() {
        let s = Span::point("f".into(), 1, 1, 0);
        assert!(s.is_point());
        assert_eq!(s.byte_len(), 0);
        assert_eq!(s.end_offset, s.offset);
    }

    #[test]
    fn join_covers_both() {
        let a = Span { file: "f".into(), line: 1, col: 1, offset: 0, end_line: 1, end_col: 4, end_offset: 3 };
        let b = Span { file: "f".into(), line: 1, col: 5, offset: 4, end_line: 1, end_col: 9, end_offset: 8 };
        let j = a.join(&b);
        assert_eq!(j.offset,     0);
        assert_eq!(j.end_offset, 8);
        assert_eq!(j.col,        1);
        assert_eq!(j.end_col,    9);
    }

    #[test]
    fn join_is_order_insensitive() {
        let a = Span { file: "f".into(), line: 2, col: 3, offset: 10, end_line: 2, end_col: 7, end_offset: 14 };
        let b = Span { file: "f".into(), line: 1, col: 1, offset:  0, end_line: 1, end_col: 2, end_offset:  1 };
        let ab = a.join(&b);
        let ba = b.join(&a);
        assert_eq!(ab.offset,     ba.offset);
        assert_eq!(ab.end_offset, ba.end_offset);
    }

    #[test]
    fn display_elides_point_and_same_col() {
        let p = Span::point("f".into(), 3, 5, 42);
        assert_eq!(p.to_string(), "f:3:5");

        let inline = Span { file: "f".into(), line: 3, col: 5, offset: 42, end_line: 3, end_col: 11, end_offset: 48 };
        assert_eq!(inline.to_string(), "f:3:5-11");

        let multi = Span { file: "f".into(), line: 3, col: 5, offset: 42, end_line: 4, end_col: 2, end_offset: 60 };
        assert_eq!(multi.to_string(), "f:3:5-4:2");
    }
}