//! Unified error / warning reporting for sigmakee-rs-core.
//!
//! [`Diagnostic`] is the single representation every consumer (CLI, LSP, REPL,
//! JSON reporter, CI tooling) renders. Parser hard errors, semantic-validation
//! findings, and tell-time warnings all convert into a `Diagnostic` through
//! [`ToDiagnostic`] and render through one path.
//!
//! Producers do not print directly: call `.to_diagnostic()` to obtain a
//! `Diagnostic`, then [`Diagnostic::render`] (returns a `String`) or
//! [`Diagnostic::emit`] (forwards through the `log` crate). The optional `ctx`
//! is anything implementing [`DiagnosticSource`]; [`crate::KnowledgeBase`] is
//! the canonical impl and adds source-line context. The renderer always picks
//! colors from [`Severity`].
//!
//! `Diagnostic` uses this crate's [`Span`] and [`SentenceId`] and has no LSP /
//! IDE dependency; consumers that need LSP types convert at their own boundary.

use inline_colorization::*;

use crate::parse::Span;
use crate::types::SentenceId;

// -- Severity -----------------------------------------------------------------

/// Diagnostic severity: the four levels error, warning, info, hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    /// The lowercase name of this severity (`"error"`, `"warning"`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error   => "error",
            Self::Warning => "warning",
            Self::Info    => "info",
            Self::Hint    => "hint",
        }
    }

    /// Map this severity onto a `log::Level`. `Hint` maps to `Trace`.
    pub fn log_level(self) -> log::Level {
        match self {
            Self::Error   => log::Level::Error,
            Self::Warning => log::Level::Warn,
            Self::Info    => log::Level::Info,
            Self::Hint    => log::Level::Trace,
        }
    }

    fn ansi_color(self) -> &'static str {
        match self {
            Self::Error   => color_red,
            Self::Warning => color_yellow,
            Self::Info    => color_cyan,
            Self::Hint    => color_white,
        }
    }
}

// -- Diagnostic ---------------------------------------------------------------

/// A single actionable problem with optional source context.
///
/// `kind` is a coarse category (e.g. `"parse"`, `"semantic"`, `"db"`) and
/// `code` the specific leaf within it (e.g. `"kif/unexpected-eof"`,
/// `"arity-mismatch"`); the full path rendered to consumers is `kind/code`.
/// `range` uses this crate's [`Span`]. `related` carries extra locations that
/// clarify the diagnostic, each with its own note. When a [`DiagnosticSource`]
/// is passed to [`Self::render`] / [`Self::emit`], the renderer pulls each
/// sentence in `sids` and prints it inline, highlighting the argument at
/// `highlight_arg` in the first sid.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// Coarse category, e.g. `"parse"` or `"semantic"`.
    pub kind:     &'static str,
    /// Source span this diagnostic points at.
    pub range:    Span,
    /// Severity level.
    pub severity: Severity,
    /// Specific leaf identifier within `kind`.
    pub code:     &'static str,
    /// Human-readable message.
    pub message:  String,
    /// Supplementary locations that clarify this diagnostic.
    pub related:  Vec<RelatedInfo>,
    /// Sentences implicated by this diagnostic. Empty for source-positional
    /// errors that reference no stored sentence (e.g. parse errors).
    pub sids:          Vec<SentenceId>,
    /// Argument index to highlight inside the first entry of `sids`, or `-1`
    /// for no highlight / whole-sentence highlight.
    pub highlight_arg: i32,
    /// A variable name (without the leading `?`) to underline in the rendered
    /// snippet, for variable-centric lints. The renderer underlines the
    /// variable's first occurrence. `None` = no variable highlight.
    pub highlight_var: Option<String>,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}/{}] {}", self.kind, self.code, self.message)
    }
}

impl std::error::Error for Diagnostic {}

