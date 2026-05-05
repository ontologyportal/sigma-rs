// crates/core/src/syntactic/load.rs
//
// Top-level KIF loader.  Drives the parser and feeds recovered AST
// nodes into `SyntacticLayer::load`.

use crate::KbError;
use crate::parse::ast::Span;

use super::SyntacticLayer;

/// Parse `text` (tagged as `file`) into `store`.  Returns hard parse errors.
///
/// Formulas containing row variables (`@VAR`) are automatically expanded into
/// up to [`crate::row_vars::MAX_ARITY`] concrete variants before being stored.
/// This follows the approach of Java's `RowVars.expandRowVars`.
///
/// ## Error-recovery semantics
///
/// The KIF parser is error-recovering: it returns every top-level
/// sentence it *could* parse alongside a diagnostic for each bad
/// one.  We commit the recovered nodes so mid-edit state (e.g. a
/// user typing a new sentence at the end of a file) doesn't blow
/// away the rest of the file's symbols.  The caller is responsible
/// for putting those nodes through the full semantic / dedup /
/// taxonomy pipeline -- see `KnowledgeBase::ingest`.
pub(crate) fn load_kif(store: &mut SyntacticLayer, text: &str, file: &str) -> Vec<(Span, KbError)> {
    use crate::parse::Parser;
    let mut errors: Vec<(Span, KbError)> = Vec::new();

    let (nodes, parse_err) = Parser::Kif.parse(text, file);
    errors.extend(parse_err.into_iter().map(|(span, p)| { (span, KbError::Parse(p)) }));
    errors.extend(store.load(&nodes, file));
    crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Info, target: "sigmakee_rs_core::syntactic", message: format!("loaded '{}': {} root sentences, {} errors", file, store.roots.len(), errors.len()) });
    errors
}
