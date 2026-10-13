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
and stages the SDK facade beside the generated bindings. The package has no
install hook, so build it once after cloning; `npm run web` does this for you.

The Vampire backend builds separately, and needs the Emscripten SDK plus GNU
awk (see `packages/vampire/build.sh` for the full list):

```bash
npm run build --workspace @sigma/vampire
```

### Publish `sigmakee` to npm

The package's `prepack` script rebuilds `dist/` on `npm pack` and
`npm publish`, so a publish never ships a stale build. To publish exactly the
bytes you inspected, build once and skip the scripts afterwards:

```bash
npm run build --workspace sigmakee
npm publish --dry-run --ignore-scripts --workspace sigmakee   # inspect the file list
npm publish --ignore-scripts --workspace sigmakee             # unscoped, public by default
```

Without `--ignore-scripts`, each command rebuilds, and the build starts by
deleting `dist/`. Keep the package version in step with the workspace crates,
and log in first (`npm login`, or trusted publishing in CI) with publish
rights to `sigmakee`.

The package's `homepage` (the link on its npm page) is the TypeDoc API
reference at <https://ontologyportal.github.io/sigma-rs/docs/js/>. It is built from the
`dist/*.d.ts` declarations and published with the GitHub Pages site, beside
the Rust docs at `/docs/rust/`. The source README stays reachable through
`repository`.
