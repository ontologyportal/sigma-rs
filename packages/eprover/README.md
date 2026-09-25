# E prover builds

Private workspace package building upstream E and e_axfilter as native
executables or separate Emscripten ES modules. Source is pinned in build.sh; nothing fetched or
generated is committed. The browser exposes E in Ask/Tell and Audit, including optional e_axfilter subsets.

From the repository root, with emsdk activated and git, GNU make, Python 3,
and tar installed:

```bash
npm run build --workspace @sigma/eprover
npm test --workspace @sigma/eprover
```

Output: dist/eprover.js, dist/eprover.wasm, dist/e_axfilter.js,
dist/e_axfilter.wasm, dist/runner.mjs, and dist/COPYING. The .gitignore allows only the
package manifest, build script, tests, this README, and .gitignore.

- SKIP_EPROVER=1 skips the build.
- EPROVER_REBUILD=1 rebuilds even when the source, script, and compiler match.
- EPROVER_JOBS=4 controls compilation parallelism (default: 4).

Builds use a temporary clean tree inside .eprover-cache and leave existing
native E installations alone. Change EPROVER_REF in build.sh to upgrade E.
The build adjusts PicoSAT's archive commands, 64-bit limit constants, and
a signature cleanup loop that frees an uninitialized entry in upstream E.
It also removes stray debug output from the upstream negated-formula printer.
Single-strategy `--auto` also leaves SInE off so Sigma-selected subsets are
proved without another round of premise removal. Explicit `--sine` still works.
These patches apply only inside the temporary build tree.

Import a generated module's default factory, instantiate with
noInitialRun: true, write inputs using module.FS, then call module.callMain.
Use a fresh instance for every invocation; the programs have global state
and exit after running. e_axfilter writes its subsets to virtual files,
which the caller must read from module.FS. See build.test.mjs for examples.

Run these synchronous programs in a worker. Enforce wall-clock deadlines
by terminating the worker; native process scheduling and resource limits
are not browser execution controls. Use E's single-process --auto mode,
not its process-based --auto-schedule mode.

## Native Cargo build

The CLI's default bundled-eprover feature invokes the same script in native
mode during cargo build. It writes eprover, e_axfilter, eprover-COPYING, and
a cache stamp beside sumo in the Cargo profile directory. Native mode needs
git, cc, GNU make, Python 3, and tar; it does not need npm or Emscripten.
Cross-compilation and non-Unix targets require SKIP_EPROVER=1 or disabling
bundled-eprover, with E provided separately.

To build native binaries directly from the repository root:

```bash
bash packages/eprover/build.sh --native target/release
EPROVER_NATIVE_DIR="$PWD/target/release" npm test --workspace @sigma/eprover
```

The same smoke tests support native binaries through EPROVER_NATIVE_DIR.
Without it, they test the generated WASM modules.
