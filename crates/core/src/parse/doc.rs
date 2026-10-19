// crates/core/src/parse/doc.rs
//
// Document-level items.  A parsed document is a sequence of top-level
// statements *and* non-logical directives.  Rather than add a non-formula
// variant to [`AstNode`] (which is the logical formula tree and is matched
// exhaustively in ~30 places), directives are their own [`MetaNode`] and a
// document is a `Vec<DocItem>` — so directives interleave with statements in
// source order without taxing every AST traversal, and can never reach the
// content-addressed sentence store.
//
// Today only the TQ parser emits `Meta` items (the `time`/`answer`/`file`/
// `note` harness directives).  The same channel is where TPTP pragma-comments
// (`% Status`, hardness) land.  In-source diagnostic suppressions
// (`; sumo-ignore`) are read off [`CommentBlock`]s instead: a `Meta` item
// re-formats as a `(key args...)` form, which a comment pragma must not.

use serde::{Deserialize, Serialize};

use crate::parse::ast::AstNode;
use crate::parse::Span;

/// A non-logical document directive — the head keyword plus its raw, parsed
/// operands.  The operands are left uninterpreted here; each consumer reads
/// them per-`key` (e.g. the test harness turns `time`/`answer` into a
/// [`TestCase`](crate::parse::tq::TestCase)'s fields).
///
/// Kept deliberately generic (a `key` + `args`, not a per-directive enum) so
/// new directive families — TPTP status/hardness comments, KIF lint pragmas —
/// can ride the same node without widening a closed enum.
#[derive(Debug, Clone, PartialEq)]
pub struct MetaNode {
    /// Directive keyword — the head symbol (`note` / `time` / `answer` /
    /// `file` / later `status` / `lint` / …).
    pub key: String,
    /// The directive's operands, parsed but uninterpreted.
    pub args: Vec<AstNode>,
    /// Source span of the whole directive form.
    pub span: Span,
}

/// A contiguous block of `;` source comments, consolidated from the comment
/// tokens the KIF tokenizer emits.
///
/// Comments are lexical trivia, not logic: they never enter the [`AstNode`]
/// tree (which would perturb the content-addressed sentence fingerprints) and
/// never reach the sentence store.  They ride on
/// [`ParsedDocument::comments`](crate::parse::ParsedDocument) as a
/// span-ordered side list for consumers that care about source fidelity
/// (formatters, editors).
///
/// Consecutive comment lines -- each starting on the line immediately after
/// the previous one, with no significant token between them -- are merged
/// into one block.  Whether a block began inline after code is recoverable
/// from `span` (its start column / surrounding token spans), not modeled
/// here.
#[derive(Debug, Clone, PartialEq)]
pub struct CommentBlock {
    /// The comment text: one line per source comment, `;` markers and
    /// surrounding whitespace stripped, lines joined with `\n`.
    pub text: String,
    /// Source span covering the whole block, from the first `;` through the
    /// end of the last comment line.
    pub span: Span,
    /// A KIF `#| |#` block comment: `text` is the raw content between the
    /// markers, and formatters must re-emit it unchanged rather than reflow
    /// it.
    pub verbatim: bool,
}

/// One top-level item of a parsed document: a logical statement (an
/// [`AstNode`], possibly `Annotated` with a [`Role`](crate::parse::ast::Role))
/// or a non-logical [`MetaNode`] directive.
#[derive(Debug, Clone)]
pub enum DocItem {
    Stmt(AstNode),
    Meta(MetaNode),
}

impl DocItem {
    /// The item's source span: the statement's, or the directive form's.
    pub fn span(&self) -> &Span {
        match self {
            DocItem::Stmt(n) => n.span(),
            DocItem::Meta(m) => &m.span,
        }
    }

    /// The statement, if this item is one.
    pub fn as_stmt(&self) -> Option<&AstNode> {
        match self {
            DocItem::Stmt(n) => Some(n),
            _ => None,
        }
    }
    /// The directive, if this item is one.
    pub fn as_meta(&self) -> Option<&MetaNode> {
        match self {
            DocItem::Meta(m) => Some(m),
            _ => None,
        }
    }
}

