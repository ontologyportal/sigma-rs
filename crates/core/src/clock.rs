//! Wasm-safe clock.
//!
//! On `wasm32-unknown-unknown`, `std::time::Instant::now()` and
//! `SystemTime::now()` panic at runtime ("time not implemented on this
//! platform"). [`web_time`] re-exports the std types on every other target and
//! backs them with `performance.now()` / `Date.now()` on wasm, so code that may
//! run in the browser should take its time types from here.

pub use web_time::{Instant, SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch, or 0 if the clock is before it.
pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_millis_is_after_2020_and_non_decreasing() {
        let a = unix_millis();
        let b = unix_millis();
        assert!(a > 1_577_836_800_000);
        assert!(b >= a);
    }
}
