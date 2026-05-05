// crates/core/src/trans/sort/numeric.rs
//
// Handle numeric specific sorts

use super::Sort;

// The three SUMO class names that anchor the TFF numeric sort hierarchy.
// Everything in the KB that is a subclass of one of these (discovered
// dynamically at taxonomy-build time) maps to the corresponding Sort.
// Only these three strings are ever hardcoded; all subclasses are found
// automatically by walking the subclass edges downward.
//
// Order matters for the BFS: process least-specific (Real) first so that
// a more-specific sort (Integer) overwrites it when a class descends from
// multiple roots (e.g. NonnegativeInteger is under both Integer and
// NonnegativeRealNumber -> gets Sort::Integer because Integer is last).
pub const NUMERIC_ROOTS: &[(&str, Sort)] = &[
    ("RealNumber",    Sort::Real),
    ("RationalNumber", Sort::Rational),
    ("Integer",       Sort::Integer),
];