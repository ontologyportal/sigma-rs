# SigmaKEE-rs Core Crate

The `sigma-rs-core` crate is the core implementation of SigmaKEE-rs. It contains
all the logic and procedures which define the parsing, analysis, translation, and 
logic proving of the SUMO ontology.

The library's main API is the `KnowledgeBase` struct. If utilizing this library, 
all interfaces should be performed via instatiations of this struct (see the 
usage documentation below for some quick start instructions for loading a new
Knowledge Base and performing operations on it).

## Architecture

The system is architected into the following flow:

```
Parsing --> Symbol Table Construction +-> Semantic Analysis +-> TPTP Translation -+-> Theorem Proving -> Proof Parsing
                                      |                     |                     |
                                      |                     +-> SInE Caching -----+
                                      |                                           |
                                      +-> Clausification -------------------------+
```

The central part of the library revolves around three "layers" (defined in 
[`src/layer.rs`](src/layer.rs)). Layers are built from "inner" to "upper"
and changes made to an inner layer propogate to outer layers. Outer layers
leverage the information parsed and stored in inner layers. The layers
defined here are:

* [`SyntacticLayer`](src/syntactic/mod.rs) - Handles tracking, storage, and
lookup of symbols, sentences, and literals. This is performs solely on the 
syntax of SUO-KIF and not based on any meaning introduced by relations or
axioms.
* [`SemanticLayer`](src/semantic/mod.rs) - Handles tracking of taxonomy
and domain / range information for predicates. Performs validation against 
the meaning of symbols. For example, this will validate that operators only
receive formulas as their arguments and not functions.
* [`TranslationLayer`](src/trans/mod.rs) - Handles translation of the 
KB into TPTP, converting domain/range to types first order sorts and 
handling higher order constructs in first order.

Additionally, textual parsers are located in the `parser` folder and 
define how source is parsed into an AST for processing by the 
afforementioned `SyntacticLayer`.

## Features

There are a number of optional features that can be compiled into the 
library for additional functionality. At its base, the library performs 
all functions necessary to produce TPTP but does not include features
like persistent caching, theorem prover invocation and proof parsing
and clausification / axiom deduplication.

### `persist`

This is a **DEFAULT FEATURE**. To exclude from your compilation 
pass `--no-default-features` to `cargo build` and manually whitelist 
features using `--features "persist"`.

This library stores its state to persistent storage using "Lightning
Memory Database" (LMDB). LMDB is a database backend which essentially
saves a copy of a portion of a processes virtual memory to disk.
Loading from disk is a simple `memmap` operation and enables
persistance of pointers between structs. This enables a built KB
to be persisted to disk and reduces start up costs for subsequent
KB queries and theorem proofs. Enable this feature if you plan to use
SigmaKEE in a persistent space where loading from a KIF file occurs
once and changes are incremental.

### `cnf`

This is not a default feature. To include the integrate prover in 
your build, run `cargo build` with this specific feature enabled
`--features "cnf"`.

This feature clausifies KB axioms and caches those clause <-> axiom
mappings in the KB. Clausification is currently ONLY used to 
deduplicate axioms as normal syntactic deduplication (even with 
variable normalization) would miss ordinal based duplicates (e.g.
`(and A B C)` vs `(and B C A)`) as well as logical equivalences
(e.g. `(or (not A) B)` vs `(=> A B)`). Clausification currently
leverages Vampire's very fats clausification engine and therefore
this feature is dependent on the `integrated-prover` feature.

### `ask`

This is a **DEFAULT FEATURE**. To exclude from your compilation 
pass `--no-default-features` to `cargo build` and manually whitelist 
features using `--features "ask"`.

This feature implements interaction with the vampire theorem prover
as a subprocess. It introduces:

* Proof parsing
* Query variable inference
* TPTP -> KIF -> Natural Language transformation

Importantly, this feature is not required for simple TPTP emission.

### `integrated-prover`

This is not a default feature. To include the integrate prover in 
your build, run `cargo build` with this specific feature enabled
`--features "integrated-prover"`.

The integrated prover includes the Vampire ATP library API. Vampire
is natively compiled in C++ and is leveraged using Rust/C++ Foreign
Function Interface (FFI). An advantage of using Vampire this way is 
that passing the problem for Vampire to solve is done fast via 
`memmap`/`memcpy` rather than IPC. Additionally, vampire need not 
be installed on your local system. A disadvantage is that it is less
configurable (this library does not yet expose any options to 
customize the Vampire invocation like you could do if you invoked
Vampire yourself such as adjusting the proof schedule). Additionally, 
Vampire is single threaded to ensure memory safety and cannot be 
invoked concurrently (even with the parallel feature enabled).

### `parallel`

This is a **DEFAULT FEATURE**. To exclude from your compilation 
pass `--no-default-features` to `cargo build` and manually whitelist 
features using `--features "parallel"`.

Implements parallelism to speed up translation in multithreaded
envionrments. Locations where parallelism is employed:

1. File parsing: files are parsed into AST's in parallel before
being unified in the `SyntacticLayer` as a single global symbol table
2. Semantic Validation: Validation occurs in parallel (after the 
initial semantic pass)
3. TPTP translation: TPTP translation occurs in parallel (after
the initial semantic pass)