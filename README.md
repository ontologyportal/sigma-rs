# sumo-parser

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
$ curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

When clone this repository:

```bash

$ git clone https://github.com/ontologyportal/sigma-rs && cd sigma-rs
```

Then initialize the git submodules:

```bash
$ git submodule update --recursive --init
```

Finally, compile everything:

```bash
$ cargo build --release
```

The executable is located in `target/release/sumo`. You can link it to your PATh using:

```bash
sudo ln -s $PWD/target/release/sumo /usr/local/bin/sumo
```


---

## Workspace layout

| Crate | Description |
|---|---|
| `crates/core` (`sumo-parser-core`) | Parser, tokenizer, in-memory `KifStore`, semantic validator, TPTP FOF emitter |
| `crates/store` (`sumo-store`) | LMDB-backed persistent store: symbol interning, CNF conversion, path index, TPTP CNF emitter |
| `crates/native` (`sumo-native`) | CLI binary and library wrapping core + store |
| `crates/wasm` (`sumo-parser-wasm`) | WASM bindings (browser / Node.js) |

---

## Quick start

```bash
# 1. Parse KIF files into the database (run once, like a SQL migration)
sumo validate -f base.kif -f domain.kif --db ./my.lmdb

# 2. Ask a conjecture
sumo ask "(instance Socrates Human)" --db ./my.lmdb

# 3. Assert facts then ask
sumo ask "(instance Socrates Human)" \
     --tell "(instance Socrates Philosopher)" \
     --session demo --db ./my.lmdb

# 4. Dump the KB as TPTP CNF
sumo translate --db ./my.lmdb

# 5. Translate a single formula in-memory (no database required)
sumo translate -f base.kif "(instance Socrates Human)"
```

---

## CLI reference

### Global flags

| Flag | Default | Description |
|---|---|---|
| `-v` / `--verbose` | — | Logging verbosity: `-v` = info, `-vv` = debug, `-vvv` = trace |
| `-q` / `--quiet` | — | Suppress all warnings |
| `--config PATH` | — | Path to a SigmaKEE `config.xml` or the directory containing it |
| `--kb NAME` | — | Knowledge-base name from `config.xml` to load |
| `-W CODE\|all` | — | Treat warning `CODE` (e.g. `E005`) or all warnings as errors |

### Shared KB arguments (`-f`, `-d`, `--db`, `--max-clauses`, `--vampire`)

These flags are available on every subcommand:

| Flag | Default | Description |
|---|---|---|
| `-f FILE` | — | KIF file to load (repeatable) |
| `-d DIR` | — | Directory of `*.kif` files to load (repeatable) |
| `--db DIR` | `./sumo.lmdb` | Path to the LMDB database directory |
| `--max-clauses N` | `10000` | Hard upper bound on CNF clauses per formula. Also read from `SUMO_MAX_CLAUSES` env var |
| `--vampire PATH` | `vampire` | Path to the Vampire executable |

### `sumo validate`

Parse KIF files, validate every formula, and commit to `--db`.

```
sumo validate [FORMULA] [-f FILE]... [-d DIR]... [--db DIR]
```

- **With `-f`/`-d` files** — parse → validate → commit. The database becomes the canonical store.
- **With `FORMULA` only** — validate the formula against the existing database.
- **No files, no formula** — re-validate every formula already in the database.

### `sumo ask`

Prove a KIF conjecture using Vampire.

```
sumo ask [FORMULA] [--tell KIF]... [--timeout SECS] [--session KEY] [--keep] [--db DIR]
```

| Flag | Default | Description |
|---|---|---|
| `FORMULA` | stdin | KIF conjecture to prove |
| `--tell KIF` | — | Assert a formula into the KB before asking (repeatable). Committed under `--session` |
| `--timeout SECS` | `30` | Vampire proof-search timeout |
| `--session KEY` | `default` | Session key for `--tell` assertions |
| `--keep` | — | Keep the generated TPTP file instead of deleting it |

Exits `0` if the theorem is proved, `1` otherwise.

### `sumo translate`

Emit TPTP from the KB.

```
sumo translate [FORMULA] [--lang fof|tff] [--show-numbers] [--session KEY] [--db DIR]
```

