# sumo-lsp — language server for KIF / SUMO

`sumo-lsp` is a standard LSP-over-stdio language server for
SUO-KIF / SUMO knowledge bases.  It is editor-agnostic: any client
that speaks the [Language Server Protocol](https://microsoft.github.io/language-server-protocol/)
can consume it.  This document covers installation, per-editor
configuration, supported capabilities, custom `sumo/*` protocol
extensions, and protocol notes for downstream extension authors.

> **Looking for programmatic query access, not an LSP?**  The
> `sumo serve` command exposes a JSON-RPC kernel with
> `tell` / `ask` / `debug` / `test` methods over stdio — purpose-
> built for editor extensions and long-running tool chains that
> need to query a loaded knowledge base without paying the load
> cost per call.  See [`INTEGRATION.md`](INTEGRATION.md) for the
> wire format and a VSCode extension sketch.  `sumo-lsp` and
> `sumo serve` are complementary: the LSP handles editing-surface
> concerns (diagnostics, hover, rename), the kernel handles
> proving-surface concerns (theorem proving, consistency checks).

## Installation

### From GitHub releases (recommended)

Each tagged release under `sumo-lsp-vX.Y.Z` attaches pre-built,
stripped binaries for four target triples:

| Target           | Archive                              | Hash         |
|------------------|--------------------------------------|--------------|
| `darwin-arm64`   | `sumo-lsp-vX.Y.Z-darwin-arm64.tar.gz`  | `.sha256`    |
| `darwin-x64`     | `sumo-lsp-vX.Y.Z-darwin-x64.tar.gz`    | `.sha256`    |
| `linux-x64-gnu`  | `sumo-lsp-vX.Y.Z-linux-x64-gnu.tar.gz` | `.sha256`    |
| `win32-x64`      | `sumo-lsp-vX.Y.Z-win32-x64.zip`        | `.sha256`    |

Download, verify the hash, extract, and drop `sumo-lsp` on your
`$PATH`:

```bash
curl -L -O https://github.com/.../sumo-lsp-v0.1.0-darwin-arm64.tar.gz
curl -L -O https://github.com/.../sumo-lsp-v0.1.0-darwin-arm64.tar.gz.sha256
shasum -a 256 -c sumo-lsp-v0.1.0-darwin-arm64.tar.gz.sha256
tar -xzf sumo-lsp-v0.1.0-darwin-arm64.tar.gz
install sumo-lsp-v0.1.0-darwin-arm64/sumo-lsp ~/.local/bin/
```

### From source

```bash
git clone --recursive <repo>
cd sumo-parser
cargo install --path crates/sumo-lsp
# binary ends up at ~/.cargo/bin/sumo-lsp
```

`sumo-lsp` does not depend on CMake or the Vampire C++ library --
the binary is parse + validate + manpage only, so it builds
cleanly without the `vampire-sys` submodule.

## Running & logging

The server reads JSON-RPC over stdin and writes over stdout.  Logs
go to stderr so they don't interfere with the transport:

```bash
SUMO_LSP_LOG=info sumo-lsp 2>/tmp/sumo-lsp.log
```

`SUMO_LSP_LOG` accepts `env_logger` filter syntax:

| Value             | Meaning                                              |
|-------------------|------------------------------------------------------|
| `error`           | Only errors (default)                                |
| `warn`            | Errors + warnings                                    |
| `info`            | Lifecycle events (initialize, workspace sweep, …)    |
| `debug`           | Per-handler traces + cross-file fallback diagnostics |
| `trace`           | Token-level detail (noisy)                           |
| `sumo_lsp=debug,sumo_kb=warn` | Per-target filter                         |

## Editor setup

### Neovim (via `nvim-lspconfig` 0.1.8+)

```lua
local configs = require('lspconfig.configs')
if not configs.sumo_lsp then
  configs.sumo_lsp = {
    default_config = {
      cmd = { 'sumo-lsp' },
      filetypes = { 'kif' },
      root_dir = require('lspconfig.util').root_pattern('.git', '*.kif'),
      settings = {},
    },
  }
end
require('lspconfig').sumo_lsp.setup({})
```

Then register the filetype for `.kif` / `.kif.tq`:

```lua
vim.filetype.add({ extension = { kif = 'kif', ['kif.tq'] = 'kif' } })
```

### Helix (`languages.toml`)

```toml
[[language]]
name      = "kif"
scope     = "source.kif"
file-types = ["kif", "kif.tq"]
roots     = [".git"]
language-servers = ["sumo-lsp"]

[language-server.sumo-lsp]
command = "sumo-lsp"
```

### Emacs (`lsp-mode`)

```elisp
(with-eval-after-load 'lsp-mode
  (add-to-list 'lsp-language-id-configuration '(kif-mode . "kif"))
  (lsp-register-client
   (make-lsp-client
    :new-connection (lsp-stdio-connection "sumo-lsp")
    :major-modes '(kif-mode)
    :server-id 'sumo-lsp)))
```

### Zed (extension manifest fragment)

```toml
# sumo-lsp.toml
[[language_servers]]
name    = "sumo-lsp"
command = "sumo-lsp"
languages = ["KIF"]
```

### VSCode

The `sumo-lsp` binary speaks standard LSP; a VSCode extension
wiring it up through `vscode-languageclient` is the normal
distribution path.  A reference extension is not maintained in
this repository -- it is tracked as follow-up work in the
editor-integration ecosystem.

## Supported capabilities

| LSP method                            | Status | Notes                                                   |
|---------------------------------------|--------|---------------------------------------------------------|
| `initialize` / `initialized`          | ✅     | Walks `workspaceFolders` for `*.kif` / `*.kif.tq`       |
| `shutdown` / `exit`                   | ✅     |                                                         |
| `textDocument/didOpen`                | ✅     | Full-sync                                               |
| `textDocument/didChange`              | ✅     | Full-sync; incremental diff via sentence fingerprints   |
| `textDocument/didClose`               | ✅     | KB retains the file's sentences for cross-file refs     |
| `textDocument/publishDiagnostics`     | ✅     | Parse + semantic, precise ranges                        |
| `textDocument/hover`                  | ✅     | Renders `documentation` / `termFormat` / `format` relations as Markdown |
| `textDocument/definition`             | ✅     | First-declaration heuristic (`subclass` / `instance` / `documentation`) |
| `textDocument/references`             | ✅     | Respects `context.includeDeclaration`                   |
| `textDocument/rename`                 | ✅     | Global symbol rename + scoped variable rename (sigil preserved) |
| `textDocument/documentSymbol`         | ✅     | Flat outline: one entry per root sentence               |
| `workspace/symbol`                    | ✅     | Case-insensitive substring match; Skolem symbols hidden |
| `textDocument/semanticTokens/full`    | ✅     | 6-token legend: keyword / type / function / variable / string / number |
| `textDocument/formatting`             | ✅     | Whole-document via `AstNode::format_plain`              |
| `textDocument/rangeFormatting`        | ✅     | Intersecting root sentences                             |
| `textDocument/completion`             | ✅     | Context-aware: sentence head / arg-position / free      |
| `textDocument/prepareRename`          | ❌     | `prepareProvider = false`; clients should still submit rename requests |
| `textDocument/codeAction`             | ❌     | Future work                                             |
| Semantic tokens `range` variant       | ❌     | `full` variant only for MVP                             |
| `completionItem/resolve`              | ❌     | Items carry documentation inline                        |

## Custom protocol extensions (`sumo/*`)

`sumo-lsp` ships three SUMO-specific extensions on top of
standard LSP.  They're namespaced under `sumo/` and are safe to
ignore — clients that don't understand them get standard-LSP
behaviour throughout.

| Method                          | Kind         | Direction     | Purpose |
|---------------------------------|--------------|---------------|---------|
| `sumo/setActiveFiles`           | Notification | Client→Server | Replace the server's KB file population to match a client-managed active set |
| `sumo/setIgnoredDiagnostics`    | Notification | Client→Server | Suppress specific diagnostic codes from every `publishDiagnostics` |
| `sumo/taxonomy`                 | Request      | Client→Server | Fetch the upward taxonomy graph + documentation for a single symbol |

### `sumo/setActiveFiles`

Clients that own the "which files make up the active KB"
decision (e.g. a VSCode extension reading SigmaKEE's
`config.xml`) hand the server the authoritative set.  The
server diffs against its currently-loaded population, loads
any missing files from disk, and removes any files the client
no longer wants.

```jsonc
{
  "jsonrpc": "2.0",
  "method": "sumo/setActiveFiles",
  "params": {
    // Absolute canonical filesystem paths.  Files currently
    // loaded but not in this list are removed; files in this
    // list but not loaded are read from disk and ingested.
    "files": ["/Users/.../sumo/Merge.kif", "/Users/.../sumo/Economy.kif"]
  }
}
```

After handling a `sumo/setActiveFiles`, the server republishes
diagnostics for every affected file (added + removed).  Clients
should treat the notification as equivalent to sending N
`didOpen`s and `didClose`s — the server does the bookkeeping.

**Performance note.**  `remove_file` is O(total occurrences in
the KB) per call, so large unload batches would be quadratic.
When the server detects "more files removed than kept," it
throws the KB away and rebuilds from just the requested files —
cheaper than removing each individually.  Clients don't need to
know about the rebuild path; it's a transparent optimisation.

### `sumo/setIgnoredDiagnostics`

Silences selected semantic-diagnostic codes server-side so the
next `publishDiagnostics` round drops them before the client
ever sees them.

```jsonc
{
  "jsonrpc": "2.0",
  "method": "sumo/setIgnoredDiagnostics",
  "params": {
    // Either a `SemanticError::code()` (e.g. "E005") or a
    // `SemanticError::name()` (e.g. "arity-mismatch").  Both
    // forms are accepted; unknown entries are silently kept
    // in the filter set so a client typo doesn't crash the
    // server.
    "codes": ["W011", "unused-variable"]
  }
}
```

A fresh notification **replaces** the server-side set entirely;
send `{ "codes": [] }` to clear all filters.  Parse errors are
never filterable — they remain visible regardless of the
ignore list.  After handling, the client should trigger a
re-publish (e.g. by sending a no-op `didChange`) if immediate
UI refresh is desired.

### `sumo/taxonomy`

Fetches the upward taxonomy graph from one symbol, plus that
symbol's documentation entries.  Typical use: the client
renders the response as a Mermaid graph in a webview.

Request:

```jsonc
{
  "jsonrpc": "2.0",
  "id": 42,
  "method": "sumo/taxonomy",
  "params": { "symbol": "Human" }
}
```

Response:

```jsonc
{
  "jsonrpc": "2.0",
  "id": 42,
  "result": {
    "symbol": "Human",
    "unknown": false,                         // true when the symbol isn't in the KB
    "documentation": [
      { "language": "EnglishLanguage", "text": "Modern man …" }
    ],
    "edges": [
      // BFS upward from the root; `from` is the child, `to` is the parent.
      { "from": "Human",   "to": "Hominid",    "relation": "subclass" },
      { "from": "Hominid", "to": "Primate",    "relation": "subclass" }
    ]
  }
}
```

Traversal is upward-only (child → parent) and breadth-first
with an internal node cap to avoid unbounded walks in
pathological ontologies.  When `unknown: true`, `documentation`
and `edges` are empty and the client should render an
informational panel, not a graph.

### `initializationOptions.clientManagesFiles`

Opt out of the server's initial workspace sweep.  Set on the
LSP `initialize` request:

```jsonc
{
  "jsonrpc": "2.0",
  "id": 0,
  "method": "initialize",
  "params": {
    "initializationOptions": { "clientManagesFiles": true },
    ...
  }
}
```

With this set, the server skips recursively loading every
`*.kif` / `*.kif.tq` under `workspaceFolders` at startup.  The
client is expected to follow up with `sumo/setActiveFiles` once
it has decided the file set.  Headless clients that never
intend to send `setActiveFiles` should leave this unset or
`false` and accept the default workspace sweep.

Rationale: on large workspaces the default sweep loads every
file, and a subsequent `setActiveFiles` that wants only a
subset pays the quadratic `remove_file` cost on each
leftover.  `clientManagesFiles: true` skips the load entirely,
so the `setActiveFiles` path is a clean add-only operation.

## Configuration (per-session)

`sumo-lsp` does not require a configuration file.  Two
server-side knobs are settable:

| Key                                           | Settable via                         | Default      | Purpose                                          |
|-----------------------------------------------|--------------------------------------|--------------|--------------------------------------------------|
| `SUMO_LSP_LOG`                                | env var                              | `warn`       | `env_logger`-style level / target filter         |
| `clientManagesFiles`                          | `initializationOptions`              | `false`      | Opt out of the workspace-folder sweep at boot (see [Custom protocol extensions](#custom-protocol-extensions-sumo)) |
| `sumo-lsp.workspace.rootFiles` *(future)*     | `initializationOptions` *(unwired)*  | n/a          | Root-file load order for the initial sweep       |

When the default workspace sweep is active, files are loaded
in `WalkDir` iteration order.  Client-driven orderings (via
`setActiveFiles`) should send files in the order the client
wants them loaded; the server preserves input order.

## Protocol notes for editor-extension authors

### Position encoding

The server advertises `PositionEncodingKind::UTF16` in its
initialize response.  Clients are free to negotiate UTF-8 if they
support it.  Internally every span is a byte offset; conversion
to/from UTF-16 positions is done via [`ropey::Rope`](https://docs.rs/ropey)
at the handler boundary.

### Text document sync

`TextDocumentSyncKind::FULL` is advertised.  Incremental
sentence-level diffs are computed **internally** from the new
text — the LSP interface sees full-sync edits from the client,
and `sumo-lsp` computes a `FileDiff` from fingerprints before
calling `KnowledgeBase::apply_file_diff`.  Unchanged sentences
retain their `SentenceId` across edits so downstream caches stay
warm.

### Workspace auto-indexing

On `initialize`, every `*.kif` and `*.kif.tq` file in every
`workspaceFolders` entry is recursively loaded into a single
shared `KnowledgeBase`.  There is no SUMO-native dependency
declaration (no `#include`, no manifest) -- every file in the
workspace is assumed to be part of the same logical KB.  Symbol
resolution, references, and goto-definition all work across
workspace files automatically.

Files opened outside any workspace root (e.g. standalone
`sumo-lsp` invocations with no `workspaceFolders`) are loaded
per-document; cross-file resolution is unavailable.

**Opt-out.**  Clients that manage KB membership themselves
(e.g. via `sumo/setActiveFiles`) should set
`initializationOptions.clientManagesFiles = true` to skip the
auto-sweep.  See [Custom protocol extensions](#custom-protocol-extensions-sumo).

### Diagnostics model

Each `publishDiagnostics` notification carries:

- `range` — byte-accurate span from the sumo-kb parser.
- `severity` — `Error`, `Warning`, `Information`, or `Hint`.
- `code` — stable string (`parse/unterminated-string`, `E005`,
  `W011`, `tell/duplicate-axiom`, …) suitable for filter rules.
- `source` — always `"sumo-lsp"`.
- `related_information` — populated for cross-sentence semantic
  issues (e.g. `DisjointInstance` points at both involved
  sentences).

### Cross-file ranges

Handlers that can return locations outside the active document
(references, rename, goto, workspace symbols) resolve ranges
against either the open-document text or, on cache miss, a
best-effort disk read.  Disk failures are logged at `debug` level
and produce zero-ranged locations — clients should treat these
as warnings and let the user retry once the file is open.

### Semantic tokens legend

Fixed legend advertised on `initialize`:

| Index | Type      |
|-------|-----------|
| 0     | `keyword` |
| 1     | `type`    |
| 2     | `function`|
| 3     | `variable`|
| 4     | `string`  |
| 5     | `number`  |

No token modifiers are emitted.  Parens (`(` / `)`) produce no
tokens; clients apply their own bracket-matching.

### Rename semantics

The server distinguishes global symbol rename from scoped
variable rename by the kind of element under the cursor.
Variable ids are scope-qualified (`X__<scope-id>` internally), so
a rename on `?X` inside one `(forall (?X) …)` body does not
touch `?X` in another.  The `new_name` the user types is
transformed: for variables, the leading `?` / `@` sigil is
preserved if the user omitted it; for symbols, the name is
replaced verbatim.

The server does not advertise `prepareProvider` because the
rename-target heuristic (cursor on any `Element::Symbol` or
`Element::Variable`) is cheap enough that clients can just send
the `Rename` request directly and act on the returned
`WorkspaceEdit`.

### Completion triggers

Trigger characters: `(`, ` `, `?`, `@`.  The full context is
computed from the token stream before the cursor:

- `(<cursor>` → sentence-head position; operators + head names.
- `(head args… <cursor>` → argument position; filtered by
  declared `domain` class when available.
- Top-level / whitespace → empty response.

Clients that invoke completion via `Ctrl-Space` get the same
response as the auto-triggered form.

## Reporting issues

`sumo-lsp` is a pure LSP transport — it doesn't parse command-
line arguments, so there's no `--version` flag.  The version is
available through:

- **The server-info field of the `initialize` response**:
  `InitializeResult.server_info.version` is set from
  `CARGO_PKG_VERSION` at build time.  Any LSP client that stores
  the init response (most do) can surface this.
- **The first stderr line**: when started with any `SUMO_LSP_LOG`
  level at least `info`, the server logs
  `sumo-lsp starting (version X.Y.Z)` before reading any request.
- **The built binary's ELF / Mach-O metadata**: `strings
  $(which sumo-lsp) | grep CARGO_PKG_VERSION` as a last resort.

File issues on the main project repository with:

1. The version (any of the three sources above).
2. Editor + LSP-client version.
3. `SUMO_LSP_LOG=debug` trace for the reproducing session.
4. A minimal `.kif` file exhibiting the problem.
5. Whether the issue is on a standard LSP method or a
   `sumo/*` extension — the two are implemented in different
   subtrees and usually fail for different reasons.

For protocol-level questions (what capabilities the server
advertises, expected request shapes, `sumo/*` extension
semantics), this document is the canonical reference.
