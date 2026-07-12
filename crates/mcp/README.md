# sumo-mcp

An [MCP](https://modelcontextprotocol.io) server exposing sigma-rs's KIF/SUMO
validation, ingestion, and theorem-proving as tools for an LLM authoring
SUO-KIF ontology content that is syntactically valid, semantically
well-formed, and logically consistent with an existing knowledge base.

Protocol-only, modeled on `sumo-lsp`'s posture: a thin wire-format crate over
`sigmakee-rs-sdk`'s `Session` API. All state lives in one long-lived KB
session (the in-process native prover — no external Vampire subprocess, no
CMake), so repeated tool calls amortize ingest cost across a conversation the
way `sumo serve` does.

## Running

```sh
cargo run -p sumo-mcp -- [PATH ...]
```

`PATH` arguments (files or directories) are ingested into the KB before the
server starts serving. Communicates over stdio (MCP's standard transport);
point any MCP-compatible client at the binary. Logs go to stderr — set
`SUMO_MCP_LOG=info` (or `debug`) for more detail; default is `warn`.

## Tools

| Tool                | Purpose                                                             |
|----------------------|----------------------------------------------------------------------|
| `validate`           | Check inline KIF for syntax/semantic issues, without committing it  |
| `validate_kb`        | Run full semantic validation over the whole loaded KB               |
| `ingest`              | Load a file/directory or inline KIF and commit it as axioms        |
| `ask`                 | Prove/refute a conjecture, with optional extra hypotheses          |
| `check_consistency`   | Saturate the KB looking for a contradiction                        |
| `translate`           | Standalone KIF → TPTP syntax translation                           |
| `man`                 | Look up a symbol's kind, parents, signature, and documentation     |
| `search`              | Keyword search across documentation/termFormat/format text         |
| `list_files`          | List files currently loaded in the session                         |

See `get_info()` in `src/server.rs` for the suggested authoring workflow
surfaced to the client as MCP server instructions.