**DB mode** (database exists at `--db`): reads pre-computed CNF clauses from LMDB and emits TPTP CNF. Any `-f`/`-d`/`FORMULA` input is committed as a session assertion first.

**Legacy in-memory mode** (no database): parses `-f`/`-d` files in memory and emits TPTP FOF.

| Flag | Default | Description |
|---|---|---|
| `--lang fof\|tff` | `fof` | TPTP language variant (legacy in-memory mode only) |
| `--show-numbers` | — | Emit numeric literals as-is instead of `n__N` tokens |
| `--session KEY` | — | Filter TPTP output to a specific session |

### `sumo test`

Run KIF test files (`*.kif.tq` format).

```
sumo test PATH [-f FILE]... [-d DIR]... [--keep]
```

Test files are KIF-like but may contain special directives: `(note "…")`, `(time N)`, `(answer yes|no)`, `(query FORMULA)`. Everything else is treated as an axiom.

**TODO: Add `man` and `debug` command references**

---

## `sumo-store` API

The `sumo-store` crate is the persistence layer. It is usable independently of the CLI.

### Opening the database

```rust
use sumo_store::LmdbEnv;

let env = LmdbEnv::open("./my.lmdb")?;
```

Creates the directory if it does not exist. The map size is 10 GiB; up to 8 named databases are opened.

### Committing a KifStore

```rust
use sumo_store::{CommitOptions, commit_kifstore};

let opts = CommitOptions {
    max_clauses: 10_000,  // hard error if any formula exceeds this
    session: None,        // None = base KB, Some("name") = named session
};

let formula_ids = commit_kifstore(&env, &kif_store, &opts)?;
```

`commit_kifstore` performs a single LMDB write transaction:
1. Interns all symbols from the ephemeral `KifStore` into LMDB, assigning persistent `u64` IDs.
2. Converts each root formula to CNF (via full Skolemization).
3. Writes `StoredFormula` records (element tree + CNF clauses) to the `formulas` database.
4. Indexes each formula by head predicate (`head_index`) and by predicate+argument-position+symbol (`path_index`).
5. Commits atomically; any error aborts the whole transaction (no partial state).

### Reconstructing an in-memory KB

```rust
use sumo_store::load_kifstore_from_db;
use sumo_parser_core::KnowledgeBase;

let kif_store = load_kifstore_from_db(&env)?;
let kb = KnowledgeBase::new(kif_store);
```

Reconstructs the in-memory `KifStore` from LMDB for semantic validation or in-memory queries. Taxonomy edges are rebuilt from the reconstructed sentences.

### Generating TPTP CNF

```rust
use sumo_store::db_to_tptp_cnf;

// All formulas
let tptp = db_to_tptp_cnf(&env, "kb", None)?;

// Only a specific session
let tptp = db_to_tptp_cnf(&env, "kb", Some("my_session"))?;

print!("{}", tptp);
```

Returns a `String` of `cnf(…)` declarations suitable for passing to Vampire.

### CommitOptions defaults

```rust
use sumo_store::CommitOptions;

// Reads SUMO_MAX_CLAUSES env var, falls back to 10,000
let opts = CommitOptions::default();
```

### Error type

```rust
use sumo_store::StoreError;
```

| Variant | Meaning |
|---|---|
| `StoreError::Lmdb(e)` | Underlying LMDB error |
| `StoreError::Serialise(msg)` | Bincode serialisation failure |
| `StoreError::ClauseCountExceeded { limit }` | Formula exceeded the CNF clause limit — hard error |
| `StoreError::DatabaseNotFound { path }` | `--db` path does not exist; run `sumo validate` first |
| `StoreError::Other(msg)` | Catch-all |

### Schema types

```rust
use sumo_store::{
    StoredFormula,   // persisted formula: element tree + CNF clauses
    StoredElement,   // Symbol(SymbolId) | Variable | Literal | Sub(Box<StoredFormula>) | Op
    StoredSymbol,    // id, name, is_skolem, skolem_arity
    Clause,          // CNF clause: Vec<CnfLiteral>
    CnfLiteral,      // positive/negative literal: pred + args
    CnfTerm,         // Const(SymbolId) | Var(SymbolId) | SkolemFn { id, args } | Num | Str
    FormulaId,       // type alias: u64
};
```

---

