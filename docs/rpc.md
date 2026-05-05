# Integrating with `sumo serve` — the JSON-RPC kernel

`sumo serve` is a persistent daemon that exposes the sigmakee-rs-core
primitives — `tell`, `ask`, file reconciliation, consistency
checking, test running, TPTP export — over a **line-delimited
JSON-RPC wire format on stdio**. It's designed for editor
integrations and long-running tool chains that need to query a
loaded knowledge base many times without paying the load cost
on every invocation.

This guide walks through how to spawn the kernel, the message
format, every supported method, the lifecycle model, and a
complete TypeScript sketch for a VSCode extension that drives it.

---

## Why a kernel (vs. one-shot `sumo ask`)

One-shot mode works like this:

```bash
sumo ask -f Merge.kif "(subclass Human Animal)"    # 8 seconds
sumo ask -f Merge.kif "(subclass Dog Animal)"      # 8 seconds (reloads KB)
```

Every invocation reloads ~24 000 axioms. For an editor that
wants to check dozens of queries per session, this is
unworkable.

Kernel mode:

```bash
sumo serve -f Merge.kif &                      # 8 seconds, once
# … kernel lives, holds KB in memory …
# Editor sends { method: "ask", params: … } → reply in ~100 ms
# Editor sends another ask → reply in ~100 ms
# Editor sends "shutdown" when the window closes
```

The kernel holds one `KnowledgeBase` instance for its entire
lifetime. Reconciles are incremental (sentence-level diffs
against the LMDB store). Queries amortise to prover-only cost.

---

## Spawning the kernel

### Command line

```bash
sumo serve [OPTIONS]
```

| Flag | Purpose |
|---|---|
| `-f <FILE>` | KIF file to load into the KB at boot. Repeatable. |
| `-d <DIR>`  | Directory whose `*.kif` files are all loaded. Repeatable. |
| `--db <DIR>` | LMDB store location. Default: `./sumo.lmdb`. |
| `--no-db`   | In-memory KB only; nothing persists across restarts. |
| `--vampire <PATH>` | Path to the Vampire prover binary. Default: look up `vampire` on `$PATH`. |
| `-c`        | SigmaKEE config.xml mode — pulls `-f` list from config. |
| `--kb <NAME>` | (with `-c`) which KB section of config.xml to load. |

On boot the kernel opens the LMDB store (creating if absent),
reconciles every `-f`/`-d` file against it (incremental — no-op
when nothing changed on disk), and starts reading requests
from stdin.

### Streams

- **stdin**  — request stream. One JSON object per line,
  terminated by `\n`. No framing headers, no `Content-Length`.
- **stdout** — response stream. Same format. Each response
  line corresponds to a specific request `id`.
- **stderr** — human-readable log output. Nothing
  protocol-critical flows here — safe to pipe to a file or
  display as-is in a developer pane.

Set log verbosity with `RUST_LOG` (standard `env_logger`
syntax) or the CLI's `-v` / `-vv` / `-vvv` flags. Example:

```bash
RUST_LOG=info sumo serve -f Merge.kif
```

### Exit conditions

The kernel exits cleanly when:

- It receives a `shutdown` request (replies once, then exits 0).
- stdin closes (EOF — the editor disconnected).

It exits with non-zero status only on boot failure (can't open
LMDB, malformed `-c` config, etc.). Mid-conversation failures
— malformed requests, prover errors, file-read errors — are
always reported as JSON-RPC errors to the client, not via
process exit.

---

## Wire format

Each message is a single JSON object on its own line.
Structure loosely mirrors JSON-RPC 2.0 but we don't require
the `jsonrpc: "2.0"` preamble and don't implement the full
spec (no batching, no streaming notifications from server to
client).

### Request

```json
{"id": 1, "method": "ask", "params": {"query": "(subclass Dog Animal)"}}
```

| Field | Required | Notes |
|---|---|---|
| `id` | recommended | Any JSON value; echoed back on the response so the client can correlate. Omit/set to `null` to send a **notification** — the kernel acts on it but sends no reply. |
| `method` | yes | String; see the method reference below. |
| `params` | method-dependent | JSON object; shape depends on `method`. Missing `params` is equivalent to `{}`. |

### Response (success)