impl Diagnostic {
    /// Construct an error-severity diagnostic with no source context (empty
    /// `sids`, default span, no related info).
    pub fn new_error(
        kind:    &'static str,
        code:    &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            code,
            severity:      Severity::Error,
            range:         Span::default(),
            message:       message.into(),
            related:       Vec::new(),
            sids:          Vec::new(),
            highlight_arg: -1,
            highlight_var: None,
        }
    }

    /// Render this diagnostic as colorised text.
    ///
    /// `ctx`, when given, provides source-context lookup for `sids` (e.g.
    /// `KnowledgeBase` prints the offending sentence with a gutter). Without
    /// `ctx` the output is the message, code, and related-info notes alone.
    pub fn render(&self, ctx: Option<&dyn DiagnosticSource>) -> String {
        let color = self.severity.ansi_color();
        let mut out = String::new();

        out.push_str(&format!(
            "{color}{style_bold}{}[{}/{}]{style_reset}{color}: {}{color_reset}",
            self.severity.as_str(), self.kind, self.code, self.message,
        ));

        if let Some(src) = ctx {
            if let Some(&sid) = self.sids.first() {
                // Prefer the anchor sentence's own span; fall back to the
                // enclosing root formula's span. Nested sub-sentences are
                // content-addressed and carry no source span of their own.
                let loc = src.sentence_location(sid).or_else(|| {
                    (!self.range.file.is_empty() && !self.range.is_synthetic())
                        .then(|| self.range.clone())
                });
                if let Some(span) = loc {
                    out.push_str(&format!(
                        "\n{color_blue}  --> {color_reset}{}:{}",
                        span.file, span.line,
                    ));
                }
                if let Some(rendered) = src.render_sentence(sid, self.highlight_arg) {
                    out.push('\n');
                    if let Some(var) = &self.highlight_var {
                        // Caret beneath the variable's first occurrence.
                        let caret = find_var_occurrence(&rendered, var).map(|(line, col, len)| {
                            (line, format!("{color}{}{}{color_reset}",
                                " ".repeat(col), "^".repeat(len)))
                        });
                        out.push_str(&gutter(&rendered, caret));
                    } else if self.highlight_arg >= 0 && !rendered.contains('\n') {
                        // Single line: caret underline beneath the argument,
                        // spanned against the flat one-line form.
                        let caret = src.highlight_span(sid, self.highlight_arg)
                            .filter(|&(start, len)| len > 0 && start + len <= visible_len(&rendered))
                            .map(|(start, len)| (0usize, format!(
                                "{color}{}{}{color_reset}",
                                " ".repeat(start), "^".repeat(len),
                            )));
                        out.push_str(&gutter(&rendered, caret));
                    } else if self.highlight_arg >= 1 {
                        // Multi line: mark the offending argument's line with `<<<<`.
                        let marked = mark_arg_line(
                            &rendered, self.highlight_arg, src.arg_count(sid), color,
                        );
                        out.push_str(&gutter(&marked, None));
                    } else {
                        out.push_str(&gutter(&rendered, None));
                    }
                }
            }
            for &sid in self.sids.iter().skip(1) {
                if let Some(rendered) = src.render_sentence(sid, -1) {
                    out.push('\n');
                    out.push_str(&gutter(&rendered, None));
                }
            }
        }

        for rel in &self.related {
            out.push('\n');
            out.push_str(&format!("  {color_cyan}note{color_reset}: {}", rel.message));
        }
        out
    }

    /// Emit this diagnostic through the `log` crate at the level appropriate
    /// for its severity, targeting the `clean` logger.
    pub fn emit(&self, ctx: Option<&dyn DiagnosticSource>) {
        log::log!(target: "clean", self.severity.log_level(), "{}", self.render(ctx));
    }

    /// Whether this diagnostic is a hard error (`Severity::Error`).
    pub fn is_err(&self) -> bool {
        match self.severity {
            Severity::Error => true,
            _ => false
        }
    }
}

