// crates/core/src/diagnostic.rs
//
// Unified error / warning reporting for sigmakee-rs-core.
//
// `Diagnostic` is the single public representation any consumer
// (CLI, LSP, REPL, JSON reporter, CI tooling) renders.  Everything
// that surfaces during KB ingestion or query — parser hard errors,
// semantic-validation findings, tell-time warnings, persistence /
// prover errors, and any other [`KbError`] variant — converts into a
// `Diagnostic` through [`ToDiagnostic`] and goes through one renderer.
//
// Producers don't print directly.  The rules:
//
//   1. Whatever you have (`KbError`, `SemanticError`, `KifParseError`,
//      `TellWarning`), call `.to_diagnostic()` to get a `Diagnostic`.
//   2. To render, call `Diagnostic::render(ctx)` (returns a `String`)
//      or `Diagnostic::emit(ctx)` (forwards through the `log` crate).
//      The optional `ctx` is anything implementing `DiagnosticSource`
//      — `KnowledgeBase` is the canonical impl and adds source-line
//      context (sentence pretty-printing with argument highlighting).
//   3. The renderer ALWAYS picks colors from `Severity`; consumers
//      never branch on severity manually for color reasons.
//
// `Diagnostic` is entirely KB-flavoured — it uses `Span` and
// `SentenceId` from this crate and has no LSP / IDE dependency.
// Consumers that need LSP types convert at their own boundary.

use inline_colorization::*;

use crate::KbError;
use crate::kb::ingest::TellWarning;
use crate::parse::ParseError;
use crate::parse::ast::Span;
use crate::parse::kif::KifParseError;
use crate::semantics::errors::SemanticError;
use crate::types::SentenceId;

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

    /// Map onto a `log::Level` for the [`Diagnostic::emit`] path.
    /// `Hint` collapses into `Trace` since the `log` crate has no
    /// dedicated hint level.
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

/// A single actionable problem with source context.
///
/// `range` uses this crate's [`Span`] (byte offsets + line/col), so
/// the type is entirely independent of LSP's `Range` / `Position`.
/// The `code` is a stable short identifier (e.g. `"E005"` or
/// `"arity-mismatch"`) suitable for grep-based matching and CI
/// filtering.  `related` carries extra locations that clarify the
/// diagnostic — e.g. the second of two conflicting declarations —
/// each with its own explanatory note.
///
/// `sids` and `highlight_arg` are optional source-context hints: when
/// a [`DiagnosticSource`] is passed to [`Self::render`] / [`Self::emit`],
/// the renderer pulls each listed sentence and prints it inline,
/// highlighting the argument at `highlight_arg` in the first sid.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub range:    Span,
    pub severity: Severity,
    pub code:     &'static str,
    pub message:  String,
    pub related:  Vec<RelatedInfo>,
    /// Sentences implicated by this diagnostic.  Empty for source-
    /// positional errors that don't reference a stored sentence
    /// (e.g. parse errors).
    pub sids:          Vec<SentenceId>,
    /// Argument index to highlight inside the first entry of `sids`,
    /// or `-1` for "no highlight" / "highlight whole sentence".
    pub highlight_arg: i32,
}

impl Diagnostic {
    /// Render this diagnostic as colorised text.
    ///
    /// `ctx`, when given, provides source-context lookup for `sids`
    /// (e.g. `KnowledgeBase` prints the offending sentence with a
    /// gutter).  Without `ctx` the output is the message + code +
    /// related-info notes alone.
    pub fn render(&self, ctx: Option<&dyn DiagnosticSource>) -> String {
        let color = self.severity.ansi_color();
        let mut out = String::new();

        if let Some(src) = ctx {
            for (i, &sid) in self.sids.iter().enumerate() {
                let arg = if i == 0 { self.highlight_arg } else { -1 };
                if let Some(rendered) = src.render_sentence(sid, arg) {
                    out.push_str(&rendered);
                    out.push('\n');
                }
            }
            if !self.sids.is_empty() { out.push('\n'); }
        }

        out.push_str(&format!(
            "{}{}\t[{}] {}{}",
            color, self.severity.as_str(), self.code, self.message, color_reset,
        ));

        for rel in &self.related {
            out.push('\n');
            out.push_str(&format!("  note: {}", rel.message));
        }
        out
    }

