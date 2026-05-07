// crates/cli/src/progress.rs
//
// indicatif progress bar helpers for file ingestion.

use indicatif::{ProgressBar, ProgressStyle};

/// Create a progress bar for loading `total` KIF files.
///
/// Returns `None` when `total == 0` so callers can skip the bar
/// entirely for empty file lists.
pub fn file_load_bar(total: u64) -> Option<ProgressBar> {
    if total == 0 {
        return None;
    }
    let bar = ProgressBar::new(total);
    bar.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan} [{bar:40.cyan/blue}] {pos}/{len} {wide_msg}",
        )
        .unwrap()
        .progress_chars("=>-"),
    );
    Some(bar)
}
