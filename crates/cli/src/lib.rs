pub mod config;
pub mod cli;

// Library-level prover façade and programmatic ask() entry point are
// only available when sigmakee-rs-core's prover API is compiled in.
#[cfg(feature = "ask")]
pub mod prover;
#[cfg(feature = "ask")]
pub mod ask;

#[cfg(feature = "ask")]
pub use ask::{ask, AskOptions, AskResult};

pub use sigmakee_rs_core::{
    KnowledgeBase as Kb, ParseError, SemanticError, TellResult,
};

// Error reporting macros

#[macro_export]
macro_rules! parse_error {
    ($span:expr, $e:expr) => {
        {
            use inline_colorization::*;
            log::error!(
                "{}{}{}, {}line {}{}\n{style_bold}{color_bright_red}{}{style_reset}\n",
                color_magenta,
                $span.file,
                color_reset,
                style_bold,
                $span.line,
                style_reset,
                $e
            );
        }
    };

    ($span:expr, $e:expr, $txt:expr) => {
        {
            use inline_colorization::*;
            let line_start = $txt[..$span.offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let line_end = $txt[$span.offset..].find('\n').map(|i| i + $span.offset).unwrap_or($txt.len());
            let width: usize = $span.col as usize + 9;
            log::error!(
                "{}{}{}\n\n {:<6}| {}\n{color_bright_red}{style_bold}{:>width$} {}{color_reset}\n",
                color_magenta,
                $span.file,
                color_reset,
                $span.line,
                &$txt[line_start..line_end],
                "^",
                $e,
            );
        }
    };
}

/// Print a semantic error using the KB's built-in pretty-printer.
///
/// Usage: `semantic_error!(e, kb)` where `e: &SemanticError` and
/// `kb: KnowledgeBase`.
#[macro_export]
macro_rules! semantic_error {
    ($e:expr, $kb:expr) => {
        {
            $kb.pretty_print_error($e, log::Level::Error);
            eprintln!();
        }
    };
}

/// Print a semantic *warning* using the KB's built-in pretty-printer.
///
/// Companion to [`semantic_error!`].  Honours the `-q` /
/// `suppress_warnings(true)` flag — the macro is a no-op when
/// warnings are suppressed.  Use this for findings classified by
/// [`sigmakee_rs_core::SemanticError::is_warn`] (e.g. everything in
/// [`sigmakee_rs_core::Findings::warnings`] from `kb.validate_*_findings`).
///
/// Usage: `semantic_warning!(e, kb)` where `e: &SemanticError`.
#[macro_export]
macro_rules! semantic_warning {
    ($e:expr, $kb:expr) => {
        {
            if !sigmakee_rs_core::error::warnings_suppressed() {
                $kb.pretty_print_error($e, log::Level::Warn);
                eprintln!();
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigmakee_rs_core::KnowledgeBase;

    const BASE: &str = "
        (subclass Relation Entity)
        (subclass BinaryRelation Relation)
        (subclass Predicate Relation)
        (subclass BinaryPredicate Predicate)
        (subclass BinaryPredicate BinaryRelation)
        (instance subclass BinaryRelation)
        (domain subclass 1 Class)
        (domain subclass 2 Class)
        (instance instance BinaryPredicate)
        (domain instance 1 Entity)
        (domain instance 2 Class)
        (subclass Animal Entity)
        (subclass Human Entity)
        (subclass Human Animal)
    ";

    fn base_kb() -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        kb.load_kif(BASE, "base", None);
        kb
    }

    #[test]
    fn ask_parse_error() {
        let mut kb = base_kb();
        let r = ask(&mut kb, "(subclass Cat", AskOptions::default());
        assert!(!r.proved);
        assert!(!r.errors.is_empty());
    }
}
