# SDK Op activity flows (generated)

DO NOT EDIT — regenerate via `cargo xtask gen-flows`.
This file is compared by `cargo xtask check-flows` in CI.

## IngestOp.run (`crates/sigmakee-rs-sdk/src/ingest.rs`)

- → `expand_sources()` _(helper, inlined)_
  - for `s` in `sources`:
    - match `s`:
      - `Source :: Inline { tag , text }` →
        - _(no kb calls)_
      - `Source :: File (p)` →
        - _(no kb calls)_
      - `Source :: Dir (d)` →
        - for `child` in `scan_dir_for_kif (& d) ?`:
- if `let Some (p) = progress . as_deref_mut ()`:
- for `(idx , src)` in `resolved . into_iter () . enumerate ()`:
  - match `src`:
    - `ResolvedSource :: Inline { tag , text }` →
      - _(no kb calls)_
    - `ResolvedSource :: Disk (path)` →
      - if `let Some (p) = progress . as_deref_mut ()`:
  - → `ingest_one()` _(helper, inlined)_
    - if `! kb . file_roots (tag) . is_empty ()`:
      - → `kb.reconcile_file`
      - if `! report . parse_errors . is_empty ()`:
        - ⏎ early return
      - for `e` in `& report . semantic_errors`:
      - ⏎ early return
    - → `kb.load_kif`
    - if `! result . ok`:
      - if `let Some (first) = result . errors . into_iter () . next ()`:
        - ⏎ early return
      - ⏎ early return
    - → `kb.file_roots`
  - if `let Some (p) = progress . as_deref_mut ()`:
- → `kb.make_session_axiomatic`

## ValidateOp.run (`crates/sigmakee-rs-sdk/src/validate.rs`)

- match `self . target`:
  - `ValidateTarget :: All` →
    - → `validate_all()` _(helper, inlined)_
      - if `parse_only`:
        - ⏎ early return
      - → `kb.validate_all_findings`
  - `ValidateTarget :: Formula { tag , text }` →
    - → `validate_formula()` _(helper, inlined)_
      - if `! parse_only && ! skip_kb_check`:
        - → `kb.validate_all_findings`
      - → `kb.load_kif`
      - if `! result . ok`:
        - ⏎ early return
      - if `parse_only`:
        - ⏎ early return
      - → `kb.session_sids`
      - if `sids . is_empty ()`:
        - ⏎ early return
      - for `sid` in `sids`:
        - → `kb.validate_sentence_findings`

## TranslateOp.run (`crates/sigmakee-rs-sdk/src/translate.rs`)

- match `target`:
  - `TranslateTarget :: Kb` →
    - → `translate_kb()` _(helper, inlined)_
      - → `kb.validate_all`
      - → `kb.to_tptp`
  - `TranslateTarget :: Formula { tag , text }` →
    - → `translate_formula()` _(helper, inlined)_
      - → `kb.load_kif`
      - if `! result . ok`:
        - if `let Some (first) = result . errors . into_iter () . next ()`:
          - ⏎ early return
        - ⏎ early return
      - → `kb.session_sids`
      - if `sids . is_empty ()`:
        - ⏎ early return
      - for `& sid` in `& sids`:
        - if `let Err (e) = kb . validate_sentence (sid)`:
      - for `& sid` in `& sids`:
        - → `kb.sentence_kif_str`
        - → `kb.format_sentence_tptp`
        - if `options . show_kif_comment`:

## LoadOp.run (`crates/sigmakee-rs-sdk/src/load.rs`)

- → `materialise_sources()` _(helper, inlined)_
  - for `s` in `sources`:
    - match `s`:
      - `Source :: Inline { tag , text }` →
        - _(no kb calls)_
      - `Source :: File (p)` →
        - _(no kb calls)_
      - `Source :: Dir (d)` →
        - → `scan_dir_for_kif()` _(helper, inlined)_
  - for `(idx , path)` in `paths . into_iter () . enumerate ()`:
    - if `let Some (p) = progress . as_deref_mut ()`:
- if `materialised . is_empty ()`:
  - ⏎ early return
- if `let Some (p) = progress . as_deref_mut ()`:
- → `kb.reconcile_files`
- for `r` in `reports`:
  - if `! r . parse_errors . is_empty ()`:
    - ⏎ early return
  - for `e` in `& r . semantic_errors`:
  - if `let Some (p) = progress . as_deref_mut ()`:
- if `strict && ! report . semantic_errors . is_empty ()`:
  - ⏎ early return
