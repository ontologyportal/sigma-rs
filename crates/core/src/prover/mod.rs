//! Prover subsystem: the external provers (and their backends) and the native
//! prover (`saturate`).

#[cfg(any(feature = "ask", feature = "native-prover"))]
use crate::SineParams;
#[cfg(any(feature = "ask", feature = "native-prover"))]
use crate::layer::TopLayer;

pub mod external;
#[cfg(feature = "native-prover")]
pub mod saturate;

pub mod result;

pub mod proof;
pub(crate) use proof::tstp as tptp_proof;
pub mod axiom_source;

pub(crate) mod scale;

pub use result::*;
#[cfg(feature = "native-prover")]
pub use saturate::ProverLayer;
pub use external::{ExternalProverLayer, ExternalOpts};
pub use external::backends::{Prover, ProverRunner, ProverOpts, ProverMode};

/// A [`TopLayer`] that can discharge a proof obligation — the single seam
/// behind `KnowledgeBase::ask` that unifies the native (`ProverLayer`) and
/// external (`ExternalProverLayer`) backends.
///
/// The shared, backend-agnostic autoscaling loop lives here as the default
/// [`prove`](ProvingLayer::prove) method (it drives [`crate::prover::scale::drive`]
/// over [`prove_once`](ProvingLayer::prove_once)).  Each backend supplies only:
///
///  * [`prepare`](ProvingLayer::prepare) — turn the conjecture AST into the
///    form its engine consumes (native: detached atoms; external: store-interned
///    query sids + a rollback guard), once per ask;
///  * [`prove_once`](ProvingLayer::prove_once) — one select → build → run at a
///    fixed budget + time slice;
///  * [`check_consistency`](ProvingLayer::check_consistency).
///
/// The whole trait is `&self`: every backend's mutation goes through interior
/// mutability, so `prove` is read-only at the `&self` level and one loaded KB
/// can be shared across threads. [`ProveCtx`](crate::ProveCtx) carries the
/// progress/log sink.
#[cfg(any(feature = "ask", feature = "native-prover"))]
#[allow(dead_code)]
pub trait ProvingLayer: TopLayer {
    /// Backend-specific prover options (native: `NativeOpts`; external:
    /// `ProverOpts`).
    type Opts: Default + Clone + CommonProverOpts;

    /// One-time warm-up run at the top of [`prove`](ProvingLayer::prove)
    /// (rewrite pass, predicate-variable schema detection).  Idempotent.
    /// Default no-op (the native layer needs none).
    fn warm_up(&self) {}

    /// Intern the normalized conjecture where this backend's engine resolves it
    /// — native into its prover-local atom table, external into the shared
    /// store under a query tag — returning the conjecture roots as
    /// `(sentence, content-hash id)`.  The only per-backend step of
    /// [`prepare`](ProvingLayer::prepare).
    fn intern_conjecture(&self, asts: &[crate::AstNode])
        -> Vec<(std::sync::Arc<crate::types::Sentence>, crate::SentenceId)>;

    /// Normalize the conjecture, collect its SInE seed, and intern it (via
    /// [`intern_conjecture`](ProvingLayer::intern_conjecture)).  Shared across
    /// backends; runs once per ask.  `Err` short-circuits the whole proof
    /// (e.g. a parse error / empty query) with the carried result.
    fn prepare(&self, conjecture: Vec<crate::AstNode>)
        -> Result<Conjecture, result::ProverResult>
    {
        let normalized = Conjecture::normalize(conjecture);
        let seed_syms  = Conjecture::seed(&normalized);
        let sents      = self.intern_conjecture(&normalized);
        if sents.is_empty() {
            return Err(result::ProverResult {
                status:     ProverStatus::Unknown,
                raw_output: "No query sentence parsed".into(),
                ..Default::default()
            });
        }
        Ok(Conjecture { sents, seed_syms })
    }

    /// Undo anything [`prepare`](ProvingLayer::prepare) staged, after the proof
    /// completes.  Default no-op (the native layer interns into its atom table
    /// — nothing in the shared store to undo); the external layer truncates the
    /// conjecture's query tag here.
    fn cleanup(&self, _conj: Conjecture) {}

    /// One fixed-selection attempt at `params` (its `auto_budget` is the
    /// budget) with a `slice`-second per-run timeout (`0` = unbounded).
    /// The cross-iteration inputs (session, TPTP mode) live on `opts`.  Returns
    /// the result and the raw SInE selection size.
    fn prove_once(
        &self,
        conj:     &Conjecture,
        params:   SineParams,
        slice:    u32,
        opts:     &Self::Opts,
        ctx:      &crate::ProveCtx,
    ) -> (result::ProverResult, usize);

    /// Adjust a result's termination reason before the planner classifies it.
    /// Identity by default; the native layer maps step-exhaustion `GaveUp`.
    fn remap(
        _status: ProverStatus,
        term:    Option<TerminationReason>,
    ) -> Option<TerminationReason> {
        term
    }


