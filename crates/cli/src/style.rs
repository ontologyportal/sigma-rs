//! ANSI styling shim with a runtime kill-switch.
//!
//! Each constant (`color_bright_red`, `style_bold`, …) is a `Style` wrapper.
//! `Style` implements `Display` and consults a single process-wide atomic at
//! format time, emitting the escape when color is enabled and the empty string
//! when it is off. Toggled by the top-level `--ugly` flag.
//!
//! Call [`set_ugly`] before any output so cached log-format closures pick up
//! the correct mode on the first emission.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

/// `true` ⇒ all `Style` constants emit the empty string instead of
/// their ANSI escape.  Default is `false` (colored output).
static UGLY: AtomicBool = AtomicBool::new(false);

/// Toggle ANSI suppression for the rest of the process.
///
/// Idempotent and thread-safe; intended to be called once at startup
/// from `main_worker` after CLI parsing.
pub fn set_ugly(ugly: bool) {
    UGLY.store(ugly, Ordering::Relaxed);
}

/// Read-side accessor matching [`set_ugly`].  Used by interactive UI
/// components (e.g. the phase spinner) to opt out of fancy rendering
/// when the user explicitly asked for plain output.
pub fn is_ugly() -> bool {
    UGLY.load(Ordering::Relaxed)
}

/// Wrapper around a single ANSI escape sequence. `Display` checks the global
/// suppression flag and writes either the escape or nothing.
#[derive(Clone, Copy)]
pub struct Style(&'static str);

impl fmt::Display for Style {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if UGLY.load(Ordering::Relaxed) {
            Ok(())
        } else {
            f.write_str(self.0)
        }
    }
}

// `allow(non_upper_case_globals)` keeps the lowercase names the format strings
// embed in `{…}` syntax.

#[allow(non_upper_case_globals)] pub const style_bold:           Style = Style("\x1B[1m");
#[allow(non_upper_case_globals)] pub const style_reset:          Style = Style("\x1B[0m");

#[allow(non_upper_case_globals)] pub const color_blue:           Style = Style("\x1B[34m");
#[allow(non_upper_case_globals)] pub const color_cyan:           Style = Style("\x1B[36m");
#[allow(non_upper_case_globals)] pub const color_magenta:        Style = Style("\x1B[35m");
#[allow(non_upper_case_globals)] pub const color_white:          Style = Style("\x1B[37m");
#[allow(non_upper_case_globals)] pub const color_yellow:         Style = Style("\x1B[33m");

#[allow(non_upper_case_globals)] pub const color_bright_black:   Style = Style("\x1B[90m");
#[allow(non_upper_case_globals)] pub const color_bright_red:     Style = Style("\x1B[91m");
#[allow(non_upper_case_globals)] pub const color_bright_green:   Style = Style("\x1B[92m");
#[allow(non_upper_case_globals)] pub const color_bright_yellow:  Style = Style("\x1B[93m");
#[allow(non_upper_case_globals)] pub const color_bright_cyan:    Style = Style("\x1B[96m");

#[allow(non_upper_case_globals)] pub const color_reset:          Style = Style("\x1B[39m");

#[cfg(test)]
mod tests {
    use super::*;

    // `UGLY` is process-global, so the two assertion phases share one
    // test to keep ordering deterministic under parallel `cargo test`
    // execution.  Splitting them into two `#[test]` fns races on the
    // atomic.
    #[test]
    fn toggle_round_trip() {
        // Default phase: escapes emitted.
        set_ugly(false);
        assert_eq!(format!("{color_bright_red}"), "\x1B[91m");
        assert_eq!(format!("{style_bold}"),       "\x1B[1m");

        // Ugly phase: escapes suppressed.
        set_ugly(true);
        assert_eq!(format!("{color_bright_red}"), "");
        assert_eq!(format!("{style_bold}"),       "");

        // Restore default for any other tests in this binary.
        set_ugly(false);
    }
}