- if `let Some (p) = progress . as_deref_mut ()`:
- for `(tag , removed_sids , added_sids)` in `& pending`:
  - → `kb.persist_reconcile_diff`
- if `let Some (p) = progress . as_deref_mut ()`:

## AskOp.run (`crates/sigmakee-rs-sdk/src/ask.rs`)

- for `kif` in `& tells`:
  - → `kb.tell`
  - if `! r . ok`:
    - if `let Some (first) = r . errors . into_iter () . next ()`:
      - ⏎ early return
    - ⏎ early return
- if `let Some (p) = progress . as_deref_mut ()`:
- match `backend`:
  - `ProverBackend :: Subprocess` →
    - → `kb.ask`
  - `ProverBackend :: Embedded` →
    - → `kb.ask_embedded`
- if `let Some (p) = progress . as_deref_mut ()`:
- if `matches ! (result . status , sigmakee_rs_core :: ProverStatus :: Unknown) && backend == ProverBackend :: Subprocess && (result . raw_output . contains ("No such file") || result . raw_output . contains ("not found") || result . raw_output . contains ("cannot find"))`:
  - ⏎ early return

## TestOp.run (`crates/sigmakee-rs-sdk/src/test.rs`)

- → `expand_sources()` _(helper, inlined)_
  - for `s` in `sources`:
    - match `s`:
      - `Source :: File (p)` →
        - → `prepare_from_disk()` _(helper, inlined)_
      - `Source :: Dir (d)` →
        - for `child` in `scan_dir_for_tests (& d) ?`:
          - → `prepare_from_disk()` _(helper, inlined)_
      - `Source :: Text { tag , content }` →
        - _(no kb calls)_
      - `Source :: Parsed { tag , case }` →
        - _(no kb calls)_
- for `(idx , prepared)` in `cases . into_iter () . enumerate ()`:
  - → `run_one()` _(helper, inlined)_
    - match `& prepared . parsed`:
      - `Ok (c)` →
        - _(no kb calls)_
      - `Err (msg)` →
        - ⏎ early return
    - if `! axiom_text . is_empty ()`:
      - → `kb.load_kif`
      - if `! load_result . ok`:
        - → `kb.flush_session`
        - ⏎ early return
      - → `kb.validate_session`
      - if `! semantic . is_empty ()`:
        - → `kb.flush_session`
        - ⏎ early return
    - match `case . query . clone ()`:
      - `Some (q)` →
        - _(no kb calls)_
      - `None` →
        - → `kb.flush_session`
        - ⏎ early return
    - match `backend`:
      - `ProverBackend :: Subprocess` →
        - → `kb.ask`
      - `ProverBackend :: Embedded` →
        - → `kb.ask_embedded`
    - → `kb.flush_session`
    - if `matches ! (result . status , sigmakee_rs_core :: ProverStatus :: Unknown) && ! proved && ! result . raw_output . is_empty ()`:
      - ⏎ early return
    - if `proved == expected`:
      - if `let Some (expected_answers) = case . expected_answer . clone ()`:
        - if `! missing . is_empty ()`:
          - ⏎ early return
  - match `& case_report . outcome`:
    - `TestOutcome :: Passed` →
      - _(no kb calls)_
    - `TestOutcome :: Failed { .. }` →
      - _(no kb calls)_
    - `TestOutcome :: Incomplete { .. }` →
      - _(no kb calls)_
    - `TestOutcome :: ParseError (_)` →
      - _(no kb calls)_
    - `TestOutcome :: SemanticError (_)` →
      - _(no kb calls)_
    - `TestOutcome :: ProverError (_)` →
      - _(no kb calls)_
    - `TestOutcome :: NoQuery` →
      - _(no kb calls)_
  - if `let Some (p) = progress . as_deref_mut ()`:
  - match `case_report . outcome`:
    - `TestOutcome :: Passed` →
      - _(no kb calls)_
    - `TestOutcome :: Failed { .. } | TestOutcome :: Incomplete { .. } | TestOutcome :: ProverError (_)` →
      - _(no kb calls)_
    - `TestOutcome :: ParseError (_) | TestOutcome :: SemanticError (_) | TestOutcome :: NoQuery` →
      - _(no kb calls)_

## manpage_view.run (`crates/sigmakee-rs-sdk/src/man.rs`)

- → `kb.manpage`
- → `view_from_raw()` _(helper, inlined)_
  - if `raw . ref_args . is_empty () && raw . ref_nested . is_empty ()`:
  - else:
    - for `r` in `raw . ref_args`:

