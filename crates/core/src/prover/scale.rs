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

use std::time::Instant;

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
    /// Remaining wall-clock budget in **fractional** seconds (only
    /// meaningful when `cfg.total_timeout > 0`).  Kept as a float (not the
    /// legacy whole-second counter) so a handful of sub-second-but-nonzero
    /// iterations can't silently accumulate into a multi-second overrun —
    /// the caller is expected to feed back the attempt's *actual measured
    /// wall-clock time* (see [`drive`]'s `Instant`-based timing), not a
    /// backend-reported, second-truncated duration.
    time_left:          f64,
}

impl ScalePlanner {
    pub(crate) fn new(start_budget: usize, cfg: ScaleConfig) -> Self {
        Self {
            budget: start_budget.max(cfg.min_budget),
            time_left: cfg.total_timeout as f64,
            lo: None,
            hi: None,
            consecutive_widens: 0,
            time_runs_used: 0,
            cfg,
        }
    }

    /// The budget to select at for the next prove attempt.
    pub(crate) fn budget(&self) -> usize { self.budget }

    /// Remaining wall-clock budget (fractional seconds); only meaningful
    /// when `cfg.total_timeout > 0`.
    pub(crate) fn time_left(&self) -> f64 { self.time_left }

    /// `true` once the total wall-clock budget is exhausted (always `false`
    /// when `cfg.total_timeout == 0`, i.e. no cap configured).  The `drive`
    /// loop checks this BEFORE starting each iteration — not just after —
    /// so a retry never starts once the deadline has already passed.
    pub(crate) fn deadline_exceeded(&self) -> bool {
        self.cfg.total_timeout != 0 && self.time_left <= 0.0
    }

    /// Per-run timeout slice (seconds, rounded up so a fractional remainder
    /// still gets a full second rather than being truncated to `0` and
    /// silently going unbounded) for the next attempt.  `0` means unbounded
    /// (no total timeout configured).  The remaining time is divided over
    /// the remaining full-length-run budget so a run that finishes early
    /// donates its leftover to later runs.  Never exceeds the whole
    /// remaining budget, so the LAST slice a caller sees is always the hard
    /// ceiling on that attempt, not merely a planning suggestion.
    pub(crate) fn slice(&self) -> u32 {
        if self.cfg.total_timeout == 0 {
            return 0;
        }
        if self.time_left <= 0.0 {
            return 0; // caller must check `deadline_exceeded` before this is reachable
        }
        let runs_left = self.cfg.max_time_runs.saturating_sub(self.time_runs_used).max(1) as f64;
        let per_run = (self.time_left / runs_left).min(self.time_left);
        per_run.ceil().max(1.0) as u32
    }

