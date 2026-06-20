// crates/core/src/kb/scale.rs
//
// Pure decision logic for the prover-feedback autoscaling loop driven by
// `KnowledgeBase::run_scaling` (see `kb::prove`).
//
// Splitting the policy out of the loop keeps the I/O-heavy part (select →
// build → prove) separate from the pure state machine, so the scaling
// directions, bracketing/bisection, and give-up conditions can be unit
// tested without a KB or a real prover.
//
// Verdict → action:
//   - Proved / Inconsistent / InputError  → Done (return the result as-is).
//   - Disproved, or Unknown+Saturation/GaveUp (conjecture not entailed by
//     the *selected* subset → likely missing premises) → Widen.
//   - Timeout, or Unknown+ResourceOut (search space too large) → Narrow.
//
// The selected set is monotone in the budget, so once both a Widen and a
// Narrow have fired the sweet spot is bracketed (`lo`/`hi`) and subsequent
// steps bisect between them; before that, Widen doubles and Narrow halves.

use crate::prover::{ProverResult, ProverStatus, TerminationReason};
use crate::syntactic::sine::{default_budget, SineParams};

/// Tunable knobs for the autoscaling loop (sourced from `syntactic::sine`'s
/// `scale_*` `option_env!` helpers, plus the call's total timeout).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScaleConfig {
    /// Budget multiplier per step (widen ×, narrow ÷).  ≥ 2.
    pub factor:         usize,
    /// Give up after this many consecutive Widen verdicts that don't prove.
    pub max_disproofs:  usize,
    /// Number of full-length prover runs the total timeout is split across.
    pub max_time_runs:  usize,
    /// Floor on the budget when narrowing.
    pub min_budget:     usize,
    /// Total wall-clock budget in seconds (0 = unbounded — no time slicing).
    pub total_timeout:  u32,
}

/// What the prover verdict tells the loop to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScaleAct {
    /// Conjecture not entailed by the selected subset → add more axioms.
    Widen,
    /// Prover overwhelmed → remove axioms.
    Narrow,
    /// Definitive verdict (or nothing useful to scale) → stop.
    Done,
}

/// Map a prover `(status, termination)` pair onto a scaling action.
pub(crate) fn classify(status: ProverStatus, term: Option<TerminationReason>) -> ScaleAct {
    use ProverStatus as S;
    use TerminationReason as TR;
    match (status, term) {
        (S::Proved, _) | (S::Inconsistent, _) | (S::InputError, _) => ScaleAct::Done,
        (S::Disproved, _)                       => ScaleAct::Widen,
        (S::Timeout, _)                         => ScaleAct::Narrow,
        (S::Unknown, Some(TR::Saturation))      => ScaleAct::Widen,
        (S::Unknown, Some(TR::GaveUp))          => ScaleAct::Widen,
        (S::Unknown, Some(TR::ResourceOut))     => ScaleAct::Narrow,
        // Truly unknown (no reason / Other), or Consistent in Prove mode:
        // nothing useful to scale on.
        _                                       => ScaleAct::Done,
    }
}

/// Mutable state of the budget search across iterations.
#[derive(Debug)]
pub(crate) struct ScalePlanner {
    cfg:                ScaleConfig,
    budget:             usize,
    /// Largest budget seen to be *under-selected* (a Widen verdict).
    lo:                 Option<usize>,
    /// Smallest budget seen to be *over-selected* (a Narrow verdict).
    hi:                 Option<usize>,
    consecutive_widens: usize,
    time_runs_used:     usize,
    /// Remaining wall-clock budget in seconds (only meaningful when
    /// `cfg.total_timeout > 0`).
    time_left:          i64,
}

impl ScalePlanner {
    pub(crate) fn new(start_budget: usize, cfg: ScaleConfig) -> Self {
        Self {
            budget: start_budget.max(cfg.min_budget),
            time_left: cfg.total_timeout as i64,
            lo: None,
            hi: None,
            consecutive_widens: 0,
            time_runs_used: 0,
            cfg,
        }
    }