    /// Emit this diagnostic through the `log` crate at the level
    /// appropriate for its severity.  Targets the `clean` logger so
    /// existing log filters that route ANSI output to stderr keep
    /// working.
    pub fn emit(&self, ctx: Option<&dyn DiagnosticSource>) {
        log::log!(target: "clean", self.severity.log_level(), "{}", self.render(ctx));
    }
}

/// Supplementary source location + note attached to a [`Diagnostic`].
#[derive(Debug, Clone)]
pub struct RelatedInfo {
    pub range:   Span,
    pub message: String,
}

// -- DiagnosticSource trait ---------------------------------------------------

/// Optional context for [`Diagnostic::render`] that pulls source-line
/// snippets from a sentence id.
///
/// Implemented for [`crate::KnowledgeBase`].  External callers can
/// implement it for their own KB-equivalent types (e.g. a snapshot
/// view exposed by the SDK).
pub trait DiagnosticSource {
    /// Render the sentence at `sid` with `highlight_arg` (≥0 = arg
    /// index, -1 = no per-arg highlight).  Return `None` when the sid
    /// is not known or the sentence body has been cleared.
    fn render_sentence(&self, sid: SentenceId, highlight_arg: i32) -> Option<String>;
}

// -- ToDiagnostic trait -------------------------------------------------------

/// Convert any sigmakee-rs-core error / warning shape into a structured
/// [`Diagnostic`].  Implemented for every error type in the crate so
/// consumers don't pattern-match the variants themselves.
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
            range:         self.get_span(),
            severity:      Severity::Error,
            code,
            message:       self.to_string(),
            related:       Vec::new(),
            sids:          Vec::new(),
            highlight_arg: -1,
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
        // Pull sids + highlight info out of the variant so the
        // renderer can show source context without re-pattern-matching.
        let (sids, highlight_arg): (Vec<SentenceId>, i32) = match self {
            SemanticError::HeadNotRelation { sid, .. }
            | SemanticError::HeadInvalid   { sid, .. }
            | SemanticError::SingleArity   { sid, .. } => (vec![*sid], 0),
            SemanticError::NonLogicalArg   { sid, arg, .. }
            | SemanticError::DomainMismatch { sid, arg, .. } => (vec![*sid], *arg as i32),
            SemanticError::ArityMismatch   { sid, .. } => (vec![*sid], -1),
            SemanticError::DisjointInstance { sid, .. }
            | SemanticError::DisjointSubclass { sid, .. } => (sid.clone(), -1),
            // Symbol-level errors with no specific sentence anchor.
            _ => (Vec::new(), -1),
        };
        Diagnostic {
            range:    Span::default(),  // filled by caller from Sentence.span when needed
            severity,
            code:     self.code(),
            message:  self.to_string(),
            related:  Vec::new(),
            sids,
            highlight_arg,
        }
    }
}

impl ToDiagnostic for TellWarning {
    fn to_diagnostic(&self) -> Diagnostic {
        match self {
            TellWarning::DuplicateAxiom { formula_preview, .. } => Diagnostic {
                range:         Span::default(),
                severity:      Severity::Warning,
                code:          "tell/duplicate-axiom",
                message:       format!("duplicate axiom (skipped): {}", formula_preview),
                related:       Vec::new(),
                sids:          Vec::new(),
                highlight_arg: -1,
            },
            TellWarning::DuplicateAssertion { formula_preview, existing_session, .. } => Diagnostic {
                range:         Span::default(),
                severity:      Severity::Warning,
                code:          "tell/duplicate-assertion",
                message:       format!(
                    "duplicate of assertion in session '{}' (skipped): {}",
                    existing_session, formula_preview
                ),
                related:       Vec::new(),
                sids:          Vec::new(),
                highlight_arg: -1,
            },
            TellWarning::Semantic(e) => e.to_diagnostic(),
        }
    }
}

