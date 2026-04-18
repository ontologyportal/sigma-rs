# Phase D — Semantic-cache persistence

Phase D of the `ask()` optimisation series persists derived semantic
state (taxonomy + `SortAnnotations`) in LMDB so they survive close/
reopen.  The first cold-open-then-ask cycle is the target: without the
caches, every new process has to rescan the full axiom set to rebuild
the taxonomy (via `SemanticLayer::rebuild_taxonomy`) and derive the
sort annotations on first access.

## Numbers

Measured on Merge.kif + Mid-level-ontology.kif (~15,875 axioms) in
release mode on Apple Silicon, via `tests/cold_open_bench.rs`.

| Metric | Baseline | Phase D | Δ |
|---|---|---|---|
| Initial load + promote + close | 918 ms | 974 ms | **+56 ms** (cache write) |
| LMDB size on disk | 12.62 MiB | **12.88 MiB** | **+260 KiB** |
| Cold open wall-clock | 21 ms | **18 ms** | **−3 ms** |
| First ask after cold open | 137 ms | **113 ms** | **−24 ms** |
| Warm ask average | 79 ms | 81 ms | noise |
| Cold-open → first-ask delta | 58 ms | **32 ms** | **−26 ms** |

The 24 ms saving on first-ask comes from restoring the `SortAnnotations`
cache (which the first ask otherwise builds lazily by scanning every
`domain`/`range` axiom).  The 3 ms saving on cold open is the taxonomy
cache avoiding the full `extract_tax_edge_for` scan.

260 KiB of extra LMDB (≈ 2% of the existing footprint) buys a 45%
shorter cold-open → first-ask cycle.

## Schema

v3 adds one LMDB table: `caches`, a `Str -> Bytes` keyed store.
Entries today:

- `"taxonomy"` → bincode(`CachedTaxonomy { kb_version, tax_edges,
  numeric_sort_cache, numeric_ancestor_set, poly_variant_symbols,
  numeric_char_cache }`)
- `"sort_annotations"` → bincode(`CachedSortAnnotations { kb_version,
  sorts }`)

Each cache carries its own `kb_version` — a monotonic `u64` stored in
`sequences["kb_version"]` and bumped by `write_axioms` on every
commit.  On open, a blob is restored only when its `kb_version`
matches the current counter; stale blobs are treated as absent and
the full rebuild path runs.

Reserved keys (schema slot present, not populated today):
`"axiom_cache_tff"`, `"axiom_cache_fof"` — see "Axiom cache: tried
and rejected" below.

## Backward compatibility

Schema v2 (pre-Phase D) DBs load fine under v3: the `caches` table is
created empty on first open, and `kb_version` is stamped at 0.
Phase D caches start populating on the next `promote_assertions_unchecked`
call.  Explicit schema check accepts `v2` as a forward-compatible
read; only `v != 2 && v != 3` triggers `SchemaMigrationRequired`.

## Axiom cache: tried and rejected

A third cache slot was prototyped: the `VampireAxiomCache` (a
`vampire_prover::ir::Problem` carrying every KB axiom as IR) would be
serialised to a TPTP string and restored on first `ask()` via
`TptpParser::parse`.  The intent was to skip the ~45 ms
`NativeConverter::add_axiom` walk on cold start.

Result: the round-trip is **slower** than the rebuild it replaces.

| | Time | Notes |
|---|---|---|
| In-memory rebuild (NativeConverter) | ~45 ms | walks pre-interned KIF elements |
| LMDB read + TPTP reparse | ~66 ms | parses text, re-interns symbols |

On top of that, the serialised TPTP blob was **1.88 MiB** — 15% of the
total LMDB footprint for a net slowdown.  The schema slot and
`CachedAxiomProblem` type are left in place for a future bincode-based
implementation that skips the re-interning cost.  `ensure_axiom_cache`
takes the direct in-memory path today.

## Risks & known limitations

**Cache invalidation granularity.** `kb_version` is bumped per
`write_axioms` commit, so every successful commit invalidates every
cache — even when the committed sentences don't affect the taxonomy
(e.g., a new `(attribute Alice Tall)` axiom invalidates the taxonomy
cache unnecessarily).  Phase B's granular head-keyed invalidation
would let us hold these caches longer across commits.

**Snapshot cost at commit.** `persist_taxonomy_cache` clones all four
taxonomy tables at commit time.  For a 15k-axiom KB this is <10 ms;
larger KBs may feel it.  A streaming serialiser (bincode::serialize_into
a `RwTxn`-held writer) would avoid the full clone.

**Stale caches never fail loudly.** If the `kb_version` counter ever
got out of sync with the cache content (e.g. a non-`write_axioms`
code path that mutates the persisted axiom set), the cache would be
trusted incorrectly.  We rely on `write_axioms` being the sole writer
of axioms to LMDB, which is true today but not architecturally
enforced.

## Verification

- `cargo test -p sumo-kb --features "cnf integrated-prover persist ask" --release`
  passes 125+ tests across the lib, `cold_open_bench.rs`,
  `invalidation_bench.rs`, `dedup_bench.rs`, `phase_a_regression.rs`,
  `promote.rs`, `dedup.rs`.
- Schema migration test (`persist::env::tests::legacy_db_is_rejected`)
  still rejects pre-Phase-4 DBs.
- Phase-D benchmark (`tests/cold_open_bench.rs`) prints the numbers
  above; run with `--nocapture --ignored`.
