// crates/core/src/kb/prove.rs
//
// Theorem-proving entrypoints on KnowledgeBase: `ask`, `ask_embedded`,
// and their private helpers (`query_affects_taxonomy`, `ensure_axiom_cache`).
// Split out of kb.rs to keep the main module focused on storage / ingestion /
// promotion.

#![cfg(feature = "ask")]

use std::collections::HashSet;
use std::time::Instant;

use crate::Span;
use crate::syntactic::load_kif;
#[cfg(feature = "integrated-prover")]
use crate::prover::Binding;
use crate::prover::{
    ProverMode, ProverOpts, ProverResult, ProverRunner, ProverStatus, ProverTimings,
};
use crate::tptp::TptpLang;
use crate::types::SentenceId;

use super::{KnowledgeBase, KbError};

impl KnowledgeBase {
    /// Ask the theorem prover whether `query_kif` is entailed by the KB.
    ///
    /// **SInE filtering is always on.**  The axioms shipped to the prover
    /// are the subset SInE deems relevant to the conjecture's symbols at
    /// [`SineParams::default`] tolerance — typically a small fraction of
    /// the whole KB for focused queries.  Session assertions (if `session`
    /// is `Some`) are always included as `hypothesis` rows, regardless
    /// of SInE relevance.
    ///
    /// Power users who want to tune tolerance or inspect the selected
    /// axiom set can call [`sine_select_for_query`] directly and build
    /// their own TPTP — but the common path is `ask` with defaults.
    ///
    /// Parse `query_kif` as one or more root sentences under
    /// `query_tag`, returning the freshly-minted sids on success or
    /// a ready-made `ProverResult::Unknown` on failure (parse error
    /// *or* zero sentences).  On any failure the query sentences are
    /// scrubbed from the store so a follow-up ask starts clean.
    ///
    /// Shared between [`Self::ask`] (subprocess path) and [`ask_embedded`]
    /// (integrated-prover path) — both need to land a conjecture in
    /// the store before they diverge.
    fn parse_conjecture(
        &mut self,
        query_tag: &str,
        query_kif: &str,
    ) -> Result<Vec<SentenceId>, ProverResult> {
        let prev_count = self.layer.semantic.syntactic.file_roots
            .get(query_tag).map(|v| v.len()).unwrap_or(0);

        let parse_errors: Vec<(Span, KbError)> =
            load_kif(&mut self.layer.semantic.syntactic, query_kif, query_tag);
        if !parse_errors.is_empty() {
            self.layer.semantic.syntactic.remove_file(query_tag);
            return Err(ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: parse_errors.iter()
                    .map(|(_, e): &(Span, KbError)| e.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
                proof_tptp: String::new(),
                timings:    ProverTimings::default(),
            });
        }

        let query_sids: Vec<SentenceId> = self.layer.semantic.syntactic.file_roots
            .get(query_tag)
            .map(|v| v[prev_count..].to_vec())
            .unwrap_or_default();
        if query_sids.is_empty() {
            self.layer.semantic.syntactic.remove_file(query_tag);
            return Err(ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: "No query sentence parsed".into(),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
                proof_tptp: String::new(),
                timings:    ProverTimings::default(),
            });
        }
        Ok(query_sids)
    }

    /// `session` = optional in-memory session whose assertions become `hypothesis`.
    /// `lang` selects the TPTP language (FOF, TFF) for the generated problem file.
    pub fn ask(
        &mut self,
        query_kif: &str,
        session:   Option<&str>,
        runner:    &dyn ProverRunner,
        lang:      TptpLang,
    ) -> ProverResult {
        with_guard!(self);
        use crate::sine::SineParams;

        self.debug(format!("ask: query={}", query_kif));
        // No top-level `ask.total` span: it would hold an immutable
        // borrow on `self.profiler` across the many `&mut self` calls
        // below.  The profiler's grand-total line already aggregates
        // sibling phases within the [ask] bucket.

        // -- Step 1: SInE-select the relevant axiom subset. --------------
        // `sine_select_for_query` parses the conjecture into a temporary
        // file tag, walks its symbols, and rolls the parse back before
        // returning.  Use `profile_call!` here because
        // `sine_select_for_query` takes `&mut self`, which is
        // incompatible with `profile_span!`'s immutable borrow on
        // `self.profiler`.
        let selected_axioms = match profile_call!(self, "ask.sine_select",
            self.sine_select_for_query(query_kif, SineParams::default()))
        {
            Ok(s) => s,
            Err(e) => {
                return ProverResult {
                    status:     ProverStatus::Unknown,
                    raw_output: format!("SInE selection failed: {}", e),
                    bindings:   Vec::new(),
                    proof_kif:  Vec::new(),
                    proof_tptp: String::new(),
                    timings:    ProverTimings::default(),
                };
            }
        };

        // -- Step 2: Re-parse the conjecture for the native converter. ---
        let query_tag = crate::session_tags::SESSION_QUERY;
        let query_sids = match profile_call!(
            self,
            "ask.parse_query",
            self.parse_conjecture(query_tag, query_kif)
        ) {
            Ok(sids)  => sids,
            Err(res)  => return res,
        };

        // -- Step 3: Collect session-assertion sids. ---------------------
        let assertion_ids: HashSet<SentenceId> = session
            .and_then(|s| self.sessions.get(s))
            .map(|v| v.iter().copied().collect())
            .unwrap_or_default();

        // -- Step 4: Build TPTP, seeding the NativeConverter from the
        // whole-KB cache and applying SInE filtering at assembly time.
        //
        // Previously we rebuilt the IR per-query by iterating the
        // SInE-selected subset through `NativeConverter::add_axiom`.
        // Now we seed the converter from the cached whole-KB IR
        // (`ensure_axiom_cache`) and emit the TPTP with an
        // `axiom_filter` that admits exactly the SInE-selected set
        // plus session assertions.  The cache holds both TFF and FOF
        // shapes so either `lang` is a hot-path hit.
        //
        // Session assertions aren't in the cache (they're
        // session-scoped, not promoted axioms), so they're appended
        // to the converter via `add_axiom` and land in the tail of
        // the final `sid_map` — included in `allowed` so the
        // assembler emits them.
        use crate::vampire::assemble::{assemble_tptp, AssemblyOpts};
        use crate::vampire::converter::{Mode, NativeConverter};

        let mode = match lang {
            TptpLang::Tff => Mode::Tff,
            TptpLang::Fof => Mode::Fof,
        };
        let t_input = Instant::now();

        // Cache-warm pass — eager dual-mode build so both subprocess
        // and embedded paths share a single IR build.  `profile_call!`
        // is used (not `profile_span!`) because this takes `&mut self`.
        profile_call!(self, "ask.ensure_cache", self.ensure_axiom_cache());

        let (problem, sid_map) = {
            profile_span!(self, "ask.tptp_build");
            // Clone the mode-appropriate cached IR so we can extend
            // it with session assertions + conjecture without
            // mutating the cache itself.
            let (seed_problem, seed_sid_map) = {
                let cache = self.axiom_cache.as_ref().unwrap().get(mode);
                (cache.problem.clone(), cache.sid_map.clone())
            };
            let mut conv = NativeConverter::from_parts(
                &self.layer, seed_problem, seed_sid_map, mode,
            );
            // Session assertions always ride along as additional axioms
            // (the TPTP assembler labels them `axiom`, which the prover
            // treats identically to `hypothesis` for our purposes).
            // They're appended to the sid_map tail so the assembler
            // filter can admit them via their sids.
            for &sid in &assertion_ids { conv.add_axiom(sid); }
            for &qsid in &query_sids {
                if conv.set_conjecture(qsid).is_some() { break; }
            }
            conv.finish()
        };

        // Assembly-time filter: SInE-selected ∪ session assertions.
        // The conjecture is emitted unconditionally by
        // `assemble_tptp`; the filter only gates axiom rows.
        let mut allowed: std::collections::HashSet<SentenceId> =
            selected_axioms.iter().copied().collect();
        allowed.extend(assertion_ids.iter().copied());

        let tptp = {
            profile_span!(self, "ask.tptp_build");
            assemble_tptp(&problem, &sid_map, &AssemblyOpts {
                conjecture_name: "query_0",
                axiom_filter:    Some(&allowed),
                ..AssemblyOpts::default()
            })
        };
        let input_gen = t_input.elapsed();
        self.debug(format!("ask({:?}): TPTP size={} bytes ({} SInE-selected axioms, {} assertions, cache hit)", mode, tptp.len(), selected_axioms.len(), assertion_ids.len()));

        // Roll back the conjecture parse.  Phase A optimization:
        // only rebuild taxonomy/invalidate caches if the query head
        // itself touched taxonomy predicates (`subclass`/`instance`/
        // `subrelation`/`subAttribute`).  Non-taxonomy conjectures
        // leave derived state untouched.
        //
        // `query_affects_taxonomy` takes `&self`; `rebuild_taxonomy`
        // and `invalidate_cache` take `&mut self`.  We use
        // `profile_call!` (post-call record) to time the whole rollback
        // cleanup including the possible rebuild.
        profile_call!(self, "ask.rollback", {
            let needs_rebuild = self.query_affects_taxonomy(&query_sids);
            self.layer.semantic.syntactic.remove_file(query_tag);
            if needs_rebuild {
                self.layer.semantic.rebuild_taxonomy();
                self.layer.semantic.invalidate_cache();
            }
        });

        let prover_opts = ProverOpts { timeout_secs: runner.timeout_secs(), mode: ProverMode::Prove };
        let mut result = {
            // `runner.prove` takes `&dyn ProverRunner`, not `&mut self`,
            // so `profile_span!` works here.  Keep it as a span so the
            // inner sub-phases (`ask.output_parse`, see below) can be
            // recorded as siblings rather than nested children.
            profile_span!(self, "ask.prover_run");
            runner.prove(&tptp, &prover_opts)
        };
        // Prover-reported timings (input_gen / prover_run / output_parse)
        // ride on the returned `ProverResult.timings`.  Consumers who
        // want them in their phase-aggregator can synthesize
        // `PhaseFinished` events from those values (or just read
        // `result.timings` directly — they're a stable part of the
        // public API).
        result.timings.input_gen = input_gen;
        result
    }

    // -- Consistency check -----------------------------------------------------

    /// Ask the theorem prover whether a sentence subset of this KB is
    /// **satisfiable** — i.e. contains no contradiction.  No conjecture
    /// is attached; the prover is asked about the axioms in isolation.
    ///
    /// Returns a [`ProverResult`] whose [`status`](ProverResult::status)
    /// is:
    ///
    /// * [`Consistent`](ProverStatus::Consistent)   — the axiom set is
    ///   satisfiable (Vampire's `SZS status Satisfiable` /
    ///   `CounterSatisfiable`).
    /// * [`Inconsistent`](ProverStatus::Inconsistent) — the axiom set
    ///   derives false (Vampire's `Unsatisfiable` / `Theorem` /
    ///   `ContradictoryAxioms`).  When Vampire emits a refutation
    ///   proof, `result.proof_kif` lists the derivation steps; pair
    ///   with [`KnowledgeBase::build_axiom_source_index`] to map each
    ///   axiom-role step back to its source `file:line`.
    /// * [`Timeout`](ProverStatus::Timeout) / [`Unknown`](ProverStatus::Unknown)
    ///   — the prover couldn't decide within the budget.
    ///
    /// # Arguments
    ///
    /// * `axioms` — the sentence IDs to ship to the prover.  Any valid
    ///   `SentenceId` already in the KB works.  De-duplication and
    ///   stable ordering are handled internally.  An empty set is
    ///   trivially consistent (no TPTP axioms emitted).
    /// * `runner` — a prover runner (typically
    ///   [`VampireRunner`](crate::prover::VampireRunner)).
    /// * `lang` — TPTP flavour ([`TptpLang::Fof`] or [`TptpLang::Tff`]).
    ///
    /// # Non-prover use cases
    ///
    /// This is the primitive behind any KB consistency gate: the debug
    /// CLI, a CI hook that checks every newly-committed `.kif` against
    /// the rest of the ontology, a test harness asserting a fixture is
    /// satisfiable, or a linter walking per-file subsets.  It reads
    /// `&self` (no KB mutation) so many calls can run against the same
    /// loaded KB without interfering with each other.
    pub fn check_consistency(
        &self,
        axioms: &std::collections::HashSet<SentenceId>,
        runner: &dyn ProverRunner,
        lang:   TptpLang,
    ) -> ProverResult {
        use crate::vampire::assemble::{assemble_tptp, AssemblyOpts};
        use crate::vampire::converter::{Mode, NativeConverter};

        let mode = match lang {
            TptpLang::Tff => Mode::Tff,
            TptpLang::Fof => Mode::Fof,
        };
        let t_input = Instant::now();

        // Sort for deterministic TPTP output — makes test-diffing and
        // prover caching well-behaved.
        let mut sorted: Vec<SentenceId> = axioms.iter().copied().collect();
        sorted.sort_unstable();

        let (problem, sid_map) = {
            let mut conv = NativeConverter::new(&self.layer, mode);
            for sid in sorted { conv.add_axiom(sid); }
            // Intentionally skip `set_conjecture` — this is a pure
            // axiom-satisfiability check.  `assemble_tptp` gracefully
            // emits no conjecture line when `problem.conjecture_ref()`
            // is `None`.
            conv.finish()
        };

        let tptp = assemble_tptp(&problem, &sid_map, &AssemblyOpts::default());
        let input_gen = t_input.elapsed();
        self.debug(format!("check_consistency({:?}): TPTP size={} bytes ({} axioms)", mode, tptp.len(), problem.axioms().len()));

        let prover_opts = ProverOpts {
            timeout_secs: runner.timeout_secs(),
            mode:         ProverMode::CheckConsistency,
        };
        let mut result = runner.prove(&tptp, &prover_opts);
        result.timings.input_gen = input_gen;
        result
    }

    // -- Embedded theorem proving ----------------------------------------------

    /// Ask the embedded Vampire prover whether `query_kif` is entailed by the KB.
    ///
    /// Feature parity with [`Self::ask`] (subprocess path):
    ///
    /// - **SInE filtering** — runs `sine_select_for_query` on the
    ///   conjecture and ships only the relevant axiom subset (plus
    ///   session assertions) to Vampire.  On a SUMO-scale KB this
    ///   reduces the axiom load from ~24k to typically a few hundred.
    ///   Previously the embedded path handed Vampire the full KB,
    ///   which made it dramatically slower than the subprocess path
    ///   on realistic queries.
    /// - **Profiling** — matches `ask`'s `profile_call!` /
    ///   `profile_span!` markers (`ask.sine_select`, `ask.parse_query`,
    ///   `ask.ensure_cache`, `ask.tptp_build`, `ask.prover_run`,
    ///   `ask.rollback`).  When the `profiling` feature is off every
    ///   marker is a no-op.  Shared phase names with the subprocess
    ///   path so `--profile` reports compare apples-to-apples across
    ///   backends.
    /// - **`lang` parameter** — FOF or TFF, same as `ask`.  The
    ///   dual-mode axiom cache (see
    ///   `VampireAxiomCacheSet`)
    ///   holds both IRs, so switching modes is free at query time.
    ///   `vampire_prover::lower_problem` checks
    ///   `IrProblem::mode()` and takes the appropriate FFI path, so
    ///   both modes run through the same solver.
    ///
    /// Differences from `ask`:
    ///
    /// - Bypasses TPTP text generation — the IR Problem is lowered
    ///   directly into the Vampire FFI.  Filtering therefore happens
    ///   on the IR via `VampireAxiomCache::filtered_problem`, not
    ///   via `AssemblyOpts::axiom_filter`.
    /// - `proof_tptp` is always empty (no TPTP round-trip).
    /// - `proof_kif` is populated by walking the native `Proof` via
    ///   `crate::vampire::native_proof::native_proof_to_kif_steps`
    ///   — feature parity with the subprocess path.  The
    ///   axiom-source traceback uses the canonical-fingerprint
    ///   fallback (native proofs don't preserve `kb_<sid>` names).
    ///
    /// `session` = optional in-memory session whose assertions are
    /// included as hypotheses.
    ///
    /// `VampireAxiomCache::filtered_problem` (internal)
    #[cfg(feature = "integrated-prover")]
    pub fn ask_embedded(
        &mut self,
        query_kif:    &str,
        session:      Option<&str>,
        timeout_secs: u32,
        lang:         TptpLang,
    ) -> ProverResult {
        with_guard!(self);
        use crate::sine::SineParams;

        self.debug(format!("ask_embedded: query={}", query_kif));

        // -- Step 1: SInE-select the relevant axiom subset. --------------
        // Identical to `ask`'s Step 1 — parses the conjecture into a
        // temporary tag, walks its symbols, rolls the parse back.
        // `profile_call!` because `sine_select_for_query` takes
        // `&mut self`.
        let selected_axioms = match profile_call!(self, "ask.sine_select",
            self.sine_select_for_query(query_kif, SineParams::default()))
        {
            Ok(s) => s,
            Err(e) => {
                return ProverResult {
                    status:     ProverStatus::Unknown,
                    raw_output: format!("SInE selection failed: {}", e),
                    bindings:   Vec::new(),
                    proof_kif:  Vec::new(),
                    proof_tptp: String::new(),
                    timings:    ProverTimings::default(),
                };
            }
        };

        // -- Step 2: Parse the conjecture for the native converter. ------
        let query_tag = crate::session_tags::SESSION_QUERY_EMBEDDED;
        let query_sids = match profile_call!(
            self,
            "ask.parse_query",
            self.parse_conjecture(query_tag, query_kif)
        ) {
            Ok(sids) => sids,
            Err(res) => return res,
        };

        // -- Step 3: Collect session-assertion sids. ---------------------
        let assertion_sids: Vec<SentenceId> = session
            .and_then(|s| self.sessions.get(s))
            .cloned()
            .unwrap_or_default();

        // -- Step 4: Ensure the eager dual-mode IR axiom cache. ----------
        // The embedded path only consumes the TFF side, but
        // `ensure_axiom_cache` builds both in one pass (see
        // `VampireAxiomCacheSet`).  Identical profile-span name as
        // `ask` so reports coalesce across backends.
        profile_call!(self, "ask.ensure_cache", self.ensure_axiom_cache());

        // -- Step 5: Build the trimmed IR problem. -----------------------
        // Filter = SInE-selected axioms ∪ session assertions.  The
        // session assertions aren't in the cache (they're
        // session-scoped, not promoted), so they're appended after
        // the cache-derived axioms via `add_axiom`.  Adding them to
        // `allowed` is a no-op for the filter (they're not in the
        // cached sid_map anyway) but keeps the shape symmetric with
        // `ask`'s filter set.
        use crate::vampire::converter::{Mode, NativeConverter};
        let mode = match lang {
            TptpLang::Tff => Mode::Tff,
            TptpLang::Fof => Mode::Fof,
        };
        let mut allowed: HashSet<SentenceId> = selected_axioms.iter().copied().collect();
        allowed.extend(assertion_sids.iter().copied());

        let t_input = Instant::now();
        let (ir_problem, query_var_map) = {
            profile_span!(self, "ask.tptp_build");
            // Trim the cached whole-KB IR down to the SInE subset
            // before seeding the converter.  `filtered_problem`
            // preserves all sort / fn / pred declarations — decls are
            // KB-wide and cheap to clone, and the filtered axioms may
            // reference any of them.  The dual-mode cache means
            // either `mode` is a warm hit.
            let (seed_problem, seed_sid_map) = {
                let cache = self.axiom_cache.as_ref().unwrap().get(mode);
                cache.filtered_problem(&allowed)
            };
            let mut conv = NativeConverter::from_parts(
                &self.layer, seed_problem, seed_sid_map, mode,
            );
            // Session assertions — not in the cache, so append them.
            for &sid in &assertion_sids {
                conv.add_axiom(sid);
            }
            let mut query_var_map: Option<crate::vampire::converter::QueryVarMap> = None;
            for &sid in &query_sids {
                if let Some(qvm) = conv.set_conjecture(sid) {
                    query_var_map = Some(qvm);
                    break;
                }
            }
            let (ir_problem, _sid_map) = conv.finish();
            (ir_problem, query_var_map)
        };
        let input_gen = t_input.elapsed();
        self.debug(format!("ask_embedded({:?}): IR built ({} axioms after SInE: {} selected + {} assertions, cache hit)", mode, ir_problem.axioms().len(), selected_axioms.len(), assertion_sids.len()));

        // -- Step 6: Configure and run Vampire. --------------------------
        // SInE handling: the KB has now applied SInE externally (Step 1
        // above), so disable Vampire's internal SInE to prevent
        // double-filtering.  Same rationale as the subprocess path —
        // see `crates/core/src/prover/subprocess.rs::build_vampire_args`.
        //
        //   1. `mode = vampire`: single-strategy mode.  The `casc`
        //      portfolio's schedules contain strategies with
        //      `ss=axioms` encoded in their option-strings;
        //      `readFromEncodedOptions` applies those per-strategy
        //      and overrides any global `sine_selection=off`.  Only
        //      `vampire` mode fully escapes that re-filter.
        //   2. `sine_selection = off`: defensive explicit disable at
        //      the preprocessing level.  Vampire's default is already
        //      `off` but spelling it out makes the intent explicit
        //      and survives any future default change.
        let mut opts = vampire_prover::Options::new();
        if timeout_secs > 0 {
            opts.timeout(std::time::Duration::from_secs(timeout_secs as u64));
        }
        opts.set_option("mode", "vampire");
        opts.set_option("sine_selection", "off");

        let t_prover = Instant::now();
        let (res, proof) = {
            profile_span!(self, "ask.prover_run");
            let mut problem = vampire_prover::lower_problem(&ir_problem, opts);
            problem.solve_and_prove()
        };
        let prover_run = t_prover.elapsed();
        self.debug(format!("embedded result ({:?}): {:?}", mode, res));

        let status = match res {
            vampire_prover::ProofRes::Proved     => ProverStatus::Proved,
            vampire_prover::ProofRes::Unprovable => ProverStatus::Disproved,
            vampire_prover::ProofRes::Unknown(_) => ProverStatus::Unknown,
        };

        // Extract variable bindings + the KIF proof transcript from
        // the native proof when one is available.  Empty results are
        // non-fatal (prover may not produce a proof, or the
        // extractors may not recognise the encoding).
        //
        // Both `extract_bindings` and `native_proof_to_kif_steps`
        // consume `&Proof`, so we borrow the same proof for both and
        // move it only when the match arms close.
        let t_parse = Instant::now();
        let (bindings, proof_kif): (Vec<Binding>, Vec<crate::tptp::kif::KifProofStep>) =
            if matches!(status, ProverStatus::Proved) {
                self.debug(format!("proof extraction: proof={}, qvm={}", proof.is_some(), query_var_map.is_some()));
                match proof {
                    Some(p) => {
                        let b: Vec<Binding> = match query_var_map {
                            Some(qvm) => crate::vampire::bindings::extract_bindings(&p, &qvm)
                                .into_iter()
                                .map(|b| Binding { variable: b.variable, value: b.value })
                                .collect(),
                            None => Vec::new(),
                        };
                        let kif = crate::vampire::native_proof::native_proof_to_kif_steps(&p);
                        (b, kif)
                    }
                    None => (Vec::new(), Vec::new()),
                }
            } else {
                (Vec::new(), Vec::new())
            };
        let output_parse = t_parse.elapsed();

        // -- Step 7: Rollback the conjecture parse. ----------------------
        // Phase A: skip the full taxonomy rebuild unless the query
        // actually added a taxonomy edge.  See the comment in `ask()`.
        profile_call!(self, "ask.rollback", {
            let needs_rebuild = self.query_affects_taxonomy(&query_sids);
            self.layer.semantic.syntactic.remove_file(query_tag);
            if needs_rebuild {
                self.layer.semantic.rebuild_taxonomy();
                self.layer.semantic.invalidate_cache();
            }
        });
        // Output-parse timing rides on `ProverResult.timings` —
        // consumers that aggregate phase totals can read it from
        // there or from a `PhaseFinished` event.
        let _ = output_parse;

        ProverResult {
            status,
            raw_output: format!("{:?}", res),
            bindings,
            proof_kif,
            // Embedded path bypasses Vampire's text serializer, so
            // there's no raw TSTP transcript to preserve.
            // `--proof tptp` against the embedded backend gets the
            // empty-string branch in `print_proof`.
            proof_tptp: String::new(),
            timings:    ProverTimings { input_gen, prover_run, output_parse },
        }
    }

    // -- Internal helpers ------------------------------------------------------

    /// `true` if any sentence in `sids` has a taxonomy-relation head
    /// (`subclass`, `instance`, `subrelation`, or `subAttribute`).
    ///
    /// Used by `ask()` / `ask_embedded()` to decide whether the
    /// post-proof cleanup needs a `rebuild_taxonomy` + `invalidate_cache`
    /// cycle.  For the overwhelming majority of conjectures (which are
    /// not taxonomy relations), both sides are no-ops and can be
    /// skipped -- saving an O(total KB) rebuild per ask.
    ///
    /// This check is intentionally conservative: it only looks at the
    /// head of each root sentence, not sub-sentences.  A negated
    /// taxonomy-head query (`(not (subclass X Y))`) returns `false`
    /// here because its head is `not`, not `subclass`; we'd miss the
    /// rebuild in that case.  In practice, negated taxonomy queries
    /// don't add taxonomy edges because `extract_tax_edge_for` only
    /// acts on positive top-level taxonomy sentences, so this
    /// conservativeness is safe.
    #[inline]
    fn query_affects_taxonomy(&self, sids: &[SentenceId]) -> bool {
        self.layer.semantic.syntactic.any_touches_taxonomy(sids)
    }

    /// Ensure the IR axiom cache is populated for **both** TFF and
    /// FOF; build whichever isn't already warm.
    ///
    /// After this call `self.axiom_cache` is guaranteed to be `Some`,
    /// carrying both modes (`VampireAxiomCacheSet::{tff, fof}`).
    ///
    /// Three-tier resolution:
    ///
    ///   1. **In-memory hit** — `self.axiom_cache` already `Some` →
    ///      nothing to do.
    ///   2. **LMDB restore** — if both `axiom_cache_tff` and
    ///      `axiom_cache_fof` blobs are present AND both match the
    ///      current `kb_version`, rehydrate both without running the
    ///      `NativeConverter`.  A missing-or-stale blob in either
    ///      mode triggers a full rebuild of **both** (partial
    ///      restore would leave the set asymmetric; simpler to
    ///      recompute).
    ///   3. **Full rebuild** — walk `axiom_ids_set()` twice
    ///      (once per mode) via [`VampireAxiomCacheSet::build`],
    ///      persist both blobs, and store the set.
    ///
    /// Eager dual-mode generation lets both `ask` (subprocess,
    /// either TFF or FOF) and `ask_embedded` (TFF) share a single
    /// cache-warm pass — the first query in a session pays the
    /// ~90 ms rebuild cost (2× the single-mode build), every
    /// subsequent one hits the warm IR and the SInE filter is
    /// applied at TPTP-assembly time instead of per-axiom.
    fn ensure_axiom_cache(&mut self) {
        if self.axiom_cache.is_some() { return; }

        // -- Fast path: restore both modes from LMDB --------------------
        #[cfg(feature = "persist")]
        if let Some(env) = &self.db {
            if let Ok(Some((tff_blob, fof_blob))) = (|| -> Result<Option<(
                crate::persist::CachedAxiomProblem,
                crate::persist::CachedAxiomProblem,
            )>, KbError> {
                let rtxn = env.read_txn()?;
                let current = env.kb_version(&rtxn)?;
                let tff = env.get_cache::<crate::persist::CachedAxiomProblem>(
                    &rtxn, crate::persist::CACHE_KEY_AXIOM_CACHE_TFF,
                )?;
                let fof = env.get_cache::<crate::persist::CachedAxiomProblem>(
                    &rtxn, crate::persist::CACHE_KEY_AXIOM_CACHE_FOF,
                )?;
                let tff_present = tff.is_some();
                let fof_present = fof.is_some();
                match (tff, fof) {
                    // Both present, both matching the current version,
                    // and each blob's `mode_tff` flag agrees with the
                    // key it was stored under.
                    (Some(t), Some(f))
                        if t.kb_version == current
                        && f.kb_version == current
                        && t.mode_tff
                        && !f.mode_tff
                    => Ok(Some((t, f))),
                    _ => {
                        self.debug(format!("Phase D: axiom cache restore declined \
                             (tff_present={}, fof_present={}, current={}); \
                             rebuilding both modes", tff_present, fof_present, current));
                        Ok(None)
                    }
                }
            })() {
                self.info(format!("Phase D: restored axiom cache from bincode blobs \
                     (TFF: {} axioms, FOF: {} axioms)", tff_blob.sid_map.len(), fof_blob.sid_map.len()));
                self.axiom_cache = Some(crate::vampire::VampireAxiomCacheSet {
                    tff: crate::vampire::VampireAxiomCache {
                        problem: tff_blob.problem,
                        sid_map: tff_blob.sid_map,
                    },
                    fof: crate::vampire::VampireAxiomCache {
                        problem: fof_blob.problem,
                        sid_map: fof_blob.sid_map,
                    },
                });
                return;
            }
        }

        // -- Slow path: rebuild both modes from the in-memory store ------
        let axiom_ids = self.axiom_ids_set();
        let cache = crate::vampire::VampireAxiomCacheSet::build(
            &self.layer,
            &axiom_ids,
        );

        // -- Phase D: persist both freshly-built caches so the next
        //    cold open skips the rebuild.  Failures are logged but
        //    non-fatal — the in-memory cache is still usable.
        #[cfg(feature = "persist")]
        if let Some(env) = &self.db {
            if let Err(e) = crate::persist::persist_axiom_cache(
                env, /* mode_tff */ true, &cache.tff.problem, &cache.tff.sid_map,
            ) {
                self.warn(format!("Phase D: axiom cache (TFF) persist failed: {}", e));
            }
            if let Err(e) = crate::persist::persist_axiom_cache(
                env, /* mode_tff */ false, &cache.fof.problem, &cache.fof.sid_map,
            ) {
                self.warn(format!("Phase D: axiom cache (FOF) persist failed: {}", e));
            }
        }

        self.axiom_cache = Some(cache);
    }
}
