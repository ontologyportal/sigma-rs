# Plan: audit findings cache (contradictions that survive across runs)

*2026-10-05 -- from design discussion. Status: IMPLEMENTED on branch
`feat/contradictions-cache` (2026-10-26). Deviations from the design below:
findings are keyed by `Scoped<Vec<SentenceId>>` (session scope in the key);
incomplete-provenance findings follow the same cited-sid eviction rule and
are only flagged `complete = false`; a finding found again keeps its first
record (origin, time) and only a proof-less import skeleton is filled in;
reports are imported by resolving cited axioms (fingerprint, then file:line,
then head symbol); web persistence is an OPFS mirror re-imported at boot
rather than the KB snapshot; the web replaces the nightly replace-KB replay
with recorded findings + contradiction sources (GitHub issues / URL / upload)
under KB > Contradictions, and `sumo audit --import` is the CLI face.*

## Goal

Contradictions found by a consistency audit currently live only in the web
Audit tab's component state: every run (including Continue) clears them, a
page reload loses them, and a later run cannot tell a known contradiction
from a new one. Keep every found contradiction in core, across runs and
backends, and drop it exactly when it stops being valid.

## Validity rule

A refutation stays valid as long as every axiom it cites still exists.
FOL is monotone: adding axioms cannot undo a refutation, and removing or
editing an axiom it does not cite cannot affect it. Because `SentenceId` is
a content hash, editing an axiom = `RootRemoved` for the old id (+ a new id),
and moving an axiom without changing its text keeps its id. So:

- Key a finding by its sorted, deduped cited `source_sid`s -- the same key
  `KnowledgeBase::audit_sampled_with` (`kb/prove.rs`, dedup point) and
  `saturate/consistency.rs` already build.
- Evict a finding when any cited sid is removed.
- Resolve file:line for display at read time, never store it.

### Incomplete provenance

Some proofs do not cite every axiom they use, so eviction-by-cited-sid
would be unsound for them:

- Embedded Vampire never sets `source_sid`
  (`prover/external/backends/vampire/native_proof.rs`).
- Native proofs leave `source_sid` empty on some leaves (`saturate/proof.rs`);
  known holes: ill-sorted sids, `list_theory` origin, EC negatives, eq-class
  Leibniz drop.

A finding is `complete` iff every leaf step (no premises) carries a
`source_sid` or is the conjecture. Incomplete findings fall back to the
validate cache's policy: cleared on ANY KB change (`RootAdded`,
`RootRemoved`, `TaxonomyChanged`, `DomainRangeChanged`). Open question:
keep them under that policy, or do not cache them at all.

## Design

| Piece | Where |
|---|---|
| Event | `Event::ContradictionFound { culprits: Vec<SentenceId>, steps, complete, session }` + `EventKind::ContradictionFound`, in `cache/events.rs` |
| Producer | `KnowledgeBase::audit_sampled_with` / `audit_with` (`kb/prove.rs`): one `self.layer.cascade(...)` per fresh contradiction at the existing dedup point |
| Consumer | `semantics/caches/audit_findings.rs`: `EagerMap` on `SemanticLayer` |
| Read | `kb.audit_findings()` -> SDK view -> wasm call -> web Audit tab |

### Event

- Named for the result, not the request ("Prove" would read like
  `ValidateKB`). Specific to contradictions: the producer filters on
  Inconsistent / ContradictoryAxioms once, at the source, so consumers do
  not re-check status and asks do not carry full results nobody consumes.
  Generalize (e.g. `ProofFinished { result }`) only when a second consumer
  needs non-contradiction results.
- Payload uses `prover::proof::KifProofStep`, and the whole `prover` module
  is gated, so the variant and its `EventKind` are gated on
  `any(feature = "external-prover", feature = "native-prover")`.
- Why an event and not a direct insert: decouples producers from where the
  cache lives (it can move layers without touching `kb/prove.rs`), keeps data
  entering through `react` like every other cache, and lets other consumers
  (LSP / Problems-tab diagnostics, a future ask-result cache) subscribe for
  free. Precedent: `Event::Diagnostic` (payload event), `ValidateKB` /
  `ValidateSentence` / `ValidateSession` (events that populate the validate
  cache).
- New ground: no production code seeds a cascade with a result event yet
  (the `Validate*` triggers are parked). `Layer::cascade(&self, seed)` already
  supports it; the schedule is computed once per stack, so the per-round
  cost is negligible next to a proof.