// -- Suppression directives ---------------------------------------------------

/// Comment keyword suppressing diagnostics on the next top-level form.
const IGNORE_NEXT: &str = "sumo-ignore";
/// Comment keyword suppressing diagnostics across the whole file.
const IGNORE_FILE: &str = "sumo-ignore-file";

/// The diagnostic names one or more suppression directives cover.
///
/// A directive with no names, or one naming `all`, covers every diagnostic.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SuppressSet {
    all: bool,
    names: Vec<String>,
}

impl SuppressSet {
    fn parse(operands: &str) -> Self {
        let names: Vec<String> = operands
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .collect();
        Self {
            all: names.is_empty() || names.iter().any(|n| n == "all"),
            names,
        }
    }

    fn is_empty(&self) -> bool {
        !self.all && self.names.is_empty()
    }

    fn extend(&mut self, other: SuppressSet) {
        self.all |= other.all;
        self.names.extend(other.names);
    }

    /// Whether a diagnostic named `name` (kebab-case) is covered.
    pub(crate) fn covers(&self, name: &str) -> bool {
        self.all || self.names.iter().any(|n| n == name)
    }
}

/// One suppression directive read from a comment line.
#[derive(Debug, Clone, PartialEq)]
enum Directive {
    /// `; sumo-ignore[: names]` -- the next top-level form.
    Next(SuppressSet),
    /// `; sumo-ignore-file[: names]` -- every form in the file.
    File(SuppressSet),
}

impl Directive {
    /// Parse one comment line (leading `;` markers tolerated).  The keyword
    /// may be followed by an optional `:` and comma- or space-separated names.
    fn parse(line: &str) -> Option<Self> {
        let line = line.trim_start_matches(';').trim();
        let end = line
            .find(|c: char| c == ':' || c.is_whitespace())
            .unwrap_or(line.len());
        let (keyword, rest) = line.split_at(end);
        let rest = rest.trim_start();
        let operands = rest.strip_prefix(':').unwrap_or(rest);
        match keyword {
            IGNORE_NEXT => Some(Self::Next(SuppressSet::parse(operands))),
            IGNORE_FILE => Some(Self::File(SuppressSet::parse(operands))),
            _ => None,
        }
    }
}

/// A source file's in-source suppressions: a file-wide set plus a set per
/// top-level form, keyed by that form's span.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Suppressions {
    file: SuppressSet,
    forms: Vec<(Span, SuppressSet)>,
}