/// Prefix every line of a rendered source snippet with a left gutter bar,
/// rustc-style:
///
/// ```text
///    |
///    | (=>
///    |   (instance ?X Foo) ...)
///    |
/// ```
fn gutter(snippet: &str, caret: Option<(usize, String)>) -> String {
    let bar = format!("{color_blue}   |{color_reset}");
    let (caret_after, caret_text) = match &caret {
        Some((i, t)) => (Some(*i), t.as_str()),
        None         => (None, ""),
    };
    let mut out = String::new();
    out.push_str(&bar);
    for (i, line) in snippet.lines().enumerate() {
        out.push('\n');
        out.push_str(&bar);
        out.push(' ');
        out.push_str(line);
        if caret_after == Some(i) {
            out.push('\n');
            out.push_str(&bar);
            out.push(' ');
            out.push_str(caret_text);
        }
    }
    out.push('\n');
    out.push_str(&bar);
    out
}

/// Locate the first whole-token occurrence of variable `var` (matched as
/// `?var`, not as a prefix of a longer name) in a rendered, possibly
/// ANSI-coloured, possibly multi-line `snippet`.  Returns `(line_index,
/// visible_column, token_len)` for drawing a caret beneath it, or `None` if not
/// present.
fn find_var_occurrence(snippet: &str, var: &str) -> Option<(usize, usize, usize)> {
    let needle: Vec<char> = std::iter::once('?').chain(var.chars()).collect();
    for (li, line) in snippet.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut col = 0usize;   // visible column (ANSI skipped)
        let mut i = 0usize;     // index into `chars`
        let mut in_esc = false;
        while i < chars.len() {
            let c = chars[i];
            if in_esc {
                if c == 'm' { in_esc = false; }
                i += 1;
                continue;
            }
            if c == '\u{1b}' { in_esc = true; i += 1; continue; }
            if c == '?' && matches_token(&chars, i, &needle) {
                return Some((li, col, needle.len()));
            }
            col += 1;
            i += 1;
        }
    }
    None
}

/// Does `needle` (e.g. `?TIME`) occur at `chars[start..]`, skipping any
/// interleaved ANSI escapes, and end at a token boundary (next visible char is
/// not an identifier char)?
fn matches_token(chars: &[char], start: usize, needle: &[char]) -> bool {
    let mut i = start;
    let mut n = 0usize;
    let mut in_esc = false;
    while i < chars.len() && n < needle.len() {
        let c = chars[i];
        if in_esc { if c == 'm' { in_esc = false; } i += 1; continue; }
        if c == '\u{1b}' { in_esc = true; i += 1; continue; }
        if c != needle[n] { return false; }
        n += 1;
        i += 1;
    }
    if n < needle.len() { return false; }
    // Boundary: skip ANSI, then ensure the next visible char isn't [A-Za-z0-9_].
    while i < chars.len() {
        let c = chars[i];
        if in_esc { if c == 'm' { in_esc = false; } i += 1; continue; }
        if c == '\u{1b}' { in_esc = true; i += 1; continue; }
        return !(c.is_alphanumeric() || c == '_');
    }
    true // end of line is a boundary
}

