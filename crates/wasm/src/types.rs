//! Thin serde glue for the wasm boundary.
//!
//! The actual DTO shapes live in the SDK as view types
//! (`sigmakee_rs_sdk::session::views`) -- this module only carries the
//! `serde_wasm_bindgen` conversion helper and the pure free-function
//! bindings that need no KB instance.

use wasm_bindgen::prelude::*;

/// Serialize any view struct across the wasm boundary.
pub(crate) fn to_js<T: serde::Serialize>(value: &T) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(value).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Parse a `.kif.tq` test file. Pure: no KB, no state. Throws with the
/// parse diagnostic's message on malformed input.
#[wasm_bindgen(js_name = parseTest)]
pub fn parse_test(name: &str, text: &str) -> Result<JsValue, JsValue> {
    let tc = sigmakee_rs_core::parse_test_content(text, name)
        .map_err(|d| JsValue::from_str(&d.to_string()))?;
    to_js(&sigmakee_rs_sdk::TestCaseView::from(&tc))
}