```json
{"id": 1, "result": {"status": "Proved", "bindings": [], "proofKif": [], "raw": "..."}}
```

### Response (error)

```json
{"id": 1, "error": {"code": -32602, "message": "invalid ask params: missing field `query`"}}
```

### Error codes

| Code | Meaning | When |
|---|---|---|
| `-32700` | Parse error | Line wasn't valid JSON. `id` is always `null`. |
| `-32601` | Method not found | Unknown `method` string. |
| `-32602` | Invalid params | `params` didn't match the method's schema, OR a domain-level validation failed (e.g. `thoroughness` out of range). |
| `-32603` | Internal error | Prover failure, I/O failure, LMDB commit failure. Message describes what went wrong. |

### Notifications

Omit `id` (or set it to `null`) and the kernel processes the
request without sending a reply. Useful for fire-and-forget
operations like `shutdown`:

```json
{"method": "shutdown"}
```

---

## Method reference

### `tell`

Assert KIF into a named session. Session assertions are
ephemeral — they're included as hypotheses by subsequent
`ask` calls on the same session, but aren't written to the
persistent LMDB store.

**Params**

```ts
{
  session?: string;   // default: "default"
  kif:      string;   // KIF source to ingest
}
```

**Result**

```ts
{
  ok:       boolean;
  errors:   string[];   // parse + hard semantic errors
  warnings: string[];   // soft semantic warnings
}
```

**Example**

```json
→ {"id": 1, "method": "tell", "params": {"kif": "(instance Socrates Human)"}}
← {"id": 1, "result": {"ok": true, "errors": [], "warnings": []}}
```

---

### `ask`

Run a conjecture through the Vampire prover against the
loaded KB (SInE-filtered, subprocess backend, FOF). Session
assertions from prior `tell` calls on the same session are
included as hypotheses.

**Params**

```ts
{
  session?:     string;   // default: "default"
  query:        string;   // single KIF conjecture
  timeout_secs?: number;  // default: 30
}
```

**Result**

```ts
{
  status:    "Proved" | "Disproved" | "Consistent"
           | "Inconsistent" | "Timeout" | "Unknown";
  bindings:  string[];        // variable-binding inferences
  proofKif:  string[];        // proof step formulas (flat KIF)
  raw:       string;          // Vampire transcript
}
```

**Example**

```json
→ {"id": 2, "method": "ask",
   "params": {"query": "(subclass Dog Animal)", "timeout_secs": 10}}
← {"id": 2, "result": {"status": "Proved", "bindings": [], "proofKif": [...], "raw": "..."}}
```

---

### `debug`

Consistency-check a single loaded file against the rest of
the KB via SInE + Vampire. Same behaviour as `sumo debug`
from the CLI.

**Params**

```ts
{
  file:         string;         // path or basename of a loaded file
  thoroughness?: number;        // (0.0, 1.0]; default 1.0
  scope?:       number | null;  // SInE tolerance; default = crate default (~2.0)
  timeoutSecs?: number;         // default 60
}
```

The `file` field is resolved against loaded KB tags using
three-tier matching: exact string → canonicalised absolute
path → basename suffix. So you can pass `"Economy.kif"` even
if the KB loaded it as `"/Users/.../sumo/Economy.kif"`.

**Result**

```ts
{
  file:           string;    // resolved tag (may differ from request.file)
  rootSentences:  number;
  sampled:        number;
  sineExpanded:   number;
  totalChecked:   number;
  tolerance:      number;
  filesPulled:    string[];  // other files SInE pulled axioms from
  status:         "Consistent" | "Inconsistent" | "Timeout" | "Unknown"
                  | "Proved (unexpected)" | "Disproved (unexpected)";
  contradictions: ContradictionEntry[];
  proofKif:       ProofStepEntry[];
  raw:            string;
}

interface ContradictionEntry {
  sid:  number;
  file: string;
  line: number;
  kif:  string;
}

interface ProofStepEntry {
  index:     number;
  rule:      string;       // "axiom" | "cnf_transformation" | "resolution" | ...
  premises:  number[];     // indices into this list
  formula:   string;
  sourceSid?:  number;     // populated for axiom-role steps
  sourceFile?: string;
  sourceLine?: number;
}
```

