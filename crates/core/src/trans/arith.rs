// crates/core/src/trans/arith.rs
//
// Handle TFF native arithmetic functions
//
// DEPRECATED: `ArithCond` is no longer used by `TranslationLayer`.  The
// formula rewriting pass (`trans/rewrite.rs`) replaced `build_numeric_char_cache`
// and the `numeric_char` cache.  This type is kept temporarily because
// `CachedTaxonomy` (in `persist/env.rs`) still includes `numeric_char_cache` in
// its serialized form for LMDB backward-compatibility.  Remove once the LMDB
// migration window has passed.

/// Arithmetic condition characterizing numeric-class membership.
///
/// # Deprecated
///
/// This type is no longer used internally.  See module-level comment.
///
/// When `(instance ?X C)` appears in TFF mode and `?X` has a numeric sort, the
/// translator substitutes this condition for the otherwise-unsound `$true` drop.
/// The variable is always implicit (the instance variable being checked).
/// `bound` is the raw numeric literal string from the source KIF (e.g. `"0"`, `"1"`).
// Consumed only by the LMDB schema (`persist::env`); see re-export note.
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) enum ArithCond {
    GreaterThan          { bound: String },
    GreaterThanOrEqualTo { bound: String },
    LessThan             { bound: String },
    LessThanOrEqualTo    { bound: String },
    And(Vec<ArithCond>),
    /// `(equal (fn_name ?VAR other_arg) result)` — e.g. `(equal (RemainderFn ?X 2) 0)`.
    EqualFn { fn_name: String, other_arg: String, result: String },
}
