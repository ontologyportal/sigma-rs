//! Theorem-proving entrypoints on `KnowledgeBase`.
//!
//! Gated on `any(ask, native-prover)`: the generic `ask` / `audit_consistency`
//! entry points are backend-agnostic (any `ProvingLayer`), so they must be
//! reachable by the native saturation prover WITHOUT the subprocess `ask`
//! feature — the native prover is the only proving backend that runs on wasm32.

#![cfg(any(feature = "external-prover", feature = "native-prover"))]

use std::path::PathBuf;

use crate::layer::{Layer, TopLayer};
use crate::prover::{CommonProverOpts, ProverResult, ProverStatus, ProvingLayer};
use crate::types::{SentenceId, SourceFile};
#[cfg(feature = "native-prover")]
use crate::SineParams;
use crate::{Parser, TestCase};

use super::KnowledgeBase;

impl<L: ProvingLayer + TopLayer + Layer> KnowledgeBase<L> {
    /// Discharge a test-case query against the KB with the top layer's prover.
    ///
    /// `session` is an optional in-memory session whose assertions become
    /// hypotheses.  `opts` carries the layer's proving parameters.
    pub fn ask(&self, tc: TestCase, session: Option<&str>, opts: &L::Opts) -> ProverResult {
        self.ask_with(&self.layer, tc, session, opts)
    }

    /// Saturate the base (plus optional session support) for up to `limit`
    /// distinct contradictions over `focus`'s neighborhood (empty => whole base).
    /// Selection / session ride in on `opts` -- the layer's consolidated params
    /// struct.
    pub fn audit_consistency(
        &self,
        focus: &[SentenceId],
        opts: L::Opts,
        limit: usize,
    ) -> ProverResult {
        self.audit_with(&self.layer, focus, opts, limit)
    }

    /// Single-contradiction satisfiability check (`limit = 1`) over the whole
    /// base plus optional session support carried by `opts`.
    pub fn check_satisfiable(&self, opts: L::Opts) -> ProverResult {
        self.audit_consistency(&[], opts, 1)
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
        session: Option<&str>,
        opts: &P::Opts,
    ) -> ProverResult {
        with_guard!(self);
        self.debug(format!("ask: query={}", tc.query_kif().unwrap_or_default()));

        let session = session.map_or_else(
            || format!("{:x}", crate::clock::epoch_nanos()),
            |s| s.to_string(),
        );

        let Some(query) = tc.query else {
            return ProverResult::default();
        };
        // Hypothesis-staging failures must not stay silent: a hypothesis
        // that never reached the session could be the one that makes the set
        // unsatisfiable, so its loss poisons any confident
        // Disproved/Satisfiable verdict (withheld after the prove below).
        // Assembly losses recorded by the parser (`unaccounted_inputs`)
        // ride the same gate.
        let mut input_failures = tc.unaccounted_inputs;
        // Mirror of the staged source below, kept for the rollback: the
        // source cache reconciles per source name, so the truncate must
        // target EXACTLY the source the hypotheses were ingested under —
        // truncating anything else removes nothing and the hypotheses
        // leak into the store for every later ask.
        let staged: Option<SourceFile> = (!tc.axioms.is_empty()).then(|| SourceFile {
            parser: Parser::Kif { options: None },
            name: tc.file_name.clone(),
            path: tc.file_name.clone().into(),
            origin: crate::FileOrigin::Local(crate::types::LocalProvenance::UNKNOWN),
            contents: String::new(),
            prebuilt: None,
        });
        if !tc.axioms.is_empty() {
            let p = tc.file_name.clone().into();
            let outcome = self.ingest_source(
                SourceFile {
                    parser: Parser::Kif { options: None },
                    name: tc.file_name,
                    path: p,
                    origin: crate::FileOrigin::Local(crate::types::LocalProvenance::UNKNOWN),
                    contents: String::new(),
                    prebuilt: Some(tc.axioms),
                },
                &session,
                true,
            );
            input_failures += outcome.errors.len();
        }

        let query_tag = crate::kb::session_tags::SESSION_QUERY;

        // Scope the prover to the same session the support was staged under, so
        // the engine force-includes those hypotheses.  `session` is the single
        // source of truth (a caller-set `opts.session` would otherwise diverge
        // from where `tc.axioms` just landed).
        let opts = {
            let mut o = opts.clone();
            o.set_session(Some(session.clone()));
            o
        };

        // The layer's `prove` warms up, prepares the conjecture (its own
        // intern + rollback via `cleanup`), runs the shared scaling loop, and
        // returns.  `ProveCtx` carries this KB's progress sink down to it.
        let ctx = self.prove_ctx();
        let mut result = prover.prove(vec![query], &opts, &ctx);
        // Input-completeness gate: staged-hypothesis / assembly losses make
        // a confident "no" (Disproved/Satisfiable) unsound — demote it to
        // Unknown/GaveUp with a loud reason.  Proved verdicts stand.
        result.withhold_countermodel(input_failures, "hypothesis staging / test-case assembly");

        // Roll back any session-scoped axioms staged for this ask.  The
        // '__query__' truncate covers the external layer's conjecture tag
        // (usually already cleaned by its own `cleanup` — harmless no-op);
        // the staged HYPOTHESES live under `tc.file_name` and need their
        // own truncate, or they persist in the store and feed every later
        // ask's session support and every whole-store scan.
        profile_call!(self, "ask.rollback", {
            let _ = self.ingest_source(
                SourceFile::truncate(PathBuf::from(query_tag)),
                &session,
                true,
            );
            if let Some(src) = staged {
                let _ = self.ingest_source(src, &session, true);
            }
        });

        result
    }

