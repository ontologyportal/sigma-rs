// crates/sumo-kb/src/diagnostic.rs
//
// Unified diagnostic type -- a single representation any downstream
// consumer (CLI, LSP, REPL, JSON reporter, CI tooling) can render
// without having to pattern-match over the three separate error
// enums that surface during KB ingestion:
//
//   * `KifParseError`   -- tokenizer / parser hard errors
//   * `SemanticError`   -- relation / arity / domain / taxonomy issues
//   * `TellWarning`     -- non-fatal notices (duplicate axioms, semantic
//                          warnings that didn't get promoted to errors)
//
// Each carries a span plus a short human message; this module
// synthesises a normalised shape with severity, a stable code, and
// an optional list of related source locations for cross-file
// diagnostics like `DisjointInstance` (which implicates multiple
// sentences).
//
// `Diagnostic` is entirely KB-flavoured -- it uses `Span` and
// `SentenceId` from this crate and has no LSP / IDE dependency.
// Consumers that need LSP types convert at their own boundary.

use crate::error::{SemanticError, Span, TellWarning};
use crate::parse::ParseError;
use crate::parse::kif::KifParseError;

// -- Severity -----------------------------------------------------------------

/// Diagnostic severity, mirroring the canonical four levels used by
/// every serious diagnostic consumer (compilers, LSPs, linters).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error   => "error",
            Self::Warning => "warning",
            Self::Info    => "info",
            Self::Hint    => "hint",
        }
    }
}

// -- Diagnostic ---------------------------------------------------------------

/// A single actionable problem with source context.
///
/// `range` uses this crate's [`Span`] (byte offsets + line/col), so
/// the type is entirely independent of LSP's `Range` / `Position`.
/// The `code` is a stable short identifier (e.g. `"E005"` or
/// `"arity-mismatch"`) suitable for grep-based matching and CI
/// filtering.  `related` carries extra locations that clarify the
/// diagnostic -- e.g. the second of two conflicting declarations --
/// each with its own explanatory note.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub range:    Span,
    pub severity: Severity,
    pub code:     &'static str,
    pub message:  String,
    pub related:  Vec<RelatedInfo>,
}

/// Supplementary source location + note attached to a [`Diagnostic`].
#[derive(Debug, Clone)]
pub struct RelatedInfo {
    pub range:   Span,
    pub message: String,
}

// -- ToDiagnostic trait -------------------------------------------------------

/// Convert a KB error or warning into a structured [`Diagnostic`].
///
/// Implementations pull the span out of the variant, map the existing
/// code / name helpers (e.g. `SemanticError::code`) to the unified
/// `code` field, and render the error message via `Display`.  No
/// implementation needs to touch the store -- `Diagnostic` is purely
/// source-positional.
pub trait ToDiagnostic {
    fn to_diagnostic(&self) -> Diagnostic;
}

impl ToDiagnostic for KifParseError {
    fn to_diagnostic(&self) -> Diagnostic {
        let code: &'static str = match self {
            KifParseError::UnterminatedString { .. }    => "parse/unterminated-string",
            KifParseError::UnexpectedChar      { .. }    => "parse/unexpected-char",
            KifParseError::EmptySentence       { .. }    => "parse/empty-sentence",
            KifParseError::UnexpectedEof       { .. }    => "parse/unexpected-eof",
            KifParseError::UnbalancedParens    { .. }    => "parse/unbalanced-parens",
            KifParseError::OperatorOutOfPosition { .. }  => "parse/operator-out-of-position",
            KifParseError::QuantifierArg       { .. }    => "parse/quantifier-arg",
            KifParseError::FirstTerm           { .. }    => "parse/first-term",
            KifParseError::Syntax              { .. }    => "parse/syntax",
            KifParseError::Other               { .. }    => "parse/other",
        };
        Diagnostic {
            range:    self.get_span(),
            severity: Severity::Error,
            code,
            message:  self.to_string(),
            related:  Vec::new(),
        }
    }
}

impl ToDiagnostic for SemanticError {
    fn to_diagnostic(&self) -> Diagnostic {
        // `current_level()` already folds in the user's -W promotions;
        // an error promoted from a warning surfaces here as Error.
        let severity = match self.current_level() {
            log::Level::Error => Severity::Error,
            log::Level::Warn  => Severity::Warning,
            log::Level::Info  => Severity::Info,
            _                 => Severity::Hint,
        };
        Diagnostic {
            range:    Span::default(),  // filled by caller from Sentence.span
            severity,
            code:     self.code(),
            message:  self.to_string(),
            related:  Vec::new(),
        }
    }
}

impl ToDiagnostic for TellWarning {
    fn to_diagnostic(&self) -> Diagnostic {
        match self {
            TellWarning::DuplicateAxiom { formula_preview, .. } => Diagnostic {
                range:    Span::default(),
                severity: Severity::Warning,
                code:     "tell/duplicate-axiom",
                message:  format!("duplicate axiom (skipped): {}", formula_preview),
                related:  Vec::new(),
            },
            TellWarning::DuplicateAssertion { formula_preview, existing_session, .. } => Diagnostic {
                range:    Span::default(),
                severity: Severity::Warning,
                code:     "tell/duplicate-assertion",
                message:  format!(
                    "duplicate of assertion in session '{}' (skipped): {}",
                    existing_session, formula_preview
                ),
                related:  Vec::new(),
            },
            TellWarning::Semantic(e) => e.to_diagnostic(),
        }
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::ast::Span;

    #[test]
    fn parse_error_round_trip() {
        let span = Span::point("t".into(), 1, 1, 0);
        let err  = KifParseError::UnexpectedEof { span: span.clone() };
        let d    = err.to_diagnostic();
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.code,     "parse/unexpected-eof");
        assert_eq!(d.range,    span);
        assert!(d.message.contains("unexpected end"));
    }

    #[test]
    fn semantic_warning_maps_to_warning_severity() {
        // `FunctionCase` is W011 -- a non-promoted warning by default.
        let err = SemanticError::FunctionCase { sym: "foo".into() };
        let d   = err.to_diagnostic();
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.code,     "W011");
    }

    #[test]
    fn tell_warning_passes_through_semantic() {
        let inner  = SemanticError::FunctionCase { sym: "foo".into() };
        let warn   = TellWarning::Semantic(inner);
        let d      = warn.to_diagnostic();
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.code,     "W011");
    }

    #[test]
    fn duplicate_axiom_has_stable_code() {
        let w = TellWarning::DuplicateAxiom {
            existing_id:     42,
            formula_preview: "(subclass A B)".into(),
        };
        let d = w.to_diagnostic();
        assert_eq!(d.code,     "tell/duplicate-axiom");
        assert_eq!(d.severity, Severity::Warning);
    }
}
