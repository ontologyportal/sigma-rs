# sumo-lsp — language server for KIF / SUMO

`sumo-lsp` is a standard LSP-over-stdio language server for
SUO-KIF / SUMO knowledge bases.  It is editor-agnostic: any client
that speaks the [Language Server Protocol](https://microsoft.github.io/language-server-protocol/)
can consume it.  This document covers installation, per-editor
configuration, supported capabilities, and protocol notes for
downstream extension authors.

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

## Configuration (per-session)

`sumo-lsp` does not require a configuration file.  Two
server-side knobs are settable via LSP's `initializationOptions`
or via environment variables:

| Key / env var                              | Default             | Purpose                                           |
|--------------------------------------------|---------------------|---------------------------------------------------|
| `SUMO_LSP_LOG`                             | `warn`              | `env_logger`-style level / target filter          |
| `sumo-lsp.workspace.rootFiles` (future)    | `["Merge.kif"]`     | Root-file load order for the initial sweep        |

Workspace root files are loaded in the declared order so
first-pass diagnostics are stable on large projects (e.g. loading
`Merge.kif` before `Mid-level-ontology.kif` avoids transient
"head not a declared relation" warnings).  Currently hard-coded;
a configuration surface is planned.

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

File issues on the main project repository with:

1. `sumo-lsp --version` output (the server identifies itself via
   `InitializeResult.server_info.version`).
2. Editor + LSP-client version.
3. `SUMO_LSP_LOG=debug` trace for the reproducing session.
4. A minimal `.kif` file exhibiting the problem.

For protocol-level questions (what capabilities the server
advertises, expected request shapes), this document is the
canonical reference.
