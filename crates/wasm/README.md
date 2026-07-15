# sumo-parser-wasm

WebAssembly bindings for [`sigmakee-rs-core`](../core) — load SUO-KIF, translate
to TPTP, and **prove theorems entirely in the browser** with the pure-Rust
native saturation prover. No server, no subprocess, no WASI required.

Two layers are shipped:

- **`sigmakee/sdk`** — an SDK-shaped facade (`Session`,
  `Source`, `Backend`, `Config`) that mirrors the [`sigmakee-rs-sdk`](../sdk)
  crate's surface. **Start here.**
- **`sigmakee`** (package root) — the raw wasm-bindgen classes
  (`WasmNativeProver`, `WasmKnowledgeBase`, `Config`) for direct, lower-level
  control.

> **What runs in-browser.** Native proving, KIF parsing, TPTP translation, and
> lookup are pure Rust and run client-side. The subprocess prover, LMDB
> persistence, rayon parallelism, and native git/http sources are **not**
> available on `wasm32` — they're structurally impossible in a browser sandbox.

---

## Quick start

```ts
import { init, Session, Source, Backend, Config } from "sigmakee/sdk";

await init();                                  // instantiate the WASM module (once)

const cfg = new Config();
cfg.timeLimitSecs = 10;
const session = new Session({ backend: Backend.Native, config: cfg });

await session.ingest(Source.kif(`
  (instance Socrates Man)
  (=> (instance ?X Man) (instance ?X Mortal))
`));

const r = session.ask("(instance Socrates Mortal)");
console.log(r.status);   // "Proved"
console.log(r.proof);    // [{ index, rule, premises, kif }, ...]
```

Typed example: [`examples/driver.ts`](examples/driver.ts). Raw-bindings example:
[`examples/node-demo.mjs`](examples/node-demo.mjs).

## Demo site

**Live: <https://ontologyportal.github.io/sigma-rs/browse/>** (staged into the
GitHub Pages site by the `regression-pages.yml` workflow, at `/browse`).

A browser front-end lives in [`web/`](web/), built on the facade. It **autoloads
SUMO's foundational ontology** (`Merge.kif`, ~630 KB) from
`github.com/ontologyportal/sumo` on startup, then presents four tabs:

- **Home** — search symbols and documentation; click a result to open its man
  page (kinds, documentation, taxonomy, signature), with clickable parents/children.
