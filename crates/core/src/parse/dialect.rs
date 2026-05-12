// crates/core/src/parse/dialect.rs
//
// The emission seam: turn an `AstNode` document back into a serialization
// dialect, mirroring the input-side `Parser`.  Each format implements [`Emit`]
// in its own `parse/<format>/dis.rs`; the [`Emitter`] enum is the public
// dispatch, carrying any per-format config in its variant (so format-specific
// knobs like `TptpLang` never leak into a generic method signature — the same
// shape as `Parser::Tptp { options }` on the parse side).

use super::ast::AstNode;

// The TPTP language enum lives in the shared lexical layer so the dialect, the
// translation layer, and the public API all name the same type.
pub use super::tptp::syntax::TptpLang;

/// A statement that a dialect could not represent, with why — so callers learn
/// the output was filtered rather than silently truncated.
#[derive(Debug, Clone)]
pub struct DroppedStmt {
    pub name:   Option<String>,
    pub reason: String,
}

/// The result of emitting a document: the rendered text plus any statements
/// that did not conform to the chosen dialect/language and were skipped.
#[derive(Debug, Clone, Default)]
pub struct EmitResult {
    pub text:    String,
    pub dropped: Vec<DroppedStmt>,
}

impl EmitResult {
    pub fn is_complete(&self) -> bool { self.dropped.is_empty() }
}

/// Public dispatch over output dialects.  Config rides in the variant.
#[derive(Debug, Clone)]
pub enum Emitter {
    Kif,
    Tptp(TptpLang),
    // Tq / Json / Datalog land in later phases.
}

impl Emitter {
    /// Emit a document (a slice of top-level statements — bare formulas or
    /// [`AstNode::Annotated`]) in this dialect.
    pub fn emit(&self, doc: &[AstNode]) -> EmitResult {
        match self {
            Emitter::Kif        => super::kif::dis::KifEmit.emit_document(doc),
            Emitter::Tptp(lang) => super::tptp::dis::TptpEmit { lang: *lang }.emit_document(doc),
        }
    }

    /// Convenience for a single formula/statement.
    pub fn emit_one(&self, stmt: &AstNode) -> EmitResult {
        self.emit(std::slice::from_ref(stmt))
    }
}

/// Per-format emission, implemented in each `parse/<format>/dis.rs`.
pub(crate) trait Emit {
    /// Render a bare formula body (no statement framing).
    fn emit_formula(&self, f: &AstNode) -> String;

    /// Render a full statement: frame an [`AstNode::Annotated`] (role/name), or
    /// a bare formula with the dialect's default framing.  `Ok(text)` =
    /// emitted; `Err(reason)` = does not conform to this dialect/language and is
    /// dropped.
    fn emit_statement(&self, stmt: &AstNode) -> Result<String, String>;

    /// Render a whole document, collecting dropped statements.  The default
    /// joins per-statement output with newlines; dialects that need a preamble
    /// or whole-document analysis (e.g. TPTP `Auto`) override this.
    fn emit_document(&self, doc: &[AstNode]) -> EmitResult {
        let mut out = EmitResult::default();
        for stmt in doc {
            match self.emit_statement(stmt) {
                Ok(t)  => { out.text.push_str(&t); out.text.push('\n'); }
                Err(reason) => out.dropped.push(DroppedStmt { name: stmt_name(stmt), reason }),
            }
        }
        out
    }
}

/// Styled (indented, width-wrapped, optionally ANSI-coloured) emission.
///
/// Separate from [`Emit`] because not every dialect has a meaningful "pretty"
/// form, and the two are configured differently (`Emit` frames whole
/// statements; `PrettyEmit` renders a formula for human terminal output).
/// Implemented by [`crate::parse::kif::dis::KifEmit`].
pub(crate) trait PrettyEmit {
    /// Render `node` indented by `indent` columns, wrapping long forms; `color`
    /// toggles ANSI styling.
    fn emit_pretty(&self, node: &AstNode, indent: usize, color: bool) -> String;
}

/// The `name` of an [`AstNode::Annotated`] statement, if any.
pub(crate) fn stmt_name(stmt: &AstNode) -> Option<String> {
    match stmt {
        AstNode::Annotated { name, .. } => name.clone(),
        _ => None,
    }
}
