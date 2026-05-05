# sigmakee-rs-sdk + sigmakee-rs-core activity flows

Structured natural-language description of every function-call flow exposed by
`crates/sigmakee-rs-sdk` (the public Op surface) and the corresponding pipelines it
drives inside `crates/core`. Designed as input for an LLM that will
generate one activity diagram per `## 3.x` section.

**Notation**

- `A → B` — A invokes B (sequence)
- `A ?[cond] → B; else → C` — decision diamond
- `for x in xs: A` — loop
- `[feature: ask|cnf|persist|integrated-prover]` — compile-time gate; render
  as conditional/colored
- Function names use `crate::path::fn` on first mention, short form thereafter.
- `kb.x` is shorthand for `KnowledgeBase::x`.

---

## 1. Overview

`sigmakee-rs-sdk` exposes six builder-pattern *Op* structs plus one utility:
`IngestOp`, `ValidateOp`, `TranslateOp`, `LoadOp`, `AskOp`, `TestOp`,
`manpage_view`. Each Op is constructed against a caller-owned
`KnowledgeBase`, configured fluently, and run via `.run()` which returns a
typed report. There is **no central dispatcher** — callers chain Ops
themselves. Findings (parse errors, semantic warnings, prover output) ride
in the report; only infrastructural failures bubble out as `SdkError`.

Every Op converges on a small set of `KnowledgeBase` entry points
(`tell`, `load_kif`, `reconcile_file`, `reconcile_files`, `ask`,
`ask_embedded`, `validate_*`, `to_tptp`, `format_sentence_tptp`,
`make_session_axiomatic`, `flush_session`, `persist_reconcile_diff`,
`manpage`). Section 2 names the sub-pipelines those entry points share;
sections 3.1–3.7 trace each Op end-to-end.

---

## 2. Common building blocks

These pipelines are referenced by name from the Op flows below.

### 2.1 `parse_pipeline` (`crate::parse`)
`crate::parse::parse_document(file_tag, text)` → tokenizer in
`parse::kif::tokenizer` → recursive-descent parser in `parse::kif::parser` →
returns `ParsedDocument { ast: Vec<AstNode>, diagnostics }`. Macros
(`row-vars` etc.) expand inside the parser. Always emits a document even when
errors are present.

### 2.2 `kif_store_load`
`crate::kif_store::load_kif(&mut store, text, file_tag)`:
`parse_pipeline` → for each `AstNode` → intern symbols (`store.intern_symbol`)
→ build `Sentence` → assign `SentenceId` → push to `store.sentences`,
`store.roots`, `store.file_roots[file_tag]`, `store.file_hashes[file_tag]` via
`parse::fingerprint::sentence_fingerprint` → return `Vec<(Span, KbError)>`
parse errors.

### 2.3 `ingest_pipeline` (`kb::mod::ingest`, internal)
Called by `kb.tell` and `kb.load_kif`.

1. Snapshot `prev_root_count = layer.store.file_roots[file_tag].len()`.
2. `kif_store_load` (§2.2) → `parse_errors`. If non-empty, set `result.ok=false`
   and continue (do not abort).
3. `new_roots = file_roots[file_tag][prev_root_count..]`.
4. `?[validate=true]` for each `sid` in `new_roots`:
   `crate::semantic::SemanticLayer::validate_sentence(sid)` →
   push `TellWarning::Semantic(e)` on error.
5. `[feature: cnf]` dedup loop:
   - `clausify_with_bisection(layer, new_roots)` → batched IR clauses by sid
     (one Vampire global-mutex acquisition).
   - for each `sid` in `new_roots`:
     - `crate::cnf::translate_ir_clauses(&mut store, ir_cs)` → `Vec<Clause>`.
     - `canonical::canonical_clause_hash` per clause →
       `canonical::formula_hash_from_clauses(&hashes)` → `fh: u64`.
     - `?[fh in self.fingerprints]` → push
       `TellWarning::DuplicateAxiom`/`DuplicateAssertion`; mark duplicate.
     - else → `self.fingerprints.insert(fh, (sid, Some(session)))`,
       `self.clauses.insert(sid, clauses)`, push `sid` to `accepted`.
   - `[!feature: cnf]` → all `new_roots` accepted unconditionally.
6. `self.sessions[session].extend(&accepted)`.
7. `layer.extend_taxonomy_with(&accepted)` (incremental taxonomy update).
8. Return `TellResult { ok, errors, warnings }`.

### 2.4 `semantic_pipeline` (`crate::semantic::SemanticLayer::validate_sentence`)
Walk `Sentence.formula` recursively → for each element check symbol-kind
constraints (`is_instance`/`is_relation`/`is_function`/`is_predicate`) →
check arity → check `expected_arg_class` against domain declarations →
return `Result<(), SemanticError>`. The `_findings` variants
(`kb.validate_sentence_findings`, `kb.validate_all_findings`) wrap each
sentence call in `crate::error::with_collector` to capture *every* finding
(warnings + errors) classified into `Findings { errors, warnings }`.

### 2.5 `tptp_assembly_pipeline`
Used by `kb.to_tptp`, `kb.format_sentence_tptp`, `kb.ask`, `kb.ask_embedded`,
`kb.check_consistency`, `kb.promote_assertions`.

`crate::vampire::converter::NativeConverter::new(store, layer, mode)`
(or `from_parts` when seeded from the cache) → for each axiom sid
`conv.add_axiom(sid)` → optional `conv.set_conjecture(sid)` → `conv.finish()`
→ `(IrProblem, sid_map)`. Then
`crate::vampire::assemble::assemble_tptp(problem, sid_map, AssemblyOpts {
conjecture_name, axiom_filter, show_kif, layer, .. })` → `String`. The
`axiom_filter` is the SInE-selected ∪ session-assertion sid set in the `ask`
path; `None` everywhere else.

### 2.6 `axiom_cache_pipeline` (`kb::prove::ensure_axiom_cache`) `[feature: ask]`
Three-tier resolution:

1. `?[self.axiom_cache.is_some()]` → return.
2. `[feature: persist]` LMDB restore: `env.read_txn()` →
   `env.kb_version(&rtxn)` → `env.get_cache::<CachedAxiomProblem>(rtxn,
   CACHE_KEY_AXIOM_CACHE_TFF)` and `..._FOF`. `?[both present && both versions
   match && tff.mode_tff && !fof.mode_tff]` → hydrate
   `self.axiom_cache = VampireAxiomCacheSet { tff, fof }`; return.
