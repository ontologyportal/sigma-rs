# SigmaKEE-rs

A parser, validator, and theorem-prover interface for the [SUO-KIF](https://www.ontologyportal.org/suo-kif.pdf) / [SUMO](https://www.ontologyportal.org/) knowledge representation language.

KIF files are parsed once and committed to an [LMDB](https://www.symas.com/lmdb) database. Formulas are stored in Conjunctive Normal Form (CNF) with full Skolemization so that subsequent theorem-prover queries require no runtime conversion. The [Vampire](https://vprover.github.io/) prover is used for automated reasoning.

## Install

The plan is to ultimately place this on crates.io so a user with Rust installed would just have to run:

```bash
cargo install sigmakee
```

Today, there are two installation options:

1. Use Github releases to install a native binary. Rust statically links all their dependencies
so you do not need to install anything other than copying the binary to your machine. Choose your
correct architecture (`amd64`, `aarch64`, etc).

2. Compile from source. 

To compile from source, first install Rust:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

When clone this repository:

```bash

git clone https://github.com/ontologyportal/sigma-rs && cd sigma-rs
```

Then initialize the git submodules:

```bash
$ git submodule update --recursive --init
```

Finally, compile everything:

```bash
cargo build --release
```

The executable is located in `target/release/sumo`. You can link it to your PATh using:

```bash
sudo ln -s $PWD/target/release/sumo /usr/local/bin/sumo
```


---

## Workspace layout

| Crate | Description |
|---|---|
| `crates/core` (`sigmakee-rs-core`) | Core library for the Sigmakee implementation |
| `crates/sdk` (`sigmakee-rs-sdk`) | SDK which makes software consumption of `sigmakee-rs-core` more intuitive |
| `crates/cli` (`sigmakee`) | Command line interface for SUMO, builds the `sumo` executable |
| `crates/lsp` (`lsp`) | Persistent language server for IDE integration |
| `crates/wasm` (`sumo-parser-wasm`) | WASM bindings (browser / Node.js) |

---

## Quick start

```bash
# 1. Parse KIF files specified in you config.xml into a cached database, 
# by default the cached database is ./sumo.lmdb
# NOTE: it will look for your config.xml in $SIGMA_HOME/KBs/config.xml. If you 
# have your config.xml somewhere else, pass the path using --config 
sumo -c load

# 2. Ask a conjecture
sumo ask "(instance Socrates Human)"

# 3. Assert facts then ask
sumo ask --tell "(instance Socrates Philosopher)" \
  "(instance Socrates Human)"

# 4. Dump the KB as TFF TPTP
sumo --lang tff translate > sumo.p

# 5. Look up information about a symbol
sumo man Socrates
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

By default, all other commands will first look for a cached DB either in your current
directory (`./sumo.lmdb`) or at the location specified by the `--db` flag. If you do
not have a `sumo.lmdb` and you do not specify one using `--db`, or if you use the
`--no-db` flag, it will perform all operations in memory and will parse any files you
manually pass to the command at runtime without writing it to disk. ONLY `load` and
`serve` (the persistent kernel) write to disk.

So, the following command will translate a single file to TPTP and nothing else:

```bash
sumo --no-db -f Merge.kif translate
```

Whereas this command will first cache the file to disk then use that cache to
generate the TPTP translation:

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

### Global flags

| Flag | Default | Description |
|---|---|---|
| `-v` / `--verbose` | — | Logging verbosity: `-v` = info, `-vv` = debug, `-vvv` = trace |
| `-q` / `--quiet` | — | Suppress all warnings |
| `-c` | — | Use the `config.xml` for options and KB constituents |
| `--config PATH` | — | Path to a SigmaKEE `config.xml` or the directory containing it |
| `--kb NAME` | — | Knowledge-base name from `config.xml` to load (requires `-c`) |
| `-W CODE\|all` / `--warning CODE\|all` | — | Promote semantic warning `CODE` (e.g. `E005`, `arity-mismatch`) or `all` to a hard error (repeatable) |

### Shared KB arguments (`-f`, `-d`, `--db`, `--no-db`, `--vampire`)

These flags are available on every subcommand:

| Flag | Default | Description |
|---|---|---|
| `-f` / `--file FILE` | — | KIF file to load (repeatable) |
| `-d` / `--dir DIR` | — | Directory of `*.kif` files to load (repeatable) |
| `--db DIR` | `./sumo.lmdb` | Path to the LMDB database directory |
| `--no-db` | — | Skip the LMDB database entirely — do not open or warn about it. Useful when running without a pre-built database |
| `--vampire PATH` | `vampire` | Path to the Vampire executable. If a config.xml file is specified, it will derive this setting from their by default |

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
sumo man SYMBOL [--lang LANG] [-f FILE]... [-d DIR]... [--db DIR]
```

| Flag | Default | Description |
|---|---|---|
| `SYMBOL` | — | Symbol to describe (e.g. `Human`, `subclass`, `instance`) |
| `--lang LANG` | — | Filter documentation / term-format entries by language tag (e.g. `EnglishLanguage`). When omitted, entries in all languages are shown |

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
| `kb.reconcileFile` | `{ path, text? }` | Sync one file from disk (or inline text) into the DB |
| `kb.removeFile` | `{ path }` | Drop one file from the in-memory KB + DB |
| `kb.flush` | `{}` | Wipe all persisted files (in-memory + DB) |
| `kb.listFiles` | `{}` | Return loaded files + sentence counts |
| `shutdown` | `{}` | Clean exit |

Semantic warnings never populate the reconcile report's `semanticErrors` list by default — they're logged to stderr via the standard `SemanticError::handle` path. Run `sumo -W <code> serve` (or `-W all`) to promote specific warnings to hard errors that the RPC caller sees.

---

## Environment variables

| Variable | Description |
|---|---|
| `SUMO_MAX_CLAUSES` | Default CNF clause limit per formula (overridden by `--max-clauses`) |
| `SIGMA_HOME` | Path to a SigmaKEE checkout (used by integration tests) |
| `SIGMA_CP` | Java classpath for Java-comparison integration tests |

---

## Running tests

```bash
# Unit and integration tests (no external dependencies)
cargo test

# Java-comparison integration tests (requires SIGMA_CP)
SIGMA_CP=/path/to/sigma.jar cargo test -p sumo-parser-core --test java_comparison
```
