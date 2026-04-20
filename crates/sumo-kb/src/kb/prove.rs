// crates/sumo-kb/src/kb/prove.rs
//
// Theorem-proving entrypoints on KnowledgeBase: `ask`, `ask_embedded`,
// and their private helpers (`query_affects_taxonomy`, `ensure_axiom_cache`).
// Split out of kb.rs to keep the main module focused on storage / ingestion /
// promotion.

#![cfg(feature = "ask")]

use std::collections::HashSet;
use std::time::Instant;

use super::KnowledgeBase;
use crate::error::KbError;
use crate::kif_store::load_kif;
use crate::prover::{
    Binding, ProverMode, ProverOpts, ProverResult, ProverRunner, ProverStatus, ProverTimings,
};
use crate::tptp::TptpLang;
use crate::types::SentenceId;

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
    /// `session` = optional in-memory session whose assertions become `hypothesis`.
    /// `lang` selects the TPTP language (FOF, TFF) for the generated problem file.
    pub fn ask(
        &mut self,
        query_kif: &str,
        session:   Option<&str>,
        runner:    &dyn ProverRunner,
        lang:      TptpLang,
    ) -> ProverResult {
        use crate::Span;
        use crate::sine::SineParams;

        log::debug!(target: "sumo_kb::kb", "ask: query={}", query_kif);
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
                    timings:    ProverTimings::default(),
                };
            }
        };

        // -- Step 2: Re-parse the conjecture for the native converter. ---
        let query_tag = "__query__";
        let prev_count = self.layer.store.file_roots
            .get(query_tag).map(|v| v.len()).unwrap_or(0);

        let parse_errors: Vec<(Span, KbError)> = profile_call!(self, "ask.parse_query",
            load_kif(&mut self.layer.store, query_kif, query_tag));
        if !parse_errors.is_empty() {
            self.layer.store.remove_file(query_tag);
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: parse_errors.iter()
                    .map(|(_, e): &(Span, KbError)| e.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
                timings:    ProverTimings::default(),
            };
        }

        let query_sids: Vec<SentenceId> = self.layer.store.file_roots
            .get(query_tag)
            .map(|v| v[prev_count..].to_vec())
            .unwrap_or_default();

        if query_sids.is_empty() {
            self.layer.store.remove_file(query_tag);
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: "No query sentence parsed".into(),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
                timings:    ProverTimings::default(),
            };
        }

        // -- Step 3: Collect session-assertion sids. ---------------------
        let assertion_ids: HashSet<SentenceId> = session
            .and_then(|s| self.sessions.get(s))
            .map(|v| v.iter().copied().collect())
            .unwrap_or_default();

        // -- Step 4: Build TPTP via the native converter, with
        // SInE-filtered axioms in place of the full axiom set.  Both FOF
        // and TFF go through the same path: we can no longer reuse the
        // whole-KB TFF axiom cache because the filtered set differs per
        // conjecture.  For a large KB this is still net faster than
        // unfiltered: fewer axioms => less work for the generator AND
        // much less work for the prover.
        use crate::vampire::assemble::{assemble_tptp, AssemblyOpts};
        use crate::vampire::converter::{Mode, NativeConverter};

        let mode = match lang {
            TptpLang::Tff => Mode::Tff,
            TptpLang::Fof => Mode::Fof,
        };
        let t_input = Instant::now();

        let (problem, sid_map) = {
            let _span = profile_span!(self, "ask.tptp_build");
            let mut conv = NativeConverter::new(&self.layer.store, &self.layer, mode);
            // SInE-selected axioms, sorted for deterministic output.
            let mut axioms_sorted: Vec<SentenceId> =
                selected_axioms.iter().copied().collect();
            axioms_sorted.sort_unstable();
            for sid in axioms_sorted { conv.add_axiom(sid); }
            // Session assertions always ride along as additional axioms
            // (the TPTP assembler labels them `axiom`, which the prover
            // treats identically to `hypothesis` for our purposes).
            for &sid in &assertion_ids { conv.add_axiom(sid); }
            for &qsid in &query_sids {
                if conv.set_conjecture(qsid).is_some() { break; }
            }
            conv.finish()
        };

        let tptp = {
            let _span = profile_span!(self, "ask.tptp_build");
            assemble_tptp(&problem, &sid_map, &AssemblyOpts {
                conjecture_name: "query_0",
                ..AssemblyOpts::default()
            })
        };
        let input_gen = t_input.elapsed();
        log::debug!(target: "sumo_kb::kb",
            "ask({:?}): TPTP size={} bytes ({} SInE-selected axioms, {} assertions)",
            mode, tptp.len(), selected_axioms.len(), assertion_ids.len());

        // Roll back the conjecture parse.  Phase A optimisation:
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
            self.layer.store.remove_file(query_tag);
            if needs_rebuild {
                self.layer.rebuild_taxonomy();
                self.layer.invalidate_cache();
            }
        });

        let prover_opts = ProverOpts { timeout_secs: runner.timeout_secs(), mode: ProverMode::Prove };
        let mut result = {
            // `runner.prove` takes `&dyn ProverRunner`, not `&mut self`,
            // so `profile_span!` works here.  Keep it as a span so the
            // inner sub-phases (`ask.output_parse`, see below) can be
            // recorded as siblings rather than nested children.
            let _span = profile_span!(self, "ask.prover_run");
            runner.prove(&tptp, &prover_opts)
        };
        // Also record the prover-reported sub-phase timings (output parse,
        // binding extraction) into the profiler for unified reporting.
        if let Some(p) = self.profiler.as_ref() {
            p.record("ask.output_parse", result.timings.output_parse);
        }
        result.timings.input_gen = input_gen;
        result
    }

    // -- Embedded theorem proving ----------------------------------------------

    /// Ask the embedded Vampire prover whether `query_kif` is entailed by the KB.
    ///
    /// Unlike [`ask`], this bypasses TPTP generation and calls Vampire in-process
    /// via the programmatic API.  Binding extraction is not yet supported.
    ///
    /// `session` = optional in-memory session whose assertions are included as hypotheses.
    #[cfg(feature = "integrated-prover")]
    pub fn ask_embedded(
        &mut self,
        query_kif: &str,
        session:   Option<&str>,
        timeout_secs: u32,
    ) -> ProverResult {
        let query_tag = "__query_embedded__";
        let prev_count = self.layer.store.file_roots
            .get(query_tag).map(|v| v.len()).unwrap_or(0);

        // Phase A: no preemptive invalidation.  See the comment in
        // `ask()` for the reasoning.
        let parse_errors = load_kif(&mut self.layer.store, query_kif, query_tag);
        if !parse_errors.is_empty() {
            self.layer.store.remove_file(query_tag);
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: parse_errors.iter()
                    .map(|(_, e)| e.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
                timings:    ProverTimings::default(),
            };
        }

        let query_sids: Vec<SentenceId> = self.layer.store.file_roots
            .get(query_tag)
            .map(|v| v[prev_count..].to_vec())
            .unwrap_or_default();

        if query_sids.is_empty() {
            // No sentences parsed from the query text -- nothing got
            // into the store or the taxonomy, so no cleanup beyond
            // remove_file is needed (Phase A).
            self.layer.store.remove_file(query_tag);
            return ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: "No query sentence parsed".into(),
                bindings:   Vec::new(),
                proof_kif:  Vec::new(),
                timings:    ProverTimings::default(),
            };
        }

        let assertion_sids: Vec<SentenceId> = session
            .and_then(|s| self.sessions.get(s))
            .cloned()
            .unwrap_or_default();

        // Ensure the IR axiom cache is built.
        self.ensure_axiom_cache();

        // Build the IR problem: clone the cached axiom set, extend with
        // session assertions and the conjecture.
        use crate::vampire::converter::{Mode, NativeConverter};
        let (seed_problem, seed_sid_map) = {
            let cache = self.axiom_cache.as_ref().unwrap();
            (cache.problem.clone(), cache.sid_map.clone())
        };
        let mut conv = NativeConverter::from_parts(
            &self.layer.store, &self.layer, seed_problem, seed_sid_map, Mode::Tff,
        );
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

        // Lower to the FFI problem, set options, and solve.
        let mut opts = vampire_prover::Options::new();
        if timeout_secs > 0 {
            opts.timeout(std::time::Duration::from_secs(timeout_secs as u64));
        }

        // SInE handling (mirror of the subprocess path — see
        // `crates/sumo-kb/src/prover/subprocess.rs::build_vampire_args`
        // for the full rationale).  The KB applies SInE externally
        // before lowering to the Vampire problem; we disable
        // Vampire's own SInE to prevent over-selection:
        //
        // 1. `mode = vampire`: single-strategy mode.  The `casc`
        //    portfolio's schedules contain strategies with `ss=axioms`
        //    encoded in their option-strings; `readFromEncodedOptions`
        //    applies those per-strategy and overrides any global
        //    `sine_selection=off`.  Only `vampire` mode fully escapes
        //    that re-filter.
        // 2. `sine_selection = off`: defensive explicit disable at the
        //    preprocessing level.  Vampire's default is already `off`
        //    but spelling it out makes the intent explicit.
        opts.set_option("mode", "vampire");
        opts.set_option("sine_selection", "off");

        let mut problem = vampire_prover::lower_problem(&ir_problem, opts);

        let (res, proof) = problem.solve_and_prove();
        log::debug!(target: "sumo_kb::embedded_prover", "TFF embedded result: {:?}", res);

        let status = match res {
            vampire_prover::ProofRes::Proved     => ProverStatus::Proved,
            vampire_prover::ProofRes::Unprovable => ProverStatus::Disproved,
            vampire_prover::ProofRes::Unknown(_) => ProverStatus::Unknown,
        };

        // Extract variable bindings from the native proof when one is
        // available. Empty result is non-fatal (prover may not produce a
        // proof, or the extractor may not recognise the encoding).
        let bindings: Vec<Binding> = if matches!(status, ProverStatus::Proved) {
            log::debug!(target: "sumo_kb::embedded_prover",
                "bindings eligibility: proof={}, qvm={}",
                proof.is_some(), query_var_map.is_some());
            match (proof, query_var_map) {
                (Some(p), Some(qvm)) => crate::vampire::bindings::extract_bindings(&p, &qvm)
                    .into_iter()
                    .map(|b| Binding { variable: b.variable, value: b.value })
                    .collect(),
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };

        // Phase A: skip the full taxonomy rebuild unless the query
        // actually added a taxonomy edge.  See the comment in `ask()`.
        let needs_rebuild = self.query_affects_taxonomy(&query_sids);
        self.layer.store.remove_file(query_tag);
        if needs_rebuild {
            self.layer.rebuild_taxonomy();
            self.layer.invalidate_cache();
        }

        ProverResult {
            status,
            raw_output: format!("{:?}", res),
            bindings,
            proof_kif:  Vec::new(),
            timings:    ProverTimings::default(), // profiling TODO
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
    fn query_affects_taxonomy(&self, sids: &[SentenceId]) -> bool {
        use crate::types::TaxRelation;
        sids.iter().any(|&sid| {
            let sentence = &self.layer.store.sentences[self.layer.store.sent_idx(sid)];
            match sentence.head_symbol() {
                Some(head_id) => {
                    let name = self.layer.store.sym_name(head_id);
                    TaxRelation::from_str(name).is_some()
                }
                None => false,
            }
        })
    }

    /// Ensure the TFF IR axiom cache is populated; build it if needed.
    /// After this call `self.axiom_cache` is guaranteed to be `Some`.
    ///
    /// Phase D: before rebuilding the cache via `NativeConverter` (a
    /// ~45 ms walk over every KB axiom), try to restore it from the
    /// LMDB `axiom_cache_tff` blob.  The blob is a bincode-serialised
    /// `ir::Problem` + parallel `sid_map`; deserialising is a linear
    /// byte walk with no semantic-layer lookups and no re-interning,
    /// which benchmarks faster than the rebuild path.  Stale /
    /// version-mismatched / missing blobs fall through to the rebuild.
    fn ensure_axiom_cache(&mut self) {
        if self.axiom_cache.is_some() { return; }

        // -- Fast path: restore from LMDB -------------------------------
        #[cfg(feature = "persist")]
        if let Some(env) = &self.db {
            if let Ok(Some(cached)) = (|| -> Result<Option<crate::persist::CachedAxiomProblem>, KbError> {
                let rtxn = env.read_txn()?;
                let current = env.kb_version(&rtxn)?;
                match env.get_cache::<crate::persist::CachedAxiomProblem>(
                    &rtxn, crate::persist::CACHE_KEY_AXIOM_CACHE_TFF,
                )? {
                    Some(c) if c.kb_version == current && c.mode_tff => Ok(Some(c)),
                    Some(c) => {
                        log::debug!(target: "sumo_kb::kb",
                            "Phase D: axiom cache TFF stale (kb_version {} vs current {}, mode_tff={}); rebuilding",
                            c.kb_version, current, c.mode_tff);
                        Ok(None)
                    }
                    None => Ok(None),
                }
            })() {
                log::info!(target: "sumo_kb::kb",
                    "Phase D: restored axiom cache from bincode blob ({} axioms)",
                    cached.sid_map.len());
                self.axiom_cache = Some(crate::vampire::VampireAxiomCache {
                    problem: cached.problem,
                    sid_map: cached.sid_map,
                });
                return;
            }
        }

        // -- Slow path: rebuild from in-memory store via NativeConverter.
        let axiom_ids = self.axiom_ids_set();
        let cache = crate::vampire::VampireAxiomCache::build(
            &self.layer,
            &axiom_ids,
            crate::vampire::converter::Mode::Tff,
        );

        // -- Phase D: persist the freshly-built cache so the next
        //    cold open skips the rebuild.
        #[cfg(feature = "persist")]
        if let Some(env) = &self.db {
            if let Err(e) = crate::persist::persist_axiom_cache(
                env, /* mode_tff */ true, &cache.problem, &cache.sid_map,
            ) {
                log::warn!(target: "sumo_kb::kb",
                    "Phase D: axiom cache persist failed: {}", e);
            }
        }

        self.axiom_cache = Some(cache);
    }
}