## `sumo-parser-core` API

### Loading KIF

```rust
use sumo_parser_core::{KifStore, load_kif};

let mut store = KifStore::default();
let errors = load_kif(&mut store, "(subclass Human Animal)", "my_tag");
```

### Semantic validation

```rust
use sumo_parser_core::KnowledgeBase;

let mut kb = KnowledgeBase::new(store);

// Validate all root sentences
let errors: Vec<(SentenceId, SemanticError)> = kb.validate_all();

// Validate one sentence
kb.validate_sentence(sid)?;

// Assert a formula at runtime
let result = kb.tell("my_session", "(instance Socrates Human)");
assert!(result.ok);
```

### TPTP FOF output (legacy / in-memory)

```rust
use sumo_parser_core::{kb_to_tptp, sentence_to_tptp, TptpOptions, TptpLang};

let opts = TptpOptions {
    lang: TptpLang::Fof,
    hide_numbers: true,
    ..TptpOptions::default()
};

let full_kb = kb_to_tptp(&kb, "kb", &opts, None);
let one     = sentence_to_tptp(sid, &kb, &opts);
```

### Key types

| Type | Description |
|---|---|
| `SymbolId = u64` | Persistent symbol identifier |
| `SentenceId = u64` | Persistent sentence identifier |
| `KifStore` | In-memory parsed store (symbols, sentences, taxonomy) |
| `KnowledgeBase` | Wraps `KifStore` with validation and `tell()` |
| `TellResult` | `ok: bool`, `errors: Vec<String>`, `sentence_id: Option<SentenceId>` |

---

## Path index

The `path_index` database uses 18-byte big-endian keys:

```
[ pred_id: u64 (8 bytes) ][ arg_pos: u16 (2 bytes) ][ sym_id: u64 (8 bytes) ]
```

This layout supports efficient range scans such as "all formulas where predicate P appears with symbol S at argument position N".

---

## CNF pipeline

Clausification is performed by Vampire's `NewCNF` via
`sumo_kb::cnf::sentence_to_clauses` (feature `cnf`, on by default).
The pipeline is:

1. Build a single-sentence `vampire_prover::ir::Problem` in TFF mode
   via `NativeConverter`.
2. Call `ir::Problem::clausify(Options::new())`.  Under the hood this
   runs a Rust-side `Imp`-elimination pre-pass, hands the resulting
   problem to Vampire's NewCNF, and reads the clauses back through
   structured FFI accessors (no TPTP-string round-trip).
3. Translate each `ir::Clause` to the crate-local `Clause` /
   `CnfLiteral` / `CnfTerm` shape, interning any skolem functors
   (`sK<n>`) into the `KifStore` as the walk proceeds.

Variables in KIF are scope-tagged by the parser (`X@5`); by the time
NewCNF is done, all variables have been renamed to Vampire's `X0..Xn`.
The original KIF names are no longer needed -- canonical hashing
(below) renames them again for dedup.

The `cnf` feature implies `integrated-prover` (the linked Vampire C++
library).  Builds without default features skip dedup entirely and
accept duplicate axioms silently -- convenient for
tooling-that-only-emits-TPTP-as-strings use cases.

## Clause-level deduplication

Two formulas are considered duplicates when their CNF clause sets are
equal, up to:

- variable renaming,
- skolem renaming,
- clause ordering within the formula (clause = set of literals),
- literal ordering within a clause,
- equality side orientation (`l=r` ≡ `r=l`),
- sort erasure (the canonical hash is sort-agnostic).

This is implemented in two layers:

- `sumo_kb::canonical::canonical_clause_hash(&Clause) -> u64` --
  hashes one clause's canonical form.  Tag-byte-separated, xxh64-based,
  stable across process runs.
- `sumo_kb::canonical::formula_hash_from_clauses(&[u64]) -> u64` --
  fingerprints a formula by sorting its clause's canonical hashes and
  hashing the resulting byte stream.

LMDB-side, the persistence layer interns every clause it sees by
canonical hash (`clauses` + `clause_hashes` tables) and records a
formula-hash → `SentenceId` mapping (`formula_hashes` table).  Reopening
a KB rehydrates the in-memory dedup map from `formula_hashes` in a
single pass -- no re-clausification on open.

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
