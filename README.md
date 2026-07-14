<div align="center">
  <img src="./logo.png" alt="SUPr Logo" width="100" style="background:#ddd;padding:10px;border-radius:10px">
  <div style="font-weight:bold;font-size:24px">SigmaKEE-rs + <br>SUPr (SUMO Prover) v1</div>
</div>
<br>

A parser, validator, and theorem-prover interface for the [SUO-KIF](https://www.ontologyportal.org/suo-kif.pdf) / [SUMO](https://www.ontologyportal.org/) knowledge representation language.

KIF files are parsed once and committed to an [LMDB](https://www.symas.com/lmdb) database. Formulas are stored in Conjunctive Normal Form (CNF) with full Skolemization so that subsequent theorem-prover queries require no runtime conversion. The backend prover is configurable. SigmaKEE-rs currently supports the following automated theorem provers (ATPs) for automated reasoning against SUMO:

- [Vampire](https://vprover.github.io/) - both embedded as an API and via subprocess invocation
- [E](https://github.com/eprover/eprover) - via subprocess invocation
- SUPr (SUMO Prover) - a prover tweaked specifically for reasoning over SUMO!

[Check out the current test results against the current version of SUMO (run nightly)](https://ontologyportal.github.io/sigma-rs/)

## Table of Contents
- [Install](#install)
    * [Official Release](#from-official-release-channel)
        - [UNIX](#unix-macos-intelarm64--linux-arm64--linux-amd64)
        - [Windows](#windows)
    * [Building from Source](#build-from-source)
- [Workspace Layout](#workspace-layout)
- [Quick Start](#quick-start)
- [CLI Reference](#cli-reference)
    * [LMDB Caching](#to-cache-or-not-to-cache-that-is-the-question)
    * [Global Flags](#global-flags)
    * [Shared KB Arguments](#shared-kb-arguments--f--d---db---no-db---git)
    * [`validate`](#sumo-validate)
    * [`ask`](#sumo-ask)
    * [`translate`](#sumo-translate)
    * [`test`](#sumo-test)
    * [`load`](#sumo-load)
    * [`man`](#sumo-man)
    * [`audit`](#sumo-audit)
    * [`update`](#sumo-update)
    * [`config`](#sumo-config)
- [Prover Knobs](#prover-knobs)
    * [Strategy](#strategy-fields-env-var-ab-overrides-only--no-cli-flag-no-configxml-key)
    * [NativeOpts](#nativeopts-fields-exposed-through-kbmanager--configxml--cli)
    * [Other options](#other-diagnostic--tracing-env-vars)
- [Environment Variables](#environment-variables)
- [Running Tests](#running-tests)

## Install

### From Official Release Channel

Using the official GitHub release channel is best for those who **DO NOT** wish to 
customize their installation of `sigmakee-rs`. After installing via this channel, you 
will not have to rerun this command to get updates (hopefully).

#### UNIX (macOS Intel/ARM64 + Linux arm64 + Linux amd64)

```bash
curl -fsSL https://raw.githubusercontent.com/ontologyportal/sigma-rs/main/install.sh | bash
```

#### Windows

```powershell
irm https://raw.githubusercontent.com/ontologyportal/sigma-rs/main/install.ps1 | iex
```

**Warning: The windows build does NOT include the embedded Vampire prover build due to
preexisting compilation errors. This is future work**

### Build from source

Only use this method if you intend on modifying your build. You will be responsible for maintaining provenance over your updates.

To compile from source, first install 
[Rust](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then clone this repository:

```bash

git clone https://github.com/ontologyportal/sigma-rs && cd sigma-rs
```

Compile everything (Cargo fetches the Vampire C++ bindings directly from their git repo as an ordinary dependency):

```bash
cargo build --release --bin sumo
```

For **Windows**, you have to exclude the `integrated-prover`
feature:

```powershell
cargo build --release --bin sumo --no-default-features --features ask,parallel,alloc-mi
```

The executable is located in `target/release/sumo`. You can link it to your system PATH using (UNIX):

```bash
sudo ln -s $PWD/target/release/sumo /usr/local/bin/sumo
```

For Windows, you have to manually add it to your PATH or set
up a PowerShell alias.

---

## Workspace layout

| Crate | Description |
|---|---|
| `crates/core` (`sigmakee-rs-core`) | Core library for the Sigmakee implementation |
| `crates/sdk` (`sigmakee-rs-sdk`) | SDK which makes software consumption of `sigmakee-rs-core` more intuitive |
| `crates/cli` (`sigmakee`) | Command line interface for SUMO, builds the `sumo` executable |
| `crates/lsp` (`sumo-lsp`) | Persistent language server for IDE integration |
| `crates/wasm` (`sumo-parser-wasm`) | WASM bindings (browser / Node.js) (BROKEN) |

---

## Quick start

```bash
# 1. Initialize a config.xml file with default options
sumo config --declare --kb SUMO -f Merge.kif -f Mid-level-ontology.kif

# 2. Fetch the ontology from the SUMO GitHub 
sumo -c --git https://github.com/ontologyportal/sumo load

# 3. Ask a conjecture
sumo ask "(instance Socrates Human)"

# 4. Assert facts then ask
sumo ask --tell "(instance Socrates Philosopher)" \
  "(instance Socrates Human)"

# 5. Dump the KB as TFF TPTP
sumo translate --lang tff > sumo.p

# 6. Look up information about a symbol
sumo man Socrates

# 7. Search for a term in the ontology
sumo search Philosopher

# 8. Check if any of the KB constituents have changed since
# the last time you loaded them into your KB
sumo check
```

---

## CLI reference

### To cache or not to cache, that is the question

The `sigmakee` CLI is highly optimized to amortize runtime efficiency over multiple
KB accesses. To get the FULL effect of these optimizations, you should cache your KB
to an LMDB database file prior to running any of the CLI's commands.

Running the `load` subcommand, you can pass any number of constituent files manually using
the `-f` flag, whole directories of `.kif` files using the `-d` or use the files listed
in your `config.xml` using the `-c` flag. By default, `load` will write the compiled
cache to the current directory in a file called `sumo.lmdb`. You can change the DB location
and name using the `--db` flag.

By default, all other commands will first look for a cached DB in the `editDir` directory (as specified in your `config.xml`)
or at the location specified by the `--db` flag. If you do
not have a `sumo.lmdb` and you do not specify one using `--db`, or if you use the
`--no-db` flag, it will perform all operations in memory and will parse any files you
manually pass to the command at runtime without writing it to disk. ONLY `load` write to disk.

So, the following command will translate a single file to TPTP and nothing else:

```bash
sumo --no-db -f Merge.kif translate
```

Whereas this command will first cache the file to disk then use 
that cache to generate the TPTP translation:

```bash
sumo -f Merge.kif load
sumo translate
```

`load` defaults to **per-file reconcile** semantics: each `-f`/`-d` file is diffed
against its prior contents in the DB and only the delta is committed. Files unrelated
to the supplied set stay untouched. This makes repeat loads idempotent and cheap:

```bash
sumo -f /path/to/modified/file.kif --db sumo.lmdb load
```

updates only that one file in the cache. Pass `--flush` to drop the entire DB and
rewrite it from just the supplied files (the pre-reconcile "full rewrite" behaviour):

```bash
sumo -f Merge.kif --db sumo.lmdb load --flush
```

This will use all the constituent files in the KB SUMO in your 
config.xml to overwrite the corresponding files already persisted in your LMDB stored KB ONLY for the single CLI invocation.

```bash
sumo -c ask "(instance Socrates Human)"
```

### Global flags

| Flag | Default | Description |
|---|---|---|
| `-v` / `--verbose` | — | Logging verbosity: `-v` = info, `-vv` = debug, `-vvv` = trace |
| `-q` / `--quiet` | — | Suppress all warnings |
| `-c` | — | Use the `config.xml` for options and KB constituents |
| `--config PATH` | — | Path to a SigmaKEE `config.xml` or the directory containing it |
| `--kb NAME` | — | Knowledge-base name from `config.xml` to load (requires `-c`); with `sumo config`, the KB to edit (see below) |
| `-W CODE\|all` / `--warning CODE\|all` | — | Promote semantic warning `CODE` (e.g. `E005`, `arity-mismatch`) or `all` to a hard error (repeatable) |
| `--exclude PATH` | — | With `sumo config --kb NAME`: remove a constituent from that KB (repeatable). No effect elsewhere |
| `--declare` | — | With `sumo config --kb NAME -f/-d ...`: skip the existence check when adding constituents. No effect elsewhere |

### Shared KB arguments (`-f`, `-d`, `--db`, `--no-db`, `--git`)

These flags are available on every subcommand:

| Flag | Default | Description |
|---|---|---|
| `-f` / `--file FILE` | — | KIF file to load (repeatable) |
| `-d` / `--dir DIR` | — | Directory of `*.kif` files to load (repeatable) |
| `--db DIR` | `./sumo.lmdb` | Path to the LMDB database directory |
| `--no-db` | — | Skip the LMDB database entirely — do not open or warn about it. Useful when running without a pre-built database |
| `--git URL` | — | Git repository URL to load the ontology from. With `load`: clones and commits to the LMDB database (cached). With other commands: clones on the fly into a temporary directory. `-f` / `-d` / `-c` paths are resolved relative to the repository root |

`--vampire PATH` is exposed only on the prover-driven subcommands —
`ask`, `test`, `audit`, and `serve`. Defaults to the `vampire` binary
on `PATH`; if `--config` is active and the config specifies a vampire
path, that takes precedence over the `PATH` lookup.

### `sumo validate`

Parse KIF files and semantically validate every formula.

```
sumo validate [FORMULA] [--parse] [--no-kb-check] [-f FILE]... [-d DIR]... [--db DIR]
```

- **With `-f`/`-d` files** — parse → validate.
- **With `FORMULA` only** — validate the formula against the loaded KB.
- **No files, no formula** — re-validate every formula already in the database.

| Flag | Default | Description |
|---|---|---|
| `FORMULA` | — | Inline KIF formula to validate against the KB |
| `--parse` | — | Parse-only validation; skip semantic checks entirely |
| `--no-kb-check` | — | Do not semantically validate loaded KB files; only check the inline `FORMULA` (parse errors in KB files are still reported) |

### `sumo ask`

Prove a KIF conjecture using Vampire.

```
sumo ask [FORMULA] [-t KIF]... [--timeout SECS] [--session KEY] [--backend NAME]
         [--lang fof|tff] [-k FILE] [--proof FORMAT] [--profile]
```

| Flag | Default | Description |
|---|---|---|
| `FORMULA` | stdin | KIF conjecture to prove |
| `-t` / `--tell KIF` | — | Assert a KIF formula into the KB before asking (repeatable). Committed under `--session` |
| `--timeout SECS` | `30` | Vampire proof-search timeout |
| `--session KEY` | `default` | Session key for `--tell` assertions and TPTP hypothesis filtering |
| `--backend NAME` | `subprocess` | Prover backend: `subprocess` (external `vampire` binary) or `embedded` (in-process, requires the `integrated-prover` build feature) |
| `--lang fof\|tff` | `fof` | TPTP language variant |
| `-k` / `--keep FILE` | — | Write the generated TPTP to `FILE` instead of piping it directly to Vampire (for debugging) |
| `--proof FORMAT` | — | Print proof steps when Vampire finds one. `FORMAT` is `tptp`, `kif`, or a SUMO language tag (e.g. `EnglishLanguage`) — see below |
| `--profile` | — | Print a timing breakdown of the major pipeline phases |

Exits `0` if the theorem is proved, `1` otherwise.

**`--proof FORMAT` values:**

- `tptp` — raw TSTP proof section as emitted by Vampire (no translation).
- `kif` — SUO-KIF pretty-print of each step's formula.
- Any SUMO language identifier (`EnglishLanguage`, `ChineseLanguage`, …) — natural-language rendering via the KB's `format` / `termFormat` relations. Steps whose formulas reference a symbol that lacks a language spec fall back to KIF for that step only, with a warning listing the missing specifiers.

### `sumo translate`

Emit TPTP from the KB.

```
sumo translate [FORMULA] [--lang fof|tff] [--show-numbers] [--show-kif]
               [--session KEY] [-f FILE]... [-d DIR]... [--db DIR]
```

Without `--db` (or with `--db` pointing to a non-existent path) and with
`-f` / `-d` files supplied, parses in-memory and emits TPTP FOF. With an
existing `--db`, reads CNF from the database and emits TPTP CNF;
any `-f` / `-d` / inline `FORMULA` is treated as a session assertion.

| Flag | Default | Description |
|---|---|---|
| `FORMULA` | stdin | Inline KIF formula to translate |
| `--lang fof\|tff` | `fof` | TPTP language variant (legacy in-memory mode only) |
| `--show-numbers` | — | Emit numeric literals as-is instead of `n__N` tokens |
| `--show-kif` | — | Emit a `% <original KIF>` comment before each TPTP formula |
| `--session KEY` | — | Session key controlling which assertions appear as TPTP hypotheses |

### `sumo test`

Run KIF test files (`*.kif.tq` format).

```
sumo test PATH... [-k FILE] [--backend NAME] [--lang fof|tff]
          [--timeout SECS] [--profile] [-f FILE]... [-d DIR]... [--db DIR]
```

Test files are KIF-like but may contain special directives: `(note "…")`, `(time N)`, `(answer yes|no)`, `(query FORMULA)`. Everything else is treated as an axiom.

| Flag | Default | Description |
|---|---|---|
| `PATH` | — | Path to a `.kif.tq` file or a directory containing them. Multiple paths accepted; shell globs are expanded |
| `-k` / `--keep FILE` | — | Write generated TPTP to `FILE` (for debugging) |
| `--backend NAME` | `subprocess` | Prover backend: `subprocess` or `embedded` |
| `--lang fof\|tff` | `fof` | TPTP language variant |
| `--timeout SECS` | — | Override the per-test timeout, superseding any `(time N)` directive inside the test file |
| `--profile` | — | Print a timing breakdown of the major pipeline phases |

### `sumo load`

Parse KIF files and commit them to the LMDB database. **The only command that writes to the database besides `sumo serve`.**

```
sumo load [--flush] [-f FILE]... [-d DIR]... [--db DIR]
```

Validates all loaded formulas before committing — parse errors or promoted warnings (`-W`) abort the commit and leave the database unchanged. If no files are given, the database is created / opened but left empty.

**Default (reconcile mode)** — per-file diff + incremental commit. Each `-f`/`-d` file is diffed against its prior contents in the DB under the same file tag, and only the delta (added + removed sentences) is committed. Files unrelated to the supplied set stay untouched. Idempotent — safe to run repeatedly. Cheap when nothing has changed.

**`--flush`** — drop the entire DB and rewrite it from just the supplied `-f`/`-d` files. With no files, the result is an empty initialised database. Use when the DB has accumulated stale axioms from earlier loads and you want to start clean.

| Flag | Default | Description |
|---|---|---|
| `--flush` | — | Drop the whole DB and rebuild from just the supplied files |

### `sumo man`

Show documentation, signature, and taxonomy for a symbol — the KIF-native equivalent of `man(1)`. Everything surfaced is extracted from the ontology-level relations `documentation`, `termFormat`, `format`, plus `subclass` / `instance` / `domain` / `range` declarations.

```
sumo man SYMBOL [--lang LANG] [-P] [-f FILE]... [-d DIR]... [--db DIR]
```

| Flag | Default | Description |
|---|---|---|
| `SYMBOL` | — | Symbol to describe (e.g. `Human`, `subclass`, `instance`) |
| `--lang LANG` | — | Filter documentation / term-format entries by language tag (e.g. `EnglishLanguage`). When omitted, entries in all languages are shown |
| `-P` / `--no-pager` | — | Disable the interactive pager; print directly to stdout. Pager is also disabled automatically when stdout is not a TTY or when `NO_PAGER` is set |

### `sumo audit`

Consistency-check a single loaded KIF file against the rest of the
knowledge base via Vampire, surfacing any axioms that contradict each
other.

```
sumo audit <FILE> [--thoroughness F] [--scope F] [--timeout SECS] [-k FILE]
                [--proof FORMAT] [--vampire PATH]
                [-f FILE]... [-d DIR]... [--db DIR]
```

Flow: collect the sentences of `<FILE>` (must already be in the KB →
pass `-f` / `-d` the same way as other subcommands) → randomly
subsample by `--thoroughness` → SInE-expand from the sampled
sentences' symbols at the configured `--scope` tolerance → feed the
union to Vampire with no conjecture (pure axiom-satisfiability) → if
Vampire reports `ContradictoryAxioms`, trace each axiom-role step in
the refutation back to its source `file:line`.

| Flag | Default | Description |
|---|---|---|
| `FILE` | — | Path to a `.kif` file already loaded into the KB. Tag matched case-sensitively against the loaded tags |
| `--thoroughness F` | `1.0` | Fraction of root sentences to sample, in `(0.0, 1.0]`. Smaller = faster, less coverage |
| `--scope F` | crate default | SInE tolerance factor (≥ 1.0) for axiom expansion. Higher = more thorough, more expensive |
| `--timeout SECS` | `60` | Vampire proof-search timeout |
| `-k` / `--keep FILE` | — | Write generated TPTP to `FILE` (for debugging) |
| `--proof FORMAT` | — | Print the full refutation proof when one is found (same `FORMAT` values as `ask`) |

Uses TPTP FOF (TFF is not currently wired through `debug`).

### `sumo update`

Update the `sumo` binary to the latest official release, OR (for
source builds) report the latest available version and recommend the
right rebuild incantation. Release CI sets `SUMO_BUILD_KIND=release`
at build time; everything else defaults to `source`, and source builds
intentionally never overwrite themselves (replacing a developer's
local build with an upstream binary would be surprising).

```
sumo update [--check]
```

| Flag | Default | Description |
|---|---|---|
| `--check` | — | Don't apply the update — just check upstream and report |

### `sumo serve`

Run as a persistent kernel: reads newline-delimited JSON requests from stdin and writes responses to stdout. Owns one long-lived `KnowledgeBase` in memory so every request amortises the load cost — designed for editor integrations (see the VSCode extension under `crates/sumo-vscode/`).

```
sumo serve [-f FILE]... [-d DIR]... [--db DIR | --no-db] [--vampire PATH]
```

**Default (`--db`)** — opens the LMDB at `--db` (creating if absent) and reconciles every `-f`/`-d` file against it on boot. Subsequent kernel spawns on an unchanged KB are near-instant (no-op reconciles detect unchanged content). Running RPC methods that mutate the KB (`kb.reconcileFile`, `kb.removeFile`, `kb.flush`) update the DB transactionally.

**`--no-db`** — everything in memory; `-f`/`-d` files load as session axioms and vanish when the process exits.

The kernel's RPC surface is:

| Method | Params | Description |
|---|---|---|
| `tell` | `{ session, kif }` | Session-local assertion (ephemeral) |
| `ask` | `{ session, query, timeoutSecs }` | Run a conjecture through Vampire |
| `debug` | `{ path, thoroughness?, scope?, timeoutSecs? }` | RPC counterpart of the `debug` subcommand |
| `test` | `{ paths }` | Run `.kif.tq` test files and report pass/fail per case |
| `kb.reconcileFile` | `{ path, text? }` | Sync one file from disk (or inline text) into the DB |
| `kb.removeFile` | `{ path }` | Drop one file from the in-memory KB + DB |
| `kb.flush` | `{}` | Wipe all persisted files (in-memory + DB) |
| `kb.listFiles` | `{}` | Return loaded files + sentence counts |
| `kb.generateTptp` | `{ session?, lang?, showKif?, hideNumbers? }` | Emit TPTP for the current KB state |
| `shutdown` | `{}` | Clean exit |

Semantic warnings never populate the reconcile report's `semanticErrors` list by default — they're logged to stderr via the standard `SemanticError::handle` path. Run `sumo -W <code> serve` (or `-W all`) to promote specific warnings to hard errors that the RPC caller sees.

### `sumo config`

Inspect or edit the resolved `KBManager` configuration (config.xml).

```
sumo config
sumo config --<setting> VALUE ...
sumo config --kb NAME [-f FILE]... [-d DIR]... [--exclude PATH]... [--declare]
```

Three modes, chosen by the arguments given:

- **No flags** — print every option, its current value, which CLI flag(s) and config.xml key(s) it maps to, and the configured knowledge bases + their constituents. If run in an interactive terminal with a resolved config.xml, this instead opens a `ratatui`-based TUI for browsing and editing in place (options, and KBs/constituents — add, delete, or create a KB); press `s` to save, `q` to quit. Non-interactive invocations (e.g. piped output) always fall back to the read-only dump.
- **`--<setting> VALUE`** — patch one or more scalar options (e.g. `--timeout 60`, `--vampire /path/to/vampire`) and write the result back to config.xml. Every option `sumo config`'s read-only dump lists is settable this way; the same table shows each one's flag name.
- **`--kb NAME` with `-f`/`-d`/`--exclude`** — edit that KB's constituent list instead of a scalar option, creating the KB if it doesn't exist yet. `-f`/`-d` add constituents (existence-checked and deduplicated by resolved path unless `--declare` is passed); `--exclude PATH` removes one, matched by the exact path shown in the dump's "Knowledge bases" listing. Adds and removes may be combined in one invocation; only one KB may be edited per invocation.

```bash
# Add a constituent to (or create) the KB "SUMO"
sumo config --kb SUMO -f Merge.kif

# Declare a constituent before it's actually fetched (skips the existence check)
sumo config --declare --kb SUMO -f Mid-level-ontology.kif

# Remove a constituent
sumo config --kb SUMO --exclude Merge.kif

# Patch a scalar option
sumo config --timeout 60
```

config.xml is always rewritten in full on a write (comments/formatting/element order from a hand-edited file aren't preserved); any `<preference>` key this build doesn't recognize is round-tripped verbatim rather than dropped.

---

## Prover knobs

The native prover (`crates/core/src/prover/saturate/`) is tuned by two structs:

- **`Strategy`** — search-shaping knobs: queues, weights, caps, which inference
  mechanisms are on, precedence, portfolio-lane genome. Lives at
  `NativeOpts.strategy` / `NativeProverConfig.strategy`.
- **`NativeOpts`** — everything else: step/time budgets, SInE selection, proof
  rendering, and whole-attempt discharge subsystems (model-join, event-calculus,
  backward-chaining) that run as a prologue before the given-clause loop.

Only a subset of these are wired all the way through to `KBManager` (the
config.xml/CLI-facing struct). Where a knob has no `KBManager` field, it can
only be set via its environment variable — there is no config.xml key or CLI
flag for it.

### `Strategy` fields (env-var A/B overrides only — no CLI flag, no config.xml key)

These `Strategy::from_env()` switches are process-global kill switches used for
A/B measurement, not user-facing settings. The *whole* `Strategy` struct **is**
settable as one nested JSON object — via `--strategy` (inline JSON or a JSON
file, see below), or config.xml's `<prover type="native"><preference
name="strategy" value='{"schema":false,...}'/></prover>` (every field name
below is the literal JSON key) — or generated wholesale by
`sumo sweep --configs FILE.json` / `--random N --seed S`.

| Strategy field | Env var | Effect |
|---|---|---|
| `schema` | `SIGMA_NO_SCHEMA` | off → disables schema simplification (on by default) |
| `decode` | `SIGMA_NO_DECODE` | off → disables decode simplification (on by default) |
| `demod` | `SIGMA_DEMOD` | on → enables forward demodulation (off by default outside `tptp()`) |
| `bwd_demod` | `SIGMA_NO_BWD_DEMOD` / `SIGMA_BWD_DEMOD` | backward demodulation (on by default) |
| `subs_join` | `SIGMA_NO_SUBS_JOIN` / `SIGMA_SUBS_JOIN` | subsumption-join |
| `subterm_rows` | `SIGMA_NO_SUBTERM_ROWS` / `SIGMA_SUBTERM_ROWS` | subterm-row indexing |
| `recognize_roles` | `SIGMA_RECOGNIZE_ROLES` | on → shape-recognized taxonomy roles |
| `rule_join` | `SIGMA_NO_RULE_JOIN` | off → disables Horn rule-join discharge (on by default) |
| `goal_dist` | `SIGMA_GOALDIST` | on → goal-distance clause weighting |
| `liu_rescue` / `def_completion` | `SIGMA_NO_LIU` | off → disables both Liu rescue and definition completion |
| `head_filter` | `SIGMA_HEADFILTER` | on → head-symbol filtering |
| `bg_snapshot` | `SIGMA_NO_BG_SNAPSHOT` | off → disables background-KB snapshotting (on by default) |
| `semantic_guide` | `SIGMA_GUIDE` | on → semantic-model search guidance |
| `modal_k` | `SIGMA_NO_MODAL_K` | off → disables modal-K handling (on by default) |
| `deferred_passive` | `SIGMA_NO_DEFERRED_PASSIVE` / `SIGMA_DEFERRED_PASSIVE` | deferred-passive queue discipline; also gates whether the `tptp-deferred` portfolio lane exists |
| `split_naming` | `SIGMA_NO_SPLIT` / `SIGMA_SPLIT` | naming-split lane |
| `split_width` | `SIGMA_SPLIT_WIDTH=N` | naming-split width |
| `deferred_cap` | `SIGMA_DEFERRED_CAP=N` | deferred-passive queue cap |

All other `Strategy` fields (`tier_weight`, `pick_ratio`, `cw_*`, `lit_select`,
`max_depth`, `max_term_size`, `para_cap`, `demod_cap`, `bwd_demod_cap`,
`prec_seed`, `fc_*`, `bg_completion*`, `liu_rounds`/`liu_top_k`,
`defcomp_*`, `ordered_resolution`, `subsumption`, `superposition`,
`eq_factoring`, `full_saturation`, `strict_saturation`) have no individual env
var at all — they're only reachable via the nested `strategy` JSON object
(`--strategy` / config.xml), or drawn by `Strategy::sample(seed)` for
GA/portfolio sweep genomes.

### `NativeOpts` fields exposed through `KBManager` → config.xml → CLI

| `NativeOpts` field | `KBManager`/`NativeProverConfig` field | config.xml key (`<prover type="native">`) | CLI flag |
|---|---|---|---|
| `max_steps` | `native_prover.max_steps` | `maxSteps` | `--max-steps` |
| `max_lits` | `native_prover.max_lits` | `maxLits` | `--max-lits` |
| `time_limit_secs` | `native_prover.time_limit_secs` | `timeLimitSecs` | `--timeout` (shared with `external_prover.timeoutSecs`) |
| `forward_close` | `native_prover.forward_close` | `forwardClose` | `--forward-close` |
| `want_proof` | `native_prover.want_proof` | `wantProof` | `--want-proof` |
| `step` | `native_prover.step` | `step` | `--step` (on `ask`/`test` only) |
| `selection` (`SineParams.tolerance`) | `native_prover.selection.tolerance` | `selection` (nested) | `--scope` |
| `selection` (`SineParams.autoscale`) | `native_prover.selection.autoscale` | `selection` (nested) | `--autoscale` |
| `strategy` | `native_prover.strategy` | `strategy` (nested, see above) | `--strategy <JSON\|FILE>` on `ask`/`test`/`audit`/`sweep` (`native-prover` build only) |
| `profile` | `native_prover.profile` | `profile` | `--profile` — hand-declared **global** flag (`main.rs`), not projected through the `OptionMeta` table; sets `manager.native_prover.profile` directly |

`disable_selection` isn't a `NativeOpts` field itself — it's a `KBManager`-only
flag (`--full-kb` / config.xml `disableSelection`) that `KBManager::native_opts()`
folds in as `opts.selection.select_all |= self.disable_selection` when building
the `NativeOpts` the prover actually runs with.

**`--strategy`** takes either inline JSON (`--strategy '{"schema":false,"para_cap":400}'`)
or a path to a `.json` file holding the same object; either way it's a
*partial* spec — any `Strategy` field it omits keeps its default (`Strategy`'s
`#[serde(default)]`). Invalid JSON, or a named file that can't be read, is a
clap argument error (reported immediately, before the KB even loads). This
also persists correctly through `sumo config --strategy ...` — see below.

### `NativeOpts` fields with **no** `KBManager`/config.xml/CLI mirror (env-only)

These whole-attempt discharge subsystems and runtime knobs can only be set via
their environment variable — there is no way to reach them from config.xml or
a CLI flag:

| `NativeOpts` field | Env var | Default | Effect |
|---|---|---|---|
| `model` | `SIGMA_MODEL` | off (set-if-present) | Enable the Datalog(¬) model-join discharge prologue |
| `model_budget` | `SIGMA_MODEL_BUDGET` | `250_000` | Per-eval tuple budget for model discharge |
| `model_ms` | `SIGMA_MODEL_MS` | `800` | Wall-clock cap (ms) for model discharge |
| `ec` | `SIGMA_EC` | off (set-if-present) | Enable the event-calculus discharge prologue |
| `backward` | `SIGMA_BACKWARD` | off (set-if-present) | Enable the backward-chaining discharge prologue |
| `backward_ms` | `SIGMA_BACKWARD_MS` | `800` | Wall-clock cap (ms) for backward chaining |
| `backward_nodes` | `SIGMA_BACKWARD_NODES` | `200_000` | Search-node cap for backward chaining |
| `cores` | `SIGMA_CORES` | `available_parallelism()` | Portfolio-lane worker-thread cap (concurrent TPTP lane racing) |

`max_lits` also has a `SIGMA_MAX_LITS` env override applied post-construction
in `NativeProver::new` (distinct from, and layered on top of, the CLI/config
`--max-lits` path above).

### Other diagnostic / tracing env vars

Debug and trace switches, not search-shaping tunables: `SIGMA_NO_PORTFOLIO`,
`SIGMA_STATS`, `SIGMA_SELECT_DUMP`, `SIGMA_SELECT_GREP`, `SIGMA_DISJOINT_DECOMP`,
`SIGMA_DISJOINT_ALWAYS`, `SIGMA_HINTS`, `SIGMA_HINTS_DEBUG`, `SIGMA_WIDTH_DUMP`,
`SIGMA_FLOOD_DUMP`, `SIGMA_GATE0_DUMP`, `SIGMA_LIU_TRACE`, `SIGMA_ORACLE_TRACE`,
`SIGMA_TEMPORAL`, `SIGMA_MAGIC_TRACE`, `SIGMA_MODEL_TRACE`, `SIGMA_HEADLINE_DIAG`,
`SIGMA_NO_ARENA`, `SIGMA_SCALE_TRACE`, `SIGMA_ALL_LANES`, `SIGMA_PROOF_TRACE`,
`SIGMA_RESIDUE_DECODE`, `SIGMA_REACTOR_PROFILE`, `SIGMA_STEP` (also settable
via `NativeOpts.step`/`--step`, but the env var is read independently by the
`stepdbg` module).

---

## Environment variables

| Variable | Description |
|---|---|
| `SIGMA_HOME` | Path to a SigmaKEE checkout. When `-c` is passed without `--config`, the CLI looks for `$SIGMA_HOME/KBs/config.xml` |
| `NO_PAGER` | When set (to any value), `sumo man` skips the interactive pager and prints directly to stdout |
| `SINE_TOLERANCE` | Compile-time override (read by the build script of `sigmakee-rs-core`) for the default SInE tolerance factor. Defaults to `2.0` |
| `SUMO_BUILD_KIND` | Compile-time tag (set by release CI) controlling whether `sumo update` is allowed to overwrite the binary. Defaults to `source` |

---

## Running tests

```bash
# Unit and integration tests (no external dependencies)
cargo test

# Lib-only tests for the core crate
cargo test --lib -p sigmakee-rs-core
```