impl Suppressions {
    /// Bind the directives in `comments` to the forms of one file.
    ///
    /// `text` is the source the spans index into; `forms` are the file's
    /// top-level form spans in source order.  A `sumo-ignore` directive binds
    /// to the next form only when nothing but whitespace separates them, so a
    /// directive followed by a form that failed to parse binds to nothing.
    /// Directives inside a form are ignored.
    pub(crate) fn bind(text: &str, forms: &[Span], comments: &[CommentBlock]) -> Self {
        let mut out = Self::default();
        for block in comments {
            let at = block.span.offset;
            if forms.iter().any(|f| f.offset < at && at < f.end_offset) {
                continue;
            }
            for directive in block.text.lines().filter_map(Directive::parse) {
                match directive {
                    Directive::File(set) => out.file.extend(set),
                    Directive::Next(set) => {
                        let end = block.span.end_offset;
                        let next = forms.iter().find(|f| f.offset >= end).filter(|f| {
                            text.get(end..f.offset)
                                .is_some_and(|gap| gap.trim().is_empty())
                        });
                        if let Some(form) = next {
                            match out.forms.iter_mut().find(|(s, _)| s == form) {
                                Some((_, existing)) => existing.extend(set),
                                None => out.forms.push((form.clone(), set)),
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// True when no directive was bound.
    pub(crate) fn is_empty(&self) -> bool {
        self.file.is_empty() && self.forms.is_empty()
    }

    /// Whether a diagnostic named `name` located at `range` is suppressed:
    /// covered file-wide, or by the form whose span contains `range`.
    pub(crate) fn covers(&self, range: &Span, name: &str) -> bool {
        self.file.covers(name)
            || self.forms.iter().any(|(form, set)| {
                form.offset <= range.offset
                    && range.end_offset <= form.end_offset
                    && set.covers(name)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Parser;

    fn bound(src: &str) -> (Suppressions, Vec<Span>) {
        let ((items, errors), comments) = Parser::default().parse_full(src, "t.kif");
        let _ = errors;
        let forms: Vec<Span> = items.iter().map(|i| i.span().clone()).collect();
        (Suppressions::bind(src, &forms, &comments), forms)
    }

    #[test]
    fn directive_parses_keyword_and_names() {
        assert_eq!(
            Directive::parse(" sumo-ignore: arity-mismatch, head-not-relation"),
            Some(Directive::Next(SuppressSet {
                all: false,
                names: vec!["arity-mismatch".into(), "head-not-relation".into()],
            }))
        );
        assert_eq!(
            Directive::parse(";; sumo-ignore-file arity-mismatch"),
            Some(Directive::File(SuppressSet {
                all: false,
                names: vec!["arity-mismatch".into()],
            }))
        );
        assert!(matches!(Directive::parse("sumo-ignore"), Some(Directive::Next(s)) if s.all));
        assert!(matches!(Directive::parse("sumo-ignore: all"), Some(Directive::Next(s)) if s.all));
        assert_eq!(Directive::parse("sumo-ignored things"), None);
        assert_eq!(Directive::parse("just a comment"), None);
    }

    #[test]
    fn next_directive_binds_only_the_following_form() {
        let (s, forms) = bound("; sumo-ignore: arity-mismatch\n(a b)\n(c d)\n");
        assert!(s.covers(&forms[0], "arity-mismatch"));
        assert!(!s.covers(&forms[0], "other"));
        assert!(!s.covers(&forms[1], "arity-mismatch"));
    }

    #[test]
    fn next_directive_survives_blank_lines_and_doc_lines() {
        let (s, forms) = bound("; sumo-ignore\n; why it is fine\n\n(a b)\n");
        assert!(s.covers(&forms[0], "anything"));
    }

    #[test]
    fn next_directive_does_not_skip_over_an_unparsed_form() {
        let (s, forms) = bound("; sumo-ignore\n(a b\n\n(c d)\n");
        assert!(forms.iter().all(|f| !s.covers(f, "x")));
    }

    #[test]
    fn directive_inside_a_form_is_ignored() {
        let (s, forms) = bound("(a\n  ; sumo-ignore\n  b)\n(c d)\n");
        assert!(s.is_empty());
        assert!(!s.covers(&forms[1], "x"));
    }

    #[test]
    fn trailing_directive_at_end_of_file_binds_nothing() {
        let (s, _) = bound("(a b)\n; sumo-ignore\n");
        assert!(s.is_empty());
    }

    #[test]
    fn directive_inside_a_block_comment_binds() {
        let (s, forms) = bound("#| sumo-ignore: arity-mismatch |#\n(a b)\n");
        assert!(s.covers(&forms[0], "arity-mismatch"));
    }

    #[test]
    fn file_directive_covers_every_form() {
        let (s, forms) = bound("(a b)\n; sumo-ignore-file: arity-mismatch\n(c d)\n");
        assert!(forms.iter().all(|f| s.covers(f, "arity-mismatch")));
        assert!(!s.covers(&forms[0], "other"));
    }

    #[test]
    fn covers_a_range_nested_in_the_form() {
        let (s, forms) = bound("; sumo-ignore\n(a (b c))\n");
        let inner = Span {
            offset: forms[0].offset + 3,
            end_offset: forms[0].offset + 8,
            ..forms[0].clone()
        };
        assert!(s.covers(&inner, "x"));
    }
}
