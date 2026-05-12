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

## Caching Architecture

Almost every derived value in the KB — taxonomy lookups, sort inference,
converted formulas, the SInE index — is expensive to recompute and is
therefore **memoised**. Rather than scatter ad-hoc `HashMap` caches across the
codebase, the layers are built *around* a single cache abstraction.

### Philosophy

* **Every derived value is a cache object.** Each layer is a thin container of
  cache fields plus convenience wrappers; the value-producing logic lives in the
  cache, not the layer. A query like `layer.is_instance(sym)` is just
  `self.is_instance.get(self, sym)`.
* **One cache, one file.** Each cache is a self-contained `*Behavior`
  implementer under `<layer>/caches/<name>.rs`, declaring its name, how to
  compute a value (`generate`), how to respond to re-entrancy (`on_cycle`), and
  how to react to a change (`react_to_delta`). The 1:1 accessor wrapper lives in
  the same file.
* **Caches are toggleable.** Every cache shares a runtime `CacheConfig`. When a
  cache is **disabled** it becomes a transparent getter — `get` runs `generate`
  on every call and stores nothing. This means even values that *shouldn't* be
  memoised (cheap or volatile computations) are still modelled as caches for
  uniformity; they simply ship disabled (see `sentence_symbols` / `sentence_vars`).
* **No `unsafe`.** The owning layer is threaded explicitly as the first argument
  to `get(parent, key)`, so a cache's `generate` can reach sibling caches and
  inner layers through a plain `&Parent` reference.
* **Change propagation is delta-driven.** After a batch of edits, each layer's
  `on_change(delta)` (in `<layer>/delta.rs`) fans the delta out to every cache's
  `react_to_delta`, which decides independently whether to clear, evict, or keep
  its entries. Out-of-band imperative invalidation (`invalidate_cache`,
  `evict_symbols`) also exists for callers outside the delta flow.

The generic machinery lives in [`src/cache.rs`](src/cache.rs).

### Cache types

The variants form a 2×2 matrix over **keyed vs. whole-value** and **lazy
(compute-on-miss) vs. eager (maintained in place)**:

| Type | Shape | Backing | Compute model |
|------|-------|---------|---------------|
| `Cache<B>` | keyed (`K → V`) | `EntryCache` (sharded `DashMap`) | **lazy** — `generate(parent, key)` on miss, with thread-local cycle detection |
| `WholeCache<B>` | single value | `LayerCache` (`RwLock<Option<T>>`) | **lazy or installed** — `generate(parent)` on miss, or `install`ed wholesale (e.g. from LMDB) |
| `Eager<B>` | single value | `EagerIndex` | **eager** — seeded via `initial()`, maintained by explicit `modify`/`install`; no compute-on-miss |
| `EagerMap<B>` | keyed (`K → V`) | `EntryCache` | **eager** — maintained via `update`/`modify_entry`; a miss means "absent", never "compute it" |

All four honour `CacheConfig` enable/disable and, where relevant, expose
`snapshot`/`restore` for LMDB persistence.

### Caches by layer

**`SyntacticLayer`** — raw store indices (eagerly maintained as sentences load/unload):

| Cache | Type | Purpose | Invalidation |
|-------|------|---------|--------------|
| `occurrences` | `EagerMap` | symbol → every occurrence in the KB | maintained by `index_/drop_sentence_occurrences`; delta evicts affected symbols |
| `head_index` | `EagerMap` | head predicate → root sentences (`by_head`) | maintained on sentence build/remove; delta evicts affected heads |
| `axiom_index` | `EagerMap` | symbol → axiom sentence ids (SInE generality source) | maintained by `register_/unregister_axiom_symbols` + the `sine_*` methods; delta no-op |
| `sine` | `Eager` | SInE (Sine Qua Non) axiom-selection index | maintained explicitly by `sine_add_axiom` / `sine_remove_axiom` / `sine_rebuild`; delta no-op |
| `sentence_symbols` | `Cache` *(disabled by default)* | symbol set referenced by a sentence | transparent getter while disabled; clears on any change if enabled |
| `sentence_vars` | `Cache` *(disabled by default)* | variable set referenced by a sentence | transparent getter while disabled; clears on any change if enabled |

