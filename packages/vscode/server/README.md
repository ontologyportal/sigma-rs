# `server/` — bundled binary drop

At package time the release workflow (see
`.github/workflows/release.yml` or `scripts/package-platform.sh`)
copies two platform-specific binaries into this directory
**before** `vsce package --target <platform>` is run, so each
platform-specific VSIX carries exactly the binaries that match
its target:

- **`sumo-lsp`** — the language server (hover, goto, completion,
  diagnostics, taxonomy).  Always spawned on activation.
- **`sumo`** — the ask/tell kernel (the `sumo serve` subcommand).
  Lazily spawned the first time the user runs `SUMO: Ask (form
  under cursor)` or sends a `tell`.

At extension activation / command invocation, `src/extension.ts`
checks this location first:

```
<extensionPath>/server/sumo-lsp           (POSIX)
<extensionPath>/server/sumo-lsp.exe       (Windows)
<extensionPath>/server/sumo               (POSIX)
<extensionPath>/server/sumo.exe           (Windows)
```

If present, the bundled binary is spawned — zero-friction install.
If absent (e.g. a developer building the extension from source
without staging the binary), the extension falls back to the
user's `sumo.server.path` / `sumo.kernel.path` configuration,
which in turn defaults to a PATH lookup (`sumo-lsp` / `sumo`
respectively).  User-configured paths always win, which keeps the
`target/debug/<binary>` dev loop ergonomic.

Vampire is *not* bundled.  The ask/tell kernel resolves `vampire`
on PATH (or `sumo.kernel.vampirePath` if set) at startup and
surfaces a friendly per-ask error if it's missing, so the
extension activates cleanly on systems where the user hasn't
installed Vampire yet.

**This directory is intentionally empty in the source repository.**
Packaging CI drops the binaries here at build time; the
`.gitignore` in the parent of this directory excludes them.  Only
this README ships inside the VSIX when a binary isn't staged,
giving the fallback a friendly error message.
