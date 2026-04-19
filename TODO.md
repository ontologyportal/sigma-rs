# TODO

Unresolved issues identified during code review (2026-03-24).

---

## Recently completed

### ~~Architectural cleanup pass~~ — DONE (2026-04-18)

Three-agent code review (perf + redundancy + architecture) + resulting
action list landed in three commits:

- **Dead code cleanup**: retired the `VarTypeInference` subsystem from
  `semantic.rs` (struct, field, build, invalidate, 3 tests; ~330 lines,
  orphaned since the IR migration).  Also dropped: `numeric_char_for`,
  `has_poly_variant_args`, no-arg `extend_taxonomy`, string-based
  `sort_for`, `vampire/converter.rs::kif_to_ir_sort`, `persist/path_index.rs::decode_key`
  + its circular round-trip test, `vampire/bindings.rs::RE_CONST` +
  unused `HashSet` import, `cnf.rs::const _: fn() -> Option<Symbol>`
  shim, `tokenizer.rs::src` field.  `Sort::tptp` gated `#[allow(dead_code)]`
  (public helper; only exercised from its own tests).  Warning-free
  build with `--all-features`.
- **Micro-perf**: three `by_head(…).to_vec()` -> direct slice iteration
  in `semantic.rs`; `build_numeric_sort_cache` BFS queue + visited
  HashSet hoisted out of the per-root loop.
- **kb.rs split**: 1551-line `kb.rs` -> `kb/mod.rs` (999) + `kb/prove.rs`
  (403, gated on `feature = "ask"`) + `kb/export.rs` (195).  Pure
  refactor; 134 tests pass in all-features and cnf-off builds.

### ~~Vampire-backed clausification + clause-level dedup~~ — DONE (2026-04-18)

Replaced the hand-rolled 500-line `cnf.rs` clausifier and the
"half-working" alpha-equivalence `fingerprint.rs` with a unified
clause-based pipeline.  Six-phase rollout, all phases committed on
`claude/friendly-wing-52cc99`:

- **Phase 1** — 12 structured literal/term accessors on the vampire-sys
  C shim (`vampire_literal_*`, `vampire_term_*`, `vampire_functor_name`,
  `vampire_predicate_name`) + Rust FFI bindings.
- **Phase 2** — `ir::Clause` / `ir::Literal` / `ir::LitKind` types in
  vampire-prover + `ir::Problem::clausify(Options) -> Result<Vec<Clause>>`
  that walks post-clausify `vampire_problem_t*` through the new
  accessors.  Includes a Rust-side `Imp`-elimination pre-pass to
  work around a `NewCNF::process` precondition
  (`ASS(g->connective() != IMP)`) that only surfaces when NewCNF is
  called without `Shell::Preprocess`.
- **Phase 3** — `sumo_kb::cnf::sentence_to_clauses` (Vampire-backed,
  interns skolems into the KifStore as it walks) +
  `sumo_kb::canonical::{canonical_clause_hash, formula_hash_from_clauses}`
  for dedup.  `CnfTerm::Fn { id, args }` variant added for non-skolem
  function applications.
- **Phase 4** — LMDB schema bump to v2: three new DBs (`clauses`,
  `clause_hashes`, `formula_hashes`), `StoredClause` record type,
  `StoredFormula.clause_ids: Vec<ClauseId>` replaces the old inline
  `clauses: Vec<Clause>`, `KbError::SchemaMigrationRequired` for
  legacy DB detection.
- **Phase 5** — `fingerprint.rs` deleted.  Hand-rolled `cnf.rs`
  deleted (replaced by Phase-3 cnf2).  `cnf` feature is now default
  and implies `integrated-prover`.  `KnowledgeBase::fingerprints`
  gated on `#[cfg(feature = "cnf")]`; without the feature no dedup
  runs.  Reopens rehydrate `fingerprints` from `DB_FORMULA_HASHES`
  in one pass.
- **Phase 6** — README + TODO + design-note updates.

Verification: 103 lib tests + 4 promote integration tests + 5 env
unit tests pass (cnf-on).  `--no-default-features --features persist`
builds and accepts duplicates silently as designed.  Design doc lives
at `docs/clause-dedup.md`.

---

## Critical

### ~~TFF axiom translation produces contradictory axioms~~ — FIXED (2026-03-24)

