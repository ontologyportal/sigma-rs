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
// (`% Status`, hardness) and KIF inline lint overrides will land.

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
