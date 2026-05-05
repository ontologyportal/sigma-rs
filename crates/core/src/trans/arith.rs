// crates/core/src/trans/mod.rs
//
// Handle TFF native arithmatic functions

/// Arithmetic condition characterizing numeric-class membership.
///
/// When `(instance ?X C)` appears in TFF mode and `?X` has a numeric sort, the
/// translator substitutes this condition for the otherwise-unsound `$true` drop.
/// The variable is always implicit (the instance variable being checked).
/// `bound` is the raw numeric literal string from the source KIF (e.g. `"0"`, `"1"`).
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