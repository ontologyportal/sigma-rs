//! Numeric parsing, canonical rendering, and ground evaluation of SUMO's
//! arithmetic functions and comparison predicates.
//!
//! Canonical rendering is load-bearing: literals are stored as strings and
//! content-addressed, so `8`, `8.0`, and `8.00` must collapse to one spelling
//! or dedup, equality keys, and the residue index all fracture.

/// Parse a SUO-KIF numeric literal.
pub(crate) fn parse_num(s: &str) -> Option<f64> {
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Canonical rendering: integers print without a point; fractions
/// trim trailing zeros (`%.10g`-flavored).
pub(crate) fn format_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{:.10}", v);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Evaluate one of SUMO's binary arithmetic functions over ground
/// numeric arguments.  `None` ⇒ not an arithmetic function, division
/// by zero, or a non-finite result — the term stays symbolic.
pub(crate) fn eval_binary_fn(name: &str, x: f64, y: f64) -> Option<f64> {
    let v = match name {
        "AdditionFn"       => x + y,
        "SubtractionFn"    => x - y,
        "MultiplicationFn" => x * y,
        "DivisionFn" if y != 0.0 => x / y,
        _ => return None,
    };
    v.is_finite().then_some(v)
}

/// Decide one of SUMO's comparison predicates over ground numeric
/// arguments.  `None` ⇒ not a comparison predicate.
#[cfg_attr(not(feature = "native-prover"), allow(dead_code))]
pub(crate) fn eval_compare(name: &str, x: f64, y: f64) -> Option<bool> {
    Some(match name {
        "greaterThan"          => x > y,
        "lessThan"             => x < y,
        "greaterThanOrEqualTo" => x >= y,
        "lessThanOrEqualTo"    => x <= y,
        _ => return None,
    })
}

/// The equality-class key for a numeric literal: a hash of the canonical
/// rendering in its own namespace, disjoint from symbol-name hashes.
#[cfg_attr(not(feature = "native-prover"), allow(dead_code))]
pub(crate) fn num_eq_key(s: &str) -> Option<u64> {
    let v = parse_num(s)?;
    Some(xxhash_rust::xxh64::xxh64(
        format_num(v).as_bytes(),
        u64::from(b'#'), // literal namespace, disjoint from symbol-hash seed
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_rendering_collapses_spellings() {
        for (a, b) in [("8", "8.0"), ("8.0", "8.00"), ("40", "40.0"), ("-3.5", "-3.50")] {
            assert_eq!(
                format_num(parse_num(a).unwrap()),
                format_num(parse_num(b).unwrap()),
                "{a} vs {b}"
            );
            assert_eq!(num_eq_key(a), num_eq_key(b));
        }
        assert_ne!(num_eq_key("8"), num_eq_key("9"));
    }

    #[test]
    fn arithmetic_and_comparisons() {
        assert_eq!(eval_binary_fn("AdditionFn", 3.0, 5.0), Some(8.0));
        assert_eq!(eval_binary_fn("DivisionFn", 1.0, 0.0), None);
        assert_eq!(eval_binary_fn("part", 1.0, 2.0), None);
        assert_eq!(eval_compare("greaterThan", -100.0, 0.0), Some(false));
        assert_eq!(eval_compare("lessThanOrEqualTo", 2.0, 2.0), Some(true));
        assert_eq!(eval_compare("weight", 1.0, 2.0), None);
    }
}
