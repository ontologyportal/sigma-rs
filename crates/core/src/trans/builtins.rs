//! Compile-time constants and lookup types for SUMO built-in names that map
//! to TPTP interpreted symbols or logical constants.
//!
//! Two families live here:
//!
//!   - `TPTPConstant` — SUMO symbols that denote logical truth values
//!     (`True` → `$true`, `False` → `$false`).
//!   - `SumoArith` — SUMO arithmetic predicates and functions that the TFF
//!     converter replaces with TPTP interpreted symbols
//!     (`lessThan` → `$less`, `AdditionFn` → `$sum`, …).
//!
//! SUMO name strings are compile-time constants via `env!`, sourced from
//! `.cargo/config.toml [env]`.

use crate::trans::ir::{Formula as IrF, Function as IrFn, Interp, Predicate as IrPd, Sort as IrSort};
use crate::trans::Sort as KifSort;

// -- Comparison predicates ----------------------------------------------------

/// SUMO name for the `lessThan` comparison predicate.
pub(crate) const LESS_THAN:                 &str = env!("SUMO_LESS_THAN");
/// SUMO name for the `greaterThan` comparison predicate.
pub(crate) const GREATER_THAN:              &str = env!("SUMO_GREATER_THAN");
/// SUMO name for the `lessThanOrEqualTo` comparison predicate.
pub(crate) const LESS_THAN_OR_EQUAL_TO:     &str = env!("SUMO_LESS_THAN_OR_EQUAL_TO");
/// SUMO name for the `greaterThanOrEqualTo` comparison predicate.
pub(crate) const GREATER_THAN_OR_EQUAL_TO:  &str = env!("SUMO_GREATER_THAN_OR_EQUAL_TO");

// -- Math functions -----------------------------------------------------------

/// SUMO name for the `AdditionFn` math function.
pub(crate) const ADDITION_FN:       &str = env!("SUMO_ADDITION_FN");
/// SUMO name for the `SubtractionFn` math function.
pub(crate) const SUBTRACTION_FN:    &str = env!("SUMO_SUBTRACTION_FN");
/// SUMO name for the `MultiplicationFn` math function.
pub(crate) const MULTIPLICATION_FN: &str = env!("SUMO_MULTIPLICATION_FN");
/// SUMO name for the `DivisionFn` math function.
pub(crate) const DIVISION_FN:       &str = env!("SUMO_DIVISION_FN");
/// SUMO name for the `FloorFn` math function.
pub(crate) const FLOOR_FN:          &str = env!("SUMO_FLOOR_FN");
/// SUMO name for the `CeilingFn` math function.
pub(crate) const CEILING_FN:        &str = env!("SUMO_CEILING_FN");
/// SUMO name for the `RoundFn` math function.
pub(crate) const ROUND_FN:          &str = env!("SUMO_ROUND_FN");
/// SUMO name for the `RemainderFn` math function.
pub(crate) const REMAINDER_FN:      &str = env!("SUMO_REMAINDER_FN");

// -- TPTPConstant — logical truth-value symbols -------------------------------

/// SUMO symbols that denote a logical truth value in formula position.
///
/// `True` and `False` are the only SUMO constants that are valid bare
/// formulas; all other bare symbols in formula position are rejected.
pub(crate) enum TPTPConstant {
    True,
    False,
}

impl TPTPConstant {
    /// Return `Some(TPTPConstant)` if `symbol` is a logical constant, else `None`.
    pub(crate) fn from_sym(symbol: &str) -> Option<Self> {
        match symbol {
            "True"  => Some(Self::True),
            "False" => Some(Self::False),
            _       => None,
        }
    }
}

impl From<TPTPConstant> for IrF {
    fn from(c: TPTPConstant) -> IrF {
        match c {
            TPTPConstant::True  => IrF::True,
            TPTPConstant::False => IrF::False,
        }
    }
}

// -- SumoArith — arithmetic predicates and functions --------------------------

