// crates/core/src/trans/sort.rs
//
// Sub-module handling sort generation from SUMO type constraints

/// Primitive sort of a SUMO term, independent of any proof target.
///
/// Ordered by specificity: Individual (least) < Real < Rational < Integer (most).
/// `Ord` lets `max(a, b)` pick the more specific sort when multiple constraints
/// conflict -- the winner is always the strongest supported sort.
///
/// TPTP mapping (call `.tptp()` at the tptp/ boundary only):
///   Individual -> "$i"
///   Real       -> "$real"
///   Rational   -> "$rat"
///   Integer    -> "$int"
///
/// `$o` (formula/Boolean sort) is NOT in this enum. It is a TPTP-specific
/// concept with no semantic meaning and is emitted as a literal string inside
/// `tptp/tff.rs` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord,
          serde::Serialize, serde::Deserialize)]
pub enum Sort {
    Individual = 1,
    Real       = 2,
    Rational   = 3,
    Integer    = 4,
}

impl Sort {
    /// Convert to the TPTP sort string.
    /// Call only inside `tptp/` -- never let this string escape into semantic logic.
    ///
    /// Currently only exercised from this module's tests; kept on the
    /// public API so the TFF-emitter code paths in `vampire/converter.rs`
    /// and downstream clausify consumers can call it without a re-export
    /// gymnastics.
    #[allow(dead_code)]
    pub fn tptp(self) -> &'static str {
        match self {
            Sort::Individual => "$i",
            Sort::Real       => "$real",
            Sort::Rational   => "$rat",
            Sort::Integer    => "$int",
        }
    }
}