3. Rebuild: `axiom_ids = self.axiom_ids_set()` →
   `crate::vampire::VampireAxiomCacheSet::build(layer, axiom_ids)` (builds
   both TFF and FOF in one pass) → `[feature: persist]`
   `crate::persist::persist_axiom_cache(env, mode_tff=true, ...)` and
   `mode_tff=false`.

### 2.7 `prover_invocation_pipeline` `[feature: ask]`
Subprocess: `crate::prover::subprocess::VampireRunner::prove(tptp,
&ProverOpts)` → spawn `vampire_path` child via `std::process::Command` with
args from `build_vampire_args` → write TPTP to stdin → wait with timeout →
parse TSTP-style stdout via `crate::vampire::native_proof::parse_szs_status`
and `crate::vampire::bindings::extract_bindings_from_tptp` → return
`ProverResult { status, raw_output, bindings, proof_kif, proof_tptp,
timings }`. Optional `tptp_dump_path`: write TPTP to disk before invoking.

Embedded `[feature: integrated-prover]`: `vampire_prover::Options::new()`
with `mode=vampire`, `sine_selection=off`, timeout →
`vampire_prover::lower_problem(&ir_problem, opts)` →
`problem.solve_and_prove()` → `(ProofRes, Option<Proof>)`. On `Proved`:
`crate::vampire::bindings::extract_bindings(&proof, &qvm)` and
`crate::vampire::native_proof::native_proof_to_kif_steps(&proof)` →
`Vec<KifProofStep>`.

### 2.8 `proof_rendering_pipeline` `[feature: ask]`
After `prover_invocation_pipeline` returns a proof, callers may render it via
`crate::tptp::kif::proof_steps_to_kif`. Source attribution uses
`crate::axiom_source::AxiomSourceIndex::lookup` (built lazily from
`kb.build_axiom_source_index()`) — maps each axiom-role step back to a
`(file, line)` via canonical formula fingerprint. Natural-language rendering
of formulas goes through `crate::natural_lang::RenderReport` using SUMO's
`format`/`termFormat` strings.

### 2.9 `sine_maintenance` (`crate::sine::SineIndex`)
- Add: `SineIndex::add_axioms(&store, sids)` — for each sid:
  `symbols_of_axiom(store, sid)` → update trigger maps.
- Remove: `SineIndex::remove_axiom(sid)`.
- Select: `SineIndex::select(seed_symbols, depth_limit)` →
  `HashSet<SentenceId>`.
- Used by: `make_session_axiomatic`, reconcile (`apply_removals`,
  `apply_additions`, smart revalidation), `kb.ask`/`ask_embedded`
  (via `sine_select_for_query`).

### 2.10 `persist_pipeline` `[feature: persist]`
`crate::persist::commit::write_axioms(env, store, sids, clause_map, session)`
→ open LMDB write txn → for each sid: serialize `Sentence` + `Clause`s →
write to main table + head index + path index + formula-hash map → bump
`kb_version` → commit. `crate::persist::commit::delete_formula(wtxn, sid)`
removes a single sentence across all indexes.

### 2.11 `reconcile_pipeline` (`kb::reconcile`)
Single-file `kb.reconcile_file(file, new_text)`:

1. `reconcile_parse(file, new_text)` → `parse_pipeline` → `?[has_errors]`
   populate `report.parse_errors`, return early (no mutation).
2. `reconcile_compute_diff(file, doc)` → diff `(retained, removed, added)` by
   comparing per-root structural fingerprints against
   `store.file_hashes[file]`.
3. for each `(sid, new_span)` in `retained`:
   `store.update_sentence_span(sid, new_span)` (no revalidation).
4. `?[removed.is_empty() && added.is_empty()]` → return (noop fast path).
5. `removed_touches_tax = self.any_touches_taxonomy(&removed)`.
6. `reconcile_apply_removals(removed, &mut altered_syms)` — for each
   removed sid: pull from store, SInE index (`sine_maintenance.remove`),
   CNF fingerprint/clauses side-cars, axiom cache; collect symbols mentioned.
7. `reconcile_apply_additions(file, added, altered_syms, report)` — re-emit
   added AST as KIF text, run through `ingest_pipeline` (§2.3) →
   `make_session_axiomatic` (§2.12) on a temp session; collect symbols;
   returns `added_touches_tax`.
8. `?[removed_touches_tax || added_touches_tax]` →
   `layer.rebuild_taxonomy()`.
9. `[feature: ask]` `self.axiom_cache = None`.
10. `reconcile_smart_revalidate(altered_syms, report)`: SInE
    `select(altered_syms, depth)` → for each selected sid:
    `report.revalidated += 1`; `layer.validate_sentence(sid)` → push errors.

Batched `kb.reconcile_files(items)`:

- Phase 1 (per-file): parse + diff + retained-span updates + apply_removals +
  `reconcile_apply_additions_deferred` (ingests but defers promotion).
- Skip phases 2–4 `?[no adds_or_removes]`.
- Phase 2: one `kb.make_session_axiomatic(SESSION_RECONCILE_ADD)`.
- Phase 3: one `layer.rebuild_taxonomy()` `?[needs_tax_rebuild]`;
  `[feature: ask]` axiom_cache cleared.
- Phase 4: one SInE `select(altered_syms_union, depth)` → for each selected
  sid: `validate_sentence(sid)` → fan errors back to per-file report by
  `Sentence.file`.

### 2.12 `promote_pipeline` (`kb.make_session_axiomatic`)
1. `sids = self.sessions.remove(session)`.
2. `[feature: cnf]` retag fingerprints in place: any `(_, Some(s))` where
   `s == session` → `(_, None)`.
3. for each sid: `store.register_axiom_symbols(sid)`.
4. `sine_maintenance.add_axioms(store, sids)`.
5. `[feature: ask]` `self.axiom_cache = None`.

(Persistent variant `kb.promote_assertions_unchecked` adds steps before
SInE: cross-session dedup via fingerprints, `semantic_pipeline` per
surviving sid, `persist::commit::write_axioms`,
`persist::persist_taxonomy_cache`,
`persist::persist_sort_annotations_cache`. `kb.promote_assertions` runs a
prior `check_consistency` against the prover.)