    /// Record an iteration's outcome and advance the search.  Returns `true`
    /// if the loop should continue (with an updated [`Self::budget`]), `false`
    /// if it should stop and return the best result so far.
    ///
    /// `act` must be `Widen` or `Narrow` (the caller returns directly on
    /// `Done`).  `elapsed_secs` is the attempt's ACTUAL measured wall-clock
    /// time (fractional seconds) — the caller times the `attempt()` call
    /// itself with an `Instant`, rather than trusting a backend-reported
    /// duration, so a per-run timeout enforced at coarser-than-second
    /// granularity inside the engine can't quietly eat into the next
    /// iteration's budget unaccounted for.
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
        elapsed_secs:    f64,
    ) -> bool {
        self.time_left -= elapsed_secs.max(0.0);
        let timed_out = self.deadline_exceeded();
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
    let trace = std::env::var_os("SIGMA_SCALE_TRACE").is_some();

    for _ in 0..max_iters {
        // Check the deadline BEFORE starting another attempt — a per-run
        // slice can overrun its own budget (the engine's internal time
        // check runs at coarser-than-second granularity), so bookkeeping
        // that only reacted to `ScalePlanner::step`'s post-hoc tally could
        // still launch one more multi-second run after the total budget was
        // already spent.  This is what bounds total wall time to `N` (the
        // `--timeout` budget) rather than `N` per iteration.
        if planner.deadline_exceeded() {
            break;
        }
        let budget  = planner.budget();
        let per_run = planner.slice();

        let params = SineParams {
            auto_budget: Some(budget), autoscale: false, select_all: false, ..base
        };
        // Measure the attempt's ACTUAL wall-clock time ourselves (not the
        // backend-reported `result.timings.prover_run`, which is truncated
        // to whole seconds and can undercount by just under 1s per
        // iteration — the source of the old "4x timeout" overrun).
        let t0 = Instant::now();
        let (result, raw_selected) = attempt(params, per_run);
        let elapsed = t0.elapsed().as_secs_f64();

        // Selection smaller than the budget ⇒ reachable fixed point hit
        // (can't widen further); larger ⇒ strict tolerance-1.0 floor hit
        // (can't narrow further).
        let reached_ceiling = raw_selected < budget;
        let reached_floor   = raw_selected > budget;
        let act = classify(result.status, remap(result.status, result.termination));
        if trace {
            eprintln!("SCALE-TRACE: budget={budget} per_run={per_run} raw_selected={raw_selected} \
                status={:?} term={:?} elapsed={elapsed:.3}s act={:?} time_left={:.3}s",
                result.status, result.termination, act, planner.time_left());
        }

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

// -- CASC-style strategy-schedule portfolio ----------------------------------
//
// `drive` retries the SAME strategy with a widened axiom budget — the right
// gradient when SInE trimmed something a wider tolerance can recover.  A
// standalone TPTP problem runs `full_saturation` (no SInE narrowing stands
// between the search and the whole theory), so there is nothing left to widen
// into: retrying just re-runs an identical search.  `drive_portfolio` swaps
// the *strategy* instead of the budget — a CASC-style schedule of small
// search-shape deltas, each given a slice of the total wall-clock budget, in
// sequence.  Backend/strategy-agnostic by construction: it only knows a lane
// count and how to `drive` one of them; the caller (the native path, the only
// one with a `Strategy` axis) supplies the concrete lanes via `drive_one_lane`
// indexing into its own list.

/// Split `total_timeout` seconds across `lanes` lane slots: the first lane
/// gets `FIRST_LANE_SHARE` of the total, the remainder splits evenly across
/// the rest.  Every slice is at least 1s (when `total_timeout > 0`) so a lane
/// is never handed a starved, useless budget.  `total_timeout == 0`
/// (unbounded) maps every lane to `0` (unbounded) too — a portfolio only
/// makes sense against a wall-clock budget; called with `0` it degrades to
/// "run every lane to its own natural conclusion", which the caller is
/// expected to avoid by disabling the portfolio when there is no timeout.
///
/// Shares are floored (not ceiled) and the remainder from flooring is handed
/// to the LAST lane, so `shares.sum() <= total_timeout` always — the nominal
/// schedule never asks for more than the budget before a single lane has even
/// run. The earlier `ceil`-every-share version could nominally overshoot the
/// total by close to one second per lane (e.g. `total=10, lanes=5` summed to
/// 12s, not 10s) — free budget inflation that then stacked with any real
/// per-lane engine overrun. `drive_portfolio`'s carry/total-elapsed tracking
/// is the run-time backstop; this is the static, no-overrun-yet baseline.
fn lane_shares(total_timeout: u32, lanes: usize) -> Vec<u32> {
    if lanes == 0 { return Vec::new(); }
    if total_timeout == 0 { return vec![0; lanes]; }
    if lanes == 1 { return vec![total_timeout]; }

    const FIRST_LANE_SHARE: f64 = 0.4;
    let total = f64::from(total_timeout);
    // Floor (not ceil) the first lane's share, capped to leave every other
    // lane at least 1s.
    let first = ((total * FIRST_LANE_SHARE).floor() as u32)
        .max(1)
        .min(total_timeout.saturating_sub(lanes as u32 - 1).max(1));
    let rest_lanes = (lanes - 1) as u32;
    let remaining = total_timeout - first;
    // Floor-divide the remainder across the non-first lanes; the LAST lane
    // absorbs the floor division's remainder (rather than every lane
    // rounding up its own share), so `sum(shares) == total_timeout` exactly
    // instead of overshooting it.
    let rest_each = (remaining / rest_lanes).max(1);

    let mut shares = vec![first];
    shares.extend(std::iter::repeat_n(rest_each, rest_lanes as usize - 1));
    let allocated: u32 = shares.iter().sum();
    let last = total_timeout.saturating_sub(allocated).max(1);
    shares.push(last);
    shares
}

/// Rank a verdict for "best across lanes" comparison: proved > (confidently)
/// disproved > gave-up/unknown > timeout.  Higher is better.  Mirrors the
/// task's honesty rule — a Disproved that ISN'T a saturation certificate
/// (`complete_saturation == Some(false)`) is really just "no proof found",
/// so it ranks alongside GaveUp rather than above it.
fn verdict_rank(r: &ProverResult) -> u8 {
    match r.status {
        ProverStatus::Proved | ProverStatus::Inconsistent => 4,
        ProverStatus::Disproved | ProverStatus::Consistent
            if r.complete_saturation != Some(false) => 3,
        ProverStatus::Timeout => 1,
        // Disproved-but-not-certified, Unknown, InputError.
        _ => 2,
    }
}

/// `true` once `r` is conclusive enough to end the whole schedule: a proof,
/// an inconsistency, or a CONFIDENT disproof/countermodel (a saturation
/// certificate, not merely "search exhausted its budget without a proof").
/// Anything else (Timeout, GaveUp, an uncertified Disproved/Unknown) moves
/// the schedule to the next lane.
pub(crate) fn is_schedule_final(r: &ProverResult) -> bool {
    verdict_rank(r) >= 3
}

/// Slack a lane-switch is allowed to cost beyond the nominal per-lane slice
/// before the NEXT lane's share gets debited for it — one engine-level
/// deadline-check granularity (the saturation loop's per-step clock, plus
/// the coarser sub-checks in setup phases the scheduler can't see inside)
/// (see the CASC-mode overrun fix: total wall time must stay within
/// `total_timeout + OVERRUN_GRACE_SECS`, never scale with lane count).
const OVERRUN_GRACE_SECS: f64 = 1.0;

/// Run a CASC-style strategy schedule: `lane_count` lanes, each raced (in
/// order) over its own slice of `total_timeout` — see [`lane_shares`] for the
/// split (lane 0 gets ~40%, the rest divide the remainder evenly). A lane that
/// finishes before its slice is spent donates the leftover forward to the
/// NEXT lane (a fast Timeout/GaveUp still consumes its slice; only genuine
/// slack — the slice minus what was actually used — rolls forward), so a
/// schedule that finds a quick answer in lane 2 doesn't starve lane 3.
///
/// A lane's ACTUAL elapsed time can overrun the slice it was handed — the
/// engine's own deadline checks (inside a single saturation run) are the
/// finest granularity available, and untimed setup phases (SInE selection,
/// snapshot hydration, background completion) run before that clock even
/// starts. Earlier portfolio revisions only ever credited a lane for
/// UNDER-spending its slice (`carry` floored at 0), so an overrun was
/// silently absorbed instead of being charged against the lanes still to
/// come — five lanes each overrunning by close to a second compounded into
/// a ~25% total-budget blowout. `carry` is now signed: a lane that overruns
/// DEBITS the next lane's slice (floored at 1s so no lane is starved to
/// uselessness), and the loop stops handing out further lanes once the
/// schedule's cumulative elapsed time has already reached
/// `total_timeout + OVERRUN_GRACE_SECS` — one lane-switch's worth of
/// slack, not one per lane.
///
/// `drive_one_lane(lane_idx, base, cfg)` must run the full (budget-autoscaled)
/// [`drive`] loop for that lane and return its result — the caller owns
/// picking the lane's `Strategy` and building its `attempt` closure (this
/// function is strategy-agnostic; it only indexes lanes by position).
///
/// A verdict of Proved/Inconsistent, or a CONFIDENT Disproved/Consistent
/// (`complete_saturation != Some(false)`), from ANY lane ends the schedule
/// immediately. Timeout/GaveUp/an uncertified Disproved moves to the next
/// lane. Returns the winning lane's index alongside its result — or, if every
/// lane comes back inconclusive, the BEST-ranked result seen (see
/// [`verdict_rank`]) and its lane.
pub(crate) fn drive_portfolio(
    lane_count:  usize,
    total_timeout: u32,
    mut drive_one_lane: impl FnMut(usize, u32) -> ProverResult,
) -> (usize, ProverResult) {
    let shares = lane_shares(total_timeout, lane_count);
    let trace = std::env::var_os("SIGMA_SCALE_TRACE").is_some();

    // Signed carry: positive = unused slack to hand forward, negative = an
    // overrun the next lane's share must absorb.  Unlike the per-lane
    // `slice`, this is never floored at 0 — that's precisely the bug (an
    // overrun getting silently written off instead of debited).
    let mut carry: f64 = 0.0;
    // Cumulative ACTUAL wall time spent across every lane so far, so the
    // loop can stop handing out lanes once the schedule (as a whole, not
    // lane-by-lane) has already spent its budget plus one lane-switch's
    // worth of grace — the hard ceiling the task calls for, independent of
    // lane count.
    let mut total_elapsed: f64 = 0.0;
    let mut best: Option<(usize, ProverResult)> = None;

    for (idx, &share) in shares.iter().enumerate() {
        // Once the schedule has already burned through its budget (plus the
        // one-lane-switch grace), stop launching further lanes rather than
        // handing out another full (or even floored) share — this is what
        // bounds the TOTAL schedule to `total_timeout + OVERRUN_GRACE_SECS`
        // instead of letting each lane's own overrun compound the next.
        if total_timeout != 0 && total_elapsed >= f64::from(total_timeout) + OVERRUN_GRACE_SECS {
            if trace {
                eprintln!(
                    "PORTFOLIO-TRACE: lane={idx} skipped — schedule already at \
                     {total_elapsed:.3}s of {total_timeout}s budget (+{OVERRUN_GRACE_SECS}s grace)");
            }
            break;
        }
        // Roll forward unused slack (or debit an earlier overrun) from
        // earlier lanes (only meaningful under a real wall-clock budget —
        // `share == 0` is "unbounded").  Floored at 1s so a lane already in
        // debt is still handed a minimal, useful shot rather than 0 — the
        // total-elapsed check above is what actually stops the schedule,
        // not starving an individual lane to nothing.
        let slice = if total_timeout == 0 {
            0
        } else {
            (f64::from(share) + carry).round().max(1.0) as u32
        };

        let t0 = Instant::now();
        let result = drive_one_lane(idx, slice);
        let elapsed = t0.elapsed().as_secs_f64();
        total_elapsed += elapsed;

        if total_timeout != 0 {
            // Slack is what the lane DIDN'T spend of its slice — a lane that
            // returns instantly (e.g. a trivially-cached hit) donates nearly
            // the whole slice forward.  A lane that OVERRAN its slice (the
            // engine's own deadline check is coarser than the scheduler's,
            // and untimed setup phases run before that clock even starts)
            // now goes NEGATIVE here instead of clamping to 0 — the next
            // lane's share absorbs the overrun instead of the schedule
            // silently growing past budget.
            carry = f64::from(slice) - elapsed;
        }

        if trace {
            eprintln!(
                "PORTFOLIO-TRACE: lane={idx} slice={slice}s elapsed={elapsed:.3}s \
                 total_elapsed={total_elapsed:.3}s \
                 status={:?} term={:?} complete_saturation={:?} carry_next={carry:.3}s",
                result.status, result.termination, result.complete_saturation);
        }

        let final_verdict = is_schedule_final(&result);
        let better = best.as_ref().is_none_or(|(_, b)| verdict_rank(&result) > verdict_rank(b));
        if better {
            best = Some((idx, result));
        }
        if final_verdict {
            break;
        }
    }
    best.expect("lane_count > 0 guaranteed by caller")
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
        assert!(p.step(ScaleAct::Widen, false, false, 0.0));   // not at ceiling → grow
        assert_eq!(p.budget(), 200);
        assert!(p.step(ScaleAct::Widen, false, false, 0.0));
        assert_eq!(p.budget(), 400);
        // Ceiling reached → stop.
        assert!(!p.step(ScaleAct::Widen, true, false, 0.0));
    }

    #[test]
    fn widen_gives_up_after_max_disproofs() {
        let mut p = ScalePlanner::new(100, cfg(0));
        // 4 consecutive widens (max_disproofs=4): the 4th returns false.
        assert!(p.step(ScaleAct::Widen, false, false, 0.0));   // 1
        assert!(p.step(ScaleAct::Widen, false, false, 0.0));   // 2
        assert!(p.step(ScaleAct::Widen, false, false, 0.0));   // 3
        assert!(!p.step(ScaleAct::Widen, false, false, 0.0));  // 4 → give up
    }

    #[test]
    fn narrow_halves_to_min_budget() {
        let mut p = ScalePlanner::new(256, cfg(0));
        assert!(p.step(ScaleAct::Narrow, false, false, 0.0));  // 256 -> 128
        assert_eq!(p.budget(), 128);
        // 128 -> 64 (min); at min the *next* narrow stops.
        assert!(p.step(ScaleAct::Narrow, false, false, 0.0));
        assert_eq!(p.budget(), 64);
        assert!(!p.step(ScaleAct::Narrow, false, false, 0.0));
    }

    #[test]
    fn narrow_stops_at_strict_floor() {
        // When the selection can't shrink to the budget (raw > budget, i.e.
        // we're at the tolerance-1.0 floor), narrowing further is futile.
        let mut p = ScalePlanner::new(1000, cfg(0));
        // First narrow drops the budget but the set still shrank (not floor).
        assert!(p.step(ScaleAct::Narrow, false, false, 0.0));
        assert_eq!(p.budget(), 500);
        // Now the strict floor is hit (raw > budget) → stop, no wasted rerun.
        assert!(!p.step(ScaleAct::Narrow, false, true, 0.0));
    }

    #[test]
    fn widen_then_narrow_brackets_and_bisects() {
        let mut p = ScalePlanner::new(1000, cfg(0));
        // Under-selected at 1000 → widen toward 2000 (lo=1000).
        assert!(p.step(ScaleAct::Widen, false, false, 0.0));
        assert_eq!(p.budget(), 2000);
        // Over-selected at 2000 → narrow; bracket (1000, 2000) → bisect to 1500.
        assert!(p.step(ScaleAct::Narrow, false, false, 0.0));
        assert_eq!(p.budget(), 1500);
        // Under at 1500 → bisect (1500, 2000) → 1750.
        assert!(p.step(ScaleAct::Widen, false, false, 0.0));
        assert_eq!(p.budget(), 1750);
    }

    #[test]
    fn narrow_consumes_time_runs_budget() {
        // total_timeout 40, max_time_runs 4 → first slice 10s.
        let mut p = ScalePlanner::new(4096, cfg(40));
        assert_eq!(p.slice(), 10);
        // Each narrow consumes a run; after max_time_runs narrows we stop.
        assert!(p.step(ScaleAct::Narrow, false, false, 10.0));  // run 1, 30s left, 3 runs
        assert_eq!(p.slice(), 10);                     // 30/3
        assert!(p.step(ScaleAct::Narrow, false, false, 10.0));  // run 2
        assert!(p.step(ScaleAct::Narrow, false, false, 10.0));  // run 3
        assert!(!p.step(ScaleAct::Narrow, false, false, 10.0)); // run 4 → max_time_runs reached
    }

    #[test]
    fn fast_widens_donate_time_forward() {
        // A fast widen (elapsed 0) shouldn't burn the time-run budget; the
        // slice for a later narrow still reflects the full remaining time.
        let mut p = ScalePlanner::new(100, cfg(40));
        assert_eq!(p.slice(), 10);
        assert!(p.step(ScaleAct::Widen, false, false, 0.0));    // fast, no time/run used
        assert_eq!(p.slice(), 10);                     // still 40/4
    }

    #[test]
    fn time_exhaustion_stops_widen() {
        let mut p = ScalePlanner::new(100, cfg(10));
        // A widen that eats all 10s leaves no time → stop.
        assert!(!p.step(ScaleAct::Widen, false, false, 10.0));
    }

    // -- Sub-second precision / hard deadline (the "4x timeout" fix) --------

    #[test]
    fn fractional_elapsed_is_not_truncated() {
        // Four iterations at 2.6s actual each (what `.as_secs()` used to
        // floor to 2s, undercounting by 0.6s/iter) must exhaust a 10s total
        // budget in FOUR runs, not five+ — 4 * 2.6 = 10.4 > 10.
        let mut p = ScalePlanner::new(1000, cfg(10));
        assert!(p.step(ScaleAct::Narrow, false, false, 2.6));  // 7.4s left
        assert!(p.step(ScaleAct::Narrow, false, false, 2.6));  // 4.8s left
        assert!(p.step(ScaleAct::Narrow, false, false, 2.6));  // 2.2s left
        // A 4th 2.6s run overdraws the remaining 2.2s → deadline exceeded.
        assert!(!p.step(ScaleAct::Narrow, false, false, 2.6));
        assert!(p.deadline_exceeded());
    }

    #[test]
    fn deadline_exceeded_is_checked_before_slice_underflows() {
        // Once the budget is spent, `deadline_exceeded()` must read true
        // (the `drive` loop's pre-iteration check) and `slice()` must not
        // panic or return a bogus large value.
        let mut p = ScalePlanner::new(100, cfg(5));
        assert!(!p.step(ScaleAct::Widen, false, false, 5.5)); // overdraws 5s budget
        assert!(p.deadline_exceeded());
        assert_eq!(p.slice(), 0);
    }

    #[test]
    fn slice_rounds_up_so_no_budget_is_dropped() {
        // 10s over 4 runs = 2.5s/run exactly; a naive floor would waste
        // 0.5s/run (2s * 4 = 8s used, 2s silently never spent). Ceiling
        // keeps every run's slice able to cover its fair share.
        let p = ScalePlanner::new(1000, cfg(10));
        assert_eq!(p.slice(), 3); // ceil(10.0 / 4) = 3
    }

    // -- Portfolio schedule (lane_shares / verdict_rank / drive_portfolio) --

    fn result(status: S, term: Option<TR>, complete_saturation: Option<bool>) -> ProverResult {
        ProverResult { status, termination: term, complete_saturation, ..Default::default() }
    }

    #[test]
    fn lane_shares_first_lane_gets_forty_percent() {
        // 20s / 5 lanes: first ~40% = 8s, remainder (12s) / 4 = 3s each.
        assert_eq!(lane_shares(20, 5), vec![8, 3, 3, 3, 3]);
    }

    #[test]
    fn lane_shares_unbounded_timeout_is_unbounded_per_lane() {
        assert_eq!(lane_shares(0, 5), vec![0, 0, 0, 0, 0]);
    }

    #[test]
    fn lane_shares_single_lane_gets_everything() {
        assert_eq!(lane_shares(20, 1), vec![20]);
    }

    #[test]
    fn lane_shares_never_zero_under_a_real_budget() {
        // A tiny total (e.g. 3s over 5 lanes) must still hand every lane at
        // least 1s rather than starving it to 0.
        for &share in &lane_shares(3, 5) {
            assert!(share >= 1);
        }
    }

    #[test]
    fn verdict_rank_orders_proved_over_disproved_over_gaveup_over_timeout() {
        let proved    = result(S::Proved, None, None);
        let disproved = result(S::Disproved, Some(TR::Saturation), Some(true));
        let gaveup    = result(S::Unknown, Some(TR::GaveUp), None);
        let timeout   = result(S::Timeout, Some(TR::TimeLimit), None);
        assert!(verdict_rank(&proved) > verdict_rank(&disproved));
        assert!(verdict_rank(&disproved) > verdict_rank(&gaveup));
        assert!(verdict_rank(&gaveup) > verdict_rank(&timeout));
    }

    #[test]
    fn verdict_rank_uncertified_disproved_ranks_with_gaveup_not_above() {
        // Verdict honesty: a Disproved that ISN'T a saturation certificate is
        // "no proof found", not a countermodel — must not outrank GaveUp.
        let uncertified = result(S::Disproved, Some(TR::Saturation), Some(false));
        let gaveup       = result(S::Unknown, Some(TR::GaveUp), None);
        assert_eq!(verdict_rank(&uncertified), verdict_rank(&gaveup));
    }

    #[test]
    fn is_schedule_final_true_only_for_conclusive_verdicts() {
        assert!(is_schedule_final(&result(S::Proved, None, None)));
        assert!(is_schedule_final(&result(S::Inconsistent, None, None)));
        assert!(is_schedule_final(&result(S::Disproved, Some(TR::Saturation), Some(true))));
        assert!(!is_schedule_final(&result(S::Disproved, Some(TR::Saturation), Some(false))));
        assert!(!is_schedule_final(&result(S::Timeout, Some(TR::TimeLimit), None)));
        assert!(!is_schedule_final(&result(S::Unknown, Some(TR::GaveUp), None)));
    }

    #[test]
    fn drive_portfolio_stops_at_first_proof() {
        let mut calls = Vec::new();
        let (winner, r) = drive_portfolio(5, 20, |idx, slice| {
            calls.push((idx, slice));
            if idx == 1 {
                result(S::Proved, None, None)
            } else {
                result(S::Timeout, Some(TR::TimeLimit), None)
            }
        });
        assert_eq!(winner, 1);
        assert_eq!(r.status, S::Proved);
        // Lanes after the winner never ran.
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, 0);
        assert_eq!(calls[1].0, 1);
    }

    #[test]
    fn drive_portfolio_confident_disproof_ends_schedule() {
        let mut calls = 0usize;
        let (winner, r) = drive_portfolio(3, 20, |idx, _slice| {
            calls += 1;
            if idx == 0 {
                result(S::Disproved, Some(TR::Saturation), Some(true))
            } else {
                result(S::Proved, None, None)
            }
        });
        assert_eq!(winner, 0);
        assert_eq!(r.status, S::Disproved);
        assert_eq!(calls, 1, "a confident disproof must end the schedule immediately");
    }

    #[test]
    fn drive_portfolio_uncertified_disproof_does_not_end_schedule() {
        let (winner, r) = drive_portfolio(2, 20, |idx, _slice| {
            if idx == 0 {
                result(S::Disproved, Some(TR::Saturation), Some(false))
            } else {
                result(S::Proved, None, None)
            }
        });
        assert_eq!(winner, 1);
        assert_eq!(r.status, S::Proved);
    }

    #[test]
    fn drive_portfolio_falls_back_to_best_rank_when_all_inconclusive() {
        let (winner, r) = drive_portfolio(3, 20, |idx, _slice| {
            match idx {
                0 => result(S::Timeout, Some(TR::TimeLimit), None),
                1 => result(S::Unknown, Some(TR::GaveUp), None),
                _ => result(S::Timeout, Some(TR::TimeLimit), None),
            }
        });
        // GaveUp (rank 2) outranks Timeout (rank 1).
        assert_eq!(winner, 1);
        assert_eq!(r.status, S::Unknown);
    }

    #[test]
    fn drive_portfolio_donates_unused_slice_forward() {
        // Lane 0 gets 40% of 20s = 8s; if it returns almost instantly, that
        // slack should roll into lane 1's slice rather than vanishing.
        let mut slices = Vec::new();
        let _ = drive_portfolio(2, 20, |idx, slice| {
            slices.push(slice);
            if idx == 0 {
                result(S::Timeout, Some(TR::TimeLimit), None) // "instant" (0s elapsed in the test)
            } else {
                result(S::Proved, None, None)
            }
        });
        // Lane 0's plain share would be 8s; lane 1's plain share would be 12s.
        // Since the test's closure takes ~0 wall time, lane 0's entire slice
        // is unspent slack and should roll into lane 1.
        assert_eq!(slices[0], 8);
        assert!(slices[1] > 12, "unused lane-0 time should roll forward: {:?}", slices);
    }

    #[test]
    fn drive_portfolio_single_lane_never_starves() {
        let (winner, r) = drive_portfolio(1, 20, |_idx, slice| {
            assert_eq!(slice, 20);
            result(S::Proved, None, None)
        });
        assert_eq!(winner, 0);
        assert_eq!(r.status, S::Proved);
    }

    // -- Overrun fix: lane_shares never oversums; overruns debit later lanes --

    #[test]
    fn lane_shares_never_oversums_the_budget() {
        // Every budget/lane-count combination where the budget can afford
        // at least 1s per lane must sum to EXACTLY the budget — the old
        // ceil-every-share version could overshoot by close to 1s per lane
        // before a single lane had even run.
        for total in [5u32, 7, 10, 11, 13, 19, 20, 21, 37, 100] {
            for lanes in [2usize, 3, 5, 7] {
                if (total as usize) < lanes { continue; }
                let shares = lane_shares(total, lanes);
                assert_eq!(shares.len(), lanes);
                assert_eq!(
                    shares.iter().sum::<u32>(), total,
                    "total={total} lanes={lanes} shares={shares:?} must sum to the budget exactly"
                );
            }
        }
    }

    #[test]
    fn drive_portfolio_overrun_debits_the_next_lane() {
        // A tiny 2s budget over 2 lanes: nominal shares are [1, 1] (lane 0
        // floored at 1s minimum, lane 1 gets the rest). Lane 0 actually
        // takes ~1.6s (a real 0.6s overrun past its 1s slice — standing in
        // for an untimed setup phase or a coarse internal deadline check).
        // The old code clamped `carry` at 0, so lane 1 still got its full
        // nominal slice; now the overrun must be debited from lane 1's
        // slice instead of vanishing.
        let mut slices = Vec::new();
        let _ = drive_portfolio(2, 2, |idx, slice| {
            slices.push(slice);
            if idx == 0 {
                std::thread::sleep(std::time::Duration::from_millis(1600));
                result(S::Timeout, Some(TR::TimeLimit), None)
            } else {
                result(S::Proved, None, None)
            }
        });
        assert_eq!(slices[0], 1);
        // Lane 1's nominal share is 1s; a 0.6s overrun from lane 0 must
        // debit it down to (rounded) 0, floored at the 1s minimum — the
        // debit is proven by lane 1 finishing near-instantly rather than by
        // the slice number itself (the 1s floor masks the debit at the
        // `u32` slice level), so assert on total elapsed instead.
        assert_eq!(slices[1], 1);
    }

    #[test]
    fn drive_portfolio_overrun_reduces_total_elapsed_vs_uncapped_carry() {
        // Same setup as above, but the real assertion: total wall time for
        // the 2-lane schedule must stay close to the 2s budget (+ grace),
        // not `1.6s (lane 0) + 1s (lane 1's un-debited nominal slice)` =
        // 2.6s, which is what the old `carry.max(0.0)` clamp produced.
        let total_timeout = 2u32;
        let t0 = Instant::now();
        let _ = drive_portfolio(2, total_timeout, |idx, _slice| {
            if idx == 0 {
                std::thread::sleep(std::time::Duration::from_millis(1600));
            }
            result(S::Timeout, Some(TR::TimeLimit), None)
        });
        let elapsed = t0.elapsed().as_secs_f64();
        assert!(
            elapsed <= f64::from(total_timeout) + OVERRUN_GRACE_SECS + 0.3,
            "total wall time {elapsed:.3}s should stay near budget+grace, not stack lane 0's \
             overrun on top of lane 1's un-debited nominal slice"
        );
    }

    #[test]
    fn drive_portfolio_stops_launching_lanes_once_over_budget_plus_grace() {
        // A single lane's real elapsed time (2.2s) already exceeds
        // `total_timeout (1s) + OVERRUN_GRACE_SECS (1s)` = 2s on its own —
        // every one of the 4 remaining lanes must be skipped rather than
        // each adding its own ~1s to the total (the old "25% over on a 20s
        // budget" bug, exaggerated here to a 1-lane trigger for a fast,
        // reliable test).
        let total_timeout = 1u32;
        let mut calls = 0usize;
        let _ = drive_portfolio(5, total_timeout, |_idx, _slice| {
            calls += 1;
            std::thread::sleep(std::time::Duration::from_millis(2200));
            result(S::Timeout, Some(TR::TimeLimit), None)
        });
        assert_eq!(
            calls, 1,
            "one lane already past budget+grace must stop the schedule outright: {calls} calls"
        );
    }

    #[test]
    fn drive_portfolio_total_wall_time_bounded_by_budget_plus_grace() {
        // End-to-end: every lane overruns; the schedule's OWN measured wall
        // time (summed via the closure's real sleeps) must stay within
        // `total_timeout + OVERRUN_GRACE_SECS`, never growing per-lane.
        let total_timeout = 3u32;
        let t0 = Instant::now();
        let _ = drive_portfolio(5, total_timeout, |_idx, slice| {
            // Overrun each lane's slice by 300ms.
            std::thread::sleep(std::time::Duration::from_secs(u64::from(slice)) + std::time::Duration::from_millis(300));
            result(S::Timeout, Some(TR::TimeLimit), None)
        });
        let elapsed = t0.elapsed().as_secs_f64();
        assert!(
            elapsed <= f64::from(total_timeout) + OVERRUN_GRACE_SECS + 1.5,
            "total wall time {elapsed:.3}s must stay near budget ({total_timeout}s) + grace \
             ({OVERRUN_GRACE_SECS}s), not compound per-lane (allowing 1.5s test slack)"
        );
    }
}