**Was**: `(instance ?X NumericSubclass)` with `?X : $real/$int` was dropped to
`$true`, stripping conditional antecedents and producing globally contradictory
axioms.  Two separate contradictions existed:
1. SignumFn: `NonnegativeRealNumber`/`PositiveRealNumber`/`NegativeRealNumber` (biconditionals) → `1=-1 → false`
2. EvenInteger/OddInteger: `(=> (instance ?X EvenInteger) (equal (RemainderFn ?X 2) 0))` → `∀ X:$int. X%2=0` and `∀ X:$int. X%2=1` → `0=1 → false`

**Fix applied** (`crates/sumo-kb/src/semantic.rs` + `tptp/translate.rs`):
- Added `ArithCond` enum: `GreaterThan`, `GreaterThanOrEqualTo`, `LessThan`, `LessThanOrEqualTo`, `And(Vec<ArithCond>)`, `EqualFn { fn_name, other_arg, result }`.
- `SemanticLayer` builds `numeric_char_cache` during `rebuild_taxonomy()` by scanning three axiom forms in the KB:
  - Form A: `(<=> (instance ?VAR C) cond)` — biconditional (NonnegativeRealNumber, PositiveRealNumber, NegativeRealNumber)
  - Form B: `(=> cond (instance ?VAR C))` — sufficient condition
  - Form C: `(=> (instance ?VAR C) cond)` — necessary condition (NonnegativeInteger, NegativeInteger, PositiveInteger, EvenInteger, OddInteger)
- For Merge.kif: 8 numeric subclasses characterised (was 0).
- `translate_sentence()` substitutes the cached condition instead of `$true` when `(instance ?VAR C)` has a numeric-sort variable.
- `emit_arith_cond_tptp()` renders `ArithCond` to TPTP arithmetic builtins (`$greatereq`, `$less`, `$remainder_e`, etc.) with sort-aware literal formatting.

**Current baselines (tested 2026-03-24, timeout=10s):**
- FOF subprocess (`--lang fof`):  ~13-14/21 (TQG8 is timing-sensitive, rest stable)
- TFF subprocess (`--lang tff`):  17/21 genuine proofs (was 4/21 after first partial fix, was vacuous 21/21 before)
- Embedded prover: still affected by the same sort issues (typed quantifiers disabled)

**Remaining TFF failures (4 tests):**
- TQG50: duplicate assertion skip (session issue, not a TFF translation problem)
- TQG8, TQG9: complex proofs, may be genuine FOF-vs-TFF capability gap or timeout
- One more TBD

**TFF vs FOF gap** has closed significantly: TFF now matches or exceeds FOF on numeric reasoning tests. Remaining 4 failures are not caused by sort conflicts.

---

## High Priority

### ~~Remove dead `EmbeddedProverRunner` (FOF embedded path)~~ — FIXED (2026-04-17)

Deleted in commit `ae529c4` as part of the vampire-rs IR migration. The
`prover/embedded.rs` file is gone, the `pub use` re-export from `lib.rs`
and `prover/mod.rs` is gone. The current embedded path routes through
`vampire/converter.rs::NativeConverter` + `lower_problem`.

---

## Medium Priority

### ~~Consolidate the three sentence→formula converters~~ — PARTIAL (2026-04-17)

Converters (2) and (3) are now a single `NativeConverter<Mode>` in
`vampire/converter.rs`, dispatching on `Mode::Fof` / `Mode::Tff`
(commit `ae529c4`).  It produces `vampire_prover::ir::Formula` values
that can be lowered to the FFI types or serialised to TPTP via
`vampire/assemble.rs::assemble_tptp`.

Converter (1) — `tptp/translate.rs` + `tptp/tff.rs` — still exists and
produces TPTP strings directly.  Phase 4 of the migration will repoint
`sumo translate` at `NativeConverter` + `assemble_tptp`, at which
point (1) can be deleted as well.

---

### ~~Embedded prover binding extraction~~ — PARTIAL (2026-04-17)

Implemented in `vampire/bindings.rs` (commit `6af76e1`).  Single-variable
existential queries now return bindings:

```
$ sumo ask -f tiny.kif --lang tff --backend embedded "(instance ?WHO Mortal)"
  WHO = Socrates
```

Known limitation: multi-variable queries (e.g. `(grandparent ?GP ?GC)`)
still prove but return empty bindings.  See "Multi-variable binding
extraction" below for follow-up.

