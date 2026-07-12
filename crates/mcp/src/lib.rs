//! `sumo-mcp` — an MCP (Model Context Protocol) server exposing sigma-rs's
//! KIF/SUMO validation, ingestion, and theorem-proving as tools for an LLM
//! authoring SUO-KIF ontology content.
//!
//! Protocol-only, mirroring `sumo-lsp`'s posture: a thin wire-format crate
//! over `sigmakee-rs-sdk`'s `Session` API. All state lives in one long-lived,
//! mutex-guarded `Session<ProverLayer>` (the in-process native prover, no
//! external Vampire subprocess / no CMake) so repeated tool calls amortise
//! ingest cost across a conversation the way `sumo serve` does.

pub mod render;
pub mod server;

pub use server::SumoServer;
