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
    pub key:  String,
    /// The directive's operands, parsed but uninterpreted.
    pub args: Vec<AstNode>,
    /// Source span of the whole directive form.
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
        match self { DocItem::Stmt(n) => Some(n), _ => None }
    }
    /// The directive, if this item is one.
    pub fn as_meta(&self) -> Option<&MetaNode> {
        match self { DocItem::Meta(m) => Some(m), _ => None }
    }
}