### 2.13 `flush_pipeline` (`kb.flush_session`)
1. `sids = self.sessions.remove(session)`.
2. `[feature: cnf]` `fingerprints.retain(|_, (_, s)| s.as_deref() != Some(session))`.
3. `store.remove_file(session)`.
4. `layer.rebuild_taxonomy()`.
5. `layer.invalidate_cache()`.
6. `[feature: cnf]` for each sid: `clauses.remove(sid)`.

---

## 3. Op flows

### 3.1 IngestOp `[always]`

**Entry point**
`sigmakee_rs_sdk::ingest::IngestOp::new(&mut kb)` → `.add_file/add_dir/add_source/
add_sources/progress(...)` → `IngestOp::run(self) -> SdkResult<IngestReport>`.

**Inputs**
`Vec<Source>` where `Source ∈ { File(PathBuf), Dir(PathBuf), Inline { tag,
text } }`; optional `Box<dyn ProgressSink>`.

**Activity nodes**
1. `expand_sources(sources)` → `Vec<ResolvedSource>`. For each:
   - `Source::Inline { tag, text }` → `ResolvedSource::Inline { tag, text }`
   - `Source::File(p)` → `ResolvedSource::Disk(p)`
   - `Source::Dir(d)` → `scan_dir_for_kif(&d)` (`fs::read_dir` → filter
     `*.kif` extension → sort) → push each child as `Disk`.
2. Emit `ProgressEvent::LoadStarted { total_sources }`.
3. for `(idx, src)` in resolved (sequential):
   - `?[Disk(path)]` → `std::fs::read_to_string(&path)` (Err →
     `SdkError::Io`) → emit `ProgressEvent::FileRead { path, idx, total,
     bytes }` → `(tag = path.display(), text)`.
   - `?[Inline]` → `(tag, text)` directly.
   - `ingest_one(kb, &tag, &text, base_session = SESSION_FILES)`:
     - `?[!kb.file_roots(tag).is_empty()]` (tag already in KB) →
       `kb.reconcile_file(tag, text)` → §2.11 `reconcile_pipeline`. If
       `parse_errors` non-empty → return `Err(SdkError::Kb(first))`. Build
       `SourceIngestStatus { added, removed, retained, semantic_warnings,
       was_reconciled: true }`.
     - else (fresh load) → `kb.load_kif(text, tag, Some(base_session))` →
       §2.3 `ingest_pipeline`. `?[!result.ok]` → return `Err(SdkError::Kb)`
       (or `SdkError::Config` if no errors). `added = kb.file_roots(tag).
       len()`; `removed=0`; `retained=0`; `was_reconciled=false`.
   - Emit `ProgressEvent::SourceIngested { tag, added, removed, retained }`.
   - Accumulate counts and push status into `IngestReport.sources`.
4. `kb.make_session_axiomatic(SESSION_FILES)` → §2.12 `promote_pipeline`.
5. Return `Ok(IngestReport)`.

**Sub-pipelines invoked**
- §2.11 `reconcile_pipeline` (when tag exists)
- §2.3 `ingest_pipeline` (fresh load)
- §2.12 `promote_pipeline` (always, at end)

**Outputs**
`IngestReport { total_added, total_removed, total_retained,
sources: Vec<SourceIngestStatus> }`.

**Decision points**
- Source kind: `File` vs `Dir` vs `Inline` (in `expand_sources`).
- Tag presence in KB: `kb.file_roots(tag).is_empty()` decides reconcile vs
  fresh load (in `ingest_one`).
- Parse-error abort: any reconcile or load parse error → return `Err`.

**Loops**
- `for child in scan_dir_for_kif(d)` (per-Dir source).
- `for (idx, src) in resolved.into_iter().enumerate()` (main ingest loop).

---

### 3.2 ValidateOp `[always]`

**Entry point**
`ValidateOp::all(&mut kb)` or `ValidateOp::formula(&mut kb, tag, text)` →
`.parse_only(bool)` / `.skip_kb_check(bool)` → `.run() ->
SdkResult<ValidationReport>`.

**Inputs**
`ValidateTarget ∈ { All, Formula { tag, text } }`; flags `parse_only`,
`skip_kb_check`.

**Activity nodes**
1. `?[target]`:
   - `All` → `validate_all(kb, parse_only)`:
     a. `?[parse_only]` → return `ValidationReport::default()` (no-op
        success: KB sentences already parsed).
     b. else → `kb.validate_all_findings()` → §2.4
        `semantic_pipeline` over every root sid → wrap as
        `ValidationReport { semantic_errors, semantic_warnings,
        parse_errors: empty, inspected, session: None }`.
   - `Formula { tag, text }` → `validate_formula(kb, tag, text, parse_only,
     skip_kb_check)`:
     a. `?[!parse_only && !skip_kb_check]` →
        `kb.validate_all_findings()` → extend `report.semantic_errors`
        and `report.semantic_warnings` (whole-KB pre-check).
     b. `kb.load_kif(text, tag, Some(tag))` → §2.3 `ingest_pipeline`.
     c. `?[!result.ok]` → `report.parse_errors = result.errors`; return
        `Ok(report)` (errors carried in report, not bubbled).
     d. `?[parse_only]` → return `Ok(report)`.
     e. `sids = kb.session_sids(tag)`. `?[sids.is_empty()]` → return
        `Err(SdkError::Config)`.
     f. for each sid: `kb.validate_sentence_findings(sid)` → §2.4
        `semantic_pipeline` (single sentence) → extend errors and warnings.

**Sub-pipelines invoked**
- §2.4 `semantic_pipeline` (per-sentence and whole-KB variants).
- §2.3 `ingest_pipeline` (Formula target only).

**Outputs**
`ValidationReport { semantic_errors: Vec<(SentenceId, SemanticError)>,
semantic_warnings, parse_errors: Vec<KbError>, inspected: usize,
session: Option<String> }`.

**Decision points**
- Target: `All` vs `Formula`.
- Short-circuit: `parse_only` skips semantic work.
- Short-circuit: `skip_kb_check` skips whole-KB pre-check on Formula.
- Parse error in Formula: report-carried, not bubbled.
- Empty session after parse: `SdkError::Config` (bubbles).

**Loops**
- `for sid in sids: kb.validate_sentence_findings(sid)` (Formula target,
  semantic pass).

---

### 3.3 TranslateOp `[always]`

