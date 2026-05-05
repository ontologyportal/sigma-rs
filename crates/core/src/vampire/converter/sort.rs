// crates/core/src/vampire/converter/sort.rs
//
// TFF Sort handling.  The canonical `Sort` type lives in `crate::trans`
// (the translation layer owns sort inference).  Re-exported here so the
// converter's `super::sort::Sort` import path keeps working without a
// crate-level dependency on `crate::trans` from this submodule.

pub use crate::trans::Sort;
