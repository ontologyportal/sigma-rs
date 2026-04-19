// crates/sumo-lsp/src/main.rs
//
// `sumo-lsp` binary entry point.  Reads LSP messages on stdin,
// writes responses on stdout.  Logs go to stderr (and optionally
// `$SUMO_LSP_LOG_FILE`) so the transport stream stays clean.

use std::env;

use anyhow::Result;
use lsp_server::Connection;

fn main() -> Result<()> {
    init_logging();

    log::info!(target: "sumo_lsp", "sumo-lsp starting (version {})", env!("CARGO_PKG_VERSION"));

    // lsp_server::Connection::stdio() spawns internal reader/writer
    // threads and hands us crossbeam channels to the message stream.
    // The returned `IoThreads` is owned by the event loop to be
    // joined on shutdown.
    let (connection, io_threads) = Connection::stdio();
    let result = sumo_lsp::server::run(connection);

    // Always attempt to join the IO threads -- even on a handler
    // error the transport should drain cleanly.
    io_threads.join()?;
    result?;

    log::info!(target: "sumo_lsp", "sumo-lsp exiting cleanly");
    Ok(())
}

fn init_logging() {
    // Default level: `warn` unless the user sets SUMO_LSP_LOG.
    let level = env::var("SUMO_LSP_LOG").unwrap_or_else(|_| "warn".to_owned());
    env_logger::Builder::new()
        .parse_filters(&level)
        // Write to stderr -- stdout is the LSP transport.
        .target(env_logger::Target::Stderr)
        .format_timestamp_millis()
        .try_init()
        .ok();
}
