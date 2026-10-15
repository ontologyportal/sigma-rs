# sigmakee

The [sigmakee-rs](https://github.com/ontologyportal/sigma-rs) knowledge-base
engine compiled to WebAssembly. Load SUO-KIF ontologies (SUMO and others),
validate and search them, translate them to TPTP, and **prove theorems in the
browser or in Node** with the pure-Rust native saturation prover. No server or
WASI is required; external provers (Vampire, E) plug in through a JS bridge.

This package powers the [sigmakee.dev](https://sigmakee.dev) web IDE
([`packages/web`](https://github.com/ontologyportal/sigma-rs/tree/main/packages/web)).

```bash
npm install sigmakee
```

**API reference:** <https://ontologyportal.github.io/sigma-rs/docs/js/>

## Entry points

| Import | What it is |
| --- | --- |
| **`sigmakee/sdk`** | The SDK facade (`Session`, `Source`, `Backend`, `Config`), shaped like the [`sigmakee-rs-sdk`](https://ontologyportal.github.io/sigma-rs/docs/rust/sigmakee_rs_sdk/) Rust crate. **Start here.** |
| `sigmakee` | The `engine` module: the raw wasm-bindgen classes (`Session`, `Config`, `WasmLsp`) the facade wraps. Use it for lower-level control or the in-process language server. |
| `sigmakee/node` | Node-only helper that runs a local `vampire` binary for the Vampire backend (see [External provers](#external-provers)). |
| `sigmakee/engine_bg.wasm` | The compiled engine, for bundlers or hosts that load it themselves. (`sigmakee/sumo_parser_wasm_bg.wasm` is kept as an alias for older imports.) |

All three JS modules ship TypeScript declarations.

> **What runs on wasm.** Parsing, validation, search, man pages, TPTP
> translation, native proving, consistency audits and KB snapshots all run
> client-side. LMDB persistence, rayon parallelism, subprocess provers and
> native git/HTTP sources are not available on `wasm32`. Use the
> `sigmakee-rs-sdk` crate or the `sumo` CLI for those.

---

## Quick start

```ts
import { init, Session, Source, Backend, Config } from "sigmakee/sdk";

await init(); // instantiate the WASM module (once; idempotent)

const cfg = new Config();
cfg.timeLimitSecs = 10;
const session = new Session({ backend: Backend.Native, config: cfg });

await session.ingest(
  Source.kif(`
  (instance Socrates Man)
  (=> (instance ?X Man) (instance ?X Mortal))
`),
);

const r = session.ask("(instance Socrates Mortal)");
console.log(r.status); // "Proved"
console.log(r.proof); // [{ index, rule, premises, kif }, ...]
```

In the browser, `init()` fetches `engine_bg.wasm` from beside the JS. In Node,
pass the bytes:

```js
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";

const wasm = createRequire(import.meta.url).resolve("sigmakee/engine_bg.wasm");
await init({ module_or_path: await readFile(wasm) });
```

Examples: [`examples/driver.ts`](examples/driver.ts) (typed, facade) and
[`examples/node-demo.mjs`](examples/node-demo.mjs) (raw bindings, Node).

---

## The SDK facade (`sigmakee/sdk`)

### Backends

`new Session({ backend })` picks the engine that `ask` and `auditConsistency`
use. Every backend shares the same KB, validation, search and translation.

| `Backend` | Prover | Notes |
| --- | --- | --- |
| `Native` (default) | Pure-Rust saturation prover, in-process | Required for `promote`, `snapshot` and `restore` |
| `Vampire` | Vampire through the engine's external prover layer | Needs a bridge (see below) |
| `E` | E (eprover) through the same layer | Needs a bridge (see below) |
| `TranslationOnly` | None | `translate` only; `ask` throws |

### `Session`

| Area | Methods |
| --- | --- |
| Setup | `new Session({ backend?, config? })`, `configure(config)`, `backend`, `kb` (the raw `engine` session) |
| Loading | `ingest(source, { promote? })` (async), `promote(tag)`, `snapshot()`, `restore(bytes)` |
| Proving | `tell(text, session?, tptp?)`, `ask(query, { session?, tptp?, hook? })`, `flushSession(session)` |
| Auditing | `auditConsistency(request?)` (sampled consistency sweep), `sentenceAt(file, offset)` |
| Translation | `translate({ lang?: "fof" \| "tff", hideNumbers?, session? })` |
| Validation | `validate()`, `validateFormula(kif)`, `validateScratch(assertions, query)` |
| Browsing | `search(query, opts?)`, `manpage(symbol)`, `taxonomy(symbol)`, `lookup(pattern)` |
| Natural language | `naturalLanguages()`, `renderNl(kif, language, genericVars?)` |
| WordNet | `loadWordNet(files)`, `clearWordNet()`, `wordnetDiagnostics(limit?)` |

`ingest` with `{ promote: false }` only indexes the source (search and man
pages work), and `promote(tag)` later adds it to the axiom base. `snapshot()`
freezes the whole KB to bytes, and `restore(bytes)` thaws it in place. The web
IDE uses this pair to cache a loaded SUMO between visits.

`ask` returns an `AskResult`:

```ts
interface AskResult {
  status: "Proved" | "Disproved" | "Consistent" | "Inconsistent"
        | "Timeout" | "InputError" | "Unknown";
  proved: boolean;
  proof: Array<{ index: number; rule: string; premises: number[]; kif: string }>;
  raw_output: string;
  // ...plus step counts and, with Config.keepTptp, input_tptp
}
```

The full shapes of `Diagnostic`, `SearchHit`, `ManPage`, `AuditResult`,
`WordNetDiagnostics` and the other result types are in the
[API reference](https://ontologyportal.github.io/sigma-rs/docs/js/), generated from
[`src/sdk.d.ts`](src/sdk.d.ts).

### `Source`

```ts
Source.kif(text, tag?)                               // inline KIF
Source.url(url, tag?)                                // one document over HTTP
Source.file(file)                                    // a browser File upload
Source.gitHub({ owner, repo, ref?, dir?, match?, token? }) // every *.kif in a repo
```

`Source.url` and `Source.gitHub` use `fetch`, so they are subject to CORS.
`raw.githubusercontent.com` and the GitHub REST API both allow it, so public
repos work directly. The same fetch logic is exported for use with a raw
session: `loadFromUrl(kb, url)`, `loadFromFile(kb, file)` and
`loadFromGitHubRepo(kb, opts)`.

### `Config`

Mirrors the SDK's `NativeProverConfig` (the `<prover>` preference keys) as
camelCase properties:

```ts
const cfg = new Config(); // native-prover defaults, wantProof on
cfg.timeLimitSecs = 10;   // wall-clock budget (0 = unlimited)
cfg.maxSteps = 4000;      // given-clause step cap
cfg.maxLits = 8;          // max literals per retained clause
cfg.forwardClose = true;  // forward closure before the loop
cfg.wantProof = true;     // populate result.proof
cfg.profile = false;      // phase timings into raw_output
cfg.strategy = Config.strategyPresets()[0].strategy; // native strategy preset
```

The axiom selection used by the external backends is set with `selection`,
`selectionBudget`, `selectionTolerancePct` and `selectionTimeLimitSecs`
(defaults: `Config.selectionDefaults()`). `vampireArgs` appends raw Vampire
CLI text, and `keepTptp` returns the exact problem as `result.input_tptp`.

### Standalone helpers

| Function | Purpose |
| --- | --- |
| `formatKif(text, { indentUnit? })` | Re-indent KIF by paren depth (pure JS) |
| `parseTest(name, text)` / `parseTptpTest(name, text, remap?)` | Parse a `.kif.tq` or TPTP test file |
| `formatTest(opts)` | Write an Ask/Tell pair as `.kif.tq` text |
| `sumoSymbols()` | The SUMO symbol names this build was compiled with |

---

## External provers

The `Vampire` and `E` backends run the same pipeline as the CLI's external
prover: axiom selection, hypothesis staging and transcript parsing. Only the
prover run itself is delegated, to a synchronous function on `globalThis`
that you install:

| Backend | Global | Signature |
| --- | --- | --- |
| `Vampire` | `__sigmaRunVampireSync` | `(tptp, args, timeoutMs) => { stdout, stderr, code }` |
| `E` | `__sigmaRunEproverSync` | same; `args` is a JSON array of E flags |

In Node, `sigmakee/node` provides the Vampire bridge, a `spawnSync` of a local
binary:

```js
import { init, Session, Backend } from "sigmakee/sdk";
import { installVampireBridge, vampireBridge } from "sigmakee/node";

installVampireBridge(vampireBridge({ vampirePath: "/usr/local/bin/vampire" }));
const s = new Session({ backend: Backend.Vampire });
```

`installVampireBridge` is also exported from `sigmakee/sdk`, and a per-call
bridge can be passed as `ask(query, { hook })`.

In the browser, the bridge blocks the engine's worker on `Atomics.wait` while
a page-owned worker runs Vampire or E compiled to WASM. That needs
cross-origin isolation (for `SharedArrayBuffer`). The web IDE ships this
plumbing with the `@sigma/vampire` and `@sigma/eprover` workspace packages.

---

## The `engine` module (package root)

The facade wraps these classes; import them from `sigmakee` directly when you
need finer control.

| Class | Purpose |
| --- | --- |
| `Session` | The whole in-browser KB. Beyond the facade's methods it exposes `loadKif(text, tag)` (fused ingest and promote), `ingest`, `toTptpIndexed(lang?, hideNumbers?)`, `clausify`, `stats()`, `fileStats`, the audit planners (`auditTargets`, `auditSources`, `auditDocument`) and `setMaxThreads`. Its `search` and `manpage` take positional arguments. |
| `Config` | The prover configuration described above. `Config.backend` (`"native"`, `"vampire"` or `"e"`) picks the prover per call on a raw session. |
| `WasmLsp` | The `sumo-lsp` language server, in-process. `new WasmLsp(session)` shares that session's KB. Feed JSON-RPC messages to `handleMessage(json)`, which returns the reply and notifications as JSON strings. Call `flushReloads(force)` after edits to get diagnostics, because the bridge has no idle loop of its own. |

The module's default export is the wasm-bindgen `init`, which the facade's
`init` wraps. `initSync` is also available.

---

## Building

```bash
npm install                          # once, from the repository root
npm run build --workspace sigmakee   # → packages/sigmakee/dist/
```

[`scripts/build.mjs`](scripts/build.mjs) needs only `cargo` and `rustup`, not
`wasm-pack`. It:

1. installs the `wasm-bindgen-cli` version the workspace `Cargo.lock` resolved
   into a repo-local `target/wasm-bindgen/<version>/` (the generated glue and
   the crate's runtime must match), leaving any global install alone;
2. adds the `wasm32-unknown-unknown` target if missing;
3. builds the `crates/wasm` crate in release mode;
4. runs `wasm-bindgen --out-name engine`, so the generated files are
   `engine.js`, `engine.d.ts` and `engine_bg.wasm`;
5. size-optimizes the `.wasm` with `wasm-opt -Oz` from the pinned `binaryen`
   devDependency (a distro `binaryen` is usually too old for the opcodes this
   build emits; a failure here is non-fatal and ships a roughly 25% larger
   `.wasm`);
6. stages the facade (`sdk.*`, `node.*`) and a `build-info.json` engine
   fingerprint beside the bindings.

The default wasm-bindgen target is `web` (browser ESM, which also works in
Node as above). Pass `bundler` or `nodejs` as the first argument for other
targets, and give each its own output directory. `dist/` is what `exports`
point at, so CommonJS glue there would sit under an ESM manifest:

```bash
node scripts/build.mjs nodejs dist-node
```

Rust changes are not watched. Rebuild after editing anything under `crates/`.

---

## License

GPL-3.0, matching the workspace.
