# Clause-based deduplication

> Design note for the clause-based dedup pipeline introduced in commits
> `66c5ca7..b7aa34d` (2026-04-18).  Audience: future maintainers
> touching `sigmakee-rs-core`'s persistence layer or the `cnf` / `canonical`
> modules.

## Why it exists

The predecessor was two pieces of machinery that didn't quite line up:

- **`cnf.rs` (deleted)** — a hand-rolled 500-line
  NNF / Skolemize / distribute clausifier.  Worked on small inputs but
  had no verified correctness story for edge cases that Vampire
  handles directly (e.g., let-bindings, theory reasoning, equality).
- **`fingerprint.rs` (deleted)** — an alpha-equivalence hash over the
  KIF element tree.  Handled variable renaming but not AC of `and`/`or`,
  quantifier reordering, or any semantic normalisation.  Described
  internally as "half-working".

The new pipeline lets Vampire itself clausify and uses the resulting
CNF as the canonical key.  Two formulas that produce the same clause
set after clausification are by construction equivalent under all the
normalisations a production theorem prover applies -- AC, NNF,
ENNF, Skolemization, simplification.

## Pipeline shape

```
                 tell() / load_kif() / promote()
                             │
                  ┌──────────┴──────────┐
                  │                     │
        cnf::sentence_to_clauses        │
  (NativeConverter → ir::Problem →      │
   NewCNF → ir::Clause → sigmakee_rs_core Clause)│
                  │                     │
          ┌───────┴───────┐             │
          ▼               ▼             │
 canonical_clause_hash  (store in       │
   (xxh64 per clause)   self.clauses    │
          │             side-car)       │
          ▼                             │
 formula_hash_from_clauses              │
   (xxh64 of sorted      ─────────► self.fingerprints
    canonical hashes)                   │
                                        ▼
                           promote_assertions_unchecked
                                        │
                                        ▼
                         persist::commit::write_sentence
                                        │
                            ┌───────────┼───────────┐
                            ▼           ▼           ▼
                     intern_clause  clause_ids   formula_hash
                     (DB_CLAUSES)   on formula   → SentenceId
                     (DB_CLAUSE_    record       (DB_FORMULA_HASHES)
                      HASHES)
```

Every arrow is lossless up to dedup equivalence.

## Canonical form (`canonical.rs`)

A clause's canonical hash is invariant under four transformations:

1. **Variable rename** — variables renamed by first-occurrence DFS to
   `V0..Vn` before hashing.  `p(?X) | q(?X)` and `p(?Y) | q(?Y)`
   collide.  Variable identity is preserved across literals within
   the same clause (so `p(?X) & q(?X)` still differs from
   `p(?X) & q(?Y)`).
2. **Skolem rename** — Vampire names skolems `sK<n>` with a
   session-global counter, so the concrete index is an artifact of
   which formulas were clausified before this one.  We rename by the
   same DFS scheme to `sk0..skn`.
3. **Literal order** — a clause is a set; literals are sorted by
   structural pre-hash before emission into the main stream.
4. **Equality orientation** — `l = r` and `r = l` sort their sides
   by sub-hash before hashing.  Polarity (positive vs negative
   equality) still participates.

Sort annotations are **ignored**.  A TFF and an FOF clausification of
the same formula dedup to the same clause record.  A sort-aware
auxiliary hash slot (`StoredClause.sort_meta`) is reserved for a
future sort-conflict guard (see Risk 3 in the original design plan).

## LMDB schema

Three new tables, all keyed on 8-byte BE integers:

| Table             | Key            | Value          | Population      |
|-------------------|----------------|----------------|-----------------|
| `clauses`         | `ClauseId`     | `StoredClause` | `intern_clause` |
| `clause_hashes`   | canonical hash | `ClauseId`     | `intern_clause` |
| `formula_hashes`  | formula hash   | `SentenceId`   | `write_sentence`|

`StoredFormula.clauses: Vec<Clause>` was replaced by
`clause_ids: Vec<ClauseId>`.  Shared clauses dedup to a single record.

A schema version marker (`sequences["schema_version"] = 2`) is written
on fresh DBs.  Opening a DB with any entries in `formulas` but no
`schema_version` key returns `KbError::SchemaMigrationRequired`.

## Feature gating

- `cnf = ["integrated-prover"]` — can't have the new clausifier
  without the linked Vampire library.
- `default = ["cnf"]` — the normal build gets dedup.
- `--no-default-features` — no clausifier, no dedup, duplicate axioms
  accepted silently.  The `fingerprints` field on `KnowledgeBase`
  doesn't exist; `axiom_ids_set` reconstructs the axiom set by
  subtracting session sids from `store.roots`.

## Non-obvious invariants

- **Formula hash must be computed from canonical hashes, not
  ClauseIds.** Integration testing caught this: if you compute
  `formula_hash_from_clauses(&clause_ids)` at commit time and
  `formula_hash_from_clauses(&canonical_hashes)` at tell time, the
  reopen-time dedup silently fails.  `commit.rs::write_sentence`
  collects canonical hashes in parallel with clause_ids from the
  intern loop to keep them aligned.
- **Vampire's NewCNF requires `Imp` to be pre-eliminated.** The
  `clausify.rs::eliminate_imp` pass rewrites `Imp(a, b)` →
  `Or([Not(a), b])` before the FFI hand-off.  Without it, NewCNF
  asserts (`ASS(g->connective() != IMP)`) and the process dies.
  `Iff` is left alone -- NewCNF handles it via polarity expansion.
- **Skolem interning requires `&mut KifStore`.** The `cnf` module
  takes `&mut SemanticLayer` and interns skolems in-place.  The old
  signature that returned skolems via an out-parameter is gone.

## Known follow-ups

- Cross-KB clause dedup — canonical hashes use SymbolId bytes, which
  are KB-local.  A cross-KB dedup (e.g., for content-addressed sharing
  between two KnowledgeBases) would need name-based hashing.
- Sort-conflict guard — the `sort_meta` slot on `StoredClause` is
  reserved but unused.  Populating it with the sort list from the
  TFF clausification and failing-loud on disagreement is a cheap
  safety net against canonical-hash false positives.
- A `--warn-on-duplicate` startup flag for the cnf-off path is
  documented in the original plan but not implemented.  Currently the
  cnf-off build accepts all duplicates without any signal.
