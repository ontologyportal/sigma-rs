//! Theorem-proving entrypoints on `KnowledgeBase`.
//!
//! Gated on `any(ask, native-prover)`: the generic `ask` / `audit_consistency`
//! entry points are backend-agnostic (any `ProvingLayer`), so they must be
//! reachable by the native saturation prover WITHOUT the subprocess `ask`
//! feature — the native prover is the only proving backend that runs on wasm32.

#![cfg(any(feature = "external-prover", feature = "native-prover"))]

use std::collections::HashSet;
use std::path::PathBuf;

use crate::layer::{Layer, TopLayer};
use crate::prover::{CommonProverOpts, ProverResult, ProverStatus, ProvingLayer};
use crate::types::{SentenceId, SourceFile};
use crate::{Parser, TestCase};

use super::KnowledgeBase;

impl<L: ProvingLayer + TopLayer + Layer> KnowledgeBase<L> {
    /// Discharge a test-case query against the KB with the top layer's prover.
    ///
    /// `opts` carries the layer's proving parameters, including the optional
    /// in-memory session whose assertions become hypotheses.
    ///
    /// A test case with hypotheses but no conjecture is answered by a
    /// consistency audit focused on those hypotheses (`Consistent` /
    /// `Inconsistent`), never `Proved`; one with neither is an `InputError`.
    pub fn ask(&self, tc: TestCase, opts: &L::Opts) -> ProverResult {
        self.ask_with(&self.layer, tc, opts)
    }

    /// Saturate the base (plus optional session support) for up to `limit`
    /// distinct contradictions over `focus`'s neighborhood (empty => whole base).
    /// Selection / session ride in on `opts` -- the layer's consolidated params
    /// struct.
    pub fn audit_consistency(
        &self,
        focus: &[SentenceId],
        opts: &L::Opts,
        limit: usize,
    ) -> ProverResult {
        self.audit_with(&self.layer, focus, opts, limit)
    }

    /// Single-contradiction satisfiability check (`limit = 1`) over the whole
    /// base plus optional session support carried by `opts`.
    pub fn check_satisfiable(&self, opts: L::Opts) -> ProverResult {
        self.audit_consistency(&[], &opts, 1)
    }

    /// [`audit_sampled_with`](KnowledgeBase::audit_sampled_with) on the top
    /// layer's prover.
    pub fn audit_sampled(
        &self,
        order: &[SentenceId],
        sample: AuditSample,
        opts: &L::Opts,
    ) -> SampledAudit {
        self.audit_sampled_with(&self.layer, order, sample, opts)
    }
}

impl<L: TopLayer + Layer> KnowledgeBase<L> {
    /// Discharge a test-case query against the KB with `prover` -- any
    /// [`ProvingLayer`] in this KB's stack, not only the top one (a KB that
    /// nests the native prover under an external one picks per call).  The
    /// hypothesis staging, rollback, and countermodel gate are the same
    /// whichever engine runs.
    pub fn ask_with<P: ProvingLayer>(
        &self,
        prover: &P,
        tc: TestCase,
        opts: &P::Opts,
    ) -> ProverResult {
        with_guard!(self);
        self.debug(format!("ask: query={}", tc.query_kif().unwrap_or_default()));

        // No conjecture: an axioms-only problem asks whether its hypotheses
        // are consistent with the KB (the TPTP reading), answered by a
        // focused audit below. With nothing at all there is no question, and
        // an empty audit focus would mean the whole base.
        // The `.tq` / TPTP parsers wrap the conjecture in `Annotated`; the
        // conjecture normalizer interns the bare formula (a negated
        // conjecture's `not` is already baked in by `renegate`).
        let query = tc.query.map(|q| q.formula().clone());
        if query.is_none() && tc.axioms.is_empty() {
            return ProverResult {
                status: ProverStatus::InputError,
                raw_output: "test case has neither a conjecture nor hypotheses".into(),
                ..Default::default()
            };
        }
        let staged = self.stage_hypotheses(&tc.file_name, tc.axioms, opts.session());
        // Hypothesis-staging failures must not stay silent: a hypothesis
        // that never reached the session could be the one that makes the set
        // unsatisfiable, so its loss poisons any confident
        // Disproved/Satisfiable verdict (withheld after the prove below).
        // Assembly losses recorded by the parser (`unaccounted_inputs`)
        // ride the same gate.
        let input_failures = tc.unaccounted_inputs + staged.errors.len();

        // Scope the prover to the session the hypotheses were staged under,
        // so the engine force-includes them.
        let mut opts = opts.clone();
        opts.set_session(staged.session.clone());

        // The layer's `prove` warms up, prepares the conjecture (its own
        // intern + rollback via `cleanup`), runs the shared scaling loop, and
        // returns.  `ProveCtx` carries this KB's progress sink down to it.
        let ctx = self.prove_ctx();
        let mut result = match query {
            Some(query) => prover.prove(vec![query], &opts, &ctx),
            None => {
                let focus = staged
                    .key
                    .as_deref()
                    .map(|key| self.layer.semantic().syntactic.file_root_sids(key))
                    .unwrap_or_default();
                if focus.is_empty() {
                    ProverResult {
                        status: ProverStatus::InputError,
                        raw_output: "no hypothesis reached the session to audit".into(),
                        ..Default::default()
                    }
                } else {
                    prover.audit_consistency(&focus, &opts, 1, &ctx)
                }
            }
        };
        // Input-completeness gate: staged-hypothesis / assembly losses make
        // a confident "no" (Disproved/Satisfiable) unsound — demote it to
        // Unknown/GaveUp with a loud reason.  Proved verdicts stand.
        result.withhold_countermodel(input_failures, "hypothesis staging / test-case assembly");

        profile_call!(self, "ask.rollback", self.unstage_hypotheses(staged));
        result
    }