/// SUMO arithmetic predicates and functions that map to TPTP interpreted
/// symbols in TFF mode.
///
/// # Comparison predicates → `IrPd::interpreted`
///
/// | SUMO             | TPTP          |
/// |------------------|---------------|
/// | `lessThan`             | `$less`       |
/// | `greaterThan`          | `$greater`    |
/// | `lessThanOrEqualTo`    | `$lesseq`     |
/// | `greaterThanOrEqualTo` | `$greatereq`  |
///
/// # Math functions → `IrFn::interpreted`
///
/// | SUMO               | TPTP                              |
/// |--------------------|-----------------------------------|
/// | `AdditionFn`       | `$sum`                            |
/// | `SubtractionFn`    | `$difference`                     |
/// | `MultiplicationFn` | `$product`                        |
/// | `DivisionFn`       | `$quotient` / `$quotient_e`       |
/// | `FloorFn`          | `$floor` (+ optional `$to_int`)   |
/// | `CeilingFn`        | `$ceiling`                        |
/// | `RoundFn`          | `$round`                          |
/// | `RemainderFn`      | `$remainder_t`                    |
///
/// The `sort` argument selects the right `Interp` family (Int / Rat / Real).
/// When sort is `Individual` (unknown), the Real family is used as the
/// conservative default.
pub(crate) enum SumoArith {
    // Comparison predicates
    LessThan,
    GreaterThan,
    LessThanOrEqualTo,
    GreaterThanOrEqualTo,
    // Math functions
    AdditionFn,
    SubtractionFn,
    MultiplicationFn,
    DivisionFn,
    FloorFn,
    CeilingFn,
    RoundFn,
    RemainderFn,
}

impl SumoArith {
    /// Match a SUMO relation/function name to its `SumoArith` variant.
    /// Returns `None` for names that have no TPTP interpreted counterpart.
    pub(crate) fn from_sumo_name(name: &str) -> Option<Self> {
        match name {
            n if n == LESS_THAN                => Some(Self::LessThan),
            n if n == GREATER_THAN             => Some(Self::GreaterThan),
            n if n == LESS_THAN_OR_EQUAL_TO    => Some(Self::LessThanOrEqualTo),
            n if n == GREATER_THAN_OR_EQUAL_TO => Some(Self::GreaterThanOrEqualTo),
            n if n == ADDITION_FN              => Some(Self::AdditionFn),
            n if n == SUBTRACTION_FN           => Some(Self::SubtractionFn),
            n if n == MULTIPLICATION_FN        => Some(Self::MultiplicationFn),
            n if n == DIVISION_FN              => Some(Self::DivisionFn),
            n if n == FLOOR_FN                 => Some(Self::FloorFn),
            n if n == CEILING_FN               => Some(Self::CeilingFn),
            n if n == ROUND_FN                 => Some(Self::RoundFn),
            n if n == REMAINDER_FN             => Some(Self::RemainderFn),
            _                                        => None,
        }
    }

    /// `true` for the four comparison predicates, `false` for math functions.
    pub(crate) fn is_predicate(&self) -> bool {
        matches!(self,
            Self::LessThan | Self::GreaterThan
            | Self::LessThanOrEqualTo | Self::GreaterThanOrEqualTo
        )
    }

    /// Build the typed IR predicate for this comparison, using `sort` to
    /// select the Int / Rat / Real `Interp` family.
    ///
    /// Returns `None` if called on a math function variant.
    #[allow(dead_code)]
    pub(crate) fn to_ir_pred(&self, sort: KifSort) -> Option<IrPd> {
        let interp = match (self, sort) {
            (Self::LessThan,            KifSort::Integer)  => Interp::IntLess,
            (Self::LessThan,            KifSort::Rational) => Interp::RatLess,
            (Self::LessThan,            _)                 => Interp::RealLess,

            (Self::GreaterThan,         KifSort::Integer)  => Interp::IntGreater,
            (Self::GreaterThan,         KifSort::Rational) => Interp::RatGreater,
            (Self::GreaterThan,         _)                 => Interp::RealGreater,

            (Self::LessThanOrEqualTo,   KifSort::Integer)  => Interp::IntLessEqual,
            (Self::LessThanOrEqualTo,   KifSort::Rational) => Interp::RatLessEqual,
            (Self::LessThanOrEqualTo,   _)                 => Interp::RealLessEqual,

            (Self::GreaterThanOrEqualTo, KifSort::Integer)  => Interp::IntGreaterEqual,
            (Self::GreaterThanOrEqualTo, KifSort::Rational) => Interp::RatGreaterEqual,
            (Self::GreaterThanOrEqualTo, _)                 => Interp::RealGreaterEqual,

            _ => return None, // math function variant
        };
        Some(IrPd::interpreted(interp.tptp_name(), interp))
    }

