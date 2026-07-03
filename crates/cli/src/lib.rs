pub mod config;
pub mod cli;
pub mod progress;
pub mod style;

pub use sigmakee_rs_sdk::{
    KnowledgeBase as Kb, SemanticError, TellResult,
};

// Error reporting macros

#[macro_export]
macro_rules! parse_error {
    ($span:expr, $e:expr) => {
        {
            use $crate::style::*;
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
            use $crate::style::*;
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
/// Accepts `e: &SemanticError`.
/// Usage: `semantic_error!(e, kb)`.
#[macro_export]
macro_rules! semantic_error {
    ($e:expr, $kb:expr) => {
        {
            use sigmakee_rs_sdk::ToDiagnostic;
            let _d = ($e).clone().to_diagnostic();
            $kb.pretty_print_error(&_d, log::Level::Error);
            eprintln!();
        }
    };
}

/// Print a semantic *warning* using the KB's built-in pretty-printer.
///
/// Companion to [`semantic_error!`].  Suppressed when
/// `sigmakee_rs_sdk::warnings_suppressed()` is true.
/// Usage: `semantic_warning!(e, kb)` where `e: &SemanticError`.
#[macro_export]
macro_rules! semantic_warning {
    ($e:expr, $kb:expr) => {
        {
            if !sigmakee_rs_sdk::warnings_suppressed() {
                use sigmakee_rs_sdk::ToDiagnostic;
                let _d = ($e).clone().to_diagnostic();
                $kb.pretty_print_error(&_d, log::Level::Warn);
                eprintln!();
            }
        }
    };
}