    /// Stage a test case's hypotheses into `session` (a fresh one when
    /// `None`) under the source `file_name` they came from, so proof steps
    /// still cite it.  Without hypotheses nothing is staged and the
    /// session is passed through.
    pub(super) fn stage_hypotheses(
        &self,
        file_name: &str,
        mut axioms: Vec<crate::AstNode>,
        session: Option<&str>,
    ) -> StagedHypotheses {
        if axioms.is_empty() {
            return StagedHypotheses {
                session: session.map(str::to_string),
                key: None,
                errors: Vec::new(),
            };
        }
        let session = session.map_or_else(
            || self.layer.semantic().syntactic.unique_source_key("ask"),
            str::to_string,
        );
        let key = file_name.to_string();
        for ast in &mut axioms {
            ast.attribute_to(&key);
        }
        let outcome = self.ingest_source(
            SourceFile {
                parser: Parser::Kif { options: None },
                name: key.clone(),
                path: PathBuf::from(&key),
                origin: crate::FileOrigin::Local(crate::types::LocalProvenance::UNKNOWN),
                contents: String::new(),
                prebuilt: Some(axioms),
            },
            &session,
            true,
        );
        StagedHypotheses {
            errors: outcome.errors,
            session: Some(session),
            key: Some(key),
        }
    }

    /// Roll back [`stage_hypotheses`](Self::stage_hypotheses): re-ingest its
    /// source key empty, so the hypotheses do not leak into later asks or
    /// whole-store scans.
    pub(super) fn unstage_hypotheses(&self, staged: StagedHypotheses) {
        if let (Some(key), Some(session)) = (staged.key, staged.session) {
            let _ = self.ingest_source(SourceFile::truncate(PathBuf::from(key)), &session, true);
        }
    }

    /// [`audit_consistency`](KnowledgeBase::audit_consistency) with `prover`
    /// instead of the top layer.
    pub fn audit_with<P: ProvingLayer>(
        &self,
        prover: &P,
        focus: &[SentenceId],
        opts: &P::Opts,
        limit: usize,
    ) -> ProverResult {
        prover.audit_consistency(focus, opts, limit, &self.prove_ctx())
    }

    /// The sweep a sampled audit walks: every promoted axiom (or only
    /// `scope`'s, a loaded file tag) minus the excluded bookkeeping heads,
    /// in a pseudorandom order fixed by `seed`.  The order depends only on
    /// the seed and the sentences' content, so a `(seed, step)` pair names
    /// the same position again as long as the eligible set is unchanged.
    pub fn audit_sweep_order(&self, scope: Option<&str>, seed: u32) -> Vec<SentenceId> {
        let axioms = self.syntactic().axiom_ids_set();
        let pool: Vec<SentenceId> = match scope {
            Some(file) => self
                .file_roots(file)
                .into_iter()
                .filter(|sid| axioms.contains(sid))
                .collect(),
            None => axioms.into_iter().collect(),
        };
        let mut order = self.filter_excluded_heads(&pool);
        order.sort_unstable_by_key(|&sid| (splitmix64(sid ^ u64::from(seed)), sid));
        order
    }

    /// Sampled consistency audit: take `sample.count` sentences of `order`
    /// from `sample.step`, and for each group of `sample.batch` run a focused
    /// [`audit_with`](Self::audit_with) to check satisfiability over that
    /// group's SInE neighborhood, selected by `opts`' selection parameters.
    /// Any contradiction found in a neighborhood is one of the KB's.
    ///
    /// Stops early once `sample.limit` distinct contradictions (deduped by
    /// the source axioms they cite) are found.  The result is `Inconsistent`
    /// or `Unknown`.
    pub fn audit_sampled_with<P: ProvingLayer>(
        &self,
        prover: &P,
        order: &[SentenceId],
        sample: AuditSample,
        opts: &P::Opts,
    ) -> SampledAudit {
        let start = sample.step.min(order.len());
        let end = start.saturating_add(sample.count).min(order.len());
        let limit = sample.limit.max(1);
        let mut out = SampledAudit {
            total: order.len(),
            next_step: start,
            ..Default::default()
        };
        let mut seen: HashSet<Vec<SentenceId>> = HashSet::new();
        let mut proofs: Vec<Vec<crate::prover::proof::KifProofStep>> = Vec::new();
        let mut inconsistent = false;
        for focus in order[start..end].chunks(sample.batch.max(1)) {
            if proofs.len() >= limit {
                break;
            }
            let started = crate::clock::Instant::now();
            let mut r = self.audit_with(prover, focus, opts, limit - proofs.len());
            inconsistent |= r.status == ProverStatus::Inconsistent;
            let mut found = std::mem::take(&mut r.contradiction_proofs);
            if found.is_empty() && !r.proof_kif.is_empty() {
                found.push(std::mem::take(&mut r.proof_kif));
            }
            for steps in found {
                let mut culprits: Vec<SentenceId> =
                    steps.iter().filter_map(|s| s.source_sid).collect();
                culprits.sort_unstable();
                culprits.dedup();
                if seen.insert(culprits) {
                    proofs.push(steps);
                }
            }
            out.next_step += focus.len();
            out.batches.push(AuditBatch {
                focus: focus.to_vec(),
                outcome: r,
                elapsed: started.elapsed(),
            });
        }
        out.result = ProverResult {
            status: if inconsistent {
                ProverStatus::Inconsistent
            } else {
                ProverStatus::Unknown
            },
            raw_output: format!(
                "sampled audit: seed {}, positions {}..{} of {} in {} subproblem(s); \
                 {} distinct contradiction(s)",
                sample.seed,
                start,
                out.next_step,
                out.total,
                out.batches.len(),
                proofs.len()
            ),
            proof_kif: proofs.first().cloned().unwrap_or_default(),
            contradiction_proofs: proofs,
            ..Default::default()
        };
        out
    }
}

