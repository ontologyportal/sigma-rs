//! `sumo-lsp` binary entry point. Reads LSP messages on stdin, writes responses
//! on stdout. Logs go to stderr so the transport stream stays clean.

use std::env;

use anyhow::Result;
use lsp_server::Connection;

fn main() -> Result<()> {
    init_logging();

    log::info!(target: "sumo_lsp", "sumo-lsp starting (version {})", env!("CARGO_PKG_VERSION"));

    let (connection, io_threads) = Connection::stdio();
    let result = sumo_lsp::server::run(connection);

    // Join the IO threads even on a handler error so the transport drains cleanly.
    io_threads.join()?;
    result?;

    log::info!(target: "sumo_lsp", "sumo-lsp exiting cleanly");
    Ok(())
}

/// Initialize logging to stderr, honoring the `SUMO_LSP_LOG` level (default `warn`).
fn init_logging() {
    let level = env::var("SUMO_LSP_LOG").unwrap_or_else(|_| "warn".to_owned());
    env_logger::Builder::new()
        .parse_filters(&level)
        // stdout is the LSP transport.
        .target(env_logger::Target::Stderr)
        .format_timestamp_millis()
        .try_init()
        .ok();
}