- **Knowledge base** — manage constituents: see what's loaded, add more SUMO
  files (picked from the repo's `*.kif` list), or add from a URL / file upload,
  and remove or reset. Diagnostics and search update as the KB changes.
- **Diagnostics** — the validation findings for the loaded KB.
- **Theorem prover** — a `tell` box (assertions) and an `ask` box (query);
  proves in-browser against SUMO plus your assertions.

To load more of SUMO by default, edit the `SUMO_FILES` constant in
[`web/app.js`](web/app.js) (only `Merge.kif` autoloads, to keep startup fast).

Run it locally:

```bash
./serve.sh                           # rebuilds pkg/, mirrors it into web/pkg/, serves
# → open http://localhost:8080/
```

> **Must be served over HTTP.** Opening `web/index.html` directly (`file://`)
> fails with *"Module source URI is not allowed"* — browsers block ES modules
> and `fetch` (of the `.wasm`) on `file://`. The demo imports `./pkg/…`, so it's
> self-contained: `serve.sh` mirrors the built package into `web/pkg/` and
> serves `web/` as the root — the same layout the Pages deploy publishes at
> `/browse/`, so local and live behave identically.

---

## SDK-shaped API (`/sdk`)

The facade maps the `sigmakee-rs-sdk` crate onto the browser:

| SDK crate (Rust) | This facade (JS) | Notes |
| --- | --- | --- |
| `Session::new` | `new Session({ backend, config })` | |
| `Backend::{Native, TranslationOnly}` | `Backend.{Native, TranslationOnly}` | `External` → TranslationOnly + a JS `hook` |
| `Source::{Http, Git, Local/Reader}` | `Source.{url, gitHub, file, kif}` | |
| `Session::ingest(Source)` | `session.ingest(source)` | async (URL/GitHub fetch) |
| `Session::tell` / `ask` | `session.tell` / `ask` | |
| `Session::translate` | `session.translate` | TranslationOnly backend |
| `Session::validate` / `validateFormula` | `session.validate` / `validateFormula` | |
| `Session::search` / `manpage` | `session.search` / `manpage` | |
| `KBManager` `NativeProverConfig` | `Config` | |
| `Session::fork` / `persist` / `open` | — | LMDB-backed; no filesystem in the browser |

### `Session`

```ts
new Session(opts?: { backend?: Backend; config?: Config })   // default backend: Native

session.configure(config: Config): this
session.ingest(source: Source): Promise<{ loaded: number; files: string[]; errors: string[] }>
session.tell(kif: string, session?: string): { ok: boolean; errors: string[] }
session.ask(query: string, opts?: { session?: string; hook?: (tptp: string) => string }): AskResult | string
session.translate(opts?: { lang?: "fof" | "tff"; hideNumbers?: boolean; session?: string }): string
session.lookup(pattern: string): string[]
session.validate(): Diagnostic[]                             // whole-KB diagnostics ([] = clean)
session.validateFormula(kif: string): Diagnostic[]           // validate one formula, KB untouched
session.search(query: string, opts?: { kind?: string; language?: string; limit?: number }): SearchHit[]
session.manpage(symbol: string): ManPage | null              // structured symbol reference
session.flushSession(session: string): void
session.kb            // the underlying raw binding (escape hatch)
```

`Diagnostic` is `{ severity, kind, code, message, file, line, col, end_line,
end_col }`; `SearchHit` is `{ symbol, kinds, source, language, text, rank }`
(hits come sorted by `rank` descending — a relevance score that boosts
symbol-name matches over text-only matches); `ManPage`
carries `{ name, kinds, documentation, term_format, format, parents, children,
arity, domains, range, appears_in_count, consequent_count }` (see `sdk.d.ts`).
These project out the KB's internal `u64` ids and run on either backend.

`ask` returns an `AskResult` on a Native session, or the hook's string on a
TranslationOnly session:

```ts
interface AskResult {
  status: "Proved" | "Disproved" | "Consistent" | "Inconsistent"
        | "Timeout" | "InputError" | "Unknown";
  proved: boolean;
  given_steps: number | null;
  raw_output: string;
  proof: Array<{ index: number; rule: string; premises: number[]; kif: string }>;
}
```

### `Source`

```ts
Source.kif(text: string, tag?: string)                       // inline KIF
Source.url(url: string, tag?: string)                        // one document over HTTP (CORS)
Source.file(file: File)                                       // a browser file upload
Source.gitHub({ owner, repo, ref?, dir?, match?, token? })   // every *.kif in a public repo
```

`Source.url` and `Source.gitHub` are subject to the browser's CORS policy;
`raw.githubusercontent.com` and the GitHub REST API both allow it, so public
repos work directly. For server-side loading (native git clone, no CORS limits)
use the `sigmakee-rs-sdk` crate's `Source` / `KBManager`.

### `Config`

Mirrors the SDK `KBManager`'s `NativeProverConfig` — camelCase properties
matching the `<prover type="native">` preference keys.

```ts
const cfg = new Config();          // native-prover defaults, wantProof on
cfg.timeLimitSecs = 10;            // wall-clock budget (0 = unlimited)
cfg.maxSteps      = 4000;          // given-clause step cap
cfg.maxLits       = 8;             // max literals per retained clause
cfg.forwardClose  = true;          // forward-closure before the loop
cfg.wantProof     = true;          // populate result.proof
cfg.profile       = false;         // phase timings into raw_output
```

---

## Low-level bindings (package root)

The facade wraps these; use them directly for finer control. Two classes:

| Class | Purpose |
| --- | --- |
| **`WasmNativeProver`** | In-browser prover. `configure(config)`, `loadKif(text, tag)`, `tell(kif, session?)`, `ask(query, session?)`, `lookup(pattern)`, `flushSession(session)`. |
| **`WasmKnowledgeBase`** | KIF → TPTP. `loadKif`, `tell`, `toTptp(lang?, hideNumbers?, session?)`, `lookup`, `flushSession`, and `ask(query, askHook)` to drive an external prover through a JS callback. |

Both also expose the query methods `validate()`, `validateFormula(kif)`,
`search(query, kind?, language?, limit?)`, and `manpage(symbol)` (the facade's
`Session` wraps these; the raw `search`/`manpage` take positional args).

The `/sdk` module also re-exports standalone loaders for use with these raw
classes: `loadFromUrl(kb, url)`, `loadFromFile(kb, file)`,
`loadFromGitHubRepo(kb, opts)` — same fetch logic as `Session.ingest`.

---

## Building

### Recommended: `wasm-pack`

```bash
cargo install wasm-pack            # once
wasm-pack build crates/wasm --target web --release
#  → crates/wasm/pkg/  (add sdk.mjs/sdk.d.ts manually, or use build-npm.sh)
```

`--target`: `web` (browser ESM, works with Vite/webpack), `bundler`
(webpack/rollup), `nodejs` (CommonJS for Node).

### Without `wasm-pack`

[`build-npm.sh`](build-npm.sh) does the same with only `wasm-bindgen-cli`, and
also copies the facade (`sdk.mjs`/`sdk.d.ts`) and package metadata into `pkg/`:

```bash
cargo install wasm-bindgen-cli --version 0.2.121   # match the wasm-bindgen crate
crates/wasm/build-npm.sh                            # → crates/wasm/pkg/ (web target)
crates/wasm/build-npm.sh nodejs pkg-node            # → crates/wasm/pkg-node/ (Node)
```

Runs `--release` for `wasm32-unknown-unknown`, `wasm-bindgen`, then assembles a
publishable `pkg/`. If `wasm-opt` (binaryen) is on `PATH` it size-optimizes the
`.wasm`.

---

## Publishing to npm

The output directory (`pkg/`) is a complete, publishable package.

```bash
cd crates/wasm/pkg
npm publish --dry-run                # inspect the file list first
npm publish                          # unscoped `sigmakee` → public by default
```

Before the first publish:

- **Name.** [`npm/package.json`](npm/package.json) is the unscoped name
  `sigmakee` (no `--access public` needed). It's currently unclaimed on npm; the
  first publish claims it for your account/org.
- **Version.** Keep it in step with the crate (`2.0.1`).
- **Auth.** `npm login` (or an `NPM_TOKEN` in CI) with publish rights.

---

## License

GPL-3.0, matching the workspace.
