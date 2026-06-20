// crates/core/src/trans/sort.rs
//
// Sub-module handling sort generation from SUMO type constraints

use crate::{SymbolId, trans::{TranslationLayer, ir::Sort as IrSort}};

impl TranslationLayer {
    /// Map a [`SymbolId`] to its most specific primitive [`Sort`].
    ///
    /// Returns `Sort::Individual` for any symbol not found in the
    /// pre-built numeric sort map.  Sentinel `u64::MAX` → `Sort::Individual`.
    pub(crate) fn sort_for_id(&self, class_id: SymbolId) -> Sort {
        if class_id == u64::MAX { return Sort::Individual; }
        self.collapse_numeric(self.numeric_sorts.get(&class_id).unwrap_or(Sort::Individual))
    }

    /// The numeric [`Sort`] a *value* of class `class_id` carries: a
    /// numeric-sorted class maps to its sort, and the abstract `Number`
    /// superclass maps to `$real` (the conservative canonical numeric —
    /// SUMO-TFA parity; without this a `(domain p 1 Number)` relation's facts
    /// and rules land on different typed variants).  `None` for everything
    /// else (`$i`).
    pub(crate) fn numeric_sort_of_class(&self, class_id: SymbolId) -> Option<Sort> {
        if let Some(s) = self.numeric_sorts.get(&class_id) {
            return Some(self.collapse_numeric(s));
        }
        (Some(class_id) == self.number_class_id()).then_some(Sort::Real)
    }

    /// The interned id of the abstract `Number` superclass, when present.
    pub(crate) fn number_class_id(&self) -> Option<SymbolId> {
        self.semantic
            .syntactic
            .sym_id(crate::trans::caches::numeric_sorts::NUMBER_CLASS)
    }

    // `sort_for_symbol` is now the `symbol_sort` cache wrapper in
    // `caches::symbol_sort` (its `generate` does the resolution above).
}

/// Classify a numeric literal's textual shape: `/` → Rational, a decimal
/// point or exponent → Real, else Integer.
///
/// The single source of truth for literal-shape typing: the lowering,
/// term-sort inference, and semantic literal-equality classification paths
/// must agree on this grammar or facts and rules land on different typed
/// variants.
pub(crate) fn numeric_literal_sort(n: &str) -> Sort {
    if n.contains('/') {
        Sort::Rational
    } else if n.contains('.') || n.contains('e') || n.contains('E') {
        Sort::Real
    } else {
        Sort::Integer
    }
}

/// The SUMO class a numeric literal's shape classifies at — the semantic
/// counterpart of [`numeric_literal_sort`], used by the literal-equality
/// evidence path in `inferred_class`.
pub(crate) fn numeric_literal_class(n: &str) -> &'static str {
    use crate::trans::caches::numeric_sorts::{INTEGER_CLASS, RATIONAL_CLASS, REAL_CLASS};
    match numeric_literal_sort(n) {
        Sort::Rational => RATIONAL_CLASS,
        Sort::Real     => REAL_CLASS,
        _              => INTEGER_CLASS,
    }
}

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
          serde::Serialize, serde::Deserialize, Hash)]
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

impl Into<IrSort> for Sort {
    fn into(self) -> IrSort {
        match self {
            Sort::Individual => IrSort::default_sort(),
            Sort::Real => IrSort::real(),
            Sort::Rational => IrSort::rational(),
            Sort::Integer => IrSort::int(),
        }
    }
}