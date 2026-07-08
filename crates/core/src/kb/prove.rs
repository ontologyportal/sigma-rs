//! Theorem-proving entrypoints on `KnowledgeBase`.

#![cfg(feature = "ask")]

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::prover::{CommonProverOpts, ProverResult, ProverStatus, ProvingLayer};
use crate::types::{SentenceId, SourceFile};
use crate::layer::{Layer, TopLayer};
use crate::{Parser, SineParams, TestCase};

use super::KnowledgeBase;

impl<L: ProvingLayer + TopLayer + Layer> KnowledgeBase<L> {
    /// Discharge a test-case query against the KB.
    ///
    /// `session` is an optional in-memory session whose assertions become
    /// hypotheses.  `opts` carries the layer's proving parameters.
    pub fn ask(
        &self,
        tc:          TestCase,
        session:     Option<&str>,
        opts:        &L::Opts,
    ) -> ProverResult {
        with_guard!(self);
        self.debug(format!("ask: query={}", tc.query_kif().unwrap_or_default()));

        let session = session.map_or_else(|| format!("{:x}", 
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ), |s| s.to_string());

        let Some(query) = tc.query else { return ProverResult::default() };
        // Hypothesis-staging failures must not stay silent: a hypothesis
        // that never reached the session could be the one that makes the set
        // unsatisfiable, so its loss poisons any confident
        // Disproved/Satisfiable verdict (withheld after the prove below).
        // Assembly losses recorded by the parser (`unaccounted_inputs`)
        // ride the same gate.
        let mut input_failures = tc.unaccounted_inputs;
        if !tc.axioms.is_empty() {
            let p = tc.file_name.clone().into();
            let outcome = self.ingest_source(SourceFile {
                parser: Parser::Kif,
                name: tc.file_name,
                path: p,
                origin: crate::FileOrigin::Local,
                contents: String::new(),
                prebuilt: Some(tc.axioms)
            }, &session, true);
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
        let mut result = self.layer.prove(vec![query], &opts, &ctx);
        // Input-completeness gate: staged-hypothesis / assembly losses make
        // a confident "no" (Disproved/Satisfiable) unsound — demote it to
        // Unknown/GaveUp with a loud reason.  Proved verdicts stand.
        result.withhold_countermodel(
            input_failures, "hypothesis staging / test-case assembly");

        // Roll back any session-scoped axioms staged for this ask.
        profile_call!(self, "ask.rollback", {
            let _ = self.ingest_source(SourceFile::truncate(PathBuf::from(query_tag)), &session, true);
        });

        result
    }

    /// Saturate the base (plus optional session support) for up to `limit`
    /// distinct contradictions over `focus`'s neighborhood (empty ⇒ whole base).
    /// Selection / session ride in on `opts` — the layer's consolidated params
    /// struct.
    pub fn audit_consistency(
        &self,
        focus:   &[SentenceId],
        opts:    L::Opts,
        limit:   usize,
    ) -> ProverResult {
        self.layer.audit_consistency(focus, &opts, limit, &self.prove_ctx())
    }

    /// Single-contradiction satisfiability check (`limit = 1`) over the whole
    /// base plus optional session support carried by `opts`.
    pub fn check_satisfiable(&self, opts: L::Opts) -> ProverResult {
        self.audit_consistency(&[], opts, 1)
    }
}

#[cfg(feature = "native-prover")]
impl KnowledgeBase<crate::prover::saturate::ProverLayer> {
    /// Ask the native saturation prover to discharge `query_kif` (a single KIF
    /// conjecture) under SInE selection `sine` and optional in-memory `session`
    /// support.  Convenience wrapper: parses the query, folds `sine` / `session`
    /// into the consolidated [`NativeOpts`](crate::NativeOpts), and runs the
    /// `&self` native prove driver.
    pub fn ask_query(
        &self,
        query_kif: &str,
        session:   Option<&str>,
        sine:      SineParams,
        mut opts:  crate::NativeOpts,
    ) -> ProverResult {
        opts.selection = sine;
        opts.session   = session.map(|s| s.to_string());
        let doc = crate::parse_document("ask_query", query_kif.to_string(), Parser::Kif);
        // A malformed query is an input error, not an unprovable goal — and it
        // must leave no residue (the parse never reached the atom table).
        if doc.has_errors() {
            return ProverResult {
                status:     ProverStatus::InputError,
                raw_output: format!("query parse error ({} diagnostic(s))", doc.parse_errors.len()),
                ..Default::default()
            };
        }
        let asts: Vec<crate::AstNode> =
            doc.ast.into_iter()
                .filter_map(|d| d.as_stmt().cloned())
                .collect();
        self.layer.prove_native(asts, opts, &self.prove_ctx())
    }
}