    /// The budget to select at for the next prove attempt.
    pub(crate) fn budget(&self) -> usize { self.budget }

    /// Per-run timeout slice (seconds) for the next attempt.  `0` means
    /// unbounded (no total timeout configured).  The remaining time is
    /// divided over the remaining full-length-run budget so a run that
    /// finishes early donates its leftover to later runs.
    pub(crate) fn slice(&self) -> u32 {
        if self.cfg.total_timeout == 0 {
            return 0;
        }
        let runs_left = self.cfg.max_time_runs.saturating_sub(self.time_runs_used).max(1) as i64;
        (self.time_left / runs_left).max(1) as u32
    }

    /// Record an iteration's outcome and advance the search.  Returns `true`
    /// if the loop should continue (with an updated [`Self::budget`]), `false`
    /// if it should stop and return the best result so far.
    ///
    /// `act` must be `Widen` or `Narrow` (the caller returns directly on
    /// `Done`).  `elapsed_secs` is the prover's wall-clock time.
    ///
    /// `reached_ceiling` is `true` when the raw SInE selection was *smaller*
    /// than the requested budget — the fixed point, so widening can add
    /// nothing more.  `reached_floor` is `true` when it was *larger* than the
    /// budget — the strict (tolerance 1.0) floor, so narrowing can shrink it
    /// no further.  Both short-circuit their respective direction to avoid
    /// re-running an identical problem.
    pub(crate) fn step(
        &mut self,
        act:             ScaleAct,
        reached_ceiling: bool,
        reached_floor:   bool,
        elapsed_secs:    i64,
    ) -> bool {
        self.time_left -= elapsed_secs.max(0);
        let timed_out = self.cfg.total_timeout != 0 && self.time_left <= 0;
        match act {
            ScaleAct::Done => false,
            ScaleAct::Widen => {
                self.consecutive_widens += 1;
                if reached_ceiling { return false; }            // can't add more axioms
                if self.consecutive_widens >= self.cfg.max_disproofs { return false; }
                if timed_out { return false; }
                self.lo = Some(self.budget);
                let next = match self.hi {
                    Some(h) => (self.budget + h) / 2,
                    None    => self.budget.saturating_mul(self.cfg.factor),
                };
                if next <= self.budget { return false; }        // bracket collapsed
                self.budget = next;
                true
            }
            ScaleAct::Narrow => {
                self.consecutive_widens = 0;
                self.time_runs_used += 1;
                if reached_floor { return false; }              // can't drop fewer axioms
                if self.budget <= self.cfg.min_budget { return false; }
                if self.time_runs_used >= self.cfg.max_time_runs { return false; }
                if timed_out { return false; }
                self.hi = Some(self.budget);
                let next = match self.lo {
                    Some(l) => (l + self.budget) / 2,
                    None    => (self.budget / self.cfg.factor).max(self.cfg.min_budget),
                };
                if next >= self.budget { return false; }        // bracket collapsed
                self.budget = next;
                true
            }
        }
    }
}

