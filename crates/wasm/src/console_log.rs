//! Bridges the `log` crate (already used throughout `sumo-lsp` for request
//! tracing -- e.g. `handle_completion` logs when it's invoked and how long
//! it took) to the browser's `console`. Without a logger registered, `log`'s
//! macros are safe no-ops (unlike raw `eprintln!`/`println!`, which trap on
//! wasm32 -- there's no stdio there), so those calls exist but are invisible
//! in the browser until this runs.
//!
//! `init()` is idempotent (`set_boxed_logger` errors on a second call, which
//! this ignores) and installed automatically via `#[wasm_bindgen(start)]` --
//! no JS-side wiring needed, and it's registered before any other exported
//! function runs.

use wasm_bindgen::prelude::wasm_bindgen;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = log)]
    fn console_log(s: &str);
    #[wasm_bindgen(js_namespace = console, js_name = warn)]
    fn console_warn(s: &str);
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn console_error(s: &str);
}

struct ConsoleLogger;

impl log::Log for ConsoleLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        let line = format!(
            "[{}] {}: {}",
            record.level(),
            record.target(),
            record.args()
        );
        match record.level() {
            log::Level::Error => console_error(&line),
            log::Level::Warn => console_warn(&line),
            _ => console_log(&line),
        }
    }

    fn flush(&self) {}
}

#[wasm_bindgen(start)]
fn init() {
    // Ignore the error: a second wasm module instantiation on the same page
    // (or an embedder that already installed a logger) hits
    // `SetLoggerError`, which is fine -- the first registration stands.
    let _ = log::set_logger(&ConsoleLogger);
    log::set_max_level(log::LevelFilter::Debug);
}