    /// Build the IR function for this math operation, using `sort` to select
    /// the Int / Rat / Real `Interp` family.
    ///
    /// `DivisionFn` with `Integer` sort emits `$quotient_e` (Euclidean
    /// integer division); all other sorts emit `$quotient`.
    ///
    /// `RemainderFn` always uses the integer interpretation (`$remainder_t`)
    /// regardless of sort — TPTP defines remainder only over integers.
    ///
    /// Returns `None` if called on a comparison predicate variant.
    pub(crate) fn to_ir_fn(&self, sort: KifSort) -> Option<IrFn> {
        let interp = match (self, sort) {
            (Self::AdditionFn,       KifSort::Integer)  => Interp::IntPlus,
            (Self::AdditionFn,       KifSort::Rational) => Interp::RatPlus,
            (Self::AdditionFn,       _)                 => Interp::RealPlus,

            (Self::SubtractionFn,    KifSort::Integer)  => Interp::IntMinus,
            (Self::SubtractionFn,    KifSort::Rational) => Interp::RatMinus,
            (Self::SubtractionFn,    _)                 => Interp::RealMinus,

            (Self::MultiplicationFn, KifSort::Integer)  => Interp::IntMultiply,
            (Self::MultiplicationFn, KifSort::Rational) => Interp::RatMultiply,
            (Self::MultiplicationFn, _)                 => Interp::RealMultiply,

            (Self::DivisionFn,       KifSort::Integer)  => Interp::IntQuotientE,
            (Self::DivisionFn,       KifSort::Rational) => Interp::RatQuotient,
            (Self::DivisionFn,       _)                 => Interp::RealQuotient,

            (Self::FloorFn,   KifSort::Integer)  => Interp::IntFloor,
            (Self::FloorFn,   KifSort::Rational) => Interp::RatFloor,
            (Self::FloorFn,   _)                 => Interp::RealFloor,
            (Self::CeilingFn, KifSort::Integer)  => Interp::IntCeiling,
            (Self::CeilingFn, KifSort::Rational) => Interp::RatCeiling,
            (Self::CeilingFn, _)                 => Interp::RealCeiling,
            (Self::RoundFn,   KifSort::Integer)  => Interp::IntRound,
            (Self::RoundFn,   KifSort::Rational) => Interp::RatRound,
            (Self::RoundFn,   _)                 => Interp::RealRound,

            (Self::RemainderFn, _) => Interp::IntRemainderT,

            _ => return None, // comparison predicate variant
        };
        Some(IrFn::interpreted(interp.tptp_name(), interp))
    }

    /// The sort of the *result* produced by this operation, given the input
    /// sort.  Used to determine what sort annotation to place on the variable
    /// that receives the result.
    #[allow(dead_code)]
    pub(crate) fn result_sort(&self, arg_sort: KifSort) -> KifSort {
        match self {
            Self::FloorFn | Self::CeilingFn | Self::RoundFn => KifSort::Integer,
            Self::RemainderFn => KifSort::Integer,
            Self::DivisionFn if arg_sort == KifSort::Integer => KifSort::Integer,
            _ => arg_sort,
        }
    }

    /// The IR sort (`$int` / `$rat` / `$real` / `$i`) corresponding to this
    /// operation's result, given the input sort.
    #[allow(dead_code)]
    pub(crate) fn result_ir_sort(&self, arg_sort: KifSort) -> IrSort {
        self.result_sort(arg_sort).into()
    }
}

// -- Named SUMO numeric constants ---------------------------------------------

/// Return the TPTP numeric literal value for a SUMO named constant, if known.
///
/// These are SUMO ontology symbols that denote well-known numeric values and
/// should be emitted as raw TPTP numeric literals in TFF mode rather than as
/// symbolic constants (`s__Pi`).  Only called in TFF mode; FOF keeps the
/// symbolic form.
///
/// | SUMO symbol    | Value                  | SUMO class               |
/// |----------------|------------------------|--------------------------|
/// | `Pi`           | `3.141592653589793`    | `PositiveRealNumber`     |
/// | `NaperianBase` | `2.718281828459045`    | `PositiveRealNumber`     |
pub(crate) fn numeric_constant_value(name: &str) -> Option<&'static str> {
    match name {
        "Pi"           => Some("3.141592653589793"),
        "NaperianBase" => Some("2.718281828459045"),
        _              => None,
    }
}

#[cfg(test)]
mod numeric_const_tests {
    use super::numeric_constant_value;

    #[test]
    fn pi_has_value() {
        assert_eq!(numeric_constant_value("Pi"), Some("3.141592653589793"));
    }

    #[test]
    fn naperian_base_has_value() {
        assert_eq!(numeric_constant_value("NaperianBase"), Some("2.718281828459045"));
    }

    #[test]
    fn unknown_symbol_returns_none() {
        assert_eq!(numeric_constant_value("Dog"),   None);
        assert_eq!(numeric_constant_value("Human"), None);
        assert_eq!(numeric_constant_value(""),      None);
    }
}