/// The backend-agnostic autoscaling driver shared by the TPTP/Vampire path
/// (`KnowledgeBase::run_scaling`) and the native saturation path
/// (`KnowledgeBase::ask_native_scaled`).
///
/// Owns the whole budget search — the [`ScalePlanner`] state machine, the
/// `default_budget` seed, the ceiling/floor signalling, and the `max_iters`
/// safety cap — so the only thing each backend supplies is *how to run one
/// attempt at a given budget + time slice*.
///
/// - `base` is the caller's [`SineParams`]; each attempt receives a copy with
///   `auto_budget`/`autoscale`/`select_all` overridden for that iteration.
/// - `attempt(params, slice)` performs one select→build→prove at `params` with
///   a `slice`-second per-run timeout (`0` = unbounded), returning the result
///   and the **raw** SInE selection size (before bookkeeping-head filtering),
///   which is the planner's ceiling/floor signal.
/// - `remap` adjusts the result's termination reason *before* classification.
///   The TPTP path passes the identity; the native path maps its
///   step-exhaustion `GaveUp` so the planner doesn't read it as
///   prover-incompleteness and widen (the wrong gradient for a search-space
///   blow-up).
///
/// Returns the first definitive verdict, or — if the search gives up — the
/// best (last) inconclusive result it saw.
pub(crate) fn drive<F>(
    base:    SineParams,
    cfg:     ScaleConfig,
    remap:   impl Fn(ProverStatus, Option<TerminationReason>) -> Option<TerminationReason>,
    mut attempt: F,
) -> ProverResult
where
    F: FnMut(SineParams, u32) -> (ProverResult, usize),
{
    let start = base.auto_budget.unwrap_or_else(default_budget);
    let mut planner = ScalePlanner::new(start, cfg);

    // Hard safety cap on iterations (the planner's give-up conditions
    // normally stop us long before this).
    let max_iters = cfg.max_disproofs + cfg.max_time_runs + 8;
    let mut best: Option<ProverResult> = None;

    for _ in 0..max_iters {
        let budget  = planner.budget();
        let per_run = planner.slice();

        let params = SineParams {
            auto_budget: Some(budget), autoscale: false, select_all: false, ..base
        };
        let (result, raw_selected) = attempt(params, per_run);

        // Selection smaller than the budget ⇒ reachable fixed point hit
        // (can't widen further); larger ⇒ strict tolerance-1.0 floor hit
        // (can't narrow further).
        let reached_ceiling = raw_selected < budget;
        let reached_floor   = raw_selected > budget;
        let elapsed = result.timings.prover_run.as_secs() as i64;
        let act = classify(result.status, remap(result.status, result.termination));

        // Definitive verdict (or nothing useful to scale): return as-is.
        if matches!(act, ScaleAct::Done) {
            return result;
        }
        // Otherwise keep this as the best-so-far and let the planner
        // decide whether (and where) to continue.
        best = Some(result);
        if !planner.step(act, reached_ceiling, reached_floor, elapsed) {
            return best.unwrap();
        }
    }
    best.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prover::{ProverStatus as S, TerminationReason as TR};

    fn cfg(total_timeout: u32) -> ScaleConfig {
        ScaleConfig { factor: 2, max_disproofs: 4, max_time_runs: 4, min_budget: 64, total_timeout }
    }

    #[test]
    fn classify_maps_verdicts_to_actions() {
        assert_eq!(classify(S::Proved, None),                 ScaleAct::Done);
        assert_eq!(classify(S::Inconsistent, None),           ScaleAct::Done);
        assert_eq!(classify(S::InputError, None),             ScaleAct::Done);
        assert_eq!(classify(S::Disproved, Some(TR::Saturation)), ScaleAct::Widen);
        assert_eq!(classify(S::Timeout, Some(TR::TimeLimit)), ScaleAct::Narrow);
        assert_eq!(classify(S::Unknown, Some(TR::Saturation)),ScaleAct::Widen);
        assert_eq!(classify(S::Unknown, Some(TR::GaveUp)),    ScaleAct::Widen);
        assert_eq!(classify(S::Unknown, Some(TR::ResourceOut)),ScaleAct::Narrow);
        assert_eq!(classify(S::Unknown, None),                ScaleAct::Done);
    }

    #[test]
    fn widen_doubles_until_ceiling() {
        let mut p = ScalePlanner::new(100, cfg(0));
        assert_eq!(p.budget(), 100);
        assert!(p.step(ScaleAct::Widen, false, false, 0));   // not at ceiling → grow
        assert_eq!(p.budget(), 200);
        assert!(p.step(ScaleAct::Widen, false, false, 0));
        assert_eq!(p.budget(), 400);
        // Ceiling reached → stop.
        assert!(!p.step(ScaleAct::Widen, true, false, 0));
    }

    #[test]
    fn widen_gives_up_after_max_disproofs() {
        let mut p = ScalePlanner::new(100, cfg(0));
        // 4 consecutive widens (max_disproofs=4): the 4th returns false.
        assert!(p.step(ScaleAct::Widen, false, false, 0));   // 1
        assert!(p.step(ScaleAct::Widen, false, false, 0));   // 2
        assert!(p.step(ScaleAct::Widen, false, false, 0));   // 3
        assert!(!p.step(ScaleAct::Widen, false, false, 0));  // 4 → give up
    }

    #[test]
    fn narrow_halves_to_min_budget() {
        let mut p = ScalePlanner::new(256, cfg(0));
        assert!(p.step(ScaleAct::Narrow, false, false, 0));  // 256 -> 128
        assert_eq!(p.budget(), 128);
        // 128 -> 64 (min); at min the *next* narrow stops.
        assert!(p.step(ScaleAct::Narrow, false, false, 0));
        assert_eq!(p.budget(), 64);
        assert!(!p.step(ScaleAct::Narrow, false, false, 0));
    }

    #[test]
    fn narrow_stops_at_strict_floor() {
        // When the selection can't shrink to the budget (raw > budget, i.e.
        // we're at the tolerance-1.0 floor), narrowing further is futile.
        let mut p = ScalePlanner::new(1000, cfg(0));
        // First narrow drops the budget but the set still shrank (not floor).
        assert!(p.step(ScaleAct::Narrow, false, false, 0));
        assert_eq!(p.budget(), 500);
        // Now the strict floor is hit (raw > budget) → stop, no wasted rerun.
        assert!(!p.step(ScaleAct::Narrow, false, true, 0));
    }

    #[test]
    fn widen_then_narrow_brackets_and_bisects() {
        let mut p = ScalePlanner::new(1000, cfg(0));
        // Under-selected at 1000 → widen toward 2000 (lo=1000).
        assert!(p.step(ScaleAct::Widen, false, false, 0));
        assert_eq!(p.budget(), 2000);
        // Over-selected at 2000 → narrow; bracket (1000, 2000) → bisect to 1500.
        assert!(p.step(ScaleAct::Narrow, false, false, 0));
        assert_eq!(p.budget(), 1500);
        // Under at 1500 → bisect (1500, 2000) → 1750.
        assert!(p.step(ScaleAct::Widen, false, false, 0));
        assert_eq!(p.budget(), 1750);
    }

    #[test]
    fn narrow_consumes_time_runs_budget() {
        // total_timeout 40, max_time_runs 4 → first slice 10s.
        let mut p = ScalePlanner::new(4096, cfg(40));
        assert_eq!(p.slice(), 10);
        // Each narrow consumes a run; after max_time_runs narrows we stop.
        assert!(p.step(ScaleAct::Narrow, false, false, 10));  // run 1, 30s left, 3 runs
        assert_eq!(p.slice(), 10);                     // 30/3
        assert!(p.step(ScaleAct::Narrow, false, false, 10));  // run 2
        assert!(p.step(ScaleAct::Narrow, false, false, 10));  // run 3
        assert!(!p.step(ScaleAct::Narrow, false, false, 10)); // run 4 → max_time_runs reached
    }

    #[test]
    fn fast_widens_donate_time_forward() {
        // A fast widen (elapsed 0) shouldn't burn the time-run budget; the
        // slice for a later narrow still reflects the full remaining time.
        let mut p = ScalePlanner::new(100, cfg(40));
        assert_eq!(p.slice(), 10);
        assert!(p.step(ScaleAct::Widen, false, false, 0));    // fast, no time/run used
        assert_eq!(p.slice(), 10);                     // still 40/4
    }

    #[test]
    fn time_exhaustion_stops_widen() {
        let mut p = ScalePlanner::new(100, cfg(10));
        // A widen that eats all 10s leaves no time → stop.
        assert!(!p.step(ScaleAct::Widen, false, false, 10));
    }
}