> `normal_implications` and `impl_sym_index` remain raw `LayerCache` fields (not
> yet behavior objects): their build mutates the store (`&mut self`), so they
> have no read-only `generate`. They are invalidated wholesale whenever roots
> change. A future pass will fold them (and `trans`'s `predvar_cache`) into the
> framework together.

**`SemanticLayer`** — taxonomy + per-symbol semantic queries (lazy memo, evicted by change):

| Cache | Type | Purpose | Invalidation |
|-------|------|---------|--------------|
| `tax_edges` | `Eager` | full taxonomy edge list | maintained by `rebuild_taxonomy` / `add_edge` / `remove_edge` |
| `tax_incoming` | `Cache` | symbol → incoming edge indices (`parents_of`) | evicted by `add_edge`/`remove_edge`; delta no-op |
| `tax_outgoing` | `Cache` | symbol → outgoing edge indices (`children_of`) | evicted by `add_edge`/`remove_edge`; delta no-op |
| `is_instance` / `is_class` | `Cache` | instance-vs-class classification | evict taxonomy-affected symbols on taxonomy change |
| `is_relation` / `is_predicate` / `is_function` | `Cache` | relation-kind classification | evict taxonomy-affected symbols on taxonomy change |
| `has_ancestor` | `Cache` (`(sym, ancestor)`) | ancestor reachability | retain-drop entries touching taxonomy-affected symbols |
| `arity` | `Cache` | relation arity | evict taxonomy-affected + all-affected on other-sentence changes |
| `domain` / `range` | `Cache` | argument-domain / range sorts | evict taxonomy-affected + domain/range-affected symbols |
| `documentation` / `term_format` / `format` | `Cache` | `(documentation/termFormat/format …)` entries | evict taxonomy-affected + all-affected on other-sentence changes |
| `inferred_class` | `Cache` | memoised most-specific SUMO class | clear on domain/range change; evict taxonomy-affected on taxonomy change |

**`TranslationLayer`** — TPTP translation state:

| Cache | Type | Purpose | Invalidation |
|-------|------|---------|--------------|
| `sort_annotations` | `WholeCache` | KB-wide typed sort annotation table | invalidate on domain/range change |
| `numeric_sorts` | `EagerMap` | numeric class → TFF `Sort` | rebuilt wholesale by `prime_caches` on taxonomy change; delta no-op |
| `numeric_ancestor_set` | `WholeCache` | ancestors of the numeric roots | re-installed by `prime_caches` on taxonomy change; delta no-op |
| `poly_variant_symbols` | `WholeCache` | relations needing polymorphic TFF variants | re-installed by `prime_caches` on taxonomy change; delta no-op |
| `symbol_sort` | `Cache` | symbol → TFF `Sort` | cleared by `prime_caches` on taxonomy change; clear on domain/range; evict affected symbols |
| `formulas_tff` | `Cache` | sentence → converted TFF formula + decls | clear on taxonomy removal/mixed **or** domain/range change; **kept on the pure-addition fast path** |
| `formulas_fof` | `Cache` | sentence → converted FOF formula | same as `formulas_tff` |
| `relation_sorts` | `Cache` | symbol → per-argument sort annotation | same as `formulas_tff` |

The translation layer's `on_change` preserves a tuned **pure-addition fast
path**: an overlay load (adding a `-f` file on top of a stable KB) rebuilds only
the taxonomy-indexed numeric caches and leaves the large formula caches intact,
keeping incremental loads amortised O(N) rather than O(N²).

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

## Knowledge Base API

The `KnowledgeBase` struct is the public API for this library.
The entire library revolves around initialization, manipulation
and querying of a `KnowledgeBase`. Below are some of the primary
operations one can perform on a `KnowledgeBase` (please refer to the 
full docs.rs documentation for the full spectrum of features that 
`KnowledgeBase` exposes).

### Tracking Progress

The `KnowledgeBase` struct provides a progress sink mechanism by 
which a consumer of the KB and its operations can install callbacks
to be called at various checkpoints during execution. This can be
used for thinks like profiling, logging, or progress tracking. 
These progress sinks are called as emissions and cannot be used to
cancel asynchronous processes. Install a sink using 
[`KnowledgeBase::set_progress_sink(DynSink)`](src/kb/progress.rs).
Note: your sink must be thread independent (think `Arc`) as sinks are
shared across parallel thread calls.

```rust
kb.set_process_sink(Arc::new(|e: &ProgressEvent| {
    eprintln!("[sumo] {e:?}");
}));
```

### Creating a KB.

To initialize a new in-memory KB, use `KB::new()`.

To open a KB from a cached LMDB database file, use 
[`KnowledgeBase::open()`](src/kb/persist.rs) (or its variant 
`KnowledgeBase::open_with_progress()` if you want to install a progress 
sink). Note, to open a cached KB, the `persist` feature must be 
compiled into the project. 

### Adding new Axioms / reading in a new KIF file

The two primary entry points for adding formulas to the KB are `tell()` for
interactive/single-session use and `reload_kif()` / `reload_kifs()` for
file-based incremental reloads.

#### Ingestion primitives

All entry points share a common set of primitives, composed differently
depending on the code path:

| Primitive | What it does | Used by |
|-----------|-------------|---------|
| `parse_document(file, text)` | Parses KIF text into an AST; collects parse errors without aborting | `ingest`, `reconcile_syntactic_only`, `reload_kifs_impl` |
| `ingest_parsed(nodes, errs, file, session, validate)` | Interns AST nodes into the syntactic store, deduplicates (CNF hash or syntax fingerprint), registers sentences in a named session, optionally validates semantics. **Does not touch taxonomy or SInE.** | `ingest`, `reconcile_apply_additions_deferred`, `reload_kifs_impl` |
| `extend_taxonomy_with(sids)` | Incrementally extends the taxonomy with newly-accepted sentences | `ingest` (immediate), `reload_kifs_impl` (batched at the end) |
| `rebuild_taxonomy()` | Full taxonomy rebuild; used when removals make an incremental extend unsafe | `reload_kifs_impl` (when `needs_tax_rebuild`) |
| `make_session_axiomatic(session, ...)` | Promotes session assertions to permanent axioms; reindexes SInE occurrence counts and trigger index | `reload_kif` (new file), `reload_kifs_impl` (batched at the end) |
| `reconcile_compute_diff(file, doc)` | Compares stored hashes against new AST hashes; produces retained / removed / added sets | `reconcile_syntactic_only`, `reload_kifs_impl` |
| `reconcile_apply_removals(removed, altered_syms)` | Un-indexes removed sentences from SInE and CNF side-cars, then drops them from the store | `reload_kifs_impl` |
| `reconcile_apply_additions_deferred(file, added, altered_syms)` | Calls `ingest_parsed` on the added AST nodes; no taxonomy step | `reload_kifs_impl` |
| `reconcile_syntactic_only(text, file)` | LSP fast path: diff → span updates → store mutations → orphan pruning → semantic-cache invalidate. No SInE, no taxonomy, no dedup. | `reload_kif(validate=false)` on existing files |
| `reconcile_smart_revalidate(altered_syms)` | Re-validates only the axiom neighbourhood of changed symbols via a SInE seed | `reload_kifs_impl` |

The key invariant is that **taxonomy is always updated in one place per
call** — never both `extend_taxonomy_with` and `rebuild_taxonomy` in the same
pass, and never inside `ingest_parsed` itself.

**Formula-by-formula (`tell`)**
```
         USER                     SIGMA

        tell() <---------------------------------+
          |                                      |
          +----> ingest()                        |
                    |                            |
                    V                            |
              parse_document()                   |
                    |                            |
                    V                            |
            ingest_parsed()                      |
                    |                            |
                    V                            |
          syntactic.load()                       |
          (intern AST into store)                |
                    |                            |
                    V                            |
          cnf_deduplicate()                      |
          (or syntax fingerprint                 |
           dedup if cnf disabled)                |
                    |                            |
                    V                            |
          sessions[session]                      |
            .extend(accepted)                    |
                    |                            |
                    V                            |
          [validate=true only]                   |
          validate_sentence()                    |
                    |                            |
                    V                            |
        extend_taxonomy_with()                   |
          (incremental; only                     |
           runs in ingest wrapper,               |
           not in ingest_parsed)                 |
                    |                            |
                 (more formulas?)                |
                    |                            |
make_session_axiomatic() <--- no ---+---- yes ---+
          |
          +--------> SInE occurrence update
                    |
                    V
             extend SInE index
                    |
                    V
           run consistency check
         (optional: ask feature gate)
                    |
  commit to LMDB cache <----------+
   (optional: persist
      feature gate)
```

1. [Assertion (`tell()`)](src/kb/ingest.rs): Add new formula(s) to the
`KnowledgeBase` as a session assertion (hypothesis). The formulas are parsed,
deduplicated, and added to the taxonomy. They are NOT evaluated for
consistency with the rest of the KB — a potentially contradictory formula may
be added via `tell()`. Semantic validation is optional (the `validate`
parameter). **A parse error will not abort the ingestion — any sentences that
did parse are still accepted. Errors are returned in `IngestResult::errors`
and the caller can roll back the session with `flush_session()`.**

2. [Promotion (`make_session_axiomatic`)](src/kb/ingest.rs): Promote session
assertions to permanent axioms.  After this call, all formulas in the session
appear in `ask`'s axiom set (TPTP role `axiom`).  Promotion also:

  - Retagges fingerprint entries from session-owned to axiom (CNF path).
  - Updates SInE occurrence counts and trigger indices for each promoted
    sentence so the prover and smart-revalidation see the full axiom set.
  - Optionally checks the promoted batch for consistency against the rest of
    the KB via the theorem prover (`ask` feature gate). This is expensive and
    is intended for single-formula promotion, not whole-file loads.

#### Incremental file updates (`reload_kif` / `reload_kifs`)

`reload_kif` is the preferred entry point for file-based KB updates.  It runs
a sentence-level diff that preserves `SentenceId` stability on unchanged
sentences, so downstream caches (SInE index, CNF side-cars, LMDB) stay valid
without a full rebuild.  `reload_kifs` accepts a batch of `(file, text)` pairs
and folds the `make_session_axiomatic`, taxonomy, and smart-revalidation passes
into one call over the union of all files' altered-symbol sets.

Both ultimately share `reload_kifs_impl`.  There are three sub-paths:

**`reload_kif(validate=false)` — LSP / syntactic-only path**
```
    reload_kif(text, file, validate=false)
          |
          V
   file already in KB?
          |
    no    |    yes
          |     |
          V     V
       ingest() reconcile_syntactic_only()
       (text,   (parse → diff → span updates
        file,    → remove → append_root_sentence
        file,    → semantic cache invalidate;
        false)   no SInE / taxonomy / dedup)
          |
          V
   make_session_axiomatic(file)
   (SInE occurrence + trigger index)
          |
          V
       IngestResult
```

Safe under the LSP invariant that the prover will not run between `didChange`
events.

**`reload_kif(validate=true)` / `reload_kifs` — full diff pipeline**
```
    reload_kif(text, file, validate=true)
    reload_kifs([(file, text), ...])
          |
          V
    reload_kifs_impl()
          |
          +------ for each file ------+
          |                           |
          |    [new file]             |    [existing file]
          |         |                 |         |
          |         V                 |         V
          |   parse_document()        |   parse_document()
          |         |                 |         |
          |         V                 |         V
          |   ingest_parsed()         |   reconcile_compute_diff()
          |   (ADD_SESSION,           |   (retained / removed / added)
          |    validate=false;        |         |
          |    no taxonomy yet)       |         V
          |         |                 |   update retained spans
          |         V                 |   (position shift only)
          |   collect sids            |         |
          |   + altered_syms          |         V
          |                           |   reconcile_apply_removals()
          |                           |   (SInE un-index, CNF side-cars,
          |                           |    syntactic store removal)
          |                           |         |
          |                           |         V
          |                           |   reconcile_apply_additions_deferred()
          |                           |   └─ ingest_parsed(added_nodes, [],
          |                           |         file, ADD_SESSION, false)
          |                           |      (no AST→text round-trip;
          |                           |       no taxonomy yet)
          |                           |         |
          |                           |   collect sids + altered_syms
          +---------------------------+
          |
          V
   make_session_axiomatic(ADD_SESSION)
   (one batched promotion for all files:
    retag fingerprints, SInE occurrence
    counts, SInE trigger index)
          |
          V
   taxonomy: one step, never both
     ┌─ needs_tax_rebuild? ──yes──> rebuild_taxonomy()
     └─ no ──────────────────────> extend_taxonomy_with(all_new_sids)
          |
          V
   drop axiom cache
   (ask feature gate)
          |
          V
   reconcile_smart_revalidate(altered_syms)
   (SInE-seed from altered symbol set;
    validate only the neighbourhood)
          |
          V
   Vec<IngestResult>
   (one per input file;
    retained / added_sids /
    removed_sids / errors)
```

#### API examples

**Load a KIF file into the KB for the first time**

```rust
use sigmakee_rs_core::KnowledgeBase;

let mut kb = KnowledgeBase::new();
let text = std::fs::read_to_string("Merge.kif")?;

// reload_kif is the preferred entry point for file-based loading.
// validate=true runs semantic checks and builds a full SInE index.
let result = kb.reload_kif(&text, "Merge.kif", true);
assert!(result.ok, "load failed: {:?}", result.errors);
println!("loaded {} axioms", result.added());
```

**Assert a hypothesis interactively then promote it to an axiom**

```rust
// tell() parks formulas in a named session without touching the axiom set.
let r = kb.tell("(instance Fido Dog)", "hypothesis", false);
assert!(r.ok);

// Promote the whole session to permanent axioms (updates SInE index).
kb.make_session_axiomatic(
    "hypothesis",
    Some(false), // check_consistency (ask feature)
    None,        // custom prover runner
    None,        // TPTP language variant
)?;
```

**Roll back a session on parse error**

```rust
let r = kb.tell("(bad kif (", "scratch", false);
if !r.ok {
    // Discard every sentence that *did* parse in this session.
    kb.flush_session("scratch");
}
```

**Incrementally reload an edited file (LSP / editor path)**

```rust
// validate=false is the fast path: span updates and store mutations only.
// No SInE or taxonomy rebuild — safe to call on every keystroke.
let new_text = "(subclass Dog Mammal)\n(subclass Cat Mammal)";
let r = kb.reload_kif(new_text, "Merge.kif", false);
println!("added={} removed={}", r.added(), r.removed());
```

**Reload multiple files in one batched pass**

```rust
// reload_kifs amortises make_session_axiomatic, taxonomy rebuild,
// and smart revalidation across all files in a single pass.
let edits = vec![
    ("Merge.kif",    new_merge_text.as_str()),
    ("Mid-level.kif", new_mid_text.as_str()),
];
let results = kb.reload_kifs(edits);
for r in &results {
    println!("{}: +{} -{} errors={}", r.session, r.added(), r.removed(), r.errors.len());
}
```

**Inspect the result**

```rust
// IngestResult fields:
// r.ok           — false only on hard errors (parse failures)
// r.errors       — Vec<KbError> (hard errors)
// r.warnings     — Vec<TellWarning> (duplicates skipped, semantic notices)
// r.sids         — Vec<SentenceId> newly added by this call
// r.removed_sids — Vec<SentenceId> removed by this call
// r.retained     — count of sentences carried over unchanged
// r.added()      — r.sids.len()
// r.removed()    — r.removed_sids.len()
// r.is_noop()    — true when nothing changed

for w in &result.warnings {
    eprintln!("warning: {w}");
}
```

### Translating a KB into TPTP

The process of transforming a Knowledge Base into TPTP involves multiple 
phases, orchestrated through the `to_tptp()` and `format_sentence_tptp()`
entry points in [`kb/export.rs`](src/kb/export.rs). This translation is 
performed by the `TranslationLayer`. Essentially, the role of the 
`TranslationLayer` is to convert SUMO formulas into a logic representative
intermediate form (IR), which can then be converted to TPTP via a one-to-one
correspondence. Along the way, it applies several transformation patterns
to reach valid TPTP (both first order and typed).

#### Overview

```
KnowledgeBase (SUMO KIF axioms)
         |
         v
  [to_tptp / format_sentence_tptp]
         |
         +--> SyntacticLayer (parse tree storage)
         ║
         ▼
  NativeConverter (src/trans/converter/)
         ║ ┌────────────────────────────────────────────────────────────────────────────────────────────────────────┐
         ▼ ▽                                                                                                        │
    add_axiom() ─▷ sid_to_top() ─▷ sid_to_formula()                                                                 │
╔════════╝                          △	     │                                                                        │
║                                   │ [first term?]                                                                 │
║ ┌─────────────────────────────────┘ ┌────┴────────┐                                                               │
║ │                                   ▽             ▽                                                               │
║ │                               [operator]     [symbol]                                                           │
║ │                                   │             │                                                               │
║ │                            ┌──────┘             │                                                               │
║ │                            ▽                    ▽                                                               │
║ │            ┌operator_sid_to_formula()┐  atomic_sid_to_formula()                                                 │
║ │            │               ▽         │          │                                                               │
║ │      and/or/not/    forall/exists  equals  [pred var?]                                                          │
║ │         =>/<=>             ▽         │       ┌──┴────────┐                                                      │
║ │            │     ┌─wrap_quantifier() │       │           │                                                      │
║ │            ▽     ▽                   ▽       │           │                                                      │
║ │       element_to_formula() element_to_term()◁]───────────]──────────────────────────────────────────┐           │
║ │     ┌──────┴──┐      	  │          │ △      [y]         [n]                                         │           │
║ │     ▽         ▽        END  ┌──────┘ │       │           │                                          │           │
║ └[Sentence] [var/sym]     │   │  ┌─────┘┌──────┘           │                                          │           │
║                 │         │   │  │      ▽                  ▽                                          │           │
║           [True/False?]   │   │  │ pred_var_expand()    [mode?]                                       │           │
║$true/$false◁─[y]┴[n]─▷!ERR│   │  │  ┌────┘        ┌─────────┴─────────┐                               │           │
║│   ┌──────────────────────┘   │  │  ▽             ▽                   ▽                               │           │
║│   │               [type]◁────┘  │  *            FOF                 TFF                              │           │
║│   ▽          ┌───┬──┴─┬───┬──┐  │  │             ▽                   │                               │           │
║│`<op>(...)`  sym sent var op lit │  │       fof_atomic_rel()      [function?]                         │           │
║│   │          ▽   │    ▽   │  ▽  │  │     ┌───────┘                ┌──┴─────────────────┐             │           │
║▽   │      `s__{}` │   X{N} │  X  │  │  for each                   [y]                  [n]            │           │
║┌───┘sid_to_term()◁┘        ▽     │  │    arg                       ▽                    ▽             │           │
║│          │          `s__{op}_op`│  │     │                tff_atomic_pred()    tff_atomic_func()     │           │
║│    [quantifier?]                │  │     ├──then─┐                ▽                    ▽             │           │
║│    ┌─────┴─────┐                │  │     │       ▽            tff_ir_fn()          tff_ir_fn()       │           │
║│   [y]         [n]               │  │     │  `s__pred(<args>)`         └──────┐ ┌────┘                │           │
║│    │           │                │  │     │          │                        │ │                     │           │
║│ collect_vars()─┴────────────────┘  │     │          │      select_sig()◁──arg_sorts()──▷select_sig() │           │
║│                                    │     │          │            │           │ │            │        │           │
║│                                    │     │          │            │           │ │            │        │           │
║│                                    │     │          │            ▽           │ │            ▽        │           │
║│                                    │     │          │     `s__predXX(...)`   │ │    `s__funcXX(...)` │           │
║│                                    │     │          │            │           │ │            │        │           │
║│                                    │     │          │            │           ▽ ▽            │        │           │
║│                                    │     └──────────]────────────]──────────────────────────]────────┘           │
║▽                                    ▽                ▽            ▽                          ▽                    │
║└────────────────────────────────────┴────────────────┴────────────────────────────────────────────────────────────┘
╚▶IR::Problem (vampire_prover::ir)
         |
         v
  assemble_tptp (src/trans/assemble.rs)
         |
         +--> Names axioms kb_<sid>
         +--> Optionally adds original KIF comments
         +--> Filters axioms by SID set
         |
         v
  TPTP String ( ready for Vampire )
```

#### The Conversion Pipeline

**Phase 1: Entry Point (`kb/export.rs`)**

The `KnowledgeBase::to_tptp()` method is the main entry point:

```rust
pub fn to_tptp(&mut self, opts: &TptpOptions, session: Option<&str>) -> String
```

**Note**: `to_tptp` and `format_sentence_tptp` take `&mut self` so they can run
the deferred rewrite pass (`ensure_rewrite_pass`) on first read after a
taxonomy / domain / range change.  This means consumers holding the KB behind
a shared-ownership wrapper (`Arc<KnowledgeBase>`, `RwLock<KnowledgeBase>` with
concurrent readers) must serialize TPTP emission behind a write lock.  See
TODO.md for the alternative design (eager rewrite at end-of-ingest) that
would restore `&self`.

1. Collects axiom IDs via `axiom_ids_set()` (collect ALL axioms)
2. Optionally adds hypotheses (assertions) from a named session
3. Filters out excluded predicates (via `opts.excluded`) - By default, excluded predicates are those predicates which are development specific (no logical content, like `documentation` and `format`)
4. Creates a `NativeConverter` in the requested mode (`Tff` or `Fof`)
5. Adds each axiom to the converter
6. Calls `finish()` to get `(IrProblem, sid_map)`
7. Delegates to `assemble_tptp()` for final string emission

The `format_sentence_tptp()` helper converts a single sentence, handling
quantifier wrapping for conjectures vs axioms.

**Phase 2: Conversion to IR (`trans/converter/`)**

The `NativeConverter` walks KIF AST nodes and produces an intermediate
representation (`vampire_prover::ir::Problem`). Two modes are supported:le

- **TFF (Typed First-Order Formula)**: Direct typed predicate encoding
  - `(instance A Entity)` → `s__instance(A, Entity)`
  - Registers type declarations: `tff(..., type, s__instance: ($i * $i) > $o)`
  - Functions use `IrFn::typed()` when sorts are all `Individual`

- **FOF (First-Order Formula)**: Holds-reification encoding
  - `(instance A Entity)` → `s__holds(s__instance__m, A, Entity)`
  - No type declarations emitted
  - Functions represented as terms in predicate position

Key conversion rules:

| KIF construct | TFF output | FOF output |
|--------------|------------|------------|
| `(P a b)` | `s__P(a, b)` (typed predicate) | `s__holds(s__P__m, a, b)` |
| `(SuccessorFn ?N)` | `s__SuccessorFn(?N)` (function term) | `s__SuccessorFn__m(?N)` |
| `(forall (?X) P)` | `![X:$i] : P` | `![X] : P` |
| `(exists (?X) P)` | `?[X:$i] : P` | `?[X] : P` |
| String literal `"foo"` | `s__foo` (quoted constant) | `s__foo` |
| Number literal `42` | `n__42` or raw TPTP int | raw TPTP int |
| `(?P a b)` | `s__holds_app`

The converter maintains:
- Per-sentence variable allocation (`vars`, `var_ids`)
- Cross-sentence declaration deduplication (`declared_sorts`, etc.)
- Reified quantifier scopes (to avoid variable shadowing collisions)

**Phase 3: Assembly (`trans/assemble.rs`)**

The final `assemble_tptp()` function serializes the IR problem to TPTP:

```rust
pub fn assemble_tptp(
    problem: &IrProblem,
    sid_map: &[SentenceId],
    opts: &AssemblyOpts,
) -> String
```

The assembler:
1. Outputs sort declarations first (TFF only)
2. Outputs function declarations
3. Outputs predicate declarations
4. Numbers axioms as `kb_<sid>` using the parallel `sid_map`
5. Optionally prepends original KIF as `%` comments
6. Supports axiom filtering (by SID set) for SInE-selected subsets
7. Adds the conjecture line with configurable name

Output format:
```tptp
% Original KIF comment from the KB
tff(kb_42, axiom, s__instance(s__Alice, s__Human)).
tff(kb_43, axiom, s__instance(s__Bob, s__Human)).
tff(kb_44, axiom, ! [X] : (s__instance(X,s__Human) => s__mortal(X))).
tff(conjecture, conjecture, ? [X] : (s__instance(X,s__Human) & ~s__mortal(X))).
```

#### Usage Example

```rust
use sigmakee_rs_core::{KnowledgeBase, TptpOptions};

let mut kb = KnowledgeBase::new();
kb.reload_kif("path/to/sumo.kif", "Merge.kif", true)?;

let opts = TptpOptions {
    lang: TptpLang::Tff,
    show_kif_comment: true,
    hide_numbers: false,
    ..Default::default()
};

let tptp = kb.to_tptp(&opts, None);
std::fs::write("output.tptp", tptp)?;
```

#### TPTP Options

The `TptpOptions` struct controls emission behavior:

| Field | Description |
|-------|-------------|
| `lang: TptpLang` | `Tff` for typed, `Fof` for untyped |
| `show_kif_comment: bool` | Prepend each axiom's original KIF as `%` comment |
| `hide_numbers: bool` | Emit numbers as `n__42` constants (true) vs raw literals (false) |
| `excluded: HashSet<String>` | Predicate names to exclude from output |
| `query: bool` | For single-sentence export: wrap free vars existentially (conjecture) |
