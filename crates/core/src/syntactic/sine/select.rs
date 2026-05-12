//! SInE axiom selection: the BFS over the D-relation and the auto-tolerance
//! budget search.  The maintenance methods live in `index.rs`.

use std::collections::{HashSet, VecDeque};

use crate::types::{SentenceId, SymbolId};

use super::SineIndex;
use super::params::MAX_AUTO_TOLERANCE;

/// Hard cap on the number of breakpoint steps [`SineIndex::tolerance_breakpoints`]
/// will climb, guarding against pathological conjectures with very many
/// distinct activation thresholds.
const MAX_CLIMB_STEPS: usize = 256;

/// Tolerance resolution for the auto-selector's binary search.  Bisection
/// stops once the in-budget/over-budget bracket is narrower than this, which
/// (since distinct breakpoints are integer ratios) is fine enough to land on
/// the correct selected set for any realistic KB.
const AUTO_TOLERANCE_RESOLUTION: f32 = 1e-3;

/// Hard cap on auto-selector bisection iterations.  `log2((MAX_AUTO_TOLERANCE
/// - 1) / AUTO_TOLERANCE_RESOLUTION) ≈ 16`, so 32 is generous headroom.
const MAX_BISECT_STEPS: usize = 32;

/// Slack added to the multiplicative trigger comparison so that an axiom
/// whose exact activation tolerance equals the query tolerance is admitted
/// despite f32 rounding.  Safe because all generalities are integers, so a
/// `1e-3` cushion never crosses an integer boundary.
const TRIGGER_EPS: f32 = 1e-3;

impl SineIndex {
    // -- Selection -----------------------------------------------------------

    /// Run the SInE BFS from `seed_syms` at the given `tolerance`, returning
    /// the sids of every axiom reached.
    ///
    /// Tolerance is a plain parameter; the stored `self.tolerance` field is
    /// updated by the [`SyntacticLayer::select_axioms`] wrapper, not here.
    pub fn select(
        &self,
        seed_syms:   &HashSet<SymbolId>,
        tolerance:   f32,
        depth_limit: Option<usize>,
    ) -> HashSet<SentenceId> {
        let (selected, _next, _over) =
            self.select_inner(seed_syms, tolerance, depth_limit, false, None);
        selected
    }

    /// Like [`Self::select`], but also returns the *next* tolerance strictly
    /// greater than `tolerance` at which the selected set would grow — i.e.
    /// the smallest activation threshold among edges examined-but-rejected
    /// during this BFS.  Returns `None` when the fixed point has been
    /// reached (no rejected edges) so no larger tolerance changes the result.
    ///
    /// The reported breakpoint is exact for unlimited depth (`depth_limit ==
    /// None`); under a finite depth cap it may under-estimate.
    pub fn select_reporting_next(
        &self,
        seed_syms:   &HashSet<SymbolId>,
        tolerance:   f32,
        depth_limit: Option<usize>,
    ) -> (HashSet<SentenceId>, Option<f32>) {
        let (selected, next, _over) =
            self.select_inner(seed_syms, tolerance, depth_limit, true, None);
        (selected, next)
    }

    /// Budget-capped selection: run the BFS but abort as soon as more than
    /// `cap` axioms have been selected, returning `(partial_set, over)` where
    /// `over` is `true` iff the cap was exceeded.  This bounds the cost of an
    /// over-budget probe to `O(cap)` — the binary search in
    /// [`Self::select_within_budget`] never has to compute a full "explosion".
    pub fn select_capped(
        &self,
        seed_syms:   &HashSet<SymbolId>,
        tolerance:   f32,
        depth_limit: Option<usize>,
        cap:         usize,
    ) -> (HashSet<SentenceId>, bool) {
        let (selected, _next, over) =
            self.select_inner(seed_syms, tolerance, depth_limit, false, Some(cap));
        (selected, over)
    }