`contradictions` is only populated when `status === "Inconsistent"`
AND Vampire emitted a traceable refutation. Empty otherwise.

---

### `test`

Run `.kif.tq` test files against the loaded KB and report
pass/fail per file. Each test gets its own ephemeral session
so axioms don't bleed across tests.

**Params**

```ts
{
  paths:        string[];                // .kif.tq files or directories
  timeoutSecs?: number;                  // per-test override
  backend?:     "subprocess" | "embedded";  // default "subprocess"
  lang?:        "fof" | "tff";           // default "fof"
}
```

**Result**

```ts
{
  total:   number;
  passed:  number;
  failed:  number;
  results: TestCaseResult[];
}

interface TestCaseResult {
  file:             string;
  note:             string;                // (note "…") from the test file
  outcome:          "Passed" | "Failed" | "Incomplete" | "Error";
  expectedProof:    boolean;
  actualProved:     boolean;
  expectedAnswers?: string[];
  foundAnswers:     string[];
  missingAnswers:   string[];
  error?:           string;
}
```

Outcomes:

- **Passed** — verdict met AND all expected answers found.
- **Incomplete** — verdict met BUT at least one expected answer missing.
- **Failed** — verdict mismatch.
- **Error** — parse/load/prover error; `error` populated.

One broken test doesn't stop the batch; every file produces a
result entry.

---

### `kb.reconcileFile`

Sync one file from disk (or an inline text buffer) into the
KB via sentence-level diff. Handles the save-triggered flow
used by the VSCode extension.

**Params**

```ts
{
  path:  string;          // file tag (also the disk path when `text` omitted)
  text?: string | null;   // inline text; when omitted, kernel reads from `path`
}
```

**Result**

```ts
{
  path:           string;
  added:          number;
  removed:        number;
  retained:       number;
  revalidated:    number;
  parseErrors:    string[];
  semanticErrors: string[];
  persisted:      boolean;  // true if the delta was committed to LMDB
}
```

Parse errors abort the commit for this file (delta discarded,
`persisted: false`). Other files remain loadable.

---

### `kb.removeFile`

Drop one file from the KB (memory + LMDB).

**Params**

```ts
{ path: string }
```

**Result**

```ts
{
  removed:   number;    // sentences that were dropped (0 if file wasn't loaded)
  persisted: boolean;
}
```

---

### `kb.flush`

Wipe every loaded file from the KB.

**Params**: none (or `{}`).

**Result**

```ts
{
  filesRemoved:     number;
  sentencesRemoved: number;
  persisted:        boolean;
}
```

---

### `kb.listFiles`

Enumerate every file currently loaded, with root-sentence
counts.

**Params**: none (or `{}`).

**Result**

```ts
{
  files: Array<{ path: string; sentenceCount: number }>;
}
```

---

### `kb.generateTptp`

Emit the loaded KB as TPTP text. Useful for shipping a
self-contained problem file to an external prover or for
debugging how sigmakee-rs-core translates to FOL.

**Params**

```ts
{
  lang?:    "fof" | "tff";  // default "fof"; unknown → "fof" with a warning
  session?: string | null;  // include a session's assertions as hypotheses
}
```

**Result**