/// Hypotheses [`KnowledgeBase::stage_hypotheses`] staged for one call.
pub(super) struct StagedHypotheses {
    /// The session the prover reasons in: the caller's, a fresh one holding
    /// the hypotheses, or `None` when there was neither.
    pub(super) session: Option<String>,
    /// The source key the hypotheses were ingested under, if any.
    key: Option<String>,
    /// Diagnostics of hypotheses that failed to stage.
    pub(super) errors: Vec<crate::Diagnostic>,
}

/// Which slice of the sweep a sampled audit checks (see
/// [`KnowledgeBase::audit_sampled_with`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditSample {
    /// Seed of the pseudorandom sweep order.
    pub seed: u32,
    /// Sweep position to start from.
    pub step: usize,
    /// How many sentences to check (clamped to what remains).
    pub count: usize,
    /// Sentences per subproblem (their neighborhoods are checked together).
    pub batch: usize,
    /// Stop after this many distinct contradictions.
    pub limit: usize,
}

/// One subproblem of a sampled audit: the focus sentences whose SInE
/// neighborhood was checked, and that check's outcome.  The outcome's
/// contradiction proofs are moved into [`SampledAudit::result`].
#[derive(Debug, Clone)]
pub struct AuditBatch {
    pub focus: Vec<SentenceId>,
    pub outcome: ProverResult,
    pub elapsed: std::time::Duration,
}

/// Outcome of [`KnowledgeBase::audit_sampled_with`].
#[derive(Debug, Clone, Default)]
pub struct SampledAudit {
    /// `Inconsistent` (with the deduped proofs) or `Unknown`.
    pub result: ProverResult,
    pub batches: Vec<AuditBatch>,
    /// Size of the whole sweep.
    pub total: usize,
    /// Where to resume: the position after the last sentence checked.
    pub next_step: usize,
}

/// A cheap, well-mixed bijection for seeding the
/// sweep order without a random-number dependency.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The single conjecture a parsed query asks: its one formula, or the
/// conjunction of several.
fn conjoin(mut asts: Vec<crate::AstNode>) -> Option<crate::AstNode> {
    match asts.len() {
        0 => None,
        1 => asts.pop(),
        _ => {
            let span = asts[0].span().clone();
            let and = crate::AstNode::Operator {
                op: crate::OpKind::And,
                span: span.clone(),
            };
            Some(crate::AstNode::List {
                elements: std::iter::once(and).chain(asts).collect(),
                span,
            })
        }
    }
}

/// Parse an ad hoc `query` in `dialect` into the [`TestCase`] `ask` takes.
/// A TPTP problem is partitioned by role (`conjecture` -> query,
/// `hypothesis` and background `axiom`s -> hypotheses); any other dialect
/// carries no roles, so its formulas are conjoined into one conjecture.
/// `Err` carries the `InputError` result for a malformed or empty query.
fn query_test_case(query: &str, dialect: Parser) -> Result<TestCase, Box<ProverResult>> {
    let input_error = |raw_output: String| {
        Box::new(ProverResult {
            status: ProverStatus::InputError,
            raw_output,
            ..Default::default()
        })
    };
    match dialect {
        Parser::Tptp { options } => crate::parse::tptp::test_case::parse_tptp_test_with(
            query,
            "ask_query",
            options.unwrap_or_default(),
        )
        .map_err(|d| input_error(format!("query parse error: {}", d.message))),
        other => match crate::prover::Conjecture::parse(query, other).map(conjoin)? {
            Some(ast) => Ok(TestCase::conjecture("ask_query", ast)),
            None => Err(input_error("query parsed to no formula".into())),
        },
    }
}

/// The native prover's query entry point, shared by the bare native stack
/// and the native layer nested under an external one.
#[cfg(feature = "native-prover")]
fn native_ask_query_dialect<L: TopLayer + Layer, S: TopLayer + 'static>(
    kb: &KnowledgeBase<L>,
    prover: &crate::prover::saturate::ProverLayer<S>,
    query: &str,
    opts: &crate::NativeOpts,
    dialect: Parser,
) -> ProverResult {
    match query_test_case(query, dialect) {
        Ok(tc) => kb.ask_with(prover, tc, opts),
        Err(r) => *r,
    }
}

#[cfg(feature = "external-prover")]
impl<T: crate::trans::HasTranslation + 'static>
    KnowledgeBase<crate::prover::ExternalProverLayer<T>>
{
    /// Ask the external prover to discharge `query` (parsed in `dialect`,
    /// see [`query_test_case`]) under `opts`, whose `session` names optional
    /// in-memory support and whose `selection` seeds axiom selection.
    pub fn ask_query_dialect(
        &self,
        query: &str,
        opts: &crate::ExternalOpts,
        dialect: Parser,
    ) -> ProverResult {
        match query_test_case(query, dialect) {
            Ok(tc) => self.ask(tc, opts),
            Err(r) => *r,
        }
    }
}