### Consumer: `semantic::audit_findings`

- `Key = Vec<SentenceId>` (sorted cited sids), plus session scope (key
  shaped like validate's `Scoped<_>`).
- `Value = Arc<Finding>`: proof steps, prose (if any), backend label,
  `complete`, found-at time.
- `Side`: reverse index cited sid -> finding keys, so `RootRemoved` evicts
  in O(cited) (pattern: `syntactic/caches/axiom_index.rs`).
- `consumes`: `ContradictionFound`, `RootRemoved`, `SessionRetracted` /
  `SessionReferenced`, and the any-change kinds for incomplete findings.
- Persistence via `snapshot_side` / `restore_side` gives LMDB persistence on
  the CLI for free.
- Field on `SemanticLayer`, cfg-gated like the event.

### Why SemanticLayer

- Closest analog is the validate cache (`semantics/caches/validate.rs`):
  stored results of an expensive "what is wrong with the KB" analysis,
  invalidated by KB change. A contradiction is a KB diagnostic found by a
  prover.
- It is in every stack: `ProverLayer<SemanticLayer>`,
  `ExternalProverLayer<TranslationLayer>`, and
  `ExternalProverLayer<ProverLayer<TranslationLayer>>`.
- Differences from validate: no compute-on-miss (the key is discovered by
  search, so `EagerMap` not `Cache`), and no clear-on-any-change for
  complete findings.

### Rejected placements

- **Native `ProverLayer` and/or `ExternalProverLayer`:** neither is in
  every stack; the nested stack would hold two stores that cannot see each
  other's findings; and `kb/prove.rs` already sees both backends' output.
  This holds even if the `saturate` edit restriction is lifted.
- **New `ProofLayer` on top:** changes `KnowledgeBase<L>` in every stack
  (CLI, SDK, wasm, LSP) for one field, and `*_native` entry points
  (`kb/prove.rs` `native()`) bypass the top layer anyway, so recording would
  still live in `KnowledgeBase`.
- **SyntacticLayer:** in every stack, but it holds raw text and indexes, not
  findings about the KB.

## Stages

1. Event variant + kind (gated), `audit_findings` cache on `SemanticLayer`,
   producer cascade in `kb/prove.rs`, `kb.audit_findings()`. Unit tests:
   insert on event; evict on removal of a cited sid; survive removal of an
   uncited sid; survive an unchanged re-ingest; incomplete finding cleared
   on any change; session retraction drops session findings; snapshot
   round-trip.
2. SDK view + wasm call (`auditFindings`).
3. Web Audit tab: load known findings on open, merge new ones per round
   (dedup by culprit key), show found-at time (already displayed per card
   from the web side).
4. Web persistence (see open questions).

## Open questions

1. Incomplete findings: cache with clear-on-any-change, or skip.
2. Web reload: `packages/web/src/services/kb-cache.ts` rewrites the OPFS
   snapshot only after a promote and discards it when the file set or edits
   change -- exactly when invalidation should do the work instead. Options:
   re-snapshot after an audit that found something, or a small separate
   findings store keyed by culprit sids and re-validated against the live
   KB on load.
3. Timestamps: `SystemTime` panics on wasm; use the `crate::clock` shim or
   let the caller stamp the time.
4. Session audits: confirm findings citing session assertions are keyed by
   session scope and dropped on retraction (or not cached).

## Follow-ups (need the `saturate` restriction lifted)

- Close the native provenance holes so more findings are `complete`.
- Let the native enumerator take the known culprit keys (via opts / prove
  ctx -- a read of the cache, not ownership) and skip or block them, so a
  run's contradiction limit goes to new ones.

## Related: one proving layer, backends as plug-ins

`ExternalProverLayer` holds no caches (`own_reactors()` returns `vec![]`):
it is a backend that needs translation, shaped as a layer. That is what
causes the nested stack, `KnowledgeBase::native()`, ~12 `*_native` method
pairs across `kb/`, SDK and wasm, and the CLI dispatch tiers. A future
refactor could make one proving layer own proof-level state, with the
native engine and external runners as backends (external only when the
inner layer has translation). Mostly inside `saturate` (struct, `TopLayer`
impl, caches' `Parent = ProverLayer<S>`), so it needs the restriction
lifted and should not collide with active prover work. Do not block this
cache on it: if it lands, move `audit_findings` into that layer -- the event
keeps producers unchanged.
