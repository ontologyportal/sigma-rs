# TODO

Unresolved issues identified during code review (2026-03-24).

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

### Remove dead `EmbeddedProverRunner` (FOF embedded path)

`crates/sumo-kb/src/prover/embedded.rs` contains `EmbeddedProverRunner`, a FOF
holds-reification converter that was the original embedded prover. It is no
longer called by the CLI or by `kb.ask_embedded()`. The current embedded path
uses the TFF converter in `crates/sumo-kb/src/vampire/convert.rs` directly.

`EmbeddedProverRunner` is still exported as a public type (`pub use
prover::EmbeddedProverRunner` in `lib.rs`) but is dead code from the
application's perspective.

**Proposed action**: Delete `prover/embedded.rs`, remove the `pub use`
re-export from `lib.rs`, and remove `pub mod embedded` from `prover/mod.rs`.
Confirm that no downstream code depends on the public type before deleting.

---

## Medium Priority

### Consolidate the three sentence→formula converters

The same sentence-tree traversal (operators, quantifiers, terms, literals) is
implemented three times:

1. `tptp/translate.rs` + `tptp/tff.rs` — produces TPTP strings (FOF and TFF)
2. `prover/embedded.rs` `Converter` — produces FOF `Formula` objects (dead, see above)
3. `vampire/convert.rs` `TffConverter` — produces TFF `Formula` objects

After removing (2), the operator-dispatch logic in (1) and (3) is still
duplicated: `And`, `Or`, `Not`, `Implies`, `Iff`, `Equal`, `ForAll`, `Exists`
are handled identically in both, with only the leaf encoding differing.

Variable collection helpers (`collect_all_vars`, `collect_bound_var_names`,
`collect_free_vars`) similarly appear independently in both modules.

**Proposed action**: Extract a shared visitor/walker utility that handles
operator dispatch and variable collection, with a pluggable leaf encoder.
Both the string-based (TPTP) and native-formula paths would use it.

---

### Embedded prover binding extraction

`kb.ask_embedded()` uses `solve_and_prove()` but discards the returned `Proof`
object. Variable bindings from embedded-prover queries are never extracted.

The `Proof` struct in `vampire-prover` exposes `steps()` → `&[ProofStep]`,
`ProofStep::conclusion()` → `Formula`, and `ProofStep::rule()` →
`ProofRule`. Bindings could be extracted by:

1. Finding the step with `rule == NegatedConjecture`
2. Walking resolution steps that derive from it
3. Extracting ground resolvent variable assignments structurally

Until this is implemented, `ask_embedded()` always returns empty `bindings`
regardless of the proof found.

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
