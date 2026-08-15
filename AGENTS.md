# Agent: sigma-rs Coder

You are an agentic Rust coder working in this repository. Read this whole
file before making changes, and **read `README.md`** for the authoritative
description of the project, CLI, workspace layout, and prover options —
do not guess at CLI flags or behavior that README.md documents.

## What this project is

SigmaKEE-rs / SUPr: a parser, validator, and theorem-prover interface for
the SUO-KIF / SUMO knowledge representation language. KIF files are parsed
once and committed to an LMDB database in CNF with full Skolemization, so
theorem-prover queries need no runtime conversion. Multiple ATP backends are
supported (embedded/subprocess Vampire, subprocess E, and the native SUPr
prover).

## Workspace layout

This repo is **both a Cargo workspace and an npm workspace**. Rust members
live in `crates/`, JavaScript members in `packages/`; they meet at
`crates/wasm`, whose build output is what the `sigmakee` npm package ships.

| Crate | Description |
|---|---|
| `crates/core` (`sigmakee-rs-core`) | Core library: KB, cache, parsing, semantics, translation, prover layers |
| `crates/sdk` (`sigmakee-rs-sdk`) | Consumption-friendly SDK over `sigmakee-rs-core` |
| `crates/cli` (`sigmakee`) | Command line interface, builds the `sumo` executable |
| `crates/lsp` (`sumo-lsp`) | Persistent language server for IDE integration |
| `crates/wasm` (`sumo-parser-wasm`) | WASM bindings for the browser / Node.js |

| Package | Published? | Description |
|---|---|---|
| `packages/sigmakee` (`sigmakee`) | yes | The publishable wasm package: `crates/wasm` via `wasm-bindgen`, plus the `./sdk` facade. Built by `scripts/build.mjs` into `dist/` |
| `packages/web` (`@sigma/web`) | no | The SUMO browser demo (Vite). Consumes `sigmakee` as a workspace dependency -- there is no `pkg/` mirroring |
| `packages/vampire` (`@sigma/vampire`) | no | Emscripten build of Vampire for the optional in-browser backend. Everything but `package.json`/`build.sh` is gitignored |
| `packages/language` (`@sigma/language`) | no | Editor-neutral SUO-KIF / TPTP language assets |

Inside `crates/core/src`, the major subsystems are `kb/` (knowledge base),
`cache/`, `parse/`, `semantics/`, `syntactic/`, `trans/` (translation to
TPTP dialects etc.), `persist/`, and `prover/` (backend integrations,
including the native prover under `prover/saturate/`).

### npm commands

Run these from the repository root; `npm install` once first.

| Command | Effect |
|---|---|
| `npm run web` | Rebuild the wasm package, best-effort build Vampire, serve the demo on :8080 |
| `npm run build --workspace sigmakee` | Build the publishable package into `packages/sigmakee/dist/` |
| `npm run build --workspace @sigma/web` | Production `vite build` into `packages/web/dist/` |
| `npm run build --workspace @sigma/vampire` | Emscripten build (needs emsdk + gawk) |

`SKIP_VAMPIRE=1` / `NO_REBUILD=1` / `VAMPIRE_RECLONE=1` apply to `npm run web`.
Prefer `SKIP_VAMPIRE=1 npm run web` unless you are specifically testing the
Vampire backend -- it is a multi-minute Emscripten build.

Notes that matter when editing this area:
- The Rust→wasm step is **not** watched. Re-run after changing Rust code.
- `wasm-opt` comes from the pinned `binaryen` devDependency, not the system.
  Do not reintroduce a distro `binaryen` install: the version apt ships is too
  old for the opcodes this build emits, and the failure is non-fatal, so it
  silently ships a ~25% larger `.wasm`.
- Vite statically rewrites `new URL('.', import.meta.url)`. `app.js` derives
  its router `BASE` from `import.meta.env.BASE_URL` for that reason -- the demo
  deploys both at a site root and under `/browse/`, so do not reintroduce a
  module-URL-derived base.

## Cache architecture: lazy vs. eager, and what it means for new features

`crates/core/src/cache/` defines a small, fixed set of reactive cache shapes
that every KB layer (`semantics/`, `syntactic/`, `trans/`, `prover/saturate/`)
builds on. There are exactly two axes:

|        | lazy (compute-on-miss) | eager (maintained) |
|---|---|---|
| keyed  | `CacheBehavior` / `Cache<B>` | `EagerMapBehavior` / `EagerMap<B>` |
| whole  | `WholeCacheBehavior` / `WholeCache<B>` | `EagerBehavior` / `Eager<B>` |

- **Lazy** caches (`CacheBehavior`, `WholeCacheBehavior`) compute a value the
  first time it's asked for (`generate`), memoize it, and are the default
  choice for anything that's read on demand.
- **Eager** caches (`EagerBehavior`, `EagerMapBehavior`) have no
  compute-on-miss; they're maintained incrementally by reacting to change
  `Event`s (`consumes`/`produces`/`reads` declare their place in the reactor
  schedule) and are seeded via `initial`/`initialize`. Reach for eager only
  when a value must stay hot without being recomputed from scratch on every
  KB mutation — it costs more bookkeeping (side-state snapshotting, cycle/
  ordering declarations) than lazy, so it isn't the default.

Concretely: one cache = one `struct` implementing one of these four traits,
in its own file under the owning layer's `caches/` directory (see
`semantics/caches/`, `syntactic/caches/`, `trans/caches/`,
`prover/saturate/caches/` for the existing set), embedded as a field on the
layer via the matching `frontends` wrapper type. `CacheConfig` gives
per-cache enable/disable for free; persistence (`snapshot_side`/
`restore_side`) and the event-driven cascade are handled by the shared
`backends`/`events`/`router` machinery — an individual cache's file only
ever contains domain logic (`generate`, `react`, `initial`), never plumbing.

**What this means for adding a feature that needs caching or memoization:**
before writing a new mechanism (a bespoke `HashMap` + manual invalidation, a
`OnceCell`, a hand-rolled reactive callback, etc.), check whether it's just
another instance of one of the four shapes above. If it computes from
read-only parent data and can be recomputed on demand, it's a `Cache`/
`WholeCache`. If it must track incremental deltas, it's an `Eager`/
`EagerMap` reacting to existing `EventKind`s (extend `events.rs` only if a
genuinely new kind of change needs to be observable — don't invent a
parallel notification mechanism). New caching logic that doesn't fit this
model is a signal to stop and ask, not to bolt on a new pattern next to it.

## Hard rule: do not increase architectural complexity

Before adding any new type, trait, module, or mechanism, **search the
codebase for an existing feature, function, or abstraction that already does
this or something close to it**, and prefer extending or reusing it over
adding something new. This applies everywhere, not just caching:
- Grep for likely existing names/behavior before writing a new helper,
  trait, or config knob. Adding a second way to do something the codebase
  already does once is not acceptable.
- Follow the existing project organization: put new code in the module/
  layer it structurally belongs to (mirroring the patterns in `kb/`,
  `cache/`, `semantics/`, `syntactic/`, `trans/`, `persist/`, `prover/`),
  not in a new top-level module or a convenient-but-wrong location.
- Match established patterns in the area you're touching (e.g. the
  cache-shape table above, the layer/behavior split, error-type
  conventions) rather than introducing a new abstraction that does
  something similar in a different way.
- If a task seems to require a genuinely new architectural mechanism (a new
  cache shape, a new cross-cutting subsystem, a new persistence path),
  treat that as a signal to stop and confirm with the user before
  proceeding, rather than adding it unilaterally.

## Hard rule: do not touch `saturate`

**Never edit, refactor, or otherwise modify anything under
`crates/core/src/prover/saturate/`.** This is the native SUPr prover
implementation — it is tuned, delicate, and actively worked on outside of
agentic sessions. Treat it as read-only:
- You may read it to understand how other code calls into it.
- You may write code elsewhere that calls its public API.
- You must not change its files, add files inside it, or suggest inline
  edits to it, even if asked to "just fix a small thing" there. If a task
  seems to require changing `saturate` internals, stop and say so instead
  of proceeding.


## Rust conventions

- Write idiomatic, safe Rust. No `unsafe` unless there is truly no safe
  alternative, and if you do, justify it with a comment explaining the
  invariant that makes it sound.
- Use `Result`/`Option` and `?` for error handling; do not `unwrap()` or
  `expect()` outside of tests and cases where a panic is genuinely the
  correct behavior (invariant violations, not recoverable errors).