**Entry point**
`TranslateOp::kb(&mut kb)` or `TranslateOp::formula(&mut kb, tag, text)` →
`.lang(TptpLang)` / `.show_numbers(bool)` / `.show_kif_comments(bool)` /
`.session(s)` / `.options(TptpOptions)` → `.run() ->
SdkResult<TranslateReport>`.

**Inputs**
`TranslateTarget ∈ { Kb, Formula { tag, text } }`; `TptpOptions` (lang,
hide_numbers, show_kif_comment, query, excluded); optional session name.

**Activity nodes**
1. `?[target]`:
   - `Kb` → `translate_kb(kb, opts, session)`:
     a. `kb.validate_all()` → §2.4 `semantic_pipeline` → `semantic_warnings`
        (warnings only — never fatal).
     b. `kb.to_tptp(opts, session)` → §2.5 `tptp_assembly_pipeline` (whole
        KB; iterates `axiom_ids_set()` sorted, optionally appends session
        assertions, applies `excluded` head-name filter).
     c. Return `TranslateReport { tptp, sentences: empty,
        semantic_warnings, session }`.
   - `Formula { tag, text }` → `translate_formula(kb, tag, text, opts)`:
     a. `kb.load_kif(text, tag, Some(tag))` → §2.3 `ingest_pipeline`.
     b. `?[!result.ok]` → `Err(SdkError::Kb(first))` (parse failures are
        infrastructural here — can't translate unparseable text).
     c. `sids = kb.session_sids(tag)`. `?[sids.is_empty()]` →
        `Err(SdkError::Config)`.
     d. for each sid (parallel-conceptually but serial in code):
        `kb.validate_sentence(sid)` → push `(sid, e)` into
        `semantic_warnings` on Err (warnings, not fatal).
     e. for each sid (sequential): `kb.sentence_kif_str(sid)` (renders KIF
        via `crate::kif_store::sentence_to_plain_kif`) +
        `kb.format_sentence_tptp(sid, opts)` → §2.5
        `tptp_assembly_pipeline` (single sentence). Optionally prepend
        `% <kif>\n` `?[opts.show_kif_comment]`. Push
        `TranslatedSentence { sid, kif, tptp }` and concatenate into
        `combined`.
     f. `report.tptp = combined`.

**Sub-pipelines invoked**
- §2.4 `semantic_pipeline` (whole-KB on Kb target; per-sentence on Formula).
- §2.3 `ingest_pipeline` (Formula target only).
- §2.5 `tptp_assembly_pipeline` (always — once for Kb, per-sentence for
  Formula).

**Outputs**
`TranslateReport { tptp: String, sentences: Vec<TranslatedSentence>,
semantic_warnings, session: Option<String> }`.

**Decision points**
- Target: `Kb` vs `Formula`.
- Formula parse error: bubbles as `Err` (unlike ValidateOp).
- Per-sentence: `opts.show_kif_comment` decides KIF prefix.

**Loops**
- `for sid in sids` (twice on Formula target — semantic pass, then
  per-sentence emit).

---

### 3.4 LoadOp `[feature: persist]`

**Entry point**
`LoadOp::new(&mut kb)` → `.add_file/add_dir/add_source/add_sources/strict/
progress(...)` → `.run() -> SdkResult<LoadReport>`.

**Inputs**
`Vec<Source>` (same shape as IngestOp); `strict: bool` (default `true`);
optional progress sink.

**Activity nodes**
1. `materialise_sources(sources, &mut progress)`:
   a. Partition into `paths: Vec<PathBuf>` (File + Dir-expanded via
      `scan_dir_for_kif`) and `inline: Vec<(String, String)>`.
   b. for `(idx, path)` in paths: `fs::read_to_string(&path)` (Err →
      `SdkError::Io`) → emit `ProgressEvent::FileRead { path, idx, total,
      bytes }` → push `(path.display(), text)`.
   c. Append `inline` after disk reads (path-displayed tags first,
      preserving order across both kinds).
2. `?[materialised.is_empty()]` → return `Ok(LoadReport::default())`
   (committed=false, no-op explicit).
3. Emit `ProgressEvent::LoadStarted { total_sources }`.
4. `kb.reconcile_files(materialised.iter().map(|(t,x)| (t.as_str(),
   x.as_str())))` → §2.11 batched variant — single bulk SInE rebuild +
   single taxonomy rebuild for the whole batch.
5. for each `r: ReconcileReport` (sequential):
   a. `?[!r.parse_errors.is_empty()]` → return `Err(SdkError::Kb(first))`
      (in-memory KB already mutated for files reconciled before this point;
      caller decides whether to drop).
   b. for each semantic error: push `(tag, e)` into
      `report.semantic_errors`.
   c. Emit `ProgressEvent::SourceIngested`.
   d. Push `LoadFileStatus { tag, added, removed, retained,
      semantic_warnings: r.semantic_errors }`.
   e. Push `(tag, r.removed_sids, r.added_sids)` into `pending`.
6. `?[strict && !report.semantic_errors.is_empty()]` → log warn, return
   `Ok(report)` with `committed=false` (DB untouched).
7. Emit `ProgressEvent::PromoteStarted { session: SESSION_LOAD }`.
8. for `(tag, removed, added)` in `pending`:
   `kb.persist_reconcile_diff(removed, added)` → §2.10 `persist_pipeline`
   (two LMDB write txns: phase-1 deletes, phase-2 `write_axioms`). Err →
   bubble as `Err(SdkError::Kb)` (earlier files already on disk;
   reconcile is idempotent so next run completes cleanly).
9. Emit `ProgressEvent::PromoteFinished { promoted, duplicates: 0,
   elapsed }`.
10. `report.committed = true`; return `Ok(report)`.

**Sub-pipelines invoked**
- §2.11 batched `reconcile_pipeline`.
- §2.10 `persist_pipeline` (per pending file, after strict gate).

**Outputs**
`LoadReport { total_added, total_removed, total_retained,
files: Vec<LoadFileStatus>, semantic_errors: Vec<(String, SemanticError)>,
committed: bool }`.

**Decision points**
- Empty source list: short-circuit before LoadStarted.
- Per-file parse error: bubbles `Err` mid-batch.
- `strict` gate: aborts commit if any semantic error.
- Per-file persist failure: bubbles `Err` mid-commit (partial commit
  acceptable; reconcile is idempotent).

**Loops**
- `for path in paths` (disk reads in `materialise_sources`).
- `for r in reports` (per-file walk after batched reconcile).
- `for (tag, removed, added) in pending` (commit loop).

---

### 3.5 AskOp `[feature: ask]`

**Entry point**
`AskOp::new(&mut kb, query)` → `.tell/tells/session/timeout_secs/backend/
lang/vampire_path/tptp_dump/progress(...)` → `.run() -> SdkResult<AskReport>`.

**Inputs**
`query: String` (KIF conjecture); `tells: Vec<String>` (extra session
hypotheses); `session: String` (default `"<inline>"`); `timeout_secs: u32`
(default 30); `backend: ProverBackend ∈ { Subprocess, Embedded }`;
`lang: TptpLang ∈ { Fof, Tff }`; optional `vampire_path`, `tptp_dump`,
progress sink.

**Activity nodes**
1. for each `kif` in `tells`: `kb.tell(&session, kif)` → §2.3
   `ingest_pipeline` (validate=true, includes per-sentence semantic check).
   `?[!r.ok]` → return `Err(SdkError::Kb(first))` (or `Config`).
2. Emit `ProgressEvent::AskStarted { backend: backend.label() }`; capture
   `t_ask = Instant::now()`.
3. `?[backend]`:
   - `Subprocess` → `path = vampire_path.unwrap_or("vampire")` →
     `runner = VampireRunner { vampire_path, timeout_secs, tptp_dump_path }`
     → `kb.ask(&query, Some(&session), &runner, lang)`:
     a. `kb.sine_select_for_query(query, SineParams::default())` (parses
        conjecture into temp tag, walks symbols, rolls back) →
        `selected_axioms`. Err → return `ProverResult::Unknown { raw_output:
        "SInE selection failed" }`.
     b. `kb.parse_conjecture(SESSION_QUERY, query)`:
        `kif_store::load_kif(&mut store, query, query_tag)` → on parse error:
        `store.remove_file(query_tag)`, return `Unknown` ProverResult.
        On empty parse: same. Else return `query_sids`.
     c. `assertion_ids = sessions[session]` (HashSet).
     d. §2.6 `axiom_cache_pipeline` (`ensure_axiom_cache`).
     e. Clone seed `(problem, sid_map)` from
        `axiom_cache.as_ref().unwrap().get(mode)`.
     f. `NativeConverter::from_parts(store, layer, seed_problem,
        seed_sid_map, mode)` → for each assertion sid: `conv.add_axiom(sid)`
        → for each query sid: `conv.set_conjecture(sid)` (break on
        first success) → `conv.finish() → (problem, sid_map)`.
     g. `allowed = selected_axioms ∪ assertion_ids` →
        `assemble_tptp(problem, sid_map, AssemblyOpts {
        conjecture_name: "query_0", axiom_filter: Some(&allowed) })`.
     h. Rollback: `query_affects_taxonomy(query_sids)` →
        `store.remove_file(query_tag)` → `?[needs_rebuild]`
        `layer.rebuild_taxonomy()` + `layer.invalidate_cache()`.
     i. §2.7 `prover_invocation_pipeline` (subprocess) →
        `runner.prove(&tptp, &ProverOpts)` → `ProverResult`.
     j. Set `result.timings.input_gen = elapsed`; return.
   - `Embedded` `[feature: integrated-prover]` →
     `kb.ask_embedded(&query, Some(&session), timeout_secs, lang)`:
     a–c. Same as Subprocess steps a–c but with
          `query_tag = SESSION_QUERY_EMBEDDED`.
     d. §2.6 `axiom_cache_pipeline`.
     e. `cache.filtered_problem(&allowed)` (IR-level filter, not
        TPTP-string filter) → seed for converter.
     f. `NativeConverter::from_parts(...)` → append assertion sids →
        `set_conjecture(sid)` (capture `query_var_map: QueryVarMap` on
        success) → `conv.finish() → (ir_problem, _)`.
     g. Configure `vampire_prover::Options { mode: "vampire",
        sine_selection: "off", timeout }`.
     h. §2.7 `prover_invocation_pipeline` (embedded):
        `lower_problem(&ir_problem, opts)` →
        `problem.solve_and_prove() → (ProofRes, Option<Proof>)`.
     i. Map `ProofRes` → `ProverStatus`.
     j. `?[status == Proved && proof.is_some()]`: `extract_bindings(&p,
        &qvm)` → `Vec<Binding>`; `native_proof_to_kif_steps(&p)` →
        `Vec<KifProofStep>`. Else `(empty, empty)`.
     k. Rollback (same as subprocess step h).
     l. Return `ProverResult` with `proof_kif` populated, `proof_tptp`
        empty, timings populated.
4. Emit `ProgressEvent::AskFinished { status, elapsed }`.
5. `?[backend == Subprocess && status == Unknown && raw_output contains
   ("No such file" || "not found" || "cannot find")]` → return
   `Err(SdkError::VampireNotFound(raw_output))`.
6. Return `Ok(AskReport { status, bindings, raw_output, proof_kif,
   proof_tptp, timings })`.

**Sub-pipelines invoked**
- §2.3 `ingest_pipeline` (per tell).
- §2.9 `sine_maintenance` (`sine_select_for_query` uses `SineIndex::select`).
- §2.6 `axiom_cache_pipeline`.
- §2.5 `tptp_assembly_pipeline` (Subprocess) or its IR-level analogue
  (Embedded).
- §2.7 `prover_invocation_pipeline`.
- §2.8 `proof_rendering_pipeline` (caller-driven; `proof_kif` field is
  populated when `Proved`).

**Decision points**
- Tell failure: bubbles `Err` mid-loop (session would be inconsistent).
- Backend dispatch: `Subprocess` vs `Embedded`.
- Conjecture parse failure: returns ready-made `Unknown` ProverResult
  (NOT `Err`).
- Vampire-not-found heuristic: `Subprocess` + `Unknown` + raw_output marker
  → promote to `Err(SdkError::VampireNotFound)`.
- Taxonomy rollback: skipped unless query head touches `subclass`/
  `instance`/`subrelation`/`subAttribute`.

**Loops**
- `for kif in &tells: kb.tell(...)`.
- `for sid in &assertion_ids: conv.add_axiom(sid)`.
- `for qsid in &query_sids: conv.set_conjecture(qsid)` (break on first
  success).

---

### 3.6 TestOp `[feature: ask]`

**Entry point**
`TestOp::new(&mut kb)` → `.add_file/add_dir/add_text/add_case/
timeout_override/backend/lang/vampire_path/tptp_dump/progress(...)` →
`.run() -> SdkResult<TestSuiteReport>`.

**Inputs**
`Vec<Source>` where `Source ∈ { File(PathBuf), Dir(PathBuf), Text { tag,
content }, Parsed { tag, case } }`; optional `timeout_override`, backend
config.

**Activity nodes**
1. `expand_sources(sources)` → `Vec<Prepared { tag, parsed: Result<TestCase,
   String> }>`. For each source:
   - `File(p)` → `prepare_from_disk(p)`: `fs::read_to_string` (Err →
     `SdkError::Io`) → `parse_test_content(content, tag)` (in
     `crate::tptp::test_case`) → wrap result in `Prepared`.
   - `Dir(d)` → `scan_dir_for_tests(d)` (`fs::read_dir` → filter
     `*.kif.tq` → sort) → `prepare_from_disk(child)` for each.
   - `Text { tag, content }` → `parse_test_content(content, tag)` →
     `Prepared`.
   - `Parsed { tag, case }` → `Prepared { tag, parsed: Ok(case) }`.
2. for `(idx, prepared)` in cases (sequential):
   - `case_report = run_one(kb, idx, &prepared, timeout_override, backend,
     lang, vampire_path, tptp_dump)`:
     a. `?[prepared.parsed]`: `Err(msg)` → return
        `TestCaseReport { outcome: ParseError(msg), .. }`.
     b. `session = "sigmakee-rs-sdk-test-{idx}"`; `load_tag =
        "sigmakee-rs-sdk-test-src-{idx}"`.
     c. `?[!case.axioms.is_empty()]` → `axiom_text = case.axioms.join("\n")`
        → `kb.load_kif(&axiom_text, &load_tag, Some(&session))` → §2.3
        `ingest_pipeline`. `?[!load_result.ok]` →
        `kb.flush_session(&session)` → §2.13; return `ParseError` outcome.
        Then `kb.validate_session(&session)` → §2.4 `semantic_pipeline`.
        `?[!empty]` → `kb.flush_session(&session)`; return `SemanticError`
        outcome.
     d. `?[case.query is None]` → `kb.flush_session(&session)`; return
        `NoQuery` outcome.
     e. `timeout = timeout_override.unwrap_or(case.timeout)`.
     f. `?[backend]`:
        - `Subprocess` → build `VampireRunner` →
          `kb.ask(&query, Some(&session), &runner, lang)` (full §3.5
          subprocess flow).
        - `Embedded` → `kb.ask_embedded(&query, Some(&session), timeout,
          lang)`.
     g. `kb.flush_session(&session)` → §2.13 (always — axioms cannot leak
        between cases).
     h. Classify outcome:
        - `?[status == Unknown && !proved && !raw_output.is_empty()]` →
          `ProverError(raw_output)`.
        - `?[proved == case.expected_proof]`:
          - `?[case.expected_answer is Some]`: compute `inferred = result.
            bindings.values()` → `missing = expected.filter(!inferred.
            contains)`. `?[!missing.is_empty()]` → `Incomplete { inferred,
            missing }`. else → `Passed`.
          - else → `Passed`.
        - else → `Failed { expected, got: proved }`.
   - Map outcome to `brief` short label.
   - Emit `ProgressEvent::TestCase { idx, total, tag, brief }`.
   - Tally counts:
     - `Passed` → `passed += 1`.
     - `Failed | Incomplete | ProverError` → `failed += 1`.
     - `ParseError | SemanticError | NoQuery` → `skipped += 1`.
   - Push into `report.cases`.
3. Return `Ok(TestSuiteReport)`.

**Sub-pipelines invoked**
- §2.3 `ingest_pipeline` (per case axioms).
- §2.4 `semantic_pipeline` (per case via `validate_session`).
- §2.13 `flush_pipeline` (after every case, always).
- §3.5 `AskOp` internals (`kb.ask` / `kb.ask_embedded`) per case.

**Decision points**
- Source kind in `expand_sources`: File/Dir/Text/Parsed.
- Per-case parse failure: report-carried `ParseError`, not bubbled.
- Empty axioms: skip load step.
- Semantic error in case axioms: flush + `SemanticError` outcome.
- No query in case: `NoQuery` outcome.
- Backend dispatch.
- Outcome classification: `ProverError`/`Passed`/`Failed`/`Incomplete`.
- Expected vs got: drives Pass/Fail.
- Expected answers vs inferred bindings: drives Incomplete check.

**Loops**
- `for child in scan_dir_for_tests` (per Dir source).
- Main case loop `for (idx, prepared) in cases.into_iter().enumerate()`.

---

### 3.7 manpage_view `[always]`

**Entry point**
`sigmakee_rs_sdk::man::manpage_view(kb: &KnowledgeBase, symbol: &str) ->
Option<ManPageView>`. Read-only — no mutation. Also exposed as
`view_from_manpage(raw: ManPage) -> ManPageView` for callers that already
hold a `ManPage`.

**Inputs**
Symbol name string.

**Activity nodes**
1. `kb.manpage(symbol)` (`crate::kb::man`):
   a. `kb.symbol_id(symbol)` → `Option<SymbolId>`. `None` → return
      `Option::None` from outer `manpage_view`.
   b. `build_manpage(kb, sym_id, name)`:
      - Classify `kinds: Vec<ManKind>` via `is_class`/`is_function`/
        `is_predicate`/`is_relation`/`is_instance` (with deduplication for
        relation vs predicate/function).
      - `collect_parents(store, sym_id)` — for each rel in
        `["subclass", "instance", "subrelation", "subAttribute"]`:
        for `sid in store.by_head(rel)` → check `elements[1]` matches
        `sym_id` → push `ParentEdge { relation, parent }`.
      - `signature(kb, sym_id)` → `(arity, domains, range)` from KIF
        `(domain ...)` / `(range ...)` declarations.
      - `collect_refs(store, sym_id)` →
        `(ref_args: Vec<SentenceRef>, ref_nested: Vec<SentenceId>)`.
      - `kb.documentation(name, None)`, `kb.term_format(name, None)`,
        `kb.format_string(name, None)` — each returns `Vec<DocEntry>`
        from the semantic layer's per-symbol indexes.
      - Return `ManPage { name, kinds, documentation, term_format,
        format, parents, arity, domains, range, ref_args, ref_nested }`.
2. `view_from_raw(raw)`:
   - `signature = SignatureView { arity, domains, range }`.
   - `documentation = raw.documentation.into_iter().map(|d| DocBlock {
     language: d.language, spans: parse_doc_spans(&d.text) })`.
   - `term_format` and `format` similarly through
     `sigmakee_rs_sdk::man::parse_doc_spans`.
   - `parse_doc_spans(text)` (the **single** place the `&%Symbol`
     cross-ref syntax is parsed): scan bytes; on `&%` followed by ASCII
     identifier: emit prior `DocSpan::Text` run + `DocSpan::Link { text:
     symbol, target: symbol }`; first non-identifier byte terminates.
   - References bucketing: `?[ref_args.is_empty() && ref_nested.is_empty()]`
     → `ReferenceSet::default()`. else → `max_pos = ref_args.iter().map(.0).
     max()`; `by_position: Vec<Vec<SentenceId>> = vec![vec![]; max_pos+1]`;
     for each `r: SentenceRef`: `by_position[r.0].push(r.1)`. Set
     `nested = ref_nested`.
   - Return `ManPageView { name, kinds, parents, signature, documentation,
     term_format, format, references }`.

**Sub-pipelines invoked**
- None outside `kb::man::build_manpage` and `man::parse_doc_spans` itself.

**Outputs**
`Option<ManPageView>` (`None` when symbol is unknown to the KB).

**Decision points**
- Symbol existence: `kb.symbol_id(symbol)?` (early `None` return).
- Empty refs: skip bucket allocation.
- Per-doc-block scan: `&%` marker detection in `parse_doc_spans`.

**Loops**
- `for rel in TAX_RELATIONS` × `for sid in store.by_head(rel)` (parent
  collection).
- `for d in raw.documentation` / `term_format` / `format` (DocBlock
  conversion, three independent loops).
- `for r in raw.ref_args` (bucket assignment).
- Byte-scan loop in `parse_doc_spans`.

---

## 4. Cross-cutting concerns

### 4.1 Progress events
Every Op that takes a `progress: Box<dyn ProgressSink>` installs it before
running. Events emitted (defined in `crate::progress::ProgressEvent`):

- `LoadStarted { total_sources }` — IngestOp, LoadOp at start.
- `FileRead { path, idx, total, bytes }` — per-disk-source read in
  IngestOp / LoadOp.
- `SourceIngested { tag, added, removed, retained }` — per-source result
  in IngestOp / LoadOp.
- `PromoteStarted { session }` / `PromoteFinished { promoted, duplicates,
  elapsed }` — LoadOp commit phase.
- `AskStarted { backend }` / `AskFinished { status, elapsed }` — AskOp.
- `TestCase { idx, total, tag, brief }` — TestOp per case.
- `Log { level, target, message }` — emitted from inside `kb` for debug
  / info / warn lines (every pipeline).
- `PhaseStarted` / `PhaseFinished` — `profile_span!` and `profile_call!`
  markers throughout `kb` (ingest, ask, promote, etc.); always on (no
  feature gate). Consumers aggregate into per-phase timings.

### 4.2 Error model
- `SdkError` variants: `Kb(KbError)`, `Io { path, source }`, `DirRead {
  path, message }`, `Config(String)`, `Persist(KbError)`,
  `VampireNotFound(String)`.
- "Findings ride in report; only infra failures bubble" applies to every
  Op:
  - Parse errors during `tell`/`load_kif`: report-carried in
    `ValidationReport.parse_errors` (ValidateOp) and `IngestReport`/
    `LoadReport`/`AskReport.tells` paths bubble as `SdkError::Kb`.
  - Semantic errors: always report-carried.
  - I/O failures (read_to_string, read_dir): always bubble.
  - Prover spawn / not-found: AskOp promotes "vampire not found" to
    `SdkError::VampireNotFound` via raw-output substring sniff; other
    Unknown verdicts ride in `AskReport`.
  - Persist failures: bubble mid-batch in LoadOp; earlier files remain
    committed (reconcile is idempotent).

### 4.3 Feature gates
| Gate | Guards | Affected Ops |
|------|--------|--------------|
| `cnf` | dedup + clausify side-cars in `ingest_pipeline`; `to_tptp_cnf`; clause-map in `persist_reconcile_diff` | All Ops (silent quality reduction without it) |
| `persist` | `LoadOp`, `LMDB-backed `KnowledgeBase::open`, `persist_reconcile_diff`, `axiom_cache` LMDB restore in `axiom_cache_pipeline` | LoadOp only (others tolerate absence) |
| `ask` | `AskOp`, `TestOp`, `kb.ask`, `kb.check_consistency`, `axiom_cache` field | AskOp, TestOp |
| `integrated-prover` | `ProverBackend::Embedded`, `kb.ask_embedded`, FFI lower path | AskOp/TestOp embedded backend only |
| `parallel` | rayon-backed hot paths inside `kb` (transparent) | All (perf) |

### 4.4 Session conventions (`crate::session_tags`)
- `SESSION_FILES` — IngestOp's promotion target.
- `SESSION_LOAD` — LoadOp's promote-event session label.
- `SESSION_QUERY` / `SESSION_QUERY_EMBEDDED` — temp tags for AskOp's
  conjecture parse, scrubbed on rollback.
- `SESSION_RECONCILE_ADD` — batched `reconcile_files` shared session.
- `"sigmakee-rs-sdk-test-{idx}"` — per-case session in TestOp (always flushed).
- `"<inline>"` — AskOp's default session.

---

## 5. Glossary of node names

For consistent labels across diagrams.

| Long name | Short label |
|-----------|-------------|
| `sigmakee_rs_sdk::IngestOp::run` | `IngestOp.run` |
| `sigmakee_rs_sdk::ValidateOp::run` | `ValidateOp.run` |
| `sigmakee_rs_sdk::TranslateOp::run` | `TranslateOp.run` |
| `sigmakee_rs_sdk::LoadOp::run` | `LoadOp.run` |
| `sigmakee_rs_sdk::AskOp::run` | `AskOp.run` |
| `sigmakee_rs_sdk::TestOp::run` | `TestOp.run` |
| `sigmakee_rs_sdk::manpage_view` | `manpage_view` |
| `sigmakee_rs_sdk::ingest::ingest_one` | `ingest_one` |
| `sigmakee_rs_sdk::test::run_one` | `test::run_one` |
| `sigmakee_rs_sdk::test::expand_sources` | `test::expand_sources` |
| `sigmakee_rs_core::KnowledgeBase::tell` | `kb.tell` |
| `sigmakee_rs_core::KnowledgeBase::load_kif` | `kb.load_kif` |
| `sigmakee_rs_core::KnowledgeBase::ingest` (private) | `kb.ingest` |
| `sigmakee_rs_core::KnowledgeBase::reconcile_file` | `kb.reconcile_file` |
| `sigmakee_rs_core::KnowledgeBase::reconcile_files` | `kb.reconcile_files` |
| `sigmakee_rs_core::KnowledgeBase::persist_reconcile_diff` | `kb.persist_reconcile_diff` |
| `sigmakee_rs_core::KnowledgeBase::ask` | `kb.ask` |
| `sigmakee_rs_core::KnowledgeBase::ask_embedded` | `kb.ask_embedded` |
| `sigmakee_rs_core::KnowledgeBase::ensure_axiom_cache` (private) | `kb.ensure_axiom_cache` |
| `sigmakee_rs_core::KnowledgeBase::parse_conjecture` (private) | `kb.parse_conjecture` |
| `sigmakee_rs_core::KnowledgeBase::sine_select_for_query` | `kb.sine_select_for_query` |
| `sigmakee_rs_core::KnowledgeBase::query_affects_taxonomy` | `kb.query_affects_taxonomy` |
| `sigmakee_rs_core::KnowledgeBase::validate_sentence` | `kb.validate_sentence` |
| `sigmakee_rs_core::KnowledgeBase::validate_all` | `kb.validate_all` |
| `sigmakee_rs_core::KnowledgeBase::validate_all_findings` | `kb.validate_all_findings` |
| `sigmakee_rs_core::KnowledgeBase::validate_session` | `kb.validate_session` |
| `sigmakee_rs_core::KnowledgeBase::validate_sentence_findings` | `kb.validate_sentence_findings` |
| `sigmakee_rs_core::KnowledgeBase::to_tptp` | `kb.to_tptp` |
| `sigmakee_rs_core::KnowledgeBase::format_sentence_tptp` | `kb.format_sentence_tptp` |
| `sigmakee_rs_core::KnowledgeBase::sentence_kif_str` | `kb.sentence_kif_str` |
| `sigmakee_rs_core::KnowledgeBase::session_sids` | `kb.session_sids` |
| `sigmakee_rs_core::KnowledgeBase::file_roots` | `kb.file_roots` |
| `sigmakee_rs_core::KnowledgeBase::make_session_axiomatic` | `kb.make_session_axiomatic` |
| `sigmakee_rs_core::KnowledgeBase::flush_session` | `kb.flush_session` |
| `sigmakee_rs_core::KnowledgeBase::manpage` | `kb.manpage` |
| `sigmakee_rs_core::parse::parse_document` | `parse_document` |
| `sigmakee_rs_core::kif_store::load_kif` | `kif_store::load_kif` |
| `sigmakee_rs_core::semantic::SemanticLayer::validate_sentence` | `layer.validate_sentence` |
| `sigmakee_rs_core::semantic::SemanticLayer::extend_taxonomy_with` | `layer.extend_taxonomy_with` |
| `sigmakee_rs_core::semantic::SemanticLayer::rebuild_taxonomy` | `layer.rebuild_taxonomy` |
| `sigmakee_rs_core::semantic::SemanticLayer::invalidate_cache` | `layer.invalidate_cache` |
| `sigmakee_rs_core::cnf::sentence_to_clauses` | `cnf::sentence_to_clauses` |
| `sigmakee_rs_core::cnf::translate_ir_clauses` | `cnf::translate_ir_clauses` |
| `sigmakee_rs_core::canonical::canonical_clause_hash` | `canonical_clause_hash` |
| `sigmakee_rs_core::canonical::formula_hash_from_clauses` | `formula_hash_from_clauses` |
| `sigmakee_rs_core::vampire::converter::NativeConverter::new`/`from_parts` | `NativeConverter::new`/`from_parts` |
| `sigmakee_rs_core::vampire::converter::NativeConverter::add_axiom` | `conv.add_axiom` |
| `sigmakee_rs_core::vampire::converter::NativeConverter::set_conjecture` | `conv.set_conjecture` |
| `sigmakee_rs_core::vampire::converter::NativeConverter::finish` | `conv.finish` |
| `sigmakee_rs_core::vampire::assemble::assemble_tptp` | `assemble_tptp` |
| `sigmakee_rs_core::vampire::VampireAxiomCacheSet::build` | `VampireAxiomCacheSet::build` |
| `sigmakee_rs_core::vampire::VampireAxiomCache::filtered_problem` | `cache.filtered_problem` |
| `sigmakee_rs_core::vampire::native_proof::native_proof_to_kif_steps` | `native_proof_to_kif_steps` |
| `sigmakee_rs_core::vampire::bindings::extract_bindings` | `extract_bindings` |
| `sigmakee_rs_core::prover::subprocess::VampireRunner::prove` | `runner.prove` |
| `vampire_prover::lower_problem` | `lower_problem` |
| `vampire_prover::Problem::solve_and_prove` | `problem.solve_and_prove` |
| `sigmakee_rs_core::sine::SineIndex::add_axioms` | `sine.add_axioms` |
| `sigmakee_rs_core::sine::SineIndex::remove_axiom` | `sine.remove_axiom` |
| `sigmakee_rs_core::sine::SineIndex::select` | `sine.select` |
| `sigmakee_rs_core::tptp::test_case::parse_test_content` | `parse_test_content` |
| `sigmakee_rs_core::persist::commit::write_axioms` | `persist::write_axioms` |
| `sigmakee_rs_core::persist::commit::delete_formula` | `persist::delete_formula` |
| `sigmakee_rs_core::persist::persist_axiom_cache` | `persist::persist_axiom_cache` |
| `sigmakee_rs_sdk::man::parse_doc_spans` | `parse_doc_spans` |
| `sigmakee_rs_sdk::man::view_from_raw` | `view_from_raw` |