```ts
{
  tptp:         string;  // full document (no trailing newline stripped)
  formulaCount: number;  // coarse count of emitted `fof(`/`tff(`/`cnf(` lines
  lang:         string;  // echo of resolved dialect
}
```

---

### `shutdown`

Graceful exit. Kernel replies once (if the request has an
`id`), flushes stdout, and exits 0.

**Params**: none.

**Result**: `null`.

```json
→ {"id": 99, "method": "shutdown"}
← {"id": 99, "result": null}
# … kernel exits …
```

---

## Concurrency model

**The kernel is strictly serial.** One request in, one response
out, in order. There is no concurrency, no cancellation,
no progress notifications.

Practical consequences:

- **Don't pipeline asks.** If you send request 1 and request 2
  back-to-back, the kernel processes them in order. Request 2
  waits for request 1 to complete.
- **Long-running requests block the kernel.** An `ask` with a
  30-second Vampire timeout really does block the kernel for
  up to 30 seconds; subsequent requests queue up.
- **Editor extensions should not block the UI thread.** Send
  the request from a worker / background task and update the
  UI when the response arrives.
- **There's no way to cancel an in-flight request.** If you
  want cancellation, the pragmatic workaround is to kill the
  subprocess and respawn it. The LMDB state persists across
  restarts so this is cheap.

Future versions may add an async mode with `$/cancelRequest`
semantics (mirroring LSP). Not there today.

---

## Lifecycle model

### Boot

On startup the kernel:

1. Opens (or creates) the LMDB store at `--db`.
2. Reads every `-f` / `-d` file from disk.
3. Reconciles each one against the LMDB store (incremental;
   no-op when the file is unchanged since the last reconcile).
4. Any file with parse errors is skipped — kernel continues
   with the rest.
5. Once the sweep is done, it prints `kernel ready` at info
   level and starts reading stdin.

Boot time on a fresh KB (first load of ~24k axioms): ~8 s.
Boot time on an unchanged KB (everything reconciles to no-op):
~200 ms.

### Steady state

The kernel owns its `KnowledgeBase` in memory. Request
handlers are methods on that KB. Mutating methods
(`tell`, `kb.reconcileFile`, `kb.removeFile`, `kb.flush`)
update the store + LMDB. Query methods (`ask`, `debug`,
`test`, `kb.generateTptp`, `kb.listFiles`) are read-only.

`tell` is session-scoped and doesn't persist across kernel
restarts; everything else is persisted when run against the
default DB-backed configuration.

### Shutdown

Either `shutdown` request or stdin EOF. In both cases the
kernel flushes pending LMDB writes (via RAII on the DB
handle's drop) before exiting. A `kill -9` may lose the last
write if it fell inside an uncommitted LMDB transaction; the
kernel's commit boundaries align with individual request
handlers, so the window is narrow.

---

## VSCode extension sketch (TypeScript)

What follows is a complete minimal extension that spawns the
kernel, reconciles open files on save, and exposes an "ask"
command. Drop it into an extension scaffold (`yo code`) and
wire it up.

### 1. Spawning the kernel

```ts
// src/kernel.ts
import { ChildProcessWithoutNullStreams, spawn } from "child_process";
import * as readline from "readline";

export class SumoKernel {
  private proc:   ChildProcessWithoutNullStreams;
  private reader: readline.Interface;
  private nextId = 1;
  private pending = new Map<number, {
    resolve: (value: unknown) => void;
    reject:  (reason: Error) => void;
  }>();

  constructor(binary: string, args: string[]) {
    this.proc = spawn(binary, ["serve", ...args], {
      stdio: ["pipe", "pipe", "pipe"],
      env:   { ...process.env, RUST_LOG: "info" },
    });

    this.proc.stderr.on("data", (buf) => {
      // Kernel's human-readable logs.  Pipe to your extension's
      // "output channel" so you can see what's happening.
      console.error(`[sumo-kernel] ${buf.toString()}`);
    });

    this.proc.on("exit", (code) => {
      // Fail every outstanding request so callers don't hang.
      for (const { reject } of this.pending.values()) {
        reject(new Error(`kernel exited with code ${code}`));
      }
      this.pending.clear();
    });

    this.reader = readline.createInterface({ input: this.proc.stdout });
    this.reader.on("line", (line) => this.handleLine(line));
  }

  async request<T = unknown>(method: string, params: object = {}): Promise<T> {
    const id = this.nextId++;
    const payload = JSON.stringify({ id, method, params });
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as any, reject });
      this.proc.stdin.write(payload + "\n", (err) => {
        if (err) {
          this.pending.delete(id);
          reject(err);
        }
      });
    });
  }

  notify(method: string, params: object = {}): void {
    // Notifications have no id and no response.
    const payload = JSON.stringify({ method, params });
    this.proc.stdin.write(payload + "\n");
  }

  async shutdown(): Promise<void> {
    try {
      await this.request("shutdown");
    } catch { /* kernel may exit before responding */ }
    this.proc.stdin.end();
  }

  private handleLine(line: string): void {
    if (!line.trim()) return;
    let msg: { id?: number; result?: unknown; error?: { code: number; message: string } };
    try {
      msg = JSON.parse(line);
    } catch (e) {
      console.error(`[sumo-kernel] bad JSON from stdout: ${line}`);
      return;
    }
    if (msg.id === undefined || msg.id === null) {
      // Kernel doesn't currently emit server-initiated messages,
      // but be tolerant if that changes.
      return;
    }
    const entry = this.pending.get(msg.id);
    if (!entry) return;
    this.pending.delete(msg.id);
    if (msg.error) {
      entry.reject(new Error(`${msg.error.code}: ${msg.error.message}`));
    } else {
      entry.resolve(msg.result);
    }
  }
}
```

### 2. Typed method wrappers

```ts
// src/api.ts
import { SumoKernel } from "./kernel";

export interface AskResult {
  status:    "Proved" | "Disproved" | "Consistent" | "Inconsistent" | "Timeout" | "Unknown";
  bindings:  string[];
  proofKif:  string[];
  raw:       string;
}

export interface ReconcileResult {
  path:           string;
  added:          number;
  removed:        number;
  retained:       number;
  revalidated:    number;
  parseErrors:    string[];
  semanticErrors: string[];
  persisted:      boolean;
}

export class SumoAPI {
  constructor(private kernel: SumoKernel) {}

  ask(query: string, opts: { session?: string; timeoutSecs?: number } = {}) {
    return this.kernel.request<AskResult>("ask", {
      query,
      session:      opts.session,
      timeout_secs: opts.timeoutSecs,
    });
  }

  reconcile(path: string, text?: string) {
    return this.kernel.request<ReconcileResult>("kb.reconcileFile", { path, text });
  }

  listFiles() {
    return this.kernel.request<{ files: Array<{ path: string; sentenceCount: number }> }>(
      "kb.listFiles"
    );
  }

  // … add wrappers for debug, test, kb.generateTptp, etc. as needed
}
```

### 3. Extension entry point

```ts
// src/extension.ts
import * as vscode from "vscode";
import { SumoKernel } from "./kernel";
import { SumoAPI } from "./api";

let kernel: SumoKernel | undefined;
let api:    SumoAPI    | undefined;

export function activate(context: vscode.ExtensionContext) {
  const config = vscode.workspace.getConfiguration("sumo");
  const binary = config.get<string>("binaryPath") ?? "sumo";
  const dbPath = config.get<string>("dbPath")     ?? `${context.globalStorageUri.fsPath}/sumo.lmdb`;

  kernel = new SumoKernel(binary, ["--db", dbPath]);
  api    = new SumoAPI(kernel);

  // Reconcile the file on save.
  context.subscriptions.push(
    vscode.workspace.onDidSaveTextDocument(async (doc) => {
      if (doc.languageId !== "kif") return;
      try {
        const r = await api!.reconcile(doc.fileName);
        vscode.window.setStatusBarMessage(
          `sumo: +${r.added} -${r.removed} (${r.retained} retained)`,
          4000,
        );
      } catch (e) {
        vscode.window.showErrorMessage(`sumo reconcile failed: ${e}`);
      }
    }),
  );

  // Ask command — prompts the user, runs the query, shows the verdict.
  context.subscriptions.push(
    vscode.commands.registerCommand("sumo.ask", async () => {
      const query = await vscode.window.showInputBox({
        prompt: "KIF conjecture",
        placeHolder: "(subclass Dog Animal)",
      });
      if (!query) return;
      try {
        const r = await api!.ask(query, { timeoutSecs: 30 });
        vscode.window.showInformationMessage(`sumo: ${r.status}`);
      } catch (e) {
        vscode.window.showErrorMessage(`sumo ask failed: ${e}`);
      }
    }),
  );
}

export async function deactivate() {
  if (kernel) await kernel.shutdown();
}
```

### 4. `package.json` manifest hooks

```jsonc
{
  "contributes": {
    "commands": [
      { "command": "sumo.ask", "title": "SUMO: Run Conjecture…" }
    ],
    "languages": [
      { "id": "kif", "extensions": [".kif"], "aliases": ["SUO-KIF"] }
    ],
    "configuration": {
      "properties": {
        "sumo.binaryPath": { "type": "string", "default": "sumo",
          "description": "Path to the `sumo` CLI binary." },
        "sumo.dbPath":     { "type": "string", "default": "",
          "description": "LMDB store path.  Empty = per-workspace default." }
      }
    }
  }
}
```

---

## Patterns + gotchas

### File tags are strings — be consistent

The kernel identifies files by the exact string you used when
loading them. `/Users/you/Merge.kif` and `./Merge.kif` resolve
to **different** tags in the KB.

VSCode extensions should standardise on **absolute paths**:
`doc.fileName` is already absolute. Don't mix
`vscode.workspace.asRelativePath` results with absolute
`fsPath` values or you'll get phantom reloads.

### The `debug` method has its own file-tag resolution

Unlike `kb.reconcileFile` and `kb.removeFile`, the `debug`
method does fuzzy resolution (exact → canonical → basename
suffix). So if your extension sends `{file: "Economy.kif"}`
to `debug` but loaded the file as an absolute path, it'll
still work. Don't depend on this for other methods — they're
strict.

### One in-flight request per kernel

The kernel is serial. If you want to run several asks
concurrently from an extension command, either:

- **Queue them** (await each one before sending the next).
  This is the simplest pattern and what most editor
  integrations use.
- **Spawn multiple kernels.** Each is a separate process with
  its own KB load cost. Only worth it if you really need
  parallelism (e.g. running a large test batch).

### Reconcile on save, not on every keystroke

The incremental diff is fast (~10 ms for small edits, ~50 ms
for a `Merge.kif`-scale rewrite) but it does mutate LMDB. For
the `onDidChangeTextDocument` event, you'd want to debounce
anyway. `onDidSaveTextDocument` is the correct hook for most
editor integrations — save is when the user has committed to
a state worth persisting.

### Proof formulas are flat KIF, not ANSI

The `proofKif[]` array in an `ask` response contains **flat
plain-KIF** strings. Indentation and ANSI colouring are
CLI-specific concerns that the kernel deliberately doesn't
emit. If you want pretty-printing in your extension:

```ts
// Pseudo-pretty-print: break on top-level args.
function pretty(kif: string): string {
  // … your preferred formatter.  VSCode's built-in KIF tokeniser
  // (if your extension contributes one) can wrap this.
}
```

### Stderr logs are your friend

When something goes wrong, the JSON error message from stdout
is often terse ("prover error: exit code 2"). The real
diagnostic is on stderr. Always plumb stderr to an
**Output channel** in your extension so the user can look at
what the kernel is doing:

```ts
const output = vscode.window.createOutputChannel("SUMO Kernel");
this.proc.stderr.on("data", (buf) => output.append(buf.toString()));
```

### Surviving an unexpected kernel crash

If the kernel dies, every outstanding `request()` promise
rejects. Your extension should catch that, offer to restart
the kernel, and re-reconcile any open buffers. The LMDB store
persists so the second boot is fast (~200 ms, since every
file reconciles to no-op against the already-committed
state).

A defensive respawn hook:

```ts
this.proc.on("exit", (code) => {
  if (code !== 0) {
    vscode.window.showWarningMessage(
      "sumo kernel crashed; click to respawn.",
      "Respawn",
    ).then(sel => { if (sel === "Respawn") respawnKernel(); });
  }
});
```

---

## Quick reference — all methods

| Method | Mutates KB? | Needs Vampire? | Typical latency |
|---|---|---|---|
| `tell`             | ✓ (session) | — | 1–10 ms |
| `ask`              | — | ✓ | prover-bound (~100 ms to timeout) |
| `debug`            | — | ✓ | prover-bound |
| `test`             | ✓ (session, per test) | ✓ | prover-bound × #tests |
| `kb.reconcileFile` | ✓ (persisted) | — | 10–500 ms |
| `kb.removeFile`    | ✓ (persisted) | — | 10–100 ms |
| `kb.flush`         | ✓ (persisted) | — | proportional to KB size |
| `kb.listFiles`     | — | — | <1 ms |
| `kb.generateTptp`  | — | — | 50–500 ms |
| `shutdown`         | — | — | <1 ms |

---

## Versioning + stability

The wire format is semver-stable: breaking changes to method
names, param keys, or response shapes bump the `sumo` CLI's
major version. Fields marked **optional** in this doc may be
added over time; clients should ignore unknown fields and
tolerate missing optional ones.

File a compatibility issue if you need a new field or method
— the surface is deliberately small and additive.
