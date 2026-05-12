// crates/core/src/parse/tq/mod.rs
//
// TQ (`.kif.tq` test-query) parsing submodule.

pub mod parser;

pub use parser::{parse_test_content, parse_tq, TestCase};
