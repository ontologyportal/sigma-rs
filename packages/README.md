## npm setup

Node 24+ and npm 11+ are expected; `cargo` and `rustup` must be on `PATH`
because the wasm package compiles Rust as part of its build.

```bash
npm install          # once, from the repository root -- links all workspaces
```

That single install also provides the `wasm-opt` used for the size pass (the
pinned `binaryen` devDependency), so no system package is needed.

### Run the demo site

```bash
npm run web          # → http://localhost:8080/
```

This rebuilds `packages/sigmakee`, best-effort builds the Vampire backend, then
starts Vite. Useful env vars:

| Variable | Effect |
|---|---|
| `NO_REBUILD=1` | Skip the wasm rebuild and serve whatever is already built |
| `SKIP_VAMPIRE=1` | Skip the Vampire backend build (a multi-minute Emscripten build) |
| `VAMPIRE_RECLONE=1` | Force a clean Vampire rebuild |

```bash
SKIP_VAMPIRE=1 npm run web    # typical: skip the Emscripten toolchain
```

The Rust source is **not** watched -- Vite's HMR covers the JS/CSS it serves,
not the Rust→wasm step. Re-run `npm run web` after changing Rust code.

### Build the `sigmakee` npm package

```bash
npm run build --workspace sigmakee     # → packages/sigmakee/dist/
```

The build installs the `wasm-bindgen-cli` version pinned in the workspace
`Cargo.lock` if it is missing — into a repo-local `target/wasm-bindgen/`, never
over a globally installed one — adds the `wasm32-unknown-unknown` target,
compiles `--release`, runs `wasm-bindgen`, size-optimizes with `wasm-opt -Oz`,
and stages the SDK facade beside the generated bindings. `npm install` also
runs it, via the package's `prepare` hook, so a fresh clone has a resolvable
`sigmakee` without a separate build step.

To inspect what would be published (`--ignore-scripts` so it reports the
already-built `dist/` rather than rebuilding it):

```bash
npm publish --dry-run --ignore-scripts --workspace sigmakee
```

The Vampire backend builds separately, and needs the Emscripten SDK plus GNU
awk (see `packages/vampire/build.sh` for the full list):

```bash
npm run build --workspace @sigma/vampire
```