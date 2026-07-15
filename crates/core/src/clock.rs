//! Monotonic-clock shim.
//!
//! On every non-wasm target this is a straight re-export of
//! [`std::time::Instant`] plus a `SystemTime`-backed [`epoch_nanos`] — zero
//! behavioural change.
//!
//! On `wasm32-unknown-unknown` the std clock is a trap: `Instant::now()` and
//! `SystemTime::now()` **panic at runtime** ("time not implemented on this
//! platform"). The native saturation prover consults `Instant::now()` on every
//! deadline check and profiling span, so without a shim the first `prove()`
//! call aborts the module. We back the same surface with `js_sys::Date::now()`
//! (wall-clock milliseconds) — monotonic *enough* for "have N seconds elapsed?"
//! deadline logic and for the coarse profiling timers, which is all the callers
//! need. Wall-clock (rather than a true monotonic source like
//! `performance.now()`) keeps the dependency to `js-sys` alone and cannot be
//! perturbed by the absence of a `web_sys::Window` (e.g. worker/Node hosts).

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::Instant;

/// Nanoseconds since the Unix epoch. Used only to mint unique session tags,
/// so millisecond granularity (all `Date::now()` offers) is fine on wasm.
#[cfg(not(target_arch = "wasm32"))]
pub fn epoch_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(target_arch = "wasm32")]
pub use wasm::Instant;

#[cfg(target_arch = "wasm32")]
pub fn epoch_nanos() -> u128 {
    // `Date::now()` is f64 milliseconds since the Unix epoch.
    (js_sys::Date::now() as u128).saturating_mul(1_000_000)
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::cmp::Ordering;
    use std::ops::Add;
    use std::time::Duration;

    /// A [`std::time::Instant`] work-alike backed by `Date::now()` milliseconds.
    ///
    /// Implements exactly the surface the prover uses: `now()`, `elapsed()`,
    /// `duration_since`, `+ Duration`, and ordering (for `now() >= deadline`).
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Instant(f64);

    impl Instant {
        pub fn now() -> Self {
            Instant(js_sys::Date::now())
        }

        pub fn elapsed(&self) -> Duration {
            Instant::now().duration_since(*self)
        }

        pub fn duration_since(&self, earlier: Instant) -> Duration {
            let ms = (self.0 - earlier.0).max(0.0);
            Duration::from_secs_f64(ms / 1000.0)
        }

        // Our `duration_since` already clamps negatives to zero, so the
        // saturating variant is an alias — kept for API parity with std.
        pub fn saturating_duration_since(&self, earlier: Instant) -> Duration {
            self.duration_since(earlier)
        }
    }

    impl Add<Duration> for Instant {
        type Output = Instant;
        fn add(self, rhs: Duration) -> Instant {
            Instant(self.0 + rhs.as_secs_f64() * 1000.0)
        }
    }

    impl Eq for Instant {}

    impl PartialOrd for Instant {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }

    impl Ord for Instant {
        fn cmp(&self, other: &Self) -> Ordering {
            self.0.partial_cmp(&other.0).unwrap_or(Ordering::Equal)
        }
    }
}