    /// Prove `conjecture` under `opts`.  Backend-agnostic: warm up, prepare
    /// once, then run either a single shot (fixed scope / `--no-autoscale`) or
    /// the shared prover-feedback autoscaling loop.  The selection seed and
    /// total budget come off `opts` via [`CommonProverOpts`].  **Not meant to be
    /// overridden.**
    fn prove(
        &self,
        conjecture: Vec<crate::AstNode>,
        opts:       &Self::Opts,
        ctx:        &crate::ProveCtx,
    ) -> result::ProverResult {
        self.warm_up();
        let prepared = match self.prepare(conjecture) {
            Ok(p)  => p,
            Err(r) => return r,
        };
        let selection    = opts.selection();
        let total_timeout = opts.timeout().min(u64::from(u32::MAX)) as u32;
        let result = if !selection.autoscaling() {
            self.prove_once(&prepared, selection, total_timeout, opts, ctx).0
        } else {
            use crate::prover::scale::{drive, ScaleConfig};
            use crate::syntactic::sine::{
                scale_factor, scale_max_disproofs, scale_max_time_runs, scale_min_budget,
            };
            let cfg = ScaleConfig {
                factor:        scale_factor(),
                max_disproofs: scale_max_disproofs(),
                max_time_runs: scale_max_time_runs(),
                min_budget:    scale_min_budget(),
                total_timeout,
            };
            drive(selection, cfg, Self::remap, |params, slice| {
                self.prove_once(&prepared, params, slice, opts, ctx)
            })
        };
        self.cleanup(prepared);
        result
    }

    /// Saturate the (selected) axiom base looking for a contradiction — no
    /// conjecture attached.  Selection / session come off `opts`.
    fn check_consistency(&self, opts: &Self::Opts, ctx: &crate::ProveCtx)
        -> result::ProverResult;

    /// Saturate the selected base (plus `opts`' session support) for up to
    /// `limit` distinct contradictions over `focus` (empty ⇒ whole base).
    /// Default: the single-shot [`check_consistency`](Self::check_consistency)
    /// (`focus`/`limit` ignored — the external backend has no enumerator yet);
    /// the native backend overrides with its enumerating driver.
    fn audit_consistency(
        &self,
        _focus: &[crate::SentenceId],
        opts:   &Self::Opts,
        _limit: usize,
        ctx:    &crate::ProveCtx,
    ) -> result::ProverResult {
        self.check_consistency(opts, ctx)
    }
}

/// The cross-backend knobs the autoscaling [`ProvingLayer::prove`] loop reads
/// off whichever concrete `Opts` it is handed.  Each prover layer's single
/// consolidated opts struct (`NativeOpts`, `ExternalOpts`) implements it.  The
/// per-backend `session` and TPTP-`mode` fields are read concretely inside each
/// engine's `prove_once`, so they stay off this shared accessor.
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub trait CommonProverOpts {
    /// The SInE axiom-selection seed the autoscaling loop perturbs per slice.
    fn selection(&self) -> SineParams;
    /// Wall-clock budget in seconds (0 = unlimited).
    fn timeout(&self) -> u64;
    /// Override the wall-clock budget (seconds).  The test runner uses this to
    /// stamp each case's `(time N)` directive onto the opts when the caller
    /// hasn't pinned an explicit `--timeout`.
    fn set_timeout(&mut self, secs: u64);
    /// Scope the prover to `session`'s overlay (its assertions ride in as
    /// force-included hypotheses).  `kb.ask` calls this so the engine reasons in
    /// the same session its conjecture's support was staged under.
    fn set_session(&mut self, session: Option<String>);
}

/// A parsed, normalized conjecture: the interned conjecture roots plus the SInE
/// seed symbols.  Produced once per ask by [`ProvingLayer::prepare`] and reused
/// across every autoscale iteration.
///
/// Backend-neutral: each backend's
/// [`intern_conjecture`](ProvingLayer::intern_conjecture) fills `sents` from
/// wherever its engine resolves the conjecture (native: the prover-local atom
/// table; external: the shared store).  The `SentenceId`s are content hashes,
/// so the same conjecture yields the same ids in either store.
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub struct Conjecture {
    /// The conjecture root sentences: `(sentence, content-hash id)`.
    pub sents:     Vec<(std::sync::Arc<crate::types::Sentence>, crate::SentenceId)>,
    /// The conjecture's concrete symbols — the SInE seed for `select_axioms`.
    pub seed_syms: std::collections::HashSet<crate::SymbolId>,
}

#[cfg(any(feature = "ask", feature = "native-prover"))]
impl Conjecture {
    /// Macro-expand + normalize the raw conjecture ASTs (preserving a leading
    /// `(forall …)` so the refutation negation skolemizes).
    pub(crate) fn normalize(asts: Vec<crate::AstNode>) -> Vec<crate::AstNode> {
        asts.into_iter()
            .flat_map(|n| crate::parse::macros::expand_node_conjecture(n))
            .flat_map(|n| crate::parse::macros::normalize_ast(&n))
            .collect()
    }

    /// The conjecture's concrete symbols — its SInE seed.
    pub(crate) fn seed(normalized: &[crate::AstNode]) -> std::collections::HashSet<crate::SymbolId> {
        let mut out = std::collections::HashSet::new();
        for n in normalized {
            collect_ast_symbols(n, &mut out);
        }
        out
    }
}

/// Collect every concrete symbol mentioned in `node` into `out` — the
/// conjecture's SInE seed.
#[cfg(any(feature = "ask", feature = "native-prover"))]
fn collect_ast_symbols(node: &crate::AstNode, out: &mut std::collections::HashSet<crate::SymbolId>) {
    use crate::parse::AstNode;
    match node {
        AstNode::Symbol { name, .. } => {
            out.insert(crate::types::Symbol::hash_name(name));
        }
        AstNode::List { elements, .. } => {
            for el in elements {
                collect_ast_symbols(el, out);
            }
        }
        _ => {}
    }
}