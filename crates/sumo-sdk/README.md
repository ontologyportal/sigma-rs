# sumo-sdk

Programmatic Rust API over [`sumo-kb`](https://crates.io/crates/sumo-kb): the
operations the `sumo` CLI is built on, exposed as plain function / builder
calls that return structured reports.

## What it does

| Builder | Purpose |
|---|---|
| `IngestOp`    | Layer KIF text into a [`KnowledgeBase`] (in-memory) |
| `ValidateOp`  | Parse + semantic checks on a KB or inline formula |
| `TranslateOp` | KIF → TPTP rendering |
| `LoadOp`      | Reconcile + commit to LMDB (`persist` feature) |
| `AskOp`       | One proof query against the KB (`ask` feature) |
| `TestOp`      | Batch `.kif.tq` test runner (`ask` feature) |
| `manpage_view`| Symbol introspection with pre-resolved cross-refs |

No clap, no stdout, no exit codes — embed inside a language server, a network
daemon, a custom CLI, or a scripted pipeline.

## Quickstart

```rust
use sumo_sdk::{IngestOp, ValidateOp, manpage_view};

// Caller owns the KB.  Use `KnowledgeBase::new()` for in-memory or
// `KnowledgeBase::open(path)` for LMDB-backed.
let mut kb = sumo_kb::KnowledgeBase::new();

// Ingest: file path, directory, or resident text — mix freely.
IngestOp::new(&mut kb)
    .add_file("base.kif")                          // SDK reads this
    .add_dir("ontology/")                          // SDK walks *.kif
    .add_source("ws://patch", "(subclass A B)")    // already in memory
    .run()?;

// Validate the whole KB.  Findings ride out in the report —
// `Err` is reserved for infrastructural failures only.
let report = ValidateOp::all(&mut kb).run()?;
if !report.is_clean() {
    for (sid, err) in &report.semantic_errors {
        eprintln!("sentence {sid:?}: {err}");
    }
}

// Structured queries:
if let Some(view) = manpage_view(&kb, "Animal") {
    println!("Animal has {} doc entries", view.documentation.len());
}
# Ok::<_, sumo_sdk::SdkError>(())
```

## Feature flags

| Flag | Default | Adds |
|---|---|---|
| `persist` | ✓ | `LoadOp`, LMDB-backed `KnowledgeBase::open` |
| `ask` | ✓ | `AskOp`, `TestOp`, prover-related re-exports |
| `parallel` | ✓ | rayon-backed parallel hot paths inside `sumo-kb` |
| `integrated-prover` |   | Embedded Vampire C++ backend; implies `ask`.  Requires CMake + the `vampire-sys` submodule at build time. |

## Documentation

Full API reference at [docs.rs/sumo-sdk](https://docs.rs/sumo-sdk).