Discovered upstream issue while implementing this — see
"vampire-prover: ProofRule::NegatedConjecture never surfaces on input
steps" below.

---

### ~~Multi-variable binding extraction from native proofs~~ — FIXED (2026-04-17)

Implemented via an `AliasTracker` in `sumo-kb/src/vampire/bindings.rs`
(commit `538551d`).  A single forward walk over the proof carries an
alias for each conjecture variable -- either a bound constant or the
step-local variable name currently holding it -- and updates on every
Resolution-family step via a one-sided unifier.  Transformation
rules (Flatten, CNFT, EENFT, Rectify, NNFT) preserve variable names;
resolution rules invoke the unifier on the resolved literal pair.

Verified:

```
$ sumo ask -f multi.kif --lang tff --backend embedded \
         "(grandparent ?GP ?GC)"
  GC = Carol
  GP = Alice
```

where `multi.kif` = `(parent Alice Bob)` + `(parent Bob Carol)` +
transitivity.  Single-variable extraction (the earlier scope) is
unchanged.

Known limitations documented in code:
- The literal regex requires no nested parens (covers atomic
  predicate calls -- the common case).
- Transformation rules are assumed to preserve variable names
  (holds in every proof shape I've observed; not guaranteed in
  general).
- Superposition and Demodulation get the generic resolution
  treatment, which may miss substitutions in unusual shapes.

---

### vampire-prover: NewCNF type-mismatch aborts clausification for a handful of SUMO axioms

`cnf::sentence_to_clauses` silently swallows per-sentence clausification
failures and logs a warning (see `kb/mod.rs::compute_formula_hash`
~line 440 and the `clausify()` loop at ~line 897).  The failure mode
observed in practice is a Vampire NewCNF type mismatch on
sort-polymorphic relations like `s__orientation` -- Vampire fails
internally with an assertion about incompatible sorts for the same
symbol when our single-sort TFF signature sends contradictory hints
into NewCNF's term builder.

Impact: the affected axioms are accepted into the KB but get *no
clause-level dedup entry* (no `DB_CLAUSES` / `DB_FORMULA_HASHES` row),
so re-telling the same sentence produces a distinct `SentenceId`.
Silent correctness gap: users get duplicates for these axioms with no
visible signal beyond a log line.

**Proposed action**:
1. Turn the warn-log into a hard `TellWarning::ClausifyFailed { sid }`
   exposed on `TellResult.warnings`, so CLI callers surface it.
2. Upstream: file a vampire-prover issue with a minimal repro
   (`(=> (orientation ?X ?Y Vertical) ...)` style) and fix the
   sort-declaration pass so that NewCNF gets a consistent signature
   for sort-polymorphic predicates.
3. Short-term workaround: detect the specific predicates in
   `NativeConverter` and emit FOF-style (sort-free) declarations for
   them even in TFF mode.

### vampire-prover: `ProofRule::NegatedConjecture` never surfaces on input steps