#[cfg(all(feature = "external-prover", feature = "native-prover"))]
impl<S: crate::trans::HasTranslation + 'static>
    KnowledgeBase<crate::prover::ExternalProverLayer<crate::prover::saturate::ProverLayer<S>>>
{
    /// The native prover nested under this KB's external layer, for the
    /// `*_with` entry points ([`audit_with`](KnowledgeBase::audit_with),
    /// [`audit_sampled_with`](KnowledgeBase::audit_sampled_with)).
    pub fn native(&self) -> &crate::prover::saturate::ProverLayer<S> {
        self.layer.inner_layer()
    }

    /// [`ask_query_dialect`](KnowledgeBase::ask_query_dialect) on the nested
    /// native prover instead of the external backend.
    pub fn ask_query_dialect_native(
        &self,
        query: &str,
        opts: &crate::NativeOpts,
        dialect: Parser,
    ) -> ProverResult {
        native_ask_query_dialect(self, self.native(), query, opts, dialect)
    }
}

#[cfg(feature = "native-prover")]
impl<S: TopLayer + 'static> KnowledgeBase<crate::prover::saturate::ProverLayer<S>> {
    /// [`ask_query_dialect`](Self::ask_query_dialect) on a SUO-KIF
    /// conjecture, with `session` and `sine` folded into `opts`.
    pub fn ask_query(
        &self,
        query_kif: &str,
        session: Option<&str>,
        sine: crate::SineParams,
        opts: crate::NativeOpts,
    ) -> ProverResult {
        let opts = crate::NativeOpts {
            session: session.map(str::to_string),
            selection: sine,
            ..opts
        };
        self.ask_query_dialect(query_kif, &opts, Parser::Kif { options: None })
    }

    /// Same as [`ask_query`](Self::ask_query) but parses `query` in `dialect`
    /// instead of always assuming SUO-KIF
    pub fn ask_query_dialect(
        &self,
        query: &str,
        opts: &crate::NativeOpts,
        dialect: Parser,
    ) -> ProverResult {
        native_ask_query_dialect(self, &self.layer, query, opts, dialect)
    }
}

#[cfg(all(test, feature = "native-prover"))]
mod dialect_tests {
    use super::KnowledgeBase;
    use crate::prover::{ProverLayer, ProverStatus};
    use crate::{NativeOpts, Parser, SineParams};

    fn kb_native(kif: &str) -> KnowledgeBase<ProverLayer> {
        let mut kb = KnowledgeBase::new_native();
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("test.kif"), "test.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("test.kif").expect("promote");
        kb
    }

    fn fast() -> NativeOpts {
        NativeOpts {
            time_limit_secs: 10,
            ..Default::default()
        }
    }

    fn audit() -> NativeOpts {
        NativeOpts {
            session: Some("audit".into()),
            ..fast()
        }
    }