impl ToDiagnostic for KbError {
    fn to_diagnostic(&self) -> Diagnostic {
        match self {
            KbError::Parse(p)    => p.to_diagnostic(),
            KbError::Semantic(e) => e.to_diagnostic(),
            #[cfg(feature = "persist")]
            KbError::Db(msg) => Diagnostic {
                range:         Span::default(),
                severity:      Severity::Error,
                code:          "kb/db-error",
                message:       format!("database error: {}", msg),
                related:       Vec::new(),
                sids:          Vec::new(),
                highlight_arg: -1,
            },
            #[cfg(feature = "persist")]
            KbError::SchemaMigrationRequired(msg) => Diagnostic {
                range:         Span::default(),
                severity:      Severity::Error,
                code:          "kb/schema-migration-required",
                message:       format!("schema migration required: {}", msg),
                related:       Vec::new(),
                sids:          Vec::new(),
                highlight_arg: -1,
            },
            #[cfg(feature = "ask")]
            KbError::Prover(msg) => Diagnostic {
                range:         Span::default(),
                severity:      Severity::Error,
                code:          "kb/prover-error",
                message:       format!("prover error: {}", msg),
                related:       Vec::new(),
                sids:          Vec::new(),
                highlight_arg: -1,
            },
            KbError::Other(msg) => Diagnostic {
                range:         Span::default(),
                severity:      Severity::Error,
                code:          "kb/other",
                message:       msg.clone(),
                related:       Vec::new(),
                sids:          Vec::new(),
                highlight_arg: -1,
            },
        }
    }
}

// `Box<dyn ParseError>` shows up as the inner of `KbError::Parse`; the
// concrete type is `KifParseError` today, but the signature is open.
impl ToDiagnostic for Box<dyn ParseError> {
    fn to_diagnostic(&self) -> Diagnostic {
        // Fall back via the trait's `to_string()` rendering.  When the
        // underlying type is `KifParseError`, the dedicated impl above
        // should already have been called via downcast; this is the
        // catch-all for future ParseError implementors.
        Diagnostic {
            range:         self.get_span(),
            severity:      Severity::Error,
            code:          "parse/error",
            message:       self.to_string(),
            related:       Vec::new(),
            sids:          Vec::new(),
            highlight_arg: -1,
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

    #[test]
    fn semantic_error_carries_sid_for_source_context() {
        let err = SemanticError::ArityMismatch {
            sid:      77,
            rel:      "instance".into(),
            expected: 2,
            got:      3,
        };
        let d = err.to_diagnostic();
        assert_eq!(d.sids,          vec![77]);
        assert_eq!(d.highlight_arg, -1);
    }

    #[test]
    fn kb_error_routes_through_inner_for_parse_and_semantic() {
        let span = Span::point("t".into(), 1, 1, 0);
        let parse: Box<dyn ParseError> = Box::new(KifParseError::UnexpectedEof { span: span.clone() });
        let kb_err = KbError::Parse(parse);
        let d = kb_err.to_diagnostic();
        assert_eq!(d.severity, Severity::Error);

        let kb_err = KbError::Semantic(SemanticError::FunctionCase { sym: "Foo".into() });
        let d = kb_err.to_diagnostic();
        assert_eq!(d.code, "W011");
    }

    #[test]
    fn kb_error_other_renders_as_error() {
        let kb_err = KbError::Other("something broke".into());
        let d = kb_err.to_diagnostic();
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.code,     "kb/other");
        assert!(d.message.contains("something broke"));
    }

    #[test]
    fn render_without_source_context_includes_code_and_message() {
        let err = SemanticError::FunctionCase { sym: "Foo".into() };
        let d   = err.to_diagnostic();
        let s   = d.render(None);
        assert!(s.contains("[W011]"));
        assert!(s.contains("uppercase"));
    }
}