When `Problem::solve_and_prove()` returns a `Proof` for a problem
submitted with `Problem::conjecture(f)`, the input step carrying the
negated conjecture comes back with `rule == ProofRule::Axiom`, not
`NegatedConjecture`.  This is despite `vampire_conjecture_formula`
being called at solve time (so the C++ side knows it's a conjecture)
and despite `ProofRule::from_raw` in `crates/vampire-rs/vampire/src/ffi.rs`
containing logic to map both `VAMPIRE_CONJECTURE` and
`VAMPIRE_NEGATED_CONJECTURE` input types to `ProofRule::NegatedConjecture`.

Reproduction: the example at the bottom of the file (can be built
off-tree to isolate).  Observed on vampire-rs commit `e2378aa` +
the Phase 1 IR changes; all input steps show `rule=Axiom`.

```
0: rule=Axiom  premises=[] conclusion='human(socrates)'                    <- axiom
1: rule=Axiom  premises=[] conclusion='! [X0] : (human(X0) => mortal(X0))' <- axiom
2: rule=Axiom  premises=[] conclusion='~? [X0] : mortal(X0)'               <- NEGATED CONJECTURE, mis-tagged
```

Impact: consumers that need to distinguish the conjecture in the proof
(binding extractors, proof translators, ...) have no reliable enum
signal to identify it.

Workaround currently used in `sumo-kb/src/vampire/bindings.rs`:
fall back to "first input step whose conclusion starts with `~?` or
`~!`" -- the negated-quantifier prefix only appears when Vampire has
inverted a quantified input, so this picks out existential and
universal conjectures.  Fails for ground conjectures (no quantifier to
flip) but those have no variables to bind anyway.

**Proposed action (upstream)**: audit `ProofRule::from_raw` against
actual Vampire C-API return values.  Either the shim in `vampire-sys`
isn't returning the right `input_type` for conjecture units, or
Vampire's preprocessing is rewriting the unit in a way that drops the
tag.  Fix should propagate the "this was submitted as a conjecture"
signal all the way to the `ProofStep` introspection path.  Track as a
vampire-rs issue once the repo URL/tracker is in use.

---

### Embedded prover pass rate

The embedded prover (`--backend embedded`) is affected by the same TFF
contradiction issue described above. Its 17/21 figure was also partly
vacuous. The 4 additional failures (vs. 21/21) were genuine timeouts.

Once the TFF translation is fixed, the embedded prover pass rate will need
to be re-evaluated from scratch. The embedded prover also lacks binding
extraction (see below).

---

## Low Priority

### Feed stored clauses directly to Vampire (follow-up note; largely superseded)

**Status (2026-04-18)**: the cold-ask speedup this item targeted --
"skip the IR-rebuild from KIF per ask" -- has largely landed via Phase
D's bincode axiom cache (`CachedAxiomProblem` in `persist/env.rs`).
Cold asks now restore a pre-built `ir::Problem` from LMDB in ~10-15 ms
instead of rebuilding through `NativeConverter` for every query; the
~3.2 s "IR rebuild" component is amortized away.

What remains:

1. **Skip Vampire's second clausification (~0.4 s on Merge.kif).**
   Vampire still re-clausifies the restored problem inside
   `Shell::Preprocess`.  `Shell::Preprocess::clausify` short-circuits
   on `u->isClause()` (Preprocess.cpp:722), so feeding pre-clausified
   units would skip NewCNF inside preprocessing.  The C API has
   stubs for this (`vampire_clause`, `vampire_axiom_clause`,
   `vampire_conjecture_clause`, `vampire_problem_from_clauses` -- all
   declared in `vampire_c_api.h`, none implemented in `.cpp`).  ~30
   lines of C++ wrapping `Kernel::Clause::fromArray`.

2. **Prerequisites** if we pursue this:
   - `StoredClause.sort_meta` needs to be populated (currently always
     `None`).  Blocker for TFF.
   - Lose Vampire's predicate-definition elimination on the axiom
     side; SUMO has few biconditionals so the capability hit is small.
   - Vampire's signature caches are wiped by
     `vampire_prepare_for_next_proof`, so we'd rebuild compiled
     clauses from `StoredClause` per ask (~20 µs/clause: cheap).

3. **Expected remaining savings**: ~0.4 s per cold ask on Merge.kif
   scale.  With the Phase D cache already in place, this is a
   marginal improvement rather than a step change.  Not worth the
   C++ shim work until something else makes the preprocess clausify
   step dominant again.

---

### `VampireAxiomCache` double-build

`VampireAxiomCache::build()` makes two independent O(n_axioms) passes over the
same sentence set:

1. `kb_to_tptp(...)` — serializes axioms to a TPTP string (subprocess path)
2. `build_tff_problem(...)` — converts axioms to native `Formula` objects
   (embedded path, `integrated-prover` feature only)

These are entirely separate walks that duplicate sort inference and symbol
encoding work. With ~13,000 Merge.kif axioms this is not a bottleneck in
practice, but it is unnecessary.

**Proposed action**: Unify into a single pass that produces both outputs
simultaneously (shared element traversal, two output sinks).

---

### `ProverOpts.timeout_secs` is unused by `VampireRunner`

`VampireRunner::prove()` reads `self.timeout_secs` (its own field) for the
Vampire `-t` flag and ignores `opts.timeout_secs` from `ProverOpts`. The
`ProverOpts` is only used for `opts.mode` (Prove vs. CheckConsistency).

The `timeout_secs` field on `ProverOpts` is therefore misleading — it exists
but has no effect. Either `VampireRunner` should switch to using
`opts.timeout_secs` exclusively (removing the duplicate field), or
`timeout_secs` should be removed from `ProverOpts`.
