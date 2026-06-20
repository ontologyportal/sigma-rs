use thiserror::Error;

use crate::{Diagnostic, Severity, Span, ToDiagnostic, SymbolId};
use super::Sort;

#[derive(Debug, Clone, Error)]
pub enum TranslationError {
    /// A symbol belongs to multiple classes whose TFF sorts are not all the
    /// same (e.g. one class maps to `$int` and another to `$real`).
    ///
    /// The symbol cannot be given a single unambiguous sort; callers should
    /// fall back to `$i` or surface this as a KB inconsistency.
    #[error("symbol {sym} has ambiguous sort — multiple incompatible classes resolve to different sorts: {}", format_sorts(sorts))]
    AmbiguousSort {
        /// The symbol whose class inference returned `ClassInference::Multiple`
        /// with conflicting TFF sorts.
        sym:   SymbolId,
        /// The distinct sorts that were found, one per incompatible class.
        sorts: Vec<Sort>,
    },
}

fn format_sorts(sorts: &[Sort]) -> String {
    sorts.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>().join(", ")
}

impl ToDiagnostic for TranslationError {
    fn to_diagnostic(&self) -> Diagnostic {
        let code = match self {
            TranslationError::AmbiguousSort { .. } => "ambiguous-sort",
        };
        Diagnostic {
            kind: "translation",
            range: Span::synthetic(),
            severity: Severity::Error,
            code,
            message: self.to_string(),
            related: Vec::new(),
            sids: Vec::new(),
            highlight_arg: -1,
            highlight_var: None,
        }
    }
}