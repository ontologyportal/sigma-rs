use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
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