// crates/sumo-lsp/src/lib.rs
//
// Language-server implementation for KIF / SUMO.
//
// Exported primarily so integration tests and the `sumo-lsp`
// binary (`src/main.rs`) can share the same entry points.
// Editor-side packaging (VSCode, Neovim, ...) is out of scope
// for this crate -- the server speaks plain LSP on stdio and
// is consumed by any standard LSP client.

pub mod conv;
pub mod state;
pub mod server;
pub mod handlers;