    /// [`audit_consistency`](KnowledgeBase::audit_consistency) with `prover`
    /// instead of the top layer.
    pub fn audit_with<P: ProvingLayer>(
        &self,
        prover: &P,
        focus: &[SentenceId],
        opts: P::Opts,
        limit: usize,
    ) -> ProverResult {
        prover.audit_consistency(focus, &opts, limit, &self.prove_ctx())
    }
}

/// Parse a query in `dialect` into the bare conjecture formulas a prover
/// takes.  `Err` carries the `InputError` result for a malformed query,
/// which must leave no residue (the parse never reached any store).
fn parse_query(query: &str, dialect: Parser) -> Result<Vec<crate::AstNode>, Box<ProverResult>> {
    // A query is never an axiom-library ingest, so a TPTP-framed query
    // must keep `conjecture`-role formulas
    let dialect = match dialect {
        Parser::Tptp { options } => {
            let mut options = options.unwrap_or_default();
            options.keep_conjectures = true;
            Parser::Tptp {
                options: Some(options),
            }
        }
        other => other,
    };
    let doc = crate::parse_document("ask_query", query.to_string(), dialect);
    if doc.has_errors() {
        return Err(Box::new(ProverResult {
            status: ProverStatus::InputError,
            raw_output: format!(
                "query parse error ({} diagnostic(s))",
                doc.parse_errors.len()
            ),
            ..Default::default()
        }));
    }
    // `as_stmt()` keeps a TPTP `fof(name, role, formula)`'s `Annotated`
    // framing; the provers (like every KIF-parsed query, which never
    // carries that framing) want the bare formula.
    Ok(doc
        .ast
        .into_iter()
        .filter_map(|d| d.as_stmt().map(|n| n.formula().clone()))
        .collect())
}

/// The native prover's query entry point, shared by the bare native stack
/// and the native layer nested under an external one.
#[cfg(feature = "native-prover")]
fn native_ask_query_dialect<S: TopLayer + 'static>(
    layer: &crate::prover::saturate::ProverLayer<S>,
    ctx: &crate::ProveCtx,
    query: &str,
    session: Option<&str>,
    sine: SineParams,
    mut opts: crate::NativeOpts,
    dialect: Parser,
) -> ProverResult {
    opts.selection = sine;
    opts.session = session.map(|s| s.to_string());
    match parse_query(query, dialect) {
        Ok(asts) => layer.prove_native(asts, opts, ctx),
        Err(r) => *r,
    }
}

#[cfg(feature = "external-prover")]
impl<T: crate::trans::HasTranslation + 'static>
    KnowledgeBase<crate::prover::ExternalProverLayer<T>>
{
    /// Ask the external prover to discharge `query` (a single conjecture in
    /// `dialect`) under `opts`, with optional in-memory `session` support --
    /// the external counterpart of the native
    /// [`ask_query_dialect`](KnowledgeBase::ask_query_dialect).
    pub fn ask_query_dialect(
        &self,
        query: &str,
        session: Option<&str>,
        opts: &crate::ExternalOpts,
        dialect: Parser,
    ) -> ProverResult {
        let ast = match parse_query(query, dialect) {
            Ok(mut asts) if !asts.is_empty() => asts.swap_remove(0),
            Ok(_) => {
                return ProverResult {
                    status: ProverStatus::InputError,
                    raw_output: "query parsed to no formula".into(),
                    ..Default::default()
                }
            }
            Err(r) => return *r,
        };
        self.ask(TestCase::conjecture("ask_query", ast), session, opts)
    }
}

#[cfg(all(feature = "external-prover", feature = "native-prover"))]
impl<S: crate::trans::HasTranslation + 'static>
    KnowledgeBase<crate::prover::ExternalProverLayer<crate::prover::saturate::ProverLayer<S>>>
{
    /// The native prover nested under this KB's external layer.
    fn native(&self) -> &crate::prover::saturate::ProverLayer<S> {
        self.layer.inner_layer()
    }

    /// [`ask_query_dialect`](KnowledgeBase::ask_query_dialect) on the nested
    /// native prover instead of the external backend.
    pub fn ask_query_dialect_native(
        &self,
        query: &str,
        session: Option<&str>,
        sine: SineParams,
        opts: crate::NativeOpts,
        dialect: Parser,
    ) -> ProverResult {
        native_ask_query_dialect(
            self.native(),
            &self.prove_ctx(),
            query,
            session,
            sine,
            opts,
            dialect,
        )
    }

    /// [`audit_consistency`](KnowledgeBase::audit_consistency) on the nested
    /// native prover instead of the external backend.
    pub fn audit_consistency_native(
        &self,
        focus: &[SentenceId],
        opts: crate::NativeOpts,
        limit: usize,
    ) -> ProverResult {
        self.audit_with(self.native(), focus, opts, limit)
    }
}