    /// Shared BFS core.
    ///
    /// - `track_next`: record the minimal activation tolerance among rejected
    ///   edges (returned as the second tuple element).
    /// - `cap`: if `Some(c)`, abort once more than `c` axioms are selected;
    ///   the third tuple element reports whether that happened.
    fn select_inner(
        &self,
        seed_syms:   &HashSet<SymbolId>,
        tolerance:   f32,
        depth_limit: Option<usize>,
        track_next:  bool,
        cap:         Option<usize>,
    ) -> (HashSet<SentenceId>, Option<f32>, bool) {
        let tolerance = tolerance.max(1.0);
        let mut selected:     HashSet<SentenceId> = HashSet::new();
        let mut visited_syms: HashSet<SymbolId>   = HashSet::new();
        let mut frontier:     VecDeque<SymbolId>  = seed_syms.iter().copied().collect();
        let mut depth = 0usize;
        // Smallest activation tolerance among edges we examined but rejected.
        let mut next_breakpoint: Option<f32> = None;
        let mut over = false;

        'bfs: while !frontier.is_empty() {
            if let Some(limit) = depth_limit {
                if depth >= limit { break; }
            }
            let wave_size = frontier.len();
            let mut next_wave: Vec<SymbolId> = Vec::new();

            for _ in 0..wave_size {
                let s = match frontier.pop_front() { Some(x) => x, None => break };
                if !visited_syms.insert(s) { continue; }

                let occ_s = self.sym_occ.get(&s).copied().unwrap_or(0);
                if occ_s == 0 { continue; }
                let occ_f = occ_s as f32;

                if let Some(entries) = self.sym_to_axioms.get(&s) {
                    // Entries are sorted DESC by g_min.  `s` triggers axiom A
                    // iff occ(s) ≤ tolerance · g_min(A); once that fails it
                    // fails for every smaller g_min, so we can stop.
                    for &(gm, axiom_id) in entries {
                        if (gm as f32) * tolerance + TRIGGER_EPS < occ_f {
                            // Rejected: occ/gm is the smallest tolerance at which
                            // `s` admits one more axiom (list is descending).
                            if track_next {
                                let cand = occ_f / gm as f32;
                                next_breakpoint = Some(match next_breakpoint {
                                    Some(b) if b <= cand => b,
                                    _ => cand,
                                });
                            }
                            break;
                        }
                        if selected.insert(axiom_id) {
                            if let Some(c) = cap {
                                if selected.len() > c {
                                    over = true;
                                    break 'bfs;
                                }
                            }
                            if let Some(syms) = self.axiom_syms.get(&axiom_id) {
                                for &s2 in syms {
                                    if !visited_syms.contains(&s2) {
                                        next_wave.push(s2);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            for s in next_wave { frontier.push_back(s); }
            depth += 1;
        }

        crate::emit_event!(crate::progress::ProgressEvent::Log {
            level:   crate::progress::LogLevel::Debug,
            target:  "sigmakee_rs_core::sine",
            message: format!(
                "SineIndex::select: {} seed syms -> {} axioms ({} syms visited, depth {}, tolerance {}{})",
                seed_syms.len(), selected.len(), visited_syms.len(), depth, tolerance,
                if over { ", capped" } else { "" },
            ),
        });
        (selected, next_breakpoint, over)
    }

    /// The sorted, distinct tolerance breakpoints reachable from `seed_syms`,
    /// i.e. every tolerance value `> 1.0` at which the SInE selection grows,
    /// up to (and excluding past) `max_t`.
    ///
    /// Computed by repeatedly climbing to the next breakpoint reported by
    /// [`Self::select_reporting_next`] — no global enumeration of the index
    /// is required, so the cost is proportional to the number of breakpoints
    /// actually crossed (capped at [`MAX_CLIMB_STEPS`]).
    ///
    /// This is the conjecture-relevant restriction of the paper's
    /// `(A_i, t_i)` activation-threshold list (Hoder & Voronkov §4.1).
    pub fn tolerance_breakpoints(
        &self,
        seed_syms:   &HashSet<SymbolId>,
        depth_limit: Option<usize>,
        max_t:       f32,
    ) -> Vec<f32> {
        let mut out: Vec<f32> = Vec::new();
        let (_set, mut next) = self.select_reporting_next(seed_syms, 1.0, depth_limit);
        let mut steps = 0usize;
        while let Some(nt) = next {
            if nt > max_t || steps >= MAX_CLIMB_STEPS { break; }
            out.push(nt);
            let (_set, n2) = self.select_reporting_next(seed_syms, nt, depth_limit);
            // Guard against a non-advancing breakpoint (FP corner case).
            if n2.map_or(false, |v| v <= nt) { break; }
            next = n2;
            steps += 1;
        }
        out
    }

    /// Auto-tolerance selection: return the largest tolerance whose selected
    /// set stays at or below `budget` axioms, together with that selected set.
    ///
    /// Implemented as a **binary search on the tolerance axis**.  Because the
    /// selected set is monotone non-decreasing in tolerance, the in-budget /
    /// over-budget boundary is a single threshold, so bisection on
    /// `[1.0, MAX_AUTO_TOLERANCE]` finds it in `O(log)` probes regardless of
    /// how many activation breakpoints lie below the budget.  Every probe uses
    /// [`Self::select_capped`], so an over-budget probe aborts at `O(budget)`
    /// work and the "explosion" set above the knee is never fully computed.
    ///
    /// Returned guarantees:
    /// - If the strict floor `t = 1.0` already exceeds the budget, the floor
    ///   selection is returned as `(1.0, floor)` (nothing smaller exists).
    /// - If the whole reachable set fits the budget, the fixed point is
    ///   returned.
    /// - Otherwise `result.len() ≤ budget`, and the tolerance is the largest
    ///   (to within [`AUTO_TOLERANCE_RESOLUTION`]) that stays in budget.
    pub fn select_within_budget(
        &self,
        seed_syms:   &HashSet<SymbolId>,
        budget:      usize,
        depth_limit: Option<usize>,
    ) -> (f32, HashSet<SentenceId>) {
        // Strict floor.  If even this overruns, nothing smaller is achievable.
        let floor = self.select(seed_syms, 1.0, depth_limit);
        if floor.len() > budget {
            return (1.0, floor);
        }

        // Does the maximal selection (top of the tolerance range) already fit?
        // If so the whole reachable neighbourhood is within budget — return it.
        let (top_set, top_over) =
            self.select_capped(seed_syms, MAX_AUTO_TOLERANCE, depth_limit, budget);
        if !top_over {
            return (MAX_AUTO_TOLERANCE, top_set);
        }

        // Invariant: `lo` is in budget, `hi` is over budget.  Bisect.
        let mut lo = 1.0f32;
        let mut hi = MAX_AUTO_TOLERANCE;
        let mut steps = 0usize;
        while hi - lo > AUTO_TOLERANCE_RESOLUTION && steps < MAX_BISECT_STEPS {
            let mid = 0.5 * (lo + hi);
            if self.select_capped(seed_syms, mid, depth_limit, budget).1 {
                hi = mid; // over budget
            } else {
                lo = mid; // in budget
            }
            steps += 1;
        }

        // `lo` is the largest tolerance verified in budget; recompute its
        // (uncapped) selection — guaranteed ≤ budget, hence cheap.
        let best = self.select(seed_syms, lo, depth_limit);
        (lo, best)
    }
}