/// On a multi-line snippet (pretty-printed one argument per line), append a
/// `<<<<` marker to the line that begins argument `arg`.
///
/// Only marks when the layout is unambiguously one-argument-per-line — i.e. the
/// number of lines at the argument indent equals `arg_count`.  A mixed layout
/// (a fallback renderer that keeps some args inline, or a quantifier head)
/// fails that check and is left unmarked rather than risk mis-attributing.
fn mark_arg_line(rendered: &str, arg: i32, arg_count: Option<usize>, color: &str) -> String {
    let arg_count = match arg_count {
        Some(n) if arg >= 1 => n,
        _ => return rendered.to_string(),
    };
    let lines: Vec<&str> = rendered.lines().collect();
    if lines.len() < 2 { return rendered.to_string(); }

    // Leading literal spaces (the pretty-printer's pad precedes any ANSI).
    let indent_of = |l: &str| l.bytes().take_while(|b| *b == b' ').count();
    let arg_indent = indent_of(lines[1]); // first argument's line
    let arg_lines: Vec<usize> = lines.iter().enumerate().skip(1)
        .filter(|(_, l)| indent_of(l) == arg_indent)
        .map(|(i, _)| i)
        .collect();
    if arg_lines.len() != arg_count { return rendered.to_string(); }
    let Some(&target) = arg_lines.get(arg as usize - 1) else { return rendered.to_string(); };

    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        if i > 0 { out.push('\n'); }
        out.push_str(l);
        if i == target {
            out.push_str(&format!(" {color}<<<<{color_reset}"));
        }
    }
    out
}

/// Visible (printable) length of a string, skipping ANSI `ESC[…m` sequences.
/// Used to keep a caret underline from overrunning the rendered snippet.
fn visible_len(s: &str) -> usize {
    let mut n = 0usize;
    let mut in_esc = false;
    for c in s.chars() {
        if in_esc {
            if c == 'm' { in_esc = false; }
        } else if c == '\u{1b}' {
            in_esc = true;
        } else {
            n += 1;
        }
    }
    n
}

/// Supplementary source location + note attached to a [`Diagnostic`].
#[derive(Debug, Clone)]
pub struct RelatedInfo {
    /// Location this note points at.
    pub range:   Span,
    /// The note text.
    pub message: String,
}

// -- DiagnosticSource trait ---------------------------------------------------

/// Optional context for [`Diagnostic::render`] that pulls source-line snippets
/// from a sentence id.
///
/// Implemented for [`crate::KnowledgeBase`]. External callers can implement it
/// for their own KB-equivalent types.
pub trait DiagnosticSource {
    /// Render the sentence at `sid` with `highlight_arg` (≥0 = arg
    /// index, -1 = no per-arg highlight).  Return `None` when the sid
    /// is not known or the sentence body has been cleared.
    fn render_sentence(&self, sid: SentenceId, highlight_arg: i32) -> Option<String>;

    /// The source location (`file:line`) `sid` was parsed from, for the
    /// compiler-style `--> file:line` header.  Returns `None` for synthetic
    /// sentences with no real origin, or when the source has been evicted.
    /// Default: no location.
    fn sentence_location(&self, _sid: SentenceId) -> Option<Span> { None }

    /// Column span `(start, len)` of argument `arg` within the one-line flat
    /// rendering of `sid`, for drawing a caret underline.  `arg` indexes the
    /// sentence's elements (as `highlight_arg` does).  Default: none.
    fn highlight_span(&self, _sid: SentenceId, _arg: i32) -> Option<(usize, usize)> { None }

    /// Number of arguments of `sid` (its element count minus the head).  Lets
    /// the multi-line renderer confirm a one-argument-per-line layout before
    /// marking the offending argument's line.  Default: none.
    fn arg_count(&self, _sid: SentenceId) -> Option<usize> { None }
}

// -- ToDiagnostic trait -------------------------------------------------------

/// Convert an error / warning shape into a structured [`Diagnostic`].
pub trait ToDiagnostic {
    /// Produce the [`Diagnostic`] representing this error or warning.
    fn to_diagnostic(&self) -> Diagnostic;
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A `DiagnosticSource` stub serving a fixed snippet + caret span.
    struct StubSrc;
    impl DiagnosticSource for StubSrc {
        fn render_sentence(&self, _sid: SentenceId, _arg: i32) -> Option<String> {
            Some("(rel Foo Bar)".to_string()) //  ( r e l _ F o o _ B a r )
        }                                      //  0 1 2 3 4 5 6 7 8 9 ...
        fn highlight_span(&self, _sid: SentenceId, arg: i32) -> Option<(usize, usize)> {
            // arg 2 → "Bar" at column 9, length 3.
            (arg == 2).then_some((9, 3))
        }
    }

