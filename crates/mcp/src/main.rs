//! `sumo-mcp` binary entry point. Reads MCP (JSON-RPC 2.0) messages on
//! stdin, writes responses on stdout. Logs go to stderr so the transport
//! stream stays clean.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser as ClapParser;
use rmcp::ServiceExt;
use rmcp::transport::io::stdio;

use sigmakee_rs_sdk::{ProverLayer, Session, Source};

#[derive(Debug, ClapParser)]
#[command(name = "sumo-mcp", about = "MCP server for KIF/SUMO ontology authoring")]
struct Args {
    /// KIF files or directories to ingest into the KB before serving.
    /// Directories are expanded non-recursively (each `.kif` child loads).
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    let args = Args::parse();

    log::info!(target: "sumo_mcp", "sumo-mcp starting (version {})", env!("CARGO_PKG_VERSION"));

    let mut session = Session::<ProverLayer>::new("mcp".to_string());
    if !args.paths.is_empty() {
        log::info!(target: "sumo_mcp", "ingesting {} startup path(s)", args.paths.len());
        let errs = session.ingest(Source::Local(args.paths.clone()), false);
        for e in &errs {
            log::warn!(target: "sumo_mcp", "startup ingest: {e}");
        }
        if !errs.is_empty() {
            log::warn!(
                target: "sumo_mcp",
                "startup ingest finished with {} finding(s); serving anyway — use `validate_kb` to inspect",
                errs.len()
            );
        }
    }

    let server = sumo_mcp::SumoServer::new(session);
    let service = server.serve(stdio()).await.context("failed to start MCP service on stdio")?;

    log::info!(target: "sumo_mcp", "sumo-mcp ready; serving tools on stdio");
    service.waiting().await.context("MCP service loop failed")?;

    log::info!(target: "sumo_mcp", "sumo-mcp exiting cleanly");
    Ok(())
}

/// Initialize logging to stderr, honoring `SUMO_MCP_LOG` (default `warn`).
fn init_logging() {
    let level = std::env::var("SUMO_MCP_LOG").unwrap_or_else(|_| "warn".to_owned());
    env_logger::Builder::new()
        .parse_filters(&level)
        // stdout is the MCP transport.
        .target(env_logger::Target::Stderr)
        .format_timestamp_millis()
        .try_init()
        .ok();
}