- Match existing patterns in the module you're editing (error types,
  builder patterns, trait boundaries) rather than introducing new ones.
- Run `cargo fmt` and `cargo clippy` on code you touch before considering
  a change complete. Fix clippy warnings rather than silencing them,
  unless there's a documented reason to allow one.
- Keep changes scoped — do not refactor unrelated code while implementing
  a feature or fix.
- Don't delete code just to silence a compile error. Recover the intent
  behind it and find the actual mechanism it depends on first — a narrow
  grep for `impl X for` can miss a fully-qualified impl elsewhere and lead
  to deleting code that's actually live.
- `cargo fix` / `cargo clippy --fix` is feature-blind: it will delete
  imports and code that are only used under a currently-disabled cargo
  feature. Before trusting an autofix, check whether the affected code is
  behind a `cfg(feature = ...)` gate and re-verify against the relevant
  feature combinations, not just the default build.

## Comments

- Do **not** write long, wordy, line-by-line comments narrating what code
  does. Well-named types, functions, and variables should make that clear.
- Use proper doc comments (`///` / `//!` in Rust, `/** */` / `//` JSDoc-style
  in JS/TS/wasm glue) for public items: functions, types, modules — describe
  behavior, parameters, and invariants concisely.
- Only add an inline comment (`//`) when a specific piece of code is
  genuinely non-obvious (a subtle invariant, a workaround for a specific
  bug, a non-local reason something is written the way it is). If removing
  the comment wouldn't confuse a future reader, don't write it.
- Don't preface an edit with a comment explaining *why you're making this
  change* (referencing the current task, a bug fix, or a caller). That
  belongs in the commit message, not the source — it rots as the codebase
  evolves. Make bare edits for things like cfg/allow changes or deletions.

## Tests

- Every new feature or non-trivial fix must come with unit tests covering
  it, colocated with the code under test (`#[cfg(test)] mod tests` in the
  same file, matching this codebase's existing convention) unless an
  integration-test-style location is clearly more appropriate.
- Test both the intended behavior and realistic edge cases (empty input,
  malformed KIF, missing terms, etc. — whatever applies to the code you
  changed).
- Run the relevant test suite before reporting a task complete. Compare
  failure *sets* before/after your change rather than assuming a clean 
  suite is the starting point, and don't attribute a pre-existing failure 
  to your own change. (Suite selection and gating for CI purposes is 
  enforced by commit hooks — you don't need to reason about which suite 
  to run for that.)
- If you find a stale `sumo.lmdb` file lying around (repo root or a
  worktree), don't trust it as current KB state for validation/testing —
  reload it from the `config.xml` constituents first.

## Character encoding

- **Do not use non-ASCII characters anywhere in the codebase** (Rust, KIF,
  config, scripts, doc comments, commit messages, etc.). Stick to plain
  ASCII quotes, hyphens, and arrows (`->`, `"`, `-`) — no smart quotes,
  em-dashes, curly punctuation, or Unicode symbols.
- The one exception is the web GUI (`crates/wasm` front-end / HTML/CSS/JS
  serving the browser UI): non-ASCII characters are fine there if they're
  part of user-facing display text or content, not source code identifiers.

## Commits

- This repo enforces Conventional Commits (see `CONTRIBUTING.md` and
  `.github/.commitlintrc.yml`). Every commit message must use one of:
  `build, ci, chore, docs, feat, fix, perf, refactor, revert, style, test`,
  lower-case type and scope, a non-empty lower-case subject with no
  trailing period, header <=100 chars, and a blank line before any body or
  footer.
- If a pre-commit hook is installed and rejects a commit message, **fix the
  message to satisfy it** — do not bypass it with `--no-verify`.
- If hooks aren't installed yet, you can set them up via
  `.github/scripts/setup.sh`, but do not disable or skip hook enforcement.

## Before finishing

1. `cargo build` (and `cargo build --workspace` if the change touches more
   than one crate) to confirm it compiles.
2. `cargo test` for affected crates.
3. `cargo fmt` / `cargo clippy` clean on touched files.
4. Confirm no edits landed inside `prover/saturate/`.
5. Confirm no non-ASCII characters were introduced outside the web GUI.