    #[test]
    fn render_draws_caret_under_highlighted_arg() {
        let d = Diagnostic {
            kind: "semantic", code: "domain-mismatch", severity: Severity::Warning,
            range: Span::default(), message: "x".into(), related: Vec::new(),
            sids: vec![1], highlight_arg: 2, highlight_var: None,
        };
        let s = d.render(Some(&StubSrc));
        // The caret line carries exactly three carets, indented to column 9 of
        // the snippet so it underlines "Bar".
        let caret_line = s.lines().find(|l| l.contains('^')).expect("a caret line");
        assert!(caret_line.contains("^^^") && !caret_line.contains("^^^^"),
            "expected exactly 3 carets, got {:?}", caret_line);
        let visible = visible_len(caret_line);
        // gutter "   |" (4) + space (1) + 9 spaces + 3 carets = 17 visible cols.
        assert_eq!(visible, 4 + 1 + 9 + 3, "caret misaligned: {:?}", caret_line);
    }

    /// A `DiagnosticSource` stub serving a multi-line snippet (one arg per line).
    struct MultiStub;
    impl DiagnosticSource for MultiStub {
        fn render_sentence(&self, _sid: SentenceId, _arg: i32) -> Option<String> {
            Some("(rel\n  Foo\n  Bar)".to_string())
        }
        fn arg_count(&self, _sid: SentenceId) -> Option<usize> { Some(2) }
    }

    #[test]
    fn render_marks_multiline_argument() {
        let d = Diagnostic {
            kind: "semantic", code: "domain-mismatch", severity: Severity::Warning,
            range: Span::default(), message: "x".into(), related: Vec::new(),
            sids: vec![1], highlight_arg: 2, highlight_var: None,
        };
        let s = d.render(Some(&MultiStub));
        let bar = s.lines().find(|l| l.contains("Bar")).expect("Bar line");
        let foo = s.lines().find(|l| l.contains("Foo")).expect("Foo line");
        assert!(bar.contains("<<<<"), "arg-2 line should be marked: {:?}", bar);
        assert!(!foo.contains("<<<<"), "arg-1 line should not be marked: {:?}", foo);
    }

    #[test]
    fn render_underlines_variable_occurrence() {
        struct VarStub;
        impl DiagnosticSource for VarStub {
            fn render_sentence(&self, _s: SentenceId, _a: i32) -> Option<String> {
                // `?USER` is on the second line; `?A` on the first must be skipped.
                Some("(rel ?A\n  ?USER)".to_string())
            }
        }
        let d = Diagnostic {
            kind: "semantic", code: "free-var-in-consequent", severity: Severity::Warning,
            range: Span::default(), message: "x".into(), related: Vec::new(),
            sids: vec![1], highlight_arg: -1, highlight_var: Some("USER".into()),
        };
        let s = d.render(Some(&VarStub));
        let lines: Vec<&str> = s.lines().collect();
        let user_idx  = lines.iter().position(|l| l.contains("?USER")).expect("?USER line");
        let caret_idx = lines.iter().position(|l| l.contains('^')).expect("caret line");
        assert_eq!(caret_idx, user_idx + 1, "caret must sit directly under ?USER");
        assert!(lines[caret_idx].contains("^^^^^") && !lines[caret_idx].contains("^^^^^^"),
            "expected 5 carets (?USER), got {:?}", lines[caret_idx]);
    }

    #[test]
    fn render_no_caret_without_highlight() {
        let d = Diagnostic {
            kind: "semantic", code: "x", severity: Severity::Warning,
            range: Span::default(), message: "x".into(), related: Vec::new(),
            sids: vec![1], highlight_arg: -1, highlight_var: None,
        };
        let s = d.render(Some(&StubSrc));
        assert!(!s.contains('^'), "no caret expected when highlight_arg < 0");
    }
}
