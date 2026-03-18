// crates/sumo-kb/src/parse/mod.rs
//
// Parse submodule — extensible for multiple input formats.
// Currently only KIF is supported.

pub mod kif;

pub use kif::*;