    #[test]
    fn ask_query_dialect_proves_a_tptp_conjecture() {
        let kb = kb_native("(subclass Dog Mammal)\n(subclass Mammal Animal)\n");
        let res = kb.ask_query_dialect(
            "fof(g, conjecture, subclass('Dog', 'Animal')).",
            &fast(),
            Parser::Tptp { options: None },
        );
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    #[test]
    fn ask_query_dialect_stages_tptp_axioms_and_hypotheses_as_support() {
        let kb = kb_native("(subclass Mammal Animal)\n");
        let unsupported = kb.ask_query_dialect(
            "fof(g, conjecture, subclass('Dog', 'Animal')).",
            &fast(),
            Parser::Tptp { options: None },
        );
        assert_ne!(unsupported.status, ProverStatus::Proved);
        for role in ["axiom", "hypothesis"] {
            let res = kb.ask_query_dialect(
                &format!(
                    "fof(a, {role}, subclass('Dog', 'Mammal')).\n\
                     fof(g, conjecture, subclass('Dog', 'Animal'))."
                ),
                &fast(),
                Parser::Tptp { options: None },
            );
            assert_eq!(
                res.status,
                ProverStatus::Proved,
                "{role}: {}",
                res.raw_output
            );
        }
        assert!(
            kb.syntactic().file_root_sids("ask_query").is_empty(),
            "staged TPTP support must roll back"
        );
    }

    #[test]
    fn ask_query_dialect_audits_a_tptp_problem_without_a_conjecture() {
        let kb = kb_native("(=> (p ?X) (q ?X))\n(p a)\n");
        let res = kb.ask_query_dialect(
            "fof(h, axiom, ~q(a)).",
            &audit(),
            Parser::Tptp { options: None },
        );
        assert_eq!(
            res.status,
            ProverStatus::Inconsistent,
            "raw: {}",
            res.raw_output
        );
    }

    #[test]
    fn ask_query_dialect_reports_input_error_for_malformed_tptp() {
        let kb = kb_native("(subclass Dog Mammal)\n");
        let res = kb.ask_query_dialect(
            "fof(g, conjecture, subclass('Dog'", // missing close
            &fast(),
            Parser::Tptp { options: None },
        );
        assert_eq!(res.status, ProverStatus::InputError);
    }

    /// A test case carrying only hypotheses, parsed from KIF.
    fn hypotheses_only(kif: &str) -> crate::TestCase {
        let doc = crate::parse_document("hyp.kif", kif.to_string(), Parser::Kif { options: None });
        let axioms: Vec<crate::AstNode> = doc
            .ast
            .into_iter()
            .filter_map(|d| d.as_stmt().cloned())
            .collect();
        let mut tc = crate::TestCase::conjecture("hyp.kif", axioms[0].clone());
        tc.query = None;
        tc.axioms = axioms;
        tc
    }

    #[test]
    fn ask_without_conjecture_audits_consistent_hypotheses() {
        let kb = kb_native("(=> (p ?X) (q ?X))\n(p a)\n");
        let res = kb.ask(hypotheses_only("(r b)"), &audit());
        assert_eq!(
            res.status,
            ProverStatus::Consistent,
            "raw: {}",
            res.raw_output
        );
    }

    #[test]
    fn ask_without_conjecture_finds_hypotheses_inconsistent_with_kb() {
        let kb = kb_native("(=> (p ?X) (q ?X))\n(p a)\n");
        let res = kb.ask(hypotheses_only("(not (q a))"), &audit());
        assert_eq!(
            res.status,
            ProverStatus::Inconsistent,
            "raw: {}",
            res.raw_output
        );
    }

    #[test]
    fn ask_with_neither_conjecture_nor_hypotheses_is_an_input_error() {
        let kb = kb_native("(p a)\n");
        let mut tc = hypotheses_only("(r b)");
        tc.axioms.clear();
        let res = kb.ask(tc, &fast());
        assert_eq!(res.status, ProverStatus::InputError);
    }

    #[test]
    fn audit_by_ask_rolls_back_its_hypotheses() {
        let kb = kb_native("(=> (p ?X) (q ?X))\n(p a)\n");
        let roots_before = kb.syntactic().root_sids().len();
        let _ = kb.ask(hypotheses_only("(not (q a))"), &audit());
        assert_eq!(kb.syntactic().root_sids().len(), roots_before);
        assert!(kb.session_sids("audit").is_empty());
    }

    #[test]
    fn ask_query_still_defaults_to_kif() {
        let kb = kb_native("(subclass Dog Mammal)\n");
        let res = kb.ask_query("(subclass Dog Mammal)", None, SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    fn sample(step: usize, count: usize, batch: usize, limit: usize) -> crate::AuditSample {
        crate::AuditSample {
            seed: 7,
            step,
            count,
            batch,
            limit,
        }
    }

    const CONTRA: &str = "(p a)\n(not (q a))\n(=> (p ?X) (q ?X))\n(r b)\n(s c)\n";

    #[test]
    fn sweep_order_is_a_seeded_permutation_without_bookkeeping() {
        let facts: String = (0..20).map(|i| format!("(p c{i})\n")).collect();
        let kb = kb_native(&format!(
            "(documentation c0 EnglishLanguage \"a thing\")\n{facts}"
        ));
        let order = kb.audit_sweep_order(None, 1);
        assert_eq!(order.len(), 20, "documentation must be excluded");
        assert_eq!(
            order,
            kb.audit_sweep_order(None, 1),
            "same seed, same order"
        );
        let mut sorted = order.clone();
        sorted.sort_unstable();
        let mut other = kb.audit_sweep_order(None, 2);
        assert_ne!(order, other, "a different seed reorders");
        other.sort_unstable();
        assert_eq!(sorted, other, "every seed covers the same sentences");
    }

    #[test]
    fn sweep_order_can_be_scoped_to_one_file() {
        let mut kb = kb_native("(p a)\n(p b)\n");
        let r = kb.reload_kif(
            "(q a)\n",
            &std::path::PathBuf::from("other.kif"),
            "other.kif",
        );
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("other.kif").expect("promote");
        assert_eq!(kb.audit_sweep_order(None, 0).len(), 3);
        assert_eq!(kb.audit_sweep_order(Some("other.kif"), 0).len(), 1);
    }

    #[test]
    fn sampled_audit_finds_a_contradiction_in_a_neighborhood() {
        let kb = kb_native(CONTRA);
        let order = kb.audit_sweep_order(None, 7);
        let out = kb.audit_sampled(&order, sample(0, order.len(), 1, 10), &fast());
        assert_eq!(
            out.result.status,
            ProverStatus::Inconsistent,
            "raw: {}",
            out.result.raw_output
        );
        assert_eq!(
            out.result.contradiction_proofs.len(),
            1,
            "deduped by culprits"
        );
        assert_eq!(out.next_step, order.len());
        assert_eq!(out.total, order.len());
    }

    #[test]
    fn sampled_audit_never_claims_consistent() {
        let kb = kb_native("(p a)\n(q a)\n(=> (p ?X) (q ?X))\n");
        let order = kb.audit_sweep_order(None, 7);
        let out = kb.audit_sampled(&order, sample(0, order.len(), 1, 10), &fast());
        assert_eq!(out.result.status, ProverStatus::Unknown);
        assert!(out.result.contradiction_proofs.is_empty());
        assert_eq!(out.batches.len(), order.len());
    }

    #[test]
    fn sampled_audit_pages_through_the_sweep() {
        let kb = kb_native(CONTRA);
        let order = kb.audit_sweep_order(None, 7);
        let out = kb.audit_sampled(&order, sample(1, 2, 1, 10), &fast());
        assert_eq!(out.batches.len(), 2);
        assert_eq!(out.next_step, 3);
        assert_eq!(out.batches[0].focus, vec![order[1]]);

        let batched = kb.audit_sampled(&order, sample(0, order.len(), 3, 10), &fast());
        assert_eq!(batched.batches.len(), order.len().div_ceil(3));
        assert_eq!(batched.batches[0].focus.len(), 3);

        let past = kb.audit_sampled(&order, sample(99, 5, 1, 10), &fast());
        assert!(past.batches.is_empty());
        assert_eq!(past.next_step, order.len());
    }

    #[test]
    fn sampled_audit_stops_at_the_limit() {
        let kb = kb_native("(p a)\n(not (p a))\n(q b)\n(not (q b))\n(r c)\n(not (r c))\n");
        let order = kb.audit_sweep_order(None, 7);
        let out = kb.audit_sampled(&order, sample(0, order.len(), 1, 1), &fast());
        assert_eq!(out.result.contradiction_proofs.len(), 1);
        assert!(out.next_step < order.len(), "stopped before the end");
    }
}

#[cfg(all(test, feature = "external-prover"))]
mod custom_runner_tests {
    use std::sync::{Arc, Mutex};

    use super::KnowledgeBase;
    use crate::layer::TopLayer;
    use crate::prover::vampire_proof::result_from_transcript;
    use crate::prover::{
        ExternalOpts, Prover, ProverOpts, ProverResult, ProverRunner, ProverStatus,
        TerminationReason,
    };
    use crate::{parse_document, Parser, TestCase};

    /// A runner that records every problem it is handed and answers with a
    /// canned Vampire transcript -- what an embedder's bridge to a remote or
    /// in-browser prover looks like from the layer's side.
    struct Canned {
        transcript: &'static str,
        seen: Mutex<Vec<String>>,
    }

    impl ProverRunner for Canned {
        fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
            self.seen.lock().unwrap().push(tptp.to_string());
            result_from_transcript(self.transcript, "", opts.mode, std::time::Duration::ZERO)
        }
        fn name(&self) -> &str {
            "canned"
        }
    }

    const THEOREM: &str = "% SZS status Theorem for input\n\
% SZS output start Proof for input\n\
fof(f1, axiom, p, file('/dev/stdin', kb_1)).\n\
fof(f2, conjecture, p, file('/dev/stdin', query_0)).\n\
fof(f3, negated_conjecture, ~p, inference(negated_conjecture, [], [f2])).\n\
fof(f4, plain, $false, inference(resolution, [], [f1, f3])).\n\
% SZS output end Proof for input\n";

    fn kb_with(runner: Arc<Canned>) -> KnowledgeBase<crate::prover::ExternalProverLayer> {
        let mut kb = KnowledgeBase::new_external(Prover::Custom(runner));
        let r = kb.reload_kif(
            "(subclass Dog Mammal)\n(subclass Mammal Animal)\n(instance Rex Dog)\n",
            &std::path::PathBuf::from("test.kif"),
            "test.kif",
        );
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("test.kif").expect("promote");
        kb
    }

    fn query(kif: &str) -> TestCase {
        let doc = parse_document("test", kif.to_string(), Parser::Kif { options: None });
        let ast = doc
            .ast
            .iter()
            .filter_map(|d| d.as_stmt().cloned())
            .next()
            .expect("one formula");
        TestCase::conjecture("test::query", ast)
    }

    #[test]
    fn a_custom_runner_receives_the_translated_problem_and_its_verdict_is_the_result() {
        let runner = Arc::new(Canned {
            transcript: THEOREM,
            seen: Mutex::new(Vec::new()),
        });
        let kb = kb_with(runner.clone());
        let res = kb.ask(query("(instance Rex Animal)"), &ExternalOpts::default());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
        assert_eq!(res.proof_kif.len(), 4, "proof steps: {:?}", res.proof_kif);

        let seen = runner.seen.lock().unwrap();
        assert!(!seen.is_empty(), "the runner was never invoked");
        let tptp = &seen[0];
        assert!(tptp.contains("conjecture"), "no conjecture in:\n{tptp}");
        assert!(tptp.contains("fof(kb_"), "no KB axioms in:\n{tptp}");
        assert!(
            tptp.contains("s__Rex") && tptp.contains("s__Animal"),
            "conjecture symbols missing in:\n{tptp}"
        );
    }

    fn kif(text: &str) -> Vec<crate::AstNode> {
        parse_document("hyp.kif", text.to_string(), Parser::Kif { options: None })
            .ast
            .iter()
            .filter_map(|d| d.as_stmt().cloned())
            .collect()
    }

    #[test]
    fn two_live_asks_of_one_conjecture_hold_separate_query_tags() {
        use crate::prover::ProvingLayer;
        let kb = kb_with(Arc::new(Canned {
            transcript: THEOREM,
            seen: Mutex::new(Vec::new()),
        }));
        let first = kb
            .layer
            .prepare(kif("(instance Rex Animal)"))
            .expect("first");
        let second = kb
            .layer
            .prepare(kif("(instance Rex Animal)"))
            .expect("second");
        let sid = first.sents[0].1;
        assert_eq!(second.sents[0].1, sid, "one content-addressed sentence");
        let stored = |kb: &KnowledgeBase<crate::prover::ExternalProverLayer>| {
            kb.layer.semantic().syntactic.sentence(sid).is_some()
        };

        kb.layer.cleanup(first);
        assert!(stored(&kb), "the other ask's conjecture survives");
        kb.layer.cleanup(second);
        assert!(!stored(&kb), "the last cleanup removes it");
    }

    #[test]
    fn an_ask_leaves_neither_its_conjecture_nor_its_hypotheses_behind() {
        let kb = kb_with(Arc::new(Canned {
            transcript: THEOREM,
            seen: Mutex::new(Vec::new()),
        }));
        let roots = |kb: &KnowledgeBase<crate::prover::ExternalProverLayer>| {
            kb.layer.semantic().syntactic.root_sids()
        };
        let before = roots(&kb);
        let tc = TestCase {
            axioms: kif("(instance Fido Dog)"),
            ..query("(instance Fido Animal)")
        };
        let res = kb.ask(tc, &ExternalOpts::default());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
        assert_eq!(roots(&kb), before);
    }

    #[test]
    fn a_multi_formula_query_asks_their_conjunction() {
        let runner = Arc::new(Canned {
            transcript: THEOREM,
            seen: Mutex::new(Vec::new()),
        });
        let kb = kb_with(runner.clone());
        let res = kb.ask_query_dialect(
            "(instance Rex Animal)\n(subclass Dog Animal)",
            &ExternalOpts::default(),
            Parser::Kif { options: None },
        );
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
        let seen = runner.seen.lock().unwrap();
        let conjecture = seen[0]
            .lines()
            .find(|l| l.contains("conjecture"))
            .expect("a conjecture line");
        assert!(
            conjecture.contains("s__Rex") && conjecture.contains("s__Dog"),
            "both formulas in the conjecture: {conjecture}"
        );
    }

    #[test]
    fn a_focused_audit_sends_only_the_neighborhood() {
        let runner = Arc::new(Canned {
            transcript: "% SZS status Satisfiable for input\n",
            seen: Mutex::new(Vec::new()),
        });
        let mut kb = kb_with(runner.clone());
        let r = kb.reload_kif(
            "(instance Tweety Bird)\n",
            &std::path::PathBuf::from("birds.kif"),
            "birds.kif",
        );
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("birds.kif").expect("promote");

        let focus = kb.file_roots("test.kif");
        let rex: Vec<_> = focus
            .into_iter()
            .filter(|sid| kb.sentence_kif_str(*sid).contains("Rex"))
            .collect();
        // A fixed tolerance: the default auto-budget exceeds this tiny KB and
        // would select all of it.
        let opts = ExternalOpts {
            selection: crate::SineParams::strict(),
            ..ExternalOpts::default()
        };
        let res = kb.audit_consistency(&rex, &opts, 1);
        assert_eq!(
            res.status,
            ProverStatus::Consistent,
            "raw: {}",
            res.raw_output
        );

        let seen = runner.seen.lock().unwrap();
        let tptp = &seen[0];
        assert!(tptp.contains("s__Rex"), "focus missing in:\n{tptp}");
        assert!(
            !tptp.contains("s__Tweety"),
            "an unrelated axiom leaked into the neighborhood:\n{tptp}"
        );
    }

    #[test]
    fn a_custom_runner_saturation_verdict_is_classified_like_a_subprocess_one() {
        let runner = Arc::new(Canned {
            transcript: "% SZS status CounterSatisfiable for input\n\
% Termination reason: Satisfiable\n",
            seen: Mutex::new(Vec::new()),
        });
        let kb = kb_with(runner.clone());
        let res = kb.ask(query("(instance Rex Animal)"), &ExternalOpts::default());
        // A saturated verdict on a three-axiom KB has nowhere to widen to,
        // so the loop's classification is what reaches the caller.
        assert_eq!(
            res.status,
            ProverStatus::Disproved,
            "raw: {}",
            res.raw_output
        );
        assert_eq!(res.termination, Some(TerminationReason::Saturation));
        assert_eq!(runner.seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn thf_mode_hands_the_runner_a_higher_order_problem() {
        let runner = Arc::new(Canned {
            transcript: THEOREM,
            seen: Mutex::new(Vec::new()),
        });
        let kb = kb_with(runner.clone());
        let opts = ExternalOpts {
            mode: crate::TptpLang::Thf,
            ..ExternalOpts::default()
        };
        let res = kb.ask(query("(instance Rex Animal)"), &opts);
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
        let seen = runner.seen.lock().unwrap();
        let tptp = &seen[0];
        assert!(tptp.contains("thf("), "expected a THF problem:\n{tptp}");
        assert!(
            !tptp.contains("fof("),
            "first-order framing leaked into THF:\n{tptp}"
        );
    }

    /// A runner that consumes the structured problem, recording which
    /// representation the driver handed it.
    struct Structured(Mutex<Vec<&'static str>>);

    impl ProverRunner for Structured {
        fn prove(&self, _tptp: &str, _opts: &ProverOpts) -> ProverResult {
            unreachable!("the driver must call prove_ir, not the text path")
        }
        fn prove_ir(
            &self,
            problem: &crate::trans::ir::ProblemIr,
            _sid_map: &[crate::SentenceId],
            _conjecture_name: &str,
            opts: &ProverOpts,
        ) -> ProverResult {
            self.0.lock().unwrap().push(match problem {
                crate::trans::ir::ProblemIr::Fo(_) => "fo",
                crate::trans::ir::ProblemIr::Ho(_) => "ho",
            });
            result_from_transcript(THEOREM, "", opts.mode, std::time::Duration::ZERO)
        }
    }

    #[cfg(feature = "snapshot")]
    #[test]
    fn restore_bytes_keeps_the_configured_runner_and_cache_config() {
        let runner = Arc::new(Structured(Mutex::new(Vec::new())));
        let mut kb = KnowledgeBase::new_external(Prover::Custom(runner.clone()));
        let r = kb.reload_kif(
            "(subclass Dog Mammal)\n(instance Rex Dog)\n",
            &std::path::PathBuf::from("test.kif"),
            "test.kif",
        );
        assert!(r.ok);
        kb.make_session_axiomatic("test.kif").expect("promote");
        kb.cache_config().set_max_threads(5);

        let bytes = kb.snapshot_bytes().expect("snapshot");
        kb.restore_bytes(&bytes).expect("restore");
        assert_eq!(kb.cache_config().max_threads(), 5);
        assert!(kb.symbol_id("Rex").is_some(), "contents thawed");

        runner.0.lock().unwrap().clear();
        let res = kb.ask(query("(instance Rex Mammal)"), &ExternalOpts::default());
        assert_eq!(res.status, ProverStatus::Proved, "{}", res.raw_output);
        assert_eq!(
            *runner.0.lock().unwrap(),
            ["fo"],
            "the configured runner, not the default one, answered"
        );
    }

    #[test]
    fn the_driver_hands_the_runner_the_representation_mode_selects() {
        let runner = Arc::new(Structured(Mutex::new(Vec::new())));
        let mut kb = KnowledgeBase::new_external(Prover::Custom(runner.clone()));
        let r = kb.reload_kif(
            "(subclass Dog Mammal)\n(instance Rex Dog)\n",
            &std::path::PathBuf::from("test.kif"),
            "test.kif",
        );
        assert!(r.ok);
        kb.make_session_axiomatic("test.kif").expect("promote");
        for (mode, expect) in [
            (crate::TptpLang::Auto, "fo"),
            (crate::TptpLang::Tff, "fo"),
            (crate::TptpLang::Thf, "ho"),
        ] {
            runner.0.lock().unwrap().clear();
            let opts = ExternalOpts {
                mode,
                ..ExternalOpts::default()
            };
            let res = kb.ask(query("(instance Rex Mammal)"), &opts);
            assert_eq!(
                res.status,
                ProverStatus::Proved,
                "{mode:?}: {}",
                res.raw_output
            );
            assert_eq!(runner.0.lock().unwrap().as_slice(), [expect], "{mode:?}");
        }
    }

    #[test]
    fn custom_debug_prints_the_runner_name() {
        let p = Prover::Custom(Arc::new(Canned {
            transcript: "",
            seen: Mutex::new(Vec::new()),
        }));
        assert_eq!(format!("{p:?}"), "Custom(\"canned\")");
    }
}

#[cfg(all(test, feature = "external-prover", feature = "native-prover"))]
mod nested_stack_tests {
    use std::sync::{Arc, Mutex};

    use super::KnowledgeBase;
    use crate::prover::vampire_proof::result_from_transcript;
    use crate::prover::{
        ExternalOpts, ExternalProverLayer, Prover, ProverLayer, ProverOpts, ProverResult,
        ProverRunner, ProverStatus,
    };
    use crate::{NativeOpts, Parser, TranslationLayer};

    struct Canned(Mutex<Vec<String>>);

    impl ProverRunner for Canned {
        fn prove(&self, tptp: &str, opts: &ProverOpts) -> ProverResult {
            self.0.lock().unwrap().push(tptp.to_string());
            result_from_transcript(
                "% SZS status Theorem for input\n\
% SZS output start Proof for input\n\
fof(f1, axiom, p, file('/dev/stdin', kb_1)).\n\
fof(f2, conjecture, p, file('/dev/stdin', query_0)).\n\
fof(f3, negated_conjecture, ~p, inference(negated_conjecture, [], [f2])).\n\
fof(f4, plain, $false, inference(resolution, [], [f1, f3])).\n\
% SZS output end Proof for input\n",
                "",
                opts.mode,
                std::time::Duration::ZERO,
            )
        }
    }

    type Both = KnowledgeBase<ExternalProverLayer<ProverLayer<TranslationLayer>>>;

    fn kb_both(runner: Arc<Canned>) -> Both {
        let mut kb = KnowledgeBase::new_external_native(Prover::Custom(runner));
        let r = kb.reload_kif(
            "(subclass Dog Mammal)\n(subclass Mammal Animal)\n(instance Rex Dog)\n",
            &std::path::PathBuf::from("test.kif"),
            "test.kif",
        );
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("test.kif").expect("promote");
        kb
    }

    fn fast() -> NativeOpts {
        NativeOpts {
            time_limit_secs: 10,
            ..Default::default()
        }
    }

    #[test]
    fn the_top_layer_asks_the_external_runner_and_the_nested_native_prover_still_proves() {
        let runner = Arc::new(Canned(Mutex::new(Vec::new())));
        let kb = kb_both(runner.clone());

        let ext = kb.ask_query_dialect(
            "(instance Rex Animal)",
            &ExternalOpts::default(),
            Parser::Kif { options: None },
        );
        assert_eq!(ext.status, ProverStatus::Proved, "raw: {}", ext.raw_output);
        assert_eq!(runner.0.lock().unwrap().len(), 1);

        let nat = kb.ask_query_dialect_native(
            "(instance Rex Animal)",
            &fast(),
            Parser::Kif { options: None },
        );
        assert_eq!(nat.status, ProverStatus::Proved, "raw: {}", nat.raw_output);
        // The native run never touched the external runner.
        assert_eq!(runner.0.lock().unwrap().len(), 1);

        let nat = kb.ask_query_dialect_native(
            "(instance Rex Plant)",
            &fast(),
            Parser::Kif { options: None },
        );
        assert_ne!(nat.status, ProverStatus::Proved, "raw: {}", nat.raw_output);
    }

    #[test]
    fn session_assertions_staged_through_the_external_top_reach_the_native_prover() {
        let runner = Arc::new(Canned(Mutex::new(Vec::new())));
        let mut kb = kb_both(runner);
        // A tell goes through the KB root (the external layer) and must
        // cascade into the native layer's caches beneath it.
        let t = kb.tell("(instance Fido Dog)", "s1");
        assert!(t.ok, "tell failed: {:?}", t.diagnostics);
        let nat = kb.ask_query_dialect_native(
            "(instance Fido Animal)",
            &NativeOpts {
                session: Some("s1".into()),
                ..fast()
            },
            Parser::Kif { options: None },
        );
        assert_eq!(nat.status, ProverStatus::Proved, "raw: {}", nat.raw_output);
    }

    #[test]
    fn the_native_audit_runs_on_the_nested_prover() {
        let runner = Arc::new(Canned(Mutex::new(Vec::new())));
        let kb = kb_both(runner.clone());
        let res = kb.audit_with(kb.native(), &[], &fast(), 1);
        assert_eq!(
            res.status,
            ProverStatus::Consistent,
            "raw: {}",
            res.raw_output
        );
        assert!(runner.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_malformed_external_query_is_an_input_error() {
        let kb = kb_both(Arc::new(Canned(Mutex::new(Vec::new()))));
        let res = kb.ask_query_dialect(
            "(instance Rex",
            &ExternalOpts::default(),
            Parser::Kif { options: None },
        );
        assert_eq!(res.status, ProverStatus::InputError);
    }
}