#[cfg(feature = "native-prover")]
impl<S: TopLayer + 'static> KnowledgeBase<crate::prover::saturate::ProverLayer<S>> {
    /// Ask the native saturation prover to discharge `query_kif` (a single KIF
    /// conjecture) under SInE selection `sine` and optional in-memory `session`
    /// support.  Convenience wrapper: parses the query, folds `sine` / `session`
    /// into the consolidated [`NativeOpts`](crate::NativeOpts), and runs the
    /// `&self` native prove driver.
    pub fn ask_query(
        &self,
        query_kif: &str,
        session: Option<&str>,
        sine: SineParams,
        opts: crate::NativeOpts,
    ) -> ProverResult {
        self.ask_query_dialect(
            query_kif,
            session,
            sine,
            opts,
            Parser::Kif { options: None },
        )
    }

    /// Same as [`ask_query`](Self::ask_query) but parses `query` in `dialect`
    /// instead of always assuming SUO-KIF
    pub fn ask_query_dialect(
        &self,
        query: &str,
        session: Option<&str>,
        sine: SineParams,
        opts: crate::NativeOpts,
        dialect: Parser,
    ) -> ProverResult {
        native_ask_query_dialect(
            &self.layer,
            &self.prove_ctx(),
            query,
            session,
            sine,
            opts,
            dialect,
        )
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

    #[test]
    fn ask_query_dialect_proves_a_tptp_conjecture() {
        let kb = kb_native("(subclass Dog Mammal)\n(subclass Mammal Animal)\n");
        let res = kb.ask_query_dialect(
            "fof(g, conjecture, subclass('Dog', 'Animal')).",
            None,
            SineParams::default(),
            fast(),
            Parser::Tptp { options: None },
        );
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }

    #[test]
    fn ask_query_dialect_reports_input_error_for_malformed_tptp() {
        let kb = kb_native("(subclass Dog Mammal)\n");
        let res = kb.ask_query_dialect(
            "fof(g, conjecture, subclass('Dog'", // missing close
            None,
            SineParams::default(),
            fast(),
            Parser::Tptp { options: None },
        );
        assert_eq!(res.status, ProverStatus::InputError);
    }

    #[test]
    fn ask_query_still_defaults_to_kif() {
        let kb = kb_native("(subclass Dog Mammal)\n");
        let res = kb.ask_query("(subclass Dog Mammal)", None, SineParams::default(), fast());
        assert_eq!(res.status, ProverStatus::Proved, "raw: {}", res.raw_output);
    }
}

#[cfg(all(test, feature = "external-prover"))]
mod custom_runner_tests {
    use std::sync::{Arc, Mutex};

    use super::KnowledgeBase;
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
        let res = kb.ask(
            query("(instance Rex Animal)"),
            None,
            &ExternalOpts::default(),
        );
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

    #[test]
    fn a_custom_runner_saturation_verdict_is_classified_like_a_subprocess_one() {
        let runner = Arc::new(Canned {
            transcript: "% SZS status CounterSatisfiable for input\n\
% Termination reason: Satisfiable\n",
            seen: Mutex::new(Vec::new()),
        });
        let kb = kb_with(runner.clone());
        let res = kb.ask(
            query("(instance Rex Animal)"),
            None,
            &ExternalOpts::default(),
        );
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
    use crate::{NativeOpts, Parser, SineParams, TranslationLayer};

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
            None,
            &ExternalOpts::default(),
            Parser::Kif { options: None },
        );
        assert_eq!(ext.status, ProverStatus::Proved, "raw: {}", ext.raw_output);
        assert_eq!(runner.0.lock().unwrap().len(), 1);

        let nat = kb.ask_query_dialect_native(
            "(instance Rex Animal)",
            None,
            SineParams::default(),
            fast(),
            Parser::Kif { options: None },
        );
        assert_eq!(nat.status, ProverStatus::Proved, "raw: {}", nat.raw_output);
        // The native run never touched the external runner.
        assert_eq!(runner.0.lock().unwrap().len(), 1);

        let nat = kb.ask_query_dialect_native(
            "(instance Rex Plant)",
            None,
            SineParams::default(),
            fast(),
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
            Some("s1"),
            SineParams::default(),
            fast(),
            Parser::Kif { options: None },
        );
        assert_eq!(nat.status, ProverStatus::Proved, "raw: {}", nat.raw_output);
    }

    #[test]
    fn the_native_audit_runs_on_the_nested_prover() {
        let runner = Arc::new(Canned(Mutex::new(Vec::new())));
        let kb = kb_both(runner.clone());
        let res = kb.audit_consistency_native(&[], fast(), 1);
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
            None,
            &ExternalOpts::default(),
            Parser::Kif { options: None },
        );
        assert_eq!(res.status, ProverStatus::InputError);
    }
}
