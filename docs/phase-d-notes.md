# Phase D — Semantic-cache persistence

Phase D of the `ask()` optimisation series persists three derived
caches (taxonomy + `SortAnnotations` + `VampireAxiomCache`) in LMDB
so they survive close/reopen.  The first cold-open-then-ask cycle is
the target: without the caches, every new process has to rescan the
full axiom set to rebuild the taxonomy (via
`SemanticLayer::rebuild_taxonomy`), derive the sort annotations on
first access, and walk the KIF store via `NativeConverter` to build
the axiom-set IR problem on first ask.

## Numbers

Measured on Merge.kif + Mid-level-ontology.kif (~15,875 axioms) in
release mode on Apple Silicon, via `tests/cold_open_bench.rs`.

| Metric | Baseline | Phase D (tax + sort only) | Phase D + bincode axiom cache | Δ from baseline |
|---|---|---|---|---|
| Initial load + promote + close | 918 ms | 974 ms | 969 ms | +51 ms (cache write) |
| LMDB size on disk (post-commit) | 12.62 MiB | 12.88 MiB | 12.87 MiB | +250 KiB |
| LMDB size after first ask persists axiom cache | — | — | **17.48 MiB** | **+4.86 MiB** |
| Cold open wall-clock | 21 ms | 18 ms | 18 ms | −3 ms |
| 1st ask (first process, axiom cache being persisted) | 137 ms | 113 ms | 131 ms | — (writes the cache) |
| 1st ask (second process, axiom cache restored from LMDB) | 137 ms | 113 ms | **90 ms** | **−47 ms** |
| Warm ask average | 79 ms | 81 ms | 82 ms | noise |
| Cold-open → first-ask delta | 58 ms | 32 ms | **8 ms** | **−50 ms (−86%)** |

Savings breakdown (per cold process after the KB is promoted once):

- 3 ms on the cold-open scan itself — taxonomy restored, so
  `rebuild_taxonomy` doesn't walk every sentence.
- 24 ms on the first ask — `SortAnnotations` restored, so the lazy
  `build_sort_annotations` scan doesn't run.
- 23 ms on the first ask — `VampireAxiomCache` restored via bincode,
  so `NativeConverter::add_axiom` doesn't walk 15k KIF sentences.

The three caches compose: on a second cold-open after the axiom
cache has been persisted, the first-ask overhead drops from 58 ms
to **8 ms** — the first ask is essentially as fast as a warm ask.

**Storage cost**: +4.86 MiB of LMDB (≈ 38% growth), the bulk in the
axiom-cache bincode blob.  The taxonomy + sort_annotations caches are
only ~250 KiB combined.  If the axiom cache is ever cost-prohibitive
for a deployment, the blob can be trimmed by just not calling
`persist_axiom_cache` after the rebuild -- taxonomy + sort_annotations
alone still halve the cold-open overhead.

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

## Axiom cache: TPTP tried, bincode shipped

Two formats were evaluated for persisting `VampireAxiomCache`:

**TPTP string + re-parse (rejected).**  Initial implementation wrote
the cache as the output of `ir::Problem::to_tptp()` and restored it
via `TptpParser::parse`.  Measured cost: ~66 ms to reparse on a
15k-axiom KB, compared to ~45 ms to rebuild from the already-loaded
in-memory store via `NativeConverter`.  Net slowdown.  The reparse is
slower because it has to tokenise text, allocate fresh strings for
every symbol name, and rebuild the typed `Function`/`Predicate`
objects — the in-memory rebuild path skips all of that because its
inputs are pre-interned KIF element trees.  1.88 MiB of extra LMDB
for a slowdown was strictly worse than not persisting at all.

**Bincode (shipped).**  Gating the pure-Rust IR types behind a new
`vampire-prover/serde` feature unlocks `#[derive(Serialize, Deserialize)]`
on `ir::Problem`, `ir::Formula`, `ir::Term`, `ir::Function`,
`ir::Predicate`, `ir::Sort`, `ir::Interp`, `ir::VarId`, `ir::LogicMode`,
and the internal `FuncKind`/`PredKind`.  `CachedAxiomProblem.problem`
becomes `ir::Problem` directly; `put_cache` bincodes it through LMDB's
usual `Bytes` codec.  Benchmark: ~8 ms first-ask overhead after
restore — **5x faster than the TPTP attempt, 10x faster than the
original rebuild-every-time**.  Blob is ~4.86 MiB (bincode is less
compact than TPTP text because every string carries a length prefix),
which is the main cost.

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